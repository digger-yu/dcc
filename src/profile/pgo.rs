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
// dcc 实现 (v2 格式扩展):
//   * `instrument_program(prog, cfg)` 在每个 FuncDecl body 顶部插入
//     `__dcc_pgo_count_fn(idx)` 调用
//   * `_dcc_pgo_runtime` 累加 idx -> counter, atexit 写 `<name>.profraw`
//   * `PgoWeights::with_edge_counts(p)` 从 profraw 构建边计数
//   * `propagate_edge_weights(w)` 把边计数转为 BranchProbMap (分支概率)
//   * `propagate_loop_trip_counts(w)` 推算循环 trip count (供 unroll 决策)
//   * codegen 消费 BranchProbMap 做基本块布局 (hot path first)
//
// v2 格式增强:
//   * Value profiling: 每个 site 可附带 call target 计数表
//     (间接调用目标 -> 次数), 供 indirect call promotion 使用
//   * Checksum: FNV-1a 哈希所有 site 信息, 检测 profraw 文件损坏
//   * Merge: 多个 profraw 合并为一个 (计数器累加)
//   * Profdata: 合并后的优化格式 (去重 + 排序 + 压缩站点表)

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use crate::ast::{BlockItem, Decl, Expr, Program, Stmt, Type};

/// profraw 格式版本
pub const PROFRAW_VERSION_V1: u32 = 1;
pub const PROFRAW_VERSION_V2: u32 = 2;
pub const PROFRAW_VERSION_CURRENT: u32 = PROFRAW_VERSION_V2;

/// profdata magic (merged format)
pub const PROFDATA_MAGIC: &[u8; 4] = b"PRFD";
pub const PROFDATA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default)]
pub struct PgoConfig {
    /// `-fprofile-instr-generate[=dir]`
    pub generate: bool,
    /// `-fprofile-values`: instrument basic-block value sampling sites
    pub value_sampling: bool,
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

/// Value profiling entry: indirect call target counts for a site.
/// Used for indirect call promotion optimization.
#[derive(Debug, Clone, Default)]
pub struct ValueProfileData {
    /// (target_func_name -> count) for indirect call sites
    pub call_targets: Vec<(String, u64)>,
}

impl ValueProfileData {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a call target observation
    pub fn add_target(&mut self, target: &str, count: u64) {
        if let Some(entry) = self.call_targets.iter_mut().find(|(n, _)| n == target) {
            entry.1 += count;
        } else {
            self.call_targets.push((target.to_string(), count));
        }
    }

    /// Get the most likely call target (highest count)
    pub fn top_target(&self) -> Option<(&str, u64)> {
        self.call_targets
            .iter()
            .max_by_key(|(_, c)| *c)
            .map(|(n, c)| (n.as_str(), *c))
    }

    /// Total count across all targets
    pub fn total_count(&self) -> u64 {
        self.call_targets.iter().map(|(_, c)| *c).sum()
    }
}

/// 简单 profraw 容器: u64 counter per instrumented site.
#[derive(Debug, Clone, Default)]
pub struct Profraw {
    pub counters: Vec<u64>,
    /// 站点标签 (按插入顺序)
    pub sites: Vec<PgoSite>,
    /// v2: value profiling data per site (indexed by site index)
    pub value_profiles: HashMap<usize, ValueProfileData>,
    /// v2: FNV-1a checksum of all sites for integrity verification
    pub checksum: u64,
    /// Format version (1 or 2)
    pub version: u32,
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
        self.version = PROFRAW_VERSION_CURRENT;
        idx
    }

    /// Add value profiling data for a specific site
    pub fn add_value_profile(&mut self, site_idx: usize, data: ValueProfileData) {
        if site_idx < self.sites.len() {
            self.value_profiles.insert(site_idx, data);
        }
    }

    /// Compute FNV-1a checksum of all site information
    fn compute_checksum(sites: &[PgoSite]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
        for site in sites {
            for byte in site.func.as_bytes() {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
            }
            for byte in site.file.as_bytes() {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            hash ^= site.line as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Finalize the profraw: compute checksum and set version
    pub fn finalize(&mut self) {
        self.checksum = Self::compute_checksum(&self.sites);
        self.version = PROFRAW_VERSION_CURRENT;
    }

    /// Write profraw v2 format with extended fields.
    /// Layout:
    ///   magic    4B  "PROF"
    ///   version  u32 2
    ///   checksum u64 FNV-1a of all sites
    ///   n_sites  u32 N
    ///   site[]   N * { func_len u32, func, file_len u32, file, line u32 }
    ///   n_vp     u32 number of sites with value profiles
    ///   vp[]     n_vp * { site_idx u32, n_targets u32,
    ///                     targets[]: { name_len u32, name, count u64 } }
    ///   n_counters u32 M
    ///   counter[]  M * u64
    pub fn write(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"PROF");
        let version = if self.version >= PROFRAW_VERSION_V2 {
            PROFRAW_VERSION_V2
        } else {
            PROFRAW_VERSION_V1
        };
        buf.extend_from_slice(&version.to_le_bytes());

        if version >= PROFRAW_VERSION_V2 {
            let checksum = if self.checksum != 0 {
                self.checksum
            } else {
                Self::compute_checksum(&self.sites)
            };
            buf.extend_from_slice(&checksum.to_le_bytes());
        }

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

        if version >= PROFRAW_VERSION_V2 {
            // Write value profiles
            let vp_count = self.value_profiles.len() as u32;
            buf.extend_from_slice(&vp_count.to_le_bytes());
            for (&site_idx, vp) in &self.value_profiles {
                buf.extend_from_slice(&(site_idx as u32).to_le_bytes());
                buf.extend_from_slice(&(vp.call_targets.len() as u32).to_le_bytes());
                for (target, count) in &vp.call_targets {
                    let t = target.as_bytes();
                    buf.extend_from_slice(&(t.len() as u32).to_le_bytes());
                    buf.extend_from_slice(t);
                    buf.extend_from_slice(&count.to_le_bytes());
                }
            }
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
        if ver != PROFRAW_VERSION_V1 && ver != PROFRAW_VERSION_V2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported profraw version: {ver}"),
            ));
        }
        let mut off = 8usize;

        // v2: read checksum
        let mut checksum: u64 = 0;
        if ver >= PROFRAW_VERSION_V2 {
            if off + 8 > buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated profraw v2 checksum",
                ));
            }
            checksum = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            off += 8;
        }

        if off + 4 > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated profraw site count",
            ));
        }
        let n = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
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

        // Verify checksum
        if ver >= PROFRAW_VERSION_V2 && checksum != 0 {
            let computed = Self::compute_checksum(&sites);
            if computed != checksum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "profraw checksum mismatch: expected {checksum:016x}, got {computed:016x}"
                    ),
                ));
            }
        }

        // v2: read value profiles
        let mut value_profiles = HashMap::new();
        if ver >= PROFRAW_VERSION_V2 && off + 4 <= buf.len() {
            let n_vp = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            for _ in 0..n_vp {
                if off + 8 > buf.len() {
                    break;
                }
                let site_idx = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                let n_targets = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                let mut vp = ValueProfileData::new();
                for _ in 0..n_targets {
                    if off + 4 > buf.len() {
                        break;
                    }
                    let tl = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                    off += 4;
                    if off + tl + 8 > buf.len() {
                        break;
                    }
                    let target = std::str::from_utf8(&buf[off..off + tl])
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                        .to_string();
                    off += tl;
                    let count = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
                    off += 8;
                    vp.add_target(&target, count);
                }
                value_profiles.insert(site_idx, vp);
            }
        }

        if off + 4 > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated profraw counter count",
            ));
        }
        let nc = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut counters = Vec::with_capacity(nc);
        for _ in 0..nc {
            if off + 8 > buf.len() {
                break;
            }
            let c = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            off += 8;
            counters.push(c);
        }
        Ok(Self {
            sites,
            counters,
            value_profiles,
            checksum,
            version: ver,
        })
    }

    /// Merge another profraw into self (counter accumulation).
    /// Both must have the same sites (same checksum); returns error otherwise.
    pub fn merge(&mut self, other: &Profraw) -> io::Result<()> {
        if self.sites.len() != other.sites.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "profraw merge: site count mismatch ({} vs {})",
                    self.sites.len(),
                    other.sites.len()
                ),
            ));
        }
        // Verify sites match
        for (i, (a, b)) in self.sites.iter().zip(other.sites.iter()).enumerate() {
            if a.func != b.func || a.file != b.file || a.line != b.line {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("profraw merge: site {} mismatch", i),
                ));
            }
        }
        // Accumulate counters
        for (i, c) in other.counters.iter().enumerate() {
            if i < self.counters.len() {
                self.counters[i] = self.counters[i].saturating_add(*c);
            }
        }
        // Merge value profiles
        for (&idx, vp) in &other.value_profiles {
            let entry = self.value_profiles.entry(idx).or_default();
            for (target, count) in &vp.call_targets {
                entry.add_target(target, *count);
            }
        }
        // Recompute checksum
        self.checksum = Self::compute_checksum(&self.sites);
        self.version = PROFRAW_VERSION_CURRENT;
        Ok(())
    }

    /// Write merged profdata format (optimized for -fprofile-use).
    /// Layout:
    ///   magic    4B  "PRFD"
    ///   version  u32 1
    ///   n_funcs  u32 (unique function count)
    ///   func[]   n_funcs * { name_len u32, name, total_count u64,
    ///                        n_sites u32, site_indices u32[] }
    ///   n_counters u32
    ///   counter[]  u64[]
    ///   n_vp     u32
    ///   vp[]     same as profraw v2
    pub fn write_profdata(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(PROFDATA_MAGIC);
        buf.extend_from_slice(&PROFDATA_VERSION.to_le_bytes());

        // Aggregate by function
        let mut func_map: HashMap<String, (u64, Vec<usize>)> = HashMap::new();
        for (i, s) in self.sites.iter().enumerate() {
            let entry = func_map.entry(s.func.clone()).or_insert((0, Vec::new()));
            entry.0 += self.counters.get(i).copied().unwrap_or(0);
            entry.1.push(i);
        }

        buf.extend_from_slice(&(func_map.len() as u32).to_le_bytes());
        // Sort functions by total count (descending) for better locality
        let mut func_list: Vec<_> = func_map.into_iter().collect();
        func_list.sort_by_key(|(_, (total, _))| std::cmp::Reverse(*total));
        for (name, (total, indices)) in &func_list {
            let nb = name.as_bytes();
            buf.extend_from_slice(&(nb.len() as u32).to_le_bytes());
            buf.extend_from_slice(nb);
            buf.extend_from_slice(&total.to_le_bytes());
            buf.extend_from_slice(&(indices.len() as u32).to_le_bytes());
            for &idx in indices {
                buf.extend_from_slice(&(idx as u32).to_le_bytes());
            }
        }

        // Counters
        buf.extend_from_slice(&(self.counters.len() as u32).to_le_bytes());
        for c in &self.counters {
            buf.extend_from_slice(&c.to_le_bytes());
        }

        // Value profiles
        buf.extend_from_slice(&(self.value_profiles.len() as u32).to_le_bytes());
        for (&site_idx, vp) in &self.value_profiles {
            buf.extend_from_slice(&(site_idx as u32).to_le_bytes());
            buf.extend_from_slice(&(vp.call_targets.len() as u32).to_le_bytes());
            for (target, count) in &vp.call_targets {
                let t = target.as_bytes();
                buf.extend_from_slice(&(t.len() as u32).to_le_bytes());
                buf.extend_from_slice(t);
                buf.extend_from_slice(&count.to_le_bytes());
            }
        }

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        std::fs::write(out, &buf)?;
        Ok(buf.len())
    }

    /// Read merged profdata format
    pub fn read_profdata(path: &Path) -> io::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        if buf.len() < 8 || &buf[0..4] != PROFDATA_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad profdata magic",
            ));
        }
        let ver = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if ver != PROFDATA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported profdata version: {ver}"),
            ));
        }
        let mut off = 8usize;

        let n_funcs = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut sites = Vec::new();
        for _ in 0..n_funcs {
            let nl = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            let _name = std::str::from_utf8(&buf[off..off + nl])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .to_string();
            off += nl;
            let _total = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            off += 8;
            let n_sites = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            for _ in 0..n_sites {
                let _idx = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
                off += 4;
                // Reconstruct site from func name
                sites.push(PgoSite {
                    func: _name.clone(),
                    file: String::new(),
                    line: 0,
                });
            }
        }

        let nc = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut counters = Vec::with_capacity(nc);
        for _ in 0..nc {
            if off + 8 > buf.len() {
                break;
            }
            let c = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            off += 8;
            counters.push(c);
        }

        // Value profiles
        let mut value_profiles = HashMap::new();
        if off + 4 <= buf.len() {
            let n_vp = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            for _ in 0..n_vp {
                if off + 8 > buf.len() {
                    break;
                }
                let site_idx = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                let n_targets = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                let mut vp = ValueProfileData::new();
                for _ in 0..n_targets {
                    if off + 4 > buf.len() {
                        break;
                    }
                    let tl = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                    off += 4;
                    if off + tl + 8 > buf.len() {
                        break;
                    }
                    let target = std::str::from_utf8(&buf[off..off + tl])
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                        .to_string();
                    off += tl;
                    let count = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
                    off += 8;
                    vp.add_target(&target, count);
                }
                value_profiles.insert(site_idx, vp);
            }
        }

        Ok(Self {
            sites,
            counters,
            value_profiles,
            checksum: 0,
            version: PROFRAW_VERSION_V2,
        })
    }
}

/// AST 改写: 在每个函数入口插入 `__dcc_pgo_count_fn(idx)` 调用
pub fn instrument_program(prog: &mut Program, cfg: &PgoConfig) {
    if !cfg.generate && !cfg.value_sampling {
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
            if let Some(body) = &mut f.body {
                if cfg.value_sampling {
                    let sample = Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: crate::ast::Ident {
                                name: "__dcc_pgo_value_sample".into(),
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
                    body.items.insert(
                        0,
                        BlockItem::Stmt(Stmt::Expr {
                            e: sample,
                            span: f.span.clone(),
                        }),
                    );
                }
                if cfg.generate {
                    body.items.insert(
                        0,
                        BlockItem::Stmt(Stmt::Expr {
                            e: call,
                            span: f.span.clone(),
                        }),
                    );
                }
            }
            idx += 1;
            let _ = site;
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
                w.edge_counts
                    .insert((func.clone(), src_local as u32, dst_local as u32), edge_cnt);
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
        func_edges
            .entry(func.clone())
            .or_default()
            .push((*src, *dst, cnt));
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
                map.branch_weights
                    .insert((func.clone(), *src), (true_cnt, false_cnt));
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
        func_edges
            .entry(func.clone())
            .or_default()
            .push((*src, *dst, cnt));
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

    #[test]
    fn instrument_program_value_sampling_inserts_site_call() {
        use crate::ast::{Block, BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type};
        let span = crate::diagnostics::Span::point("v.c", 1, 0);
        let mut prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                name: Ident {
                    name: "main".into(),
                    span: span.clone(),
                },
                ret_ty: Type::Int,
                params: vec![],
                body: Some(Block {
                    items: vec![],
                    span: span.clone(),
                }),
                span: span.clone(),
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
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
                section: None,
                alignment: None,
                alias_name: None,
                visibility: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        instrument_program(
            &mut prog,
            &PgoConfig {
                generate: false,
                value_sampling: true,
                use_file: None,
                out_dir: None,
            },
        );
        let Decl::Func(f) = &prog.decls[0] else {
            panic!("expected function");
        };
        let BlockItem::Stmt(Stmt::Expr { e, .. }) = &f.body.as_ref().unwrap().items[0] else {
            panic!("expected sampling statement");
        };
        let Expr::Call { callee, args, .. } = e else {
            panic!("expected sampling call");
        };
        let Expr::Ident { name, .. } = callee.as_ref() else {
            panic!("expected sampling callee");
        };
        assert_eq!(name.name, "__dcc_pgo_value_sample");
        assert_eq!(args.len(), 1);
    }

    /// `__dcc_pgo_count_fn(idx)` 调用. 这是 PGO "codegen 阶段自动
    /// emit 计数器" 的前端桥: AST 改写后被 IR lower 与 codegen
    /// 消费, 最终 emit 为 `call __dcc_pgo_count_fn` 指令.
    #[test]
    fn instrument_program_inserts_call_at_body_top() {
        use crate::ast::{Block, BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type};
        let span_zero = crate::diagnostics::Span::point("m.c", 1, 0);
        let mut prog = Program {
            decls: vec![],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
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
            asm_output_template: None,
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
            value_sampling: false,
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
        let mut prog = Program {
            decls: vec![],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
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
            asm_output_template: None,
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
            value_sampling: false,
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
        p.add(PgoSite {
            func: "main".into(),
            file: "m.c".into(),
            line: 1,
        });
        p.add(PgoSite {
            func: "main".into(),
            file: "m.c".into(),
            line: 2,
        });
        p.add(PgoSite {
            func: "main".into(),
            file: "m.c".into(),
            line: 3,
        });
        p.counters[0] = 100; // BB0 执行 100 次
        p.counters[1] = 80; // BB1 执行 80 次
        p.counters[2] = 60; // BB2 执行 60 次

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
        w.edge_counts.insert(("foo".into(), 0, 1), 90); // true 分支
        w.edge_counts.insert(("foo".into(), 0, 2), 10); // false 分支

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
        w.edge_counts.insert(("loop".into(), 0, 1), 10); // entry
        w.edge_counts.insert(("loop".into(), 1, 2), 100); // body
        w.edge_counts.insert(("loop".into(), 1, 1), 90); // back-edge (90 次回到 header)

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

    /// Test profraw v2 format roundtrip with checksum
    #[test]
    fn profraw_v2_roundtrip_with_checksum() {
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
        p.counters[0] = 42;
        p.counters[1] = 77;
        p.finalize();
        assert_ne!(p.checksum, 0);

        let tmp = std::env::temp_dir().join("dcc-pgo-v2-test.profraw");
        let _ = std::fs::remove_file(&tmp);
        p.write(&tmp).unwrap();
        let q = Profraw::read(&tmp).unwrap();
        assert_eq!(q.version, PROFRAW_VERSION_V2);
        assert_eq!(q.sites.len(), 2);
        assert_eq!(q.counters, vec![42, 77]);
        assert_ne!(q.checksum, 0);
        assert_eq!(q.checksum, p.checksum);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Test value profiling data roundtrip
    #[test]
    fn profraw_v2_value_profiles_roundtrip() {
        let mut p = Profraw::new();
        let idx = p.add(PgoSite {
            func: "caller".into(),
            file: "m.c".into(),
            line: 10,
        });
        p.counters[0] = 100;
        let mut vp = ValueProfileData::new();
        vp.add_target("target_a", 60);
        vp.add_target("target_b", 30);
        vp.add_target("target_a", 10); // accumulate
        p.add_value_profile(idx, vp);
        p.finalize();

        let tmp = std::env::temp_dir().join("dcc-pgo-vp-test.profraw");
        let _ = std::fs::remove_file(&tmp);
        p.write(&tmp).unwrap();
        let q = Profraw::read(&tmp).unwrap();
        assert_eq!(q.value_profiles.len(), 1);
        let vp2 = &q.value_profiles[&idx];
        assert_eq!(vp2.call_targets.len(), 2);
        assert_eq!(vp2.total_count(), 100);
        let (top, count) = vp2.top_target().unwrap();
        assert_eq!(top, "target_a");
        assert_eq!(count, 70);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Test profraw merge: two files with same sites
    #[test]
    fn profraw_merge_accumulates_counters() {
        let mut a = Profraw::new();
        a.add(PgoSite {
            func: "f".into(),
            file: "a.c".into(),
            line: 1,
        });
        a.add(PgoSite {
            func: "g".into(),
            file: "a.c".into(),
            line: 5,
        });
        a.counters[0] = 10;
        a.counters[1] = 20;
        a.finalize();

        let mut b = Profraw::new();
        b.add(PgoSite {
            func: "f".into(),
            file: "a.c".into(),
            line: 1,
        });
        b.add(PgoSite {
            func: "g".into(),
            file: "a.c".into(),
            line: 5,
        });
        b.counters[0] = 5;
        b.counters[1] = 15;
        b.finalize();

        a.merge(&b).unwrap();
        assert_eq!(a.counters[0], 15);
        assert_eq!(a.counters[1], 35);
    }

    /// Test profraw merge rejects mismatched sites
    #[test]
    fn profraw_merge_rejects_mismatched_sites() {
        let mut a = Profraw::new();
        a.add(PgoSite {
            func: "f".into(),
            file: "a.c".into(),
            line: 1,
        });
        a.finalize();

        let mut b = Profraw::new();
        b.add(PgoSite {
            func: "g".into(),
            file: "b.c".into(),
            line: 2,
        });
        b.finalize();

        let err = a.merge(&b).unwrap_err();
        assert!(err.to_string().contains("site 0 mismatch"));
    }

    /// Test profdata format roundtrip
    #[test]
    fn profdata_roundtrip() {
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
        p.counters[0] = 100;
        p.counters[1] = 80;
        p.counters[2] = 50;
        let mut vp = ValueProfileData::new();
        vp.add_target("bar", 40);
        p.add_value_profile(2, vp);
        p.finalize();

        let tmp = std::env::temp_dir().join("dcc-profdata-test.profdata");
        let _ = std::fs::remove_file(&tmp);
        p.write_profdata(&tmp).unwrap();
        let q = Profraw::read_profdata(&tmp).unwrap();
        assert_eq!(q.counters, vec![100, 80, 50]);
        assert_eq!(q.value_profiles.len(), 1);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Test checksum detects corruption
    #[test]
    fn profraw_checksum_detects_corruption() {
        let mut p = Profraw::new();
        p.add(PgoSite {
            func: "f".into(),
            file: "a.c".into(),
            line: 1,
        });
        p.counters[0] = 42;
        p.finalize();

        let tmp = std::env::temp_dir().join("dcc-pgo-corrupt.profraw");
        let _ = std::fs::remove_file(&tmp);
        p.write(&tmp).unwrap();

        // Corrupt a byte in the site data area (the func name byte).
        // v2 layout: 4(magic) + 4(ver) + 8(checksum) + 4(n_sites) +
        //            4(func_len) = 24, then func name at offset 24.
        // Corrupting the func name changes the checksum but keeps
        // parsing valid.
        let mut data = std::fs::read(&tmp).unwrap();
        if data.len() > 25 {
            data[24] = b'g'; // change func name from 'f' to 'g'
        }
        std::fs::write(&tmp, &data).unwrap();

        let result = Profraw::read(&tmp);
        // Either checksum mismatch or invalid data
        assert!(result.is_err());
        let _ = std::fs::remove_file(&tmp);
    }

    /// Test v1 format backward compatibility
    #[test]
    fn profraw_v1_backward_compat() {
        // Write a v1 format file manually
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"PROF");
        buf.extend_from_slice(&1u32.to_le_bytes()); // version 1
        buf.extend_from_slice(&1u32.to_le_bytes()); // 1 site
                                                    // site: func="main", file="m.c", line=1
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(b"main");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(b"m.c");
        buf.extend_from_slice(&1u32.to_le_bytes());
        // 1 counter
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&42u64.to_le_bytes());

        let tmp = std::env::temp_dir().join("dcc-pgo-v1-compat.profraw");
        std::fs::write(&tmp, &buf).unwrap();
        let q = Profraw::read(&tmp).unwrap();
        assert_eq!(q.version, PROFRAW_VERSION_V1);
        assert_eq!(q.sites.len(), 1);
        assert_eq!(q.counters, vec![42]);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Test ValueProfileData operations
    #[test]
    fn value_profile_data_operations() {
        let mut vp = ValueProfileData::new();
        assert!(vp.top_target().is_none());
        assert_eq!(vp.total_count(), 0);

        vp.add_target("foo", 10);
        vp.add_target("bar", 20);
        vp.add_target("foo", 5); // accumulate to 15
        assert_eq!(vp.total_count(), 35);
        let (top, count) = vp.top_target().unwrap();
        assert_eq!(top, "bar");
        assert_eq!(count, 20);
    }
}
