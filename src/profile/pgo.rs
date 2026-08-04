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
// 当前 dcc 实现: 入口计数器 (instr-PGO 简化版).
//   * `instrument_program(prog, cfg)` 在每个 FuncDecl body 顶部插入
//     `__dcc_pgo_count_fn(idx)` 调用
//   * `_dcc_pgo_runtime` 累加 idx -> counter, atexit 写 `<name>.profraw`
//   * 读回 (`read_profraw`) 仅解析 u64 array, 不做权重传播
//   * 传播接口 `pgo::use_counts(prog, counts)` 留给后端, 当前仅
//     落 `pgo_weights: HashMap<func_name, u64>` 备查.

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
}
