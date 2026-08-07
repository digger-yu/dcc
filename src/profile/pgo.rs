// src/profile/pgo.rs - PGO (Profile-Guided Optimization)
//
// 两种数据流:
//   1. **instr-PGO** (instrumentation-based, `-fprofile-instr-generate` /
//      `-fprofile-instr-use`): AST/IR 改写, 在函数入口/边/条件跳转插入
//      `inc <counter>`, 运行后写 raw profile (`<name>.profraw`),
//      `llvm-profdata merge` 合并为 `<name>.profdata`,
//      再 `-fprofile-instr-use=<name>.profdata` 把 `count` 传给后端
//      (inliner / loop-unroll / 函数布局).
//   2. **frontend-PGO** (`-fprofile-generate` / `-fprofile-use`, GCC 风格):
//      使用 .gcda 形式的 counters, 编译时把每个 BB 的入口权重视为 hot/cold.
//
// 当前 dcc 实现: 入口计数器 (instr-PGO 简化版) + 下游权重传播.
//   * `instrument_program(prog, cfg)` 在每个 FuncDecl body 顶部插入
//     `__dcc_pgo_count_fn(idx)` 调用
//   * `_dcc_pgo_runtime` 累加 idx -> counter, atexit 写 `<name>.profraw`
//   * `PgoWeights::with_edge_counts(p)` 从 profraw 构建边计数
//   * `propagate_edge_weights(w)` 把边计数转为 BranchProbMap (分支概率)
//   * `propagate_loop_trip_counts(w)` 推算循环 trip count (供 unroll 决策)
//   * codegen 消费 BranchProbMap 做基本块布局 (hot path first)

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use crate::ast::{BlockItem, Decl, Expr, Program, Stmt, Type};

#[derive(Debug, Clone, Default)]
pub struct PgoConfig {
    /// `-fprofile-instr-generate[=dir]`
    pub generate: bool,
    /// `-fprofile-instr-use=file`
    pub use_file: Option<String>,
    /// 输出目录 (默认与源码同目录)
    pub out_dir: Option<String>,
}

impl PgoConfig {
    pub fn is_active(&self) -> bool {
        self.generate || self.use_file.is_some()
    }
}

/// 简单 profraw 容器: u64 counter per instrumented site.
#[derive(Debug, Clone, Default)]
pub struct Profraw {
    pub counters: Vec<u64>,
    /// 站点标签 (按插入顺序)
    pub sites: Vec<PgoSite>,
}

#[derive(Debug, Clone)]
pub struct PgoSite {
    pub func: String,
    pub file: String,
    pub line: u32,
}

impl Profraw {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, site: PgoSite) -> usize {
        let idx = self.counters.len();
        self.sites.push(site);
        self.counters.push(0);
        idx
    }

    /// 写 `<name>.profraw`。 layout: 魔数 0x70726F66 ("prof") + version +
    /// 站点数 + 站点数组 + 计数器数组.
    pub fn write(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"PROF");
        buf.extend_from_slice(&1u32.to_le_bytes()); // version
        buf.extend_from_slice(&(self.sites.len() as u32).to_le_bytes());
        for s in &self.sites {
            let f = s.func.as_bytes();
            buf.extend_from_slice(&(f.len() as u32).to_le_bytes());
            buf.extend_from_slice(f);
            let p = s.file.as_bytes();
            buf.extend_from_slice(&(p.len() as u32).to_le_bytes());
            buf.extend_from_slice(p);
            buf.extend_from_slice(&s.line.to_le_bytes());
        }
        buf.extend_from_slice(&(self.counters.len() as u32).to_le_bytes());
        for c in &self.counters {
            buf.extend_from_slice(&c.to_le_bytes());
        }
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        std::fs::write(out, &buf)?;
        Ok(buf.len())
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        if buf.len() < 12 || &buf[0..4] != b"PROF" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad profraw magic",
            ));
        }
        let ver = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if ver != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported profraw version: {ver}"),
            ));
        }
        let n = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
        let mut off = 12usize;
        let mut sites = Vec::with_capacity(n);
        for _ in 0..n {
            let lf = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            let func = std::str::from_utf8(&buf[off..off + lf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .to_string();
            off += lf;
            let lp = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            let file = std::str::from_utf8(&buf[off..off + lp])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .to_string();
            off += lp;
            let line = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            off += 4;
            sites.push(PgoSite { func, file, line });
        }
        let nc = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut counters = Vec::with_capacity(nc);
        for _ in 0..nc {
            let c = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            off += 8;
            counters.push(c);
        }
        Ok(Self { sites, counters })
    }
}

/// AST 改写: 在每个函数入口插入 `__dcc_pgo_count_fn(idx)` 调用
pub fn instrument_program(prog: &mut Program, cfg: &PgoConfig) {
    if !cfg.generate {
        return;
    }
    let mut idx: u32 = 0;
    for d in &mut prog.decls {
        if let Decl::Func(f) = d {
            if f.body.is_none() {
                continue;
            }
            // 复用函数体首项插入, 保证 PGO 计数先于任何 user code.
            let site = PgoSite {
                func: f.name.name.clone(),
                file: f.span.file.clone(),
                line: f.span.line,
            };
            // idx 仅供运行时分发, 这里以 span.line 作为值.
            let call = Expr::Call {
                callee: Box::new(Expr::Ident {
                    name: crate::ast::Ident {
                        name: "__dcc_pgo_count_fn".into(),
                        span: f.span.clone(),
                    },
                    span: f.span.clone(),
                    ty: Type::Void,
                }),
                args: vec![Expr::Int {
                    v: idx as i128,
                    span: f.span.clone(),
                    ty: Type::Int,
                }],
                span: f.span.clone(),
                ty: Type::Void,
            };
            // 包成 expr-stmt, push 到 body 顶部
            if let Some(body) = &mut f.body {
                body.items.insert(
                    0,
                    BlockItem::Stmt(Stmt::Expr {
                        e: call,
                        span: f.span.clone(),
                    }),
                );
            }
            idx += 1;
            let _ = site; // site 由 codegen 在另一通路落 .gcov/.profraw 索引
        }
    }
}

/// PGO 权重: 来自 profraw 的 per-function 计数聚合 (供后续 IR/CFG 优化读取).
#[derive(Debug, Clone, Default)]
pub struct PgoWeights {
    /// func_name -> 入口计数
    pub func_counts: HashMap<String, u64>,
    /// func_name -> 边平均计数
    pub edge_counts: HashMap<(String, u32, u32), u64>,
}

impl PgoWeights {
    pub fn from_profraw(p: &Profraw) -> Self {
        let mut w = Self::default();
        for (i, s) in p.sites.iter().enumerate() {
            let cnt = p.counters.get(i).copied().unwrap_or(0);
            *w.func_counts.entry(s.func.clone()).or_insert(0) += cnt;
        }
        w
    }

    pub fn count_of(&self, func: &str) -> u64 {
        self.func_counts.get(func).copied().unwrap_or(0)
    }

    /// 从 profraw 构建边计数 (edge-level counts).
    ///
    /// 边计数由 (func, src_bb, dst_bb) 三元组索引. profraw 中每个 site
    /// 对应一个基本块入口计数; 边计数由相邻块的计数差值推算:
    ///   edge_count(func, bb_i, bb_{i+1}) = counter[i+1]
    /// 这假设 profraw 的 sites 按基本块顺序排列 (与 GCC instr-PGO 一致).
    pub fn with_edge_counts(p: &Profraw) -> Self {
        let mut w = Self::from_profraw(p);
        // 按函数分组 sites, 计算边计数
        let mut func_sites: HashMap<String, Vec<(usize, u64)>> = HashMap::new();
        for (i, s) in p.sites.iter().enumerate() {
            let cnt = p.counters.get(i).copied().unwrap_or(0);
            func_sites.entry(s.func.clone()).or_default().push((i, cnt));
        }
        for (func, sites) in &func_sites {
            // 按 site 索引排序 (profraw 写入顺序即 BB 顺序)
            let mut sorted = sites.clone();
            sorted.sort_by_key(|(idx, _)| *idx);
            // 相邻 site 之间构成边: edge(bb_i -> bb_{i+1}) = counter[i+1]
            for window in sorted.windows(2) {
                let (_, src_cnt) = window[0];
                let (dst_idx, dst_cnt) = window[1];
                // 边计数 = 目标块入口计数 (流入 = 执行次数)
                // 归一化为 (func, src_bb_local, dst_bb_local)
                let src_local = window[0].0 - sorted[0].0;
                let dst_local = dst_idx - sorted[0].0;
                // 边计数取 min(src_cnt, dst_cnt) 防止不一致
                let edge_cnt = src_cnt.min(dst_cnt);
                w.edge_counts.insert(
                    (func.clone(), src_local as u32, dst_local as u32),
                    edge_cnt,
                );
                let _ = src_cnt; // 保留源计数供后续扩展
            }
        }
        w
    }
}

/// 分支概率元数据: 供 codegen 做基本块布局 (hot path first).
///
/// 设计: 与 LLVM `BranchProbabilityInfo` 类似, 每个条件分支有
/// (true_prob, false_prob) 权重. codegen 用这些权重决定
/// 哪个分支放到 fall-through 位置 (减少跳转).
#[derive(Debug, Clone, Default)]
pub struct BranchProbMap {
    /// (func_name, bb_index) -> (true_weight, false_weight)
    /// weight 为归一化前的原始计数, codegen 自行计算比例
    pub branch_weights: HashMap<(String, u32), (u64, u64)>,
}

impl BranchProbMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取分支权重, 返回 (true_weight, false_weight).
    /// 未知分支返回 (1, 1) 表示等概率.
    pub fn get_weight(&self, func: &str, bb: u32) -> (u64, u64) {
        self.branch_weights
            .get(&(func.to_string(), bb))
            .copied()
            .unwrap_or((1, 1))
    }

    /// 计算 true 分支概率 (0.0 ~ 1.0)
    pub fn true_probability(&self, func: &str, bb: u32) -> f64 {
        let (t, f) = self.get_weight(func, bb);
        let total = t + f;
        if total == 0 {
            0.5
        } else {
            t as f64 / total as f64
        }
    }

    /// 判断分支是否为 hot (true 概率 > 阈值)
    pub fn is_hot_true(&self, func: &str, bb: u32, threshold: f64) -> bool {
        self.true_probability(func, bb) > threshold
    }
}

/// PGO 权重下游传播: 把 edge_counts 转换为 BranchProbMap.
///
/// 这是 `pgo::use_counts(prog, counts)` 的实现. 把 profraw 中的边计数
/// 传播到每个函数的条件分支, 供 codegen 做基本块布局优化.
///
/// 算法:
///   1. 对每个函数, 找到所有 (func, bb, _) 边
///   2. 假设 bb 是条件分支点, 有 true-edge (bb -> bb+1) 和 false-edge (bb -> bb+2)
///   3. 从 edge_counts 读取两个方向的计数, 存入 BranchProbMap
pub fn propagate_edge_weights(weights: &PgoWeights) -> BranchProbMap {
    let mut map = BranchProbMap::new();

    // 按函数分组边
    let mut func_edges: HashMap<String, Vec<(u32, u32, u64)>> = HashMap::new();
    for ((func, src, dst), &cnt) in &weights.edge_counts {
        func_edges.entry(func.clone()).or_default().push((*src, *dst, cnt));
    }

    for (func, edges) in &func_edges {
        // 对每个源块, 收集所有出边
        let mut src_edges: HashMap<u32, Vec<(u32, u64)>> = HashMap::new();
        for &(src, dst, cnt) in edges {
            src_edges.entry(src).or_default().push((dst, cnt));
        }

        // 对每个条件分支点 (有 2 条出边的块), 计算权重
        for (src, dsts) in &src_edges {
            if dsts.len() == 2 {
                // 假设 dst[0] 是 true 分支 (fall-through), dst[1] 是 false 分支
                let sorted: Vec<_> = {
                    let mut v = dsts.clone();
                    v.sort_by_key(|(dst, _)| *dst);
                    v
                };
                let true_cnt = sorted[0].1; // 较小 dst = true (fall-through)
                let false_cnt = sorted[1].1; // 较大 dst = false (jump target)
                map.branch_weights.insert((func.clone(), *src), (true_cnt, false_cnt));
            }
        }
    }

    map
}

/// 循环trip count 提示: 从 back-edge 计数推算循环迭代次数.
///
/// 用于 loop unroll 决策: 如果循环平均迭代次数 < 4, 不展开;
/// 如果 > 16, 激进展开.
#[derive(Debug, Clone, Default)]
pub struct LoopTripCounts {
    /// (func_name, header_bb) -> 平均 trip count
    pub trip_counts: HashMap<(String, u32), f64>,
}

impl LoopTripCounts {
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取循环 trip count, 未知返回 0.0
    pub fn get_trip_count(&self, func: &str, header_bb: u32) -> f64 {
        self.trip_counts
            .get(&(func.to_string(), header_bb))
            .copied()
            .unwrap_or(0.0)
    }

    /// 判断循环是否值得展开 (trip count > threshold)
    pub fn should_unroll(&self, func: &str, header_bb: u32, threshold: f64) -> bool {
        self.get_trip_count(func, header_bb) > threshold
    }
}

/// 从 edge_counts 推算循环 trip count.
///
/// 算法: 找到 back-edge (dst_bb <= src_bb), trip_count = back_edge_count / entry_count.
pub fn propagate_loop_trip_counts(weights: &PgoWeights) -> LoopTripCounts {
    let mut counts = LoopTripCounts::new();

    // 按函数分组边
    let mut func_edges: HashMap<String, Vec<(u32, u32, u64)>> = HashMap::new();
    for ((func, src, dst), &cnt) in &weights.edge_counts {
        func_edges.entry(func.clone()).or_default().push((*src, *dst, cnt));
    }

    for (func, edges) in &func_edges {
        // 找 back-edges: dst <= src (循环回边)
        let mut back_edges: HashMap<u32, u64> = HashMap::new(); // header -> back_edge_count
        let mut entry_edges: HashMap<u32, u64> = HashMap::new(); // header -> entry_count

        for &(src, dst, cnt) in edges {
            if dst <= src {
                // back-edge: dst 是循环头
                *back_edges.entry(dst).or_insert(0) += cnt;
            } else if dst > src && src == 0 || dst == 1 && src == 0 {
                // entry edge: 从入口到循环头
                *entry_edges.entry(dst).or_insert(0) += cnt;
            }
        }

        // trip_count = back_edge_count / entry_count
        for (&header, &back_cnt) in &back_edges {
            let entry_cnt = entry_edges.get(&header).copied().unwrap_or(1).max(1);
            let trip = back_cnt as f64 / entry_cnt as f64;
            counts.trip_counts.insert((func.clone(), header), trip);
        }
    }

    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profraw_roundtrip() {
        let mut p = Profraw::new();
        p.add(PgoSite {
            func: "main".into(),
            file: "m.c".into(),
            line: 1,
        });
        p.add(PgoSite {
            func: "foo".into(),
            file: "m.c".into(),
            line: 5,
        });
        p.counters[0] = 7;
        p.counters[1] = 11;
        let tmp = std::env::temp_dir().join("dcc-pgo-test.profraw");
        let _ = std::fs::remove_file(&tmp);
        p.write(&tmp).unwrap();
        let q = Profraw::read(&tmp).unwrap();
        assert_eq!(q.sites.len(), 2);
        assert_eq!(q.counters, vec![7, 11]);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn weights_aggregate_by_func() {
        let mut p = Profraw::new();
        p.add(PgoSite {
            func: "main".into(),
            file: "m.c".into(),
            line: 1,
        });
        p.add(PgoSite {
            func: "main".into(),
            file: "m.c".into(),
            line: 3,
        });
        p.add(PgoSite {
            func: "foo".into(),
            file: "m.c".into(),
            line: 7,
        });
        p.counters[0] = 4;
        p.counters[1] = 6;
        p.counters[2] = 9;
        let w = PgoWeights::from_profraw(&p);
        assert_eq!(w.count_of("main"), 10);
        assert_eq!(w.count_of("foo"), 9);
    }

    /// 验证 instrument_program 会在每个有 body 的函数顶部插入
    /// `__dcc_pgo_count_fn(idx)` 调用. 这是 PGO "codegen 阶段自动
    /// emit 计数器" 的前端桥: AST 改写后被 IR lower 与 codegen
    /// 消费, 最终 emit 为 `call __dcc_pgo_count_fn` 指令.
    #[test]
    fn instrument_program_inserts_call_at_body_top() {
        use crate::ast::{Block, BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type};
        let span_zero = crate::diagnostics::Span::point("m.c", 1, 0);
        let mut prog = Program { decls: vec![] };
        let body = Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Int {
                    v: 0,
                    span: span_zero.clone(),
                    ty: Type::Int,
                }),
                span: span_zero.clone(),
            })],
            span: span_zero.clone(),
        };
        prog.decls.push(Decl::Func(FuncDecl {
            name: Ident {
                name: "foo".into(),
                span: span_zero.clone(),
            },
            ret_ty: Type::Int,
            params: vec![],
            body: Some(body),
            span: span_zero.clone(),
            is_extern: false,
            is_static: false,
            is_unused: false,
            is_cold: false,
            is_hot: false,
            is_noinline: false,
            is_always_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            is_weak: false,
            is_nested: false,
            upvars: vec![],
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            is_global: false,
            is_device: false,
            is_host: false,
            section: None,
            alignment: None,
            alias_name: None,
            visibility: None,
        }));
        // 旧 body 顶部是 return 0, 跑 instrument 后应变为
        // call __dcc_pgo_count_fn(0)  +  return 0
        let cfg = PgoConfig {
            generate: true,
            use_file: None,
            out_dir: None,
        };
        instrument_program(&mut prog, &cfg);
        // 1 个函数 → 0 号 idx
        if let Decl::Func(f) = &prog.decls[0] {
            let body = f.body.as_ref().unwrap();
            assert_eq!(body.items.len(), 2, "instrument 应在原 1 项前插入 1 项");
            match &body.items[0] {
                BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                    if let Expr::Call { callee, args, .. } = e {
                        if let Expr::Ident { name, .. } = callee.as_ref() {
                            assert_eq!(name.name, "__dcc_pgo_count_fn");
                        } else {
                            panic!("callee not Ident");
                        }
                        assert_eq!(args.len(), 1);
                        if let Expr::Int { v, .. } = &args[0] {
                            assert_eq!(*v, 0, "第一个函数的 idx 应为 0");
                        } else {
                            panic!("arg not Int");
                        }
                    } else {
                        panic!("first stmt not Call");
                    }
                }
                _ => panic!("first item not Stmt::Expr"),
            }
        } else {
            panic!("decl not Func");
        }
    }

    /// instrument_program 对 generate=false 不做任何改写.
    #[test]
    fn instrument_program_noop_when_disabled() {
        use crate::ast::{Block, BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type};
        let span_zero = crate::diagnostics::Span::point("m.c", 1, 0);
        let mut prog = Program { decls: vec![] };
        let body = Block {
            items: vec![BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Int {
                    v: 7,
                    span: span_zero.clone(),
                    ty: Type::Int,
                }),
                span: span_zero.clone(),
            })],
            span: span_zero.clone(),
        };
        prog.decls.push(Decl::Func(FuncDecl {
            name: Ident {
                name: "foo".into(),
                span: span_zero.clone(),
            },
            ret_ty: Type::Int,
            params: vec![],
            body: Some(body),
            span: span_zero.clone(),
            is_extern: false,
            is_static: false,
            is_unused: false,
            is_cold: false,
            is_hot: false,
            is_noinline: false,
            is_always_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            is_weak: false,
            is_nested: false,
            upvars: vec![],
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            is_global: false,
            is_device: false,
            is_host: false,
            section: None,
            alignment: None,
            alias_name: None,
            visibility: None,
        }));
        let cfg = PgoConfig {
            generate: false,
            use_file: None,
            out_dir: None,
        };
        instrument_program(&mut prog, &cfg);
        if let Decl::Func(f) = &prog.decls[0] {
            assert_eq!(
                f.body.as_ref().unwrap().items.len(),
                1,
                "generate=false 时 body 不应被改写"
            );
        }
    }

    /// 测试 edge_counts 构建: 从 profraw 的 sites 推算边计数
    #[test]
    fn with_edge_counts_computes_edges() {
        let mut p = Profraw::new();
        // main 函数有 3 个 sites (BB0, BB1, BB2)
        p.add(PgoSite { func: "main".into(), file: "m.c".into(), line: 1 });
        p.add(PgoSite { func: "main".into(), file: "m.c".into(), line: 2 });
        p.add(PgoSite { func: "main".into(), file: "m.c".into(), line: 3 });
        p.counters[0] = 100; // BB0 执行 100 次
        p.counters[1] = 80;  // BB1 执行 80 次
        p.counters[2] = 60;  // BB2 执行 60 次

        let w = PgoWeights::with_edge_counts(&p);
        // edge(BB0->BB1) = min(100, 80) = 80
        assert_eq!(w.edge_counts.get(&("main".into(), 0, 1)), Some(&80));
        // edge(BB1->BB2) = min(80, 60) = 60
        assert_eq!(w.edge_counts.get(&("main".into(), 1, 2)), Some(&60));
    }

    /// 测试分支概率传播: 2 条出边 -> (true_weight, false_weight)
    #[test]
    fn propagate_edge_weights_branch_prob() {
        let mut w = PgoWeights::default();
        // foo 函数: BB0 有 2 条出边 -> BB1 (true), BB2 (false)
        w.edge_counts.insert(("foo".into(), 0, 1), 90);  // true 分支
        w.edge_counts.insert(("foo".into(), 0, 2), 10);  // false 分支

        let map = propagate_edge_weights(&w);
        let (t, f) = map.get_weight("foo", 0);
        assert_eq!(t, 90);
        assert_eq!(f, 10);
        // true 概率 = 90 / (90 + 10) = 0.9
        let prob = map.true_probability("foo", 0);
        assert!((prob - 0.9).abs() < 0.01);
        assert!(map.is_hot_true("foo", 0, 0.8));
    }

    /// 测试未知分支返回默认权重 (1, 1)
    #[test]
    fn branch_prob_unknown_returns_default() {
        let map = BranchProbMap::new();
        let (t, f) = map.get_weight("unknown_func", 0);
        assert_eq!(t, 1);
        assert_eq!(f, 1);
        // 默认概率 = 0.5
        let prob = map.true_probability("unknown_func", 0);
        assert!((prob - 0.5).abs() < 0.01);
    }

    /// 测试循环 trip count 推算
    #[test]
    fn test_propagate_loop_trip_counts() {
        let mut w = PgoWeights::default();
        // loop 函数: BB0 (entry) -> BB1 (header)
        //           BB1 -> BB2 (body), BB1 -> BB1 (back-edge, 循环 10 次)
        w.edge_counts.insert(("loop".into(), 0, 1), 10);  // entry
        w.edge_counts.insert(("loop".into(), 1, 2), 100); // body
        w.edge_counts.insert(("loop".into(), 1, 1), 90);  // back-edge (90 次回到 header)

        let counts = propagate_loop_trip_counts(&w);
        // trip_count = back_edge_count / entry_count = 90 / 10 = 9.0
        let trip = counts.get_trip_count("loop", 1);
        assert!((trip - 9.0).abs() < 0.1);
        // 应该展开 (trip > 4)
        assert!(counts.should_unroll("loop", 1, 4.0));
        // 不应该展开 (trip < 16)
        assert!(!counts.should_unroll("loop", 1, 16.0));
    }

    /// 测试未知循环返回 0.0 trip count
    #[test]
    fn loop_trip_counts_unknown_returns_zero() {
        let counts = LoopTripCounts::new();
        assert_eq!(counts.get_trip_count("unknown", 0), 0.0);
        assert!(!counts.should_unroll("unknown", 0, 4.0));
    }
}
