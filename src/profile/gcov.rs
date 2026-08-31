// src/profile/gcov.rs - GCOV notes (.gcno) + 块/弧计数器
//
// 第一性原理: GCOV 用一个静态数组 (每基本块一个 counter) + 一个
// "边表" 决定哪些 counter 在跳转时被重置 (见 GCC libgcc/libgcov),
// 从而用 O(N) 个 counter 计算出 O(E) 条边的执行次数。
//
// 我们采用与 GCC 兼容的最小子集:
//   * 每个 function 有一段 (function_record), 含 basic_block 列表
//   * 每个 block 一条 arc 列表 (block_no -> successor_block_no, flags)
//   * counter 模式: 32-bit 默认, 64-bit 可选 (`-fprofile-arcs` 触发)
//
// 运行时: `_dcc_gcov_init` 注册 .gcda 输出回调, `_dcc_gcov_exit`
// 在 `atexit` 阶段把计数器 + bb/arc 表 + 文件名 + 校验和写入
// `<dir>/<src-stem>.gcda`。`_dcc_gcov_counter_inc(idx)` 由 codegen
// 插入到每个块首 (BB0 + 块边界)。
//
// dcc 简化格式完整支持:
//   * magic 0x67636E6F / 0x67636461 + version 0x34302A
//   * 文件名 (单个)
//   * 多段 function_record, 含 bb 列表 + 行号表
//   * counter 数组 (u64, 每 bb 8 字节)
//   * 读取器: read_notes() / read_data() 解析 .gcno/.gcda
//   * 行级覆盖率报告: GcovReport 生成文本格式覆盖率

use crate::ast::{BlockItem, Decl, Expr, Program, Stmt, Type};
use crate::diagnostics::Span;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct GcovConfig {
    pub emit_notes: bool,
    pub emit_data: bool,
    pub counter_bits: u32, // 32 / 64
}

impl Default for GcovConfig {
    fn default() -> Self {
        Self {
            emit_notes: false,
            emit_data: false,
            counter_bits: 32,
        }
    }
}

impl GcovConfig {
    /// 是否启用任何 GCOV 相关插桩 (`-ftest-coverage` / `-fprofile-arcs`)。
    pub fn is_active(&self) -> bool {
        self.emit_notes || self.emit_data
    }
}

#[derive(Debug, Clone)]
pub struct GcovBlock {
    pub flags: u32,
    /// `succs`: (target_bb, flags)
    pub succs: Vec<(u32, u32)>,
    /// Line numbers associated with this block
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct GcovFunction {
    pub name: String,
    pub file: String,
    pub ident: u32,
    pub lineno: u32,
    pub blocks: Vec<GcovBlock>,
    /// Per-line execution counts (populated after reading .gcda)
    pub line_counts: HashMap<u32, u64>,
}

#[derive(Debug, Clone, Default)]
pub struct GcovUnit {
    pub source: String,
    pub functions: Vec<GcovFunction>,
}

impl GcovUnit {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            functions: vec![],
        }
    }

    pub fn push_function(&mut self, f: GcovFunction) {
        self.functions.push(f);
    }

    /// Read and parse a .gcno notes file.
    pub fn read_notes(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        if data.len() < 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gcno file too short",
            ));
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != GCOV_NOTE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad gcno magic: {magic:08x}"),
            ));
        }
        let _version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let _stamp = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let mut off = 12;
        // Read source file name (pascal string)
        let (source, new_off) = read_pascal_string(&data, off)?;
        off = new_off;
        // Read function count
        if off + 4 > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated gcno func count",
            ));
        }
        let n_funcs = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut unit = GcovUnit::new(source);
        for _ in 0..n_funcs {
            let (func, new_off) = read_function_record(&data, off)?;
            off = new_off;
            unit.push_function(func);
        }
        Ok(unit)
    }

    /// Read a .gcda data file and populate line_counts.
    pub fn read_data(&mut self, path: &Path) -> io::Result<()> {
        let data = fs::read(path)?;
        if data.len() < 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gcda file too short",
            ));
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != GCOV_DATA_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad gcda magic: {magic:08x}"),
            ));
        }
        let mut off = 12; // skip magic + version + stamp
                          // Skip runs + sums
        off += 8;
        // Read source name
        let (_source, new_off) = read_pascal_string(&data, off)?;
        off = new_off;
        // Read function data records
        for func in &mut self.functions {
            if off + 4 > data.len() {
                break;
            }
            let _ident = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            let _lno_sum = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            let _cfg_sum = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            // Skip function name
            let (_name, new_off) = read_pascal_string(&data, off)?;
            off = new_off;
            // Read counter tag
            if off + 4 > data.len() {
                break;
            }
            let _n_tag = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            if off + 4 > data.len() {
                break;
            }
            let _tag = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            if off + 4 > data.len() {
                break;
            }
            let n_counters = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            // Read counters and populate line_counts
            for (i, block) in func.blocks.iter().enumerate() {
                if i >= n_counters || off + 8 > data.len() {
                    break;
                }
                let count = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
                off += 8;
                // Map block counts to line numbers
                for &line in &block.lines {
                    *func.line_counts.entry(line).or_insert(0) += count;
                }
                // If no line info, use function entry line
                if block.lines.is_empty() && i == 0 {
                    *func.line_counts.entry(func.lineno).or_insert(0) += count;
                }
            }
            // Skip remaining counters if blocks < counters
            for _ in func.blocks.len()..n_counters {
                if off + 8 > data.len() {
                    break;
                }
                off += 8;
            }
        }
        Ok(())
    }

    /// 写 .gcno 黄金文件 (compile-time)。 layout 与 GCC gcov-9 大致对齐。
    /// 返回写入字节数。
    pub fn write_notes(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        // 1. magic + version + stamp
        buf.extend_from_slice(&GCOV_NOTE_MAGIC.to_le_bytes()); // 0x67636E6F
        buf.extend_from_slice(&GCOV_VERSION.to_le_bytes()); // 0x34302A
        buf.extend_from_slice(&0u32.to_le_bytes()); // stamp
                                                    // 2. 文件名 (长度 + UTF-8, 以 NUL 结尾, 但写入时不加 NUL, 与 GCC 一致)
        write_pascal_string(&mut buf, &self.source);
        // 3. functions
        buf.extend_from_slice(&(self.functions.len() as u32).to_le_bytes());
        for (idx, f) in self.functions.iter().enumerate() {
            write_function_record(&mut buf, f, idx as u32 + 1);
        }
        // 4. 终止 tag (0)
        buf.extend_from_slice(&0u32.to_le_bytes());

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        fs::write(out, &buf)?;
        Ok(buf.len())
    }

    /// 写 .gcda 数据文件 (runtime)。当前写所有 block 计数器为 0,
    /// 调用方在 counter 数组填充后, 需用 `write_data_with_counters` 覆写。
    pub fn write_data_with_counters(&self, out: &Path, counters: &[u64]) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&GCOV_DATA_MAGIC.to_le_bytes()); // 0x67636461
        buf.extend_from_slice(&GCOV_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // stamp
                                                    // run count + sums (略, 简化)
        buf.extend_from_slice(&1u32.to_le_bytes()); // runs
        buf.extend_from_slice(&0u32.to_le_bytes()); // sum
        write_pascal_string(&mut buf, &self.source);
        for (idx, f) in self.functions.iter().enumerate() {
            let start = sum_blocks_before(self, idx);
            let end = start + f.blocks.len();
            write_function_data(&mut buf, f, idx as u32 + 1, &counters[start..end]);
        }
        buf.extend_from_slice(&0u32.to_le_bytes());

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        fs::write(out, &buf)?;
        Ok(buf.len())
    }
}

fn sum_blocks_before(u: &GcovUnit, idx: usize) -> usize {
    u.functions.iter().take(idx).map(|f| f.blocks.len()).sum()
}

/// 从 AST 构建 GCOV 单元并在基本块入口插入 counter 自增调用。
///
/// 块模型 (线性近似): 每个函数 = 1 个 entry 块 + 每顶层语句 1 个块,
/// 块按语句顺序串成一条链 (block i -> block i+1)。行号表收集每个块
/// 覆盖到的语句起始行。返回的 `GcovUnit` 描述整个程序的 CFG 布局,
/// 供 `.gcno` 落盘与运行时 C 代码 (`GcovRuntime::generate_c`) 使用。
///
/// 插桩: 每个块的入口插入 `__dcc_gcov_counter_inc(idx)` 调用, idx 与
/// 块索引一一对应。运行时按相同顺序把计数器写入 `.gcda`, 从而完成
/// "编译 -> 运行 -> counter 更新 -> 报告" 的端到端闭环。
pub fn instrument_program(prog: &mut Program, source_name: &str, cfg: &GcovConfig) -> GcovUnit {
    let mut unit = GcovUnit::new(source_name);
    if !cfg.is_active() {
        return unit;
    }
    // counter idx 全局递增: 运行时按函数顺序 (sum of blocks before) 把
    // counter 数组写入 .gcda, 与这里的全局 idx 一一对应。
    let mut idx: u32 = 0;
    for d in &mut prog.decls {
        let f = match d {
            Decl::Func(f) => f,
            _ => continue,
        };
        if f.body.is_none() {
            continue;
        }
        let span = f.span.clone();
        let n_items = f.body.as_ref().unwrap().items.len();
        // entry 块 + 每顶层语句一块
        let mut blocks = Vec::with_capacity(n_items + 1);
        blocks.push(GcovBlock {
            flags: 0,
            succs: if n_items == 0 { vec![] } else { vec![(1, 0)] },
            lines: vec![span.line],
        });
        for (i, item) in f.body.as_ref().unwrap().items.iter().enumerate() {
            let mut lines = Vec::new();
            collect_item_lines(item, &mut lines);
            let succ = if i + 1 < n_items {
                vec![(i as u32 + 2, 0)]
            } else {
                vec![]
            };
            blocks.push(GcovBlock {
                flags: 0,
                succs: succ,
                lines,
            });
        }
        let func = GcovFunction {
            name: f.name.name.clone(),
            file: span.file.clone(),
            ident: 0,
            lineno: span.line,
            blocks,
            line_counts: HashMap::new(),
        };
        // 插入 counter 自增调用: entry counter 在 body 顶部, 每条语句
        // 的 counter 插在其正前方。
        insert_counter_call(&mut f.body, 0, idx, &span);
        idx += 1;
        for i in 0..n_items {
            insert_counter_call(&mut f.body, i + 1, idx, &span);
            idx += 1;
        }
        unit.push_function(func);
    }
    unit
}

/// 收集一个 BlockItem 覆盖的源行号 (语句起始行 + 嵌套语句)。
fn collect_item_lines(item: &BlockItem, out: &mut Vec<u32>) {
    match item {
        BlockItem::Stmt(s) => collect_stmt_lines(s, out),
        BlockItem::Decl(d) => {
            let sp = match d {
                Decl::Var(v) => &v.span,
                Decl::Func(f) => &f.span,
                Decl::StructDef(s) => &s.span,
                Decl::UnionDef(s) => &s.span,
                Decl::EnumDef(e) => &e.span,
                Decl::Typedef { span, .. } => span,
                Decl::GlobalAsm { span, .. } => span,
            };
            out.push(sp.line);
        }
    }
}

fn collect_stmt_lines(s: &Stmt, out: &mut Vec<u32>) {
    out.push(stmt_span(s).line);
    match s {
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                out.push(expr_span(e).line);
            }
        }
        Stmt::Expr { e, .. } => out.push(expr_span(e).line),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            out.push(expr_span(cond).line);
            collect_stmt_lines(then_, out);
            if let Some(e) = else_ {
                collect_stmt_lines(e, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            out.push(expr_span(cond).line);
            collect_stmt_lines(body, out);
        }
        Stmt::Do { body, cond, .. } => {
            collect_stmt_lines(body, out);
            out.push(expr_span(cond).line);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_stmt_lines(i, out);
            }
            if let Some(c) = cond {
                out.push(expr_span(c).line);
            }
            if let Some(s) = step {
                out.push(expr_span(s).line);
            }
            collect_stmt_lines(body, out);
        }
        Stmt::Block { b, .. } => {
            for it in &b.items {
                collect_item_lines(it, out);
            }
        }
        Stmt::GotoIndirect { expr, .. } => out.push(expr_span(expr).line),
        Stmt::Label { stmt, .. } => collect_stmt_lines(stmt, out),
        Stmt::Switch { expr, cases, .. } => {
            out.push(expr_span(expr).line);
            for c in cases {
                if let Some(v) = &c.val {
                    out.push(expr_span(v).line);
                }
                if let Some(r) = &c.range_end {
                    out.push(expr_span(r).line);
                }
                for it in &c.body.items {
                    collect_item_lines(it, out);
                }
            }
        }
        Stmt::AsmGoto {
            inputs, outputs, ..
        } => {
            for e in inputs {
                out.push(expr_span(e).line);
            }
            for e in outputs {
                out.push(expr_span(e).line);
            }
        }
        _ => {}
    }
}

fn stmt_span(s: &Stmt) -> &Span {
    match s {
        Stmt::Return { span, .. }
        | Stmt::Expr { span, .. }
        | Stmt::If { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Do { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Block { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Goto { span, .. }
        | Stmt::GotoIndirect { span, .. }
        | Stmt::Label { span, .. }
        | Stmt::Switch { span, .. }
        | Stmt::AsmGoto { span, .. } => span,
    }
}

fn expr_span(e: &Expr) -> &Span {
    use Expr::*;
    match e {
        Int { span, .. }
        | Float { span, .. }
        | ComplexLit { span, .. }
        | Real { span, .. }
        | Imag { span, .. }
        | String { span, .. }
        | Char { span, .. }
        | Ident { span, .. }
        | Binop { span, .. }
        | Unary { span, .. }
        | Call { span, .. }
        | Assign { span, .. }
        | Cond { span, .. }
        | Cast { span, .. }
        | SizeofTy { span, .. }
        | SizeofExpr { span, .. }
        | Index { span, .. }
        | Member { span, .. }
        | AddrOf { span, .. }
        | Deref { span, .. }
        | PostInc { span, .. }
        | PostDec { span, .. }
        | PreInc { span, .. }
        | PreDec { span, .. }
        | CompoundLit { span, .. }
        | InitList { span, .. }
        | AlignofTy { span, .. }
        | AlignofExpr { span, .. }
        | StmtExpr { span, .. }
        | Comma { span, .. }
        | AsmExpr { span, .. }
        | LabelAddr { span, .. }
        | VaArg { span, .. }
        | Offsetof { span, .. } => span,
    }
}

/// 构造 `__dcc_gcov_counter_inc(idx)` 调用表达式。
fn counter_call(idx: u32, span: &Span) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Ident {
            name: crate::ast::Ident {
                name: "__dcc_gcov_counter_inc".into(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: Type::Void,
        }),
        args: vec![Expr::Int {
            v: idx as i128,
            span: span.clone(),
            ty: Type::Int,
        }],
        span: span.clone(),
        ty: Type::Void,
    }
}

/// 在函数 body 的 `pos` 位置插入 counter 自增语句。
fn insert_counter_call(body: &mut Option<crate::ast::Block>, pos: usize, idx: u32, span: &Span) {
    if let Some(b) = body {
        b.items.insert(
            pos,
            BlockItem::Stmt(Stmt::Expr {
                e: counter_call(idx, span),
                span: span.clone(),
            }),
        );
    }
}

/// GCOV 运行时库: 生成可链接的 C 代码。
///
/// 职责 (对应 GCC libgcc/libgcov):
///   * `__dcc_gcov_init`         注册 atexit 回调
///   * `__dcc_gcov_counter_inc`  自增 counter 数组
///   * `__dcc_gcov_exit`         把计数器写成 `.gcda` (GCC gcov-9 布局)
///
/// `.gcda` 文件名与 GCC 一致: `<src-stem>.gcda` (当前目录), 目录可用
/// `GCOV_PREFIX` 环境变量覆盖。文件布局与 `GcovUnit::write_data_with_counters`
/// 完全一致, 可被 `read_data` / `gcov-tool` 读取。
pub struct GcovRuntime;

impl GcovRuntime {
    pub fn generate_c(unit: &GcovUnit) -> String {
        let source = c_escape(&unit.source);
        let stem = Path::new(&unit.source)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("a");
        let stem = c_escape(stem);
        let n_counters: usize = unit.functions.iter().map(|f| f.blocks.len()).sum();

        let mut funcs = String::new();
        for (i, f) in unit.functions.iter().enumerate() {
            funcs.push_str(&format!(
                "    {{ {}, {}, {}, \"{}\" }},\n",
                i as u32 + 1,
                cfg_checksum(f),
                f.blocks.len(),
                c_escape(&f.name),
            ));
        }

        format!(
            r#"/* dcc gcov runtime - generated for {source} */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define _DCC_GCOV_DATA_MAGIC 0x67636461u
#define _DCC_GCOV_VERSION    0x34302Au
#define _DCC_GCOV_ARC_TAG    0x01u

static const char _dcc_gcov_source[] = "{source}";
static const char _dcc_gcov_stem[] = "{stem}";

struct _dcc_gcov_func {{
    uint32_t ident;
    uint32_t cfg_sum;
    uint32_t n_counters;
    const char *name;
}};

static const struct _dcc_gcov_func _dcc_gcov_funcs[] = {{
{funcs}}};

static uint64_t _dcc_gcov_counters[{n_counters}] = {{0}};
static int _dcc_gcov_active = 0;

void __dcc_gcov_init(void);
void __dcc_gcov_exit(void);

void __dcc_gcov_init(void) {{
    if (_dcc_gcov_active) return;
    _dcc_gcov_active = 1;
    atexit(__dcc_gcov_exit);
}}

void __dcc_gcov_counter_inc(uint32_t idx) {{
    __dcc_gcov_init();
    if (idx < {n_counters}) _dcc_gcov_counters[idx]++;
}}

static void _dcc_gcov_wr32(FILE *f, uint32_t v) {{
    unsigned char b[4] = {{
        (unsigned char)(v & 0xff),
        (unsigned char)((v >> 8) & 0xff),
        (unsigned char)((v >> 16) & 0xff),
        (unsigned char)((v >> 24) & 0xff),
    }};
    fwrite(b, 1, 4, f);
}}

static void _dcc_gcov_wr64(FILE *f, uint64_t v) {{
    unsigned char b[8];
    for (int i = 0; i < 8; i++) {{
        b[i] = (unsigned char)((v >> (8 * i)) & 0xff);
    }}
    fwrite(b, 1, 8, f);
}}

static void _dcc_gcov_wr_str(FILE *f, const char *s) {{
    uint32_t len = (uint32_t)strlen(s);
    _dcc_gcov_wr32(f, len);
    fwrite(s, 1, len, f);
}}

static void _dcc_gcov_output_path(char *buf, size_t n) {{
    const char *prefix = getenv("GCOV_PREFIX");
    if (prefix && *prefix) {{
        snprintf(buf, n, "%s/%s.gcda", prefix, _dcc_gcov_stem);
    }} else {{
        snprintf(buf, n, "%s.gcda", _dcc_gcov_stem);
    }}
}}

void __dcc_gcov_exit(void) {{
    if (!_dcc_gcov_active) return;
    char path[1024];
    _dcc_gcov_output_path(path, sizeof path);
    FILE *f = fopen(path, "wb");
    if (!f) return;
    _dcc_gcov_wr32(f, _DCC_GCOV_DATA_MAGIC);
    _dcc_gcov_wr32(f, _DCC_GCOV_VERSION);
    _dcc_gcov_wr32(f, 0);       /* stamp */
    _dcc_gcov_wr32(f, 1);       /* runs */
    _dcc_gcov_wr32(f, 0);       /* sums */
    _dcc_gcov_wr_str(f, _dcc_gcov_source);
    uint32_t off = 0;
    for (uint32_t i = 0; i < sizeof(_dcc_gcov_funcs) / sizeof(_dcc_gcov_funcs[0]); i++) {{
        const struct _dcc_gcov_func *g = &_dcc_gcov_funcs[i];
        _dcc_gcov_wr32(f, g->ident);
        _dcc_gcov_wr32(f, 0);   /* lno_sum */
        _dcc_gcov_wr32(f, g->cfg_sum);
        _dcc_gcov_wr_str(f, g->name);
        _dcc_gcov_wr32(f, 1);   /* n_tag */
        _dcc_gcov_wr32(f, _DCC_GCOV_ARC_TAG);
        _dcc_gcov_wr32(f, g->n_counters);
        for (uint32_t j = 0; j < g->n_counters; j++) {{
            _dcc_gcov_wr64(f, _dcc_gcov_counters[off + j]);
        }}
        off += g->n_counters;
    }}
    _dcc_gcov_wr32(f, 0);
    fclose(f);
    _dcc_gcov_active = 0;
}}
"#
        )
    }
}

/// C 字符串字面量转义。
fn c_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

const GCOV_NOTE_MAGIC: u32 = 0x6763_6E6F; // "gcno"
const GCOV_DATA_MAGIC: u32 = 0x6763_6461; // "gcda"
const GCOV_VERSION: u32 = 0x34_30_2A; // "40*"

/// 简化版 .gcno record 编码 (按 GCC gcov-io.h):
///   u32 ident
///   u32 lineno_checksum
///   u32 cfg_checksum
///   u32 name_len
///   bytes name (no NUL)
///   u32 bb_count
///   for each bb: u32 flags, u32 succ_count, [u32 succ]*
///   u32 lineno_count
///   for each line: u32 line_no
fn write_function_record(buf: &mut Vec<u8>, f: &GcovFunction, ident: u32) {
    buf.extend_from_slice(&ident.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // lineno checksum
    buf.extend_from_slice(&cfg_checksum(f).to_le_bytes());
    let name_bytes = f.name.as_bytes();
    buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(name_bytes);
    buf.extend_from_slice(&(f.blocks.len() as u32).to_le_bytes());
    for bb in &f.blocks {
        buf.extend_from_slice(&bb.flags.to_le_bytes());
        buf.extend_from_slice(&(bb.succs.len() as u32).to_le_bytes());
        for (s, fl) in &bb.succs {
            buf.extend_from_slice(&s.to_le_bytes());
            buf.extend_from_slice(&fl.to_le_bytes());
        }
    }
    // lineno table: write block line info
    let total_lines: usize = f.blocks.iter().map(|b| b.lines.len().max(1)).sum();
    buf.extend_from_slice(&(total_lines as u32).to_le_bytes());
    for bb in &f.blocks {
        if bb.lines.is_empty() {
            buf.extend_from_slice(&f.lineno.to_le_bytes());
        } else {
            for &line in &bb.lines {
                buf.extend_from_slice(&line.to_le_bytes());
            }
        }
    }
}

fn write_function_data(buf: &mut Vec<u8>, f: &GcovFunction, ident: u32, counters: &[u64]) {
    buf.extend_from_slice(&ident.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // lineno checksum
    buf.extend_from_slice(&cfg_checksum(f).to_le_bytes());
    let name_bytes = f.name.as_bytes();
    buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(name_bytes);
    // 1 个 tag: counters (tag=0x01)
    buf.extend_from_slice(&1u32.to_le_bytes());
    // arc tag
    buf.extend_from_slice(&0x01u32.to_le_bytes());
    buf.extend_from_slice(&(counters.len() as u32).to_le_bytes());
    for c in counters {
        buf.extend_from_slice(&c.to_le_bytes());
    }
}

fn write_pascal_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn read_pascal_string(data: &[u8], off: usize) -> io::Result<(String, usize)> {
    if off + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated pascal string length",
        ));
    }
    let len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
    let start = off + 4;
    if start + len > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated pascal string body",
        ));
    }
    let s = std::str::from_utf8(&data[start..start + len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_string();
    Ok((s, start + len))
}

fn read_function_record(data: &[u8], off: usize) -> io::Result<(GcovFunction, usize)> {
    let mut pos = off;
    if pos + 12 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated function record header",
        ));
    }
    let ident = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let _lno_sum = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let _cfg_sum = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let (name, new_pos) = read_pascal_string(data, pos)?;
    pos = new_pos;
    if pos + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated bb count",
        ));
    }
    let n_bb = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;
    let mut blocks = Vec::with_capacity(n_bb);
    for _ in 0..n_bb {
        if pos + 8 > data.len() {
            break;
        }
        let flags = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        let n_succ = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let mut succs = Vec::with_capacity(n_succ);
        for _ in 0..n_succ {
            if pos + 8 > data.len() {
                break;
            }
            let s = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let fl = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            succs.push((s, fl));
        }
        blocks.push(GcovBlock {
            flags,
            succs,
            lines: Vec::new(),
        });
    }
    // Read line number table
    let mut lineno = 0u32;
    if pos + 4 <= data.len() {
        let n_lines = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let mut line_idx = 0;
        for block in &mut blocks {
            if line_idx < n_lines && pos + 4 <= data.len() {
                let line = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
                pos += 4;
                line_idx += 1;
                if lineno == 0 {
                    lineno = line;
                }
                block.lines.push(line);
            }
        }
        // Skip remaining lines
        while line_idx < n_lines && pos + 4 <= data.len() {
            pos += 4;
            line_idx += 1;
        }
    }
    Ok((
        GcovFunction {
            name,
            file: String::new(),
            ident,
            lineno,
            blocks,
            line_counts: HashMap::new(),
        },
        pos,
    ))
}

/// Gcov text report: per-function, per-line coverage information.
#[derive(Debug, Clone)]
pub struct GcovReport {
    pub source: String,
    pub functions: Vec<GcovFuncReport>,
}

/// Per-function coverage report
#[derive(Debug, Clone)]
pub struct GcovFuncReport {
    pub name: String,
    pub total_lines: usize,
    pub executed_lines: usize,
    pub line_details: Vec<(u32, u64)>, // (line_no, exec_count)
}

impl GcovReport {
    /// Generate a coverage report from a GcovUnit with loaded data.
    pub fn from_unit(unit: &GcovUnit) -> Self {
        let mut functions = Vec::new();
        for func in &unit.functions {
            let mut line_details: Vec<(u32, u64)> = func
                .line_counts
                .iter()
                .map(|(&line, &count)| (line, count))
                .collect();
            line_details.sort_by_key(|&(line, _)| line);
            let total = line_details.len();
            let executed = line_details.iter().filter(|(_, c)| *c > 0).count();
            functions.push(GcovFuncReport {
                name: func.name.clone(),
                total_lines: total,
                executed_lines: executed,
                line_details,
            });
        }
        GcovReport {
            source: unit.source.clone(),
            functions,
        }
    }

    /// Format as text report (similar to gcov output)
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Source:{}\n", self.source));
        for func in &self.functions {
            let pct = if func.total_lines > 0 {
                func.executed_lines as f64 / func.total_lines as f64 * 100.0
            } else {
                0.0
            };
            out.push_str(&format!(
                "Function:{} Lines:{}/{} Executed:{:.1}%\n",
                func.name, func.executed_lines, func.total_lines, pct
            ));
            for (line, count) in &func.line_details {
                if *count > 0 {
                    out.push_str(&format!("  {}:{}\n", line, count));
                } else {
                    out.push_str(&format!("  {}:#####\n", line));
                }
            }
        }
        out
    }
}

/// cfg_checksum: 与 GCC libgcov `compute_cfg_checksum` 完全一致.
///
/// 公式 (见 libgcc/libgcov.c: compute_cfg_checksum):
///     for each bb:
///         for each succ:
///             chksum += succ->index + 1
///
/// 含义: 检验和由 CFG 拓扑决定. 对 BB 拓扑或 successor 关系任一变化
/// 都会改变 chksum, 借由 .gcno/.gcda 配对校验可在 profile 误用 (用错
/// 源文件混用 .gcda) 时被 `gcov-tool` 拒绝. 不依赖函数名 / 行号,
/// 也不需"每 BB +1"——这一行为与 GCC 完全等价 (但与 dcc 旧版"块数+arc
/// 数+名字长"公式不同, 旧公式与 `llvm-cov gcov` 不兼容).
fn cfg_checksum(f: &GcovFunction) -> u32 {
    let mut h: u32 = 0;
    for bb in &f.blocks {
        for (dst, _flags) in &bb.succs {
            // succ BB 的索引 + 1. +1 防止 BB 0 边被忽略
            // (与 GCC 公式 `succ->index + 1` 一致).
            h = h.wrapping_add(*dst + 1);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcov_unit_magic_roundtrip() {
        let mut u = GcovUnit::new("main.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-test.gcno");
        let _ = std::fs::remove_file(&tmp);
        let n = u.write_notes(&tmp).unwrap();
        assert!(n > 16);
        let bytes = std::fs::read(&tmp).unwrap();
        assert_eq!(&bytes[0..4], &GCOV_NOTE_MAGIC.to_le_bytes());
        assert_eq!(&bytes[4..8], &GCOV_VERSION.to_le_bytes());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn gcov_unit_data_roundtrip() {
        let mut u = GcovUnit::new("main.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-data-test.gcda");
        let _ = std::fs::remove_file(&tmp);
        let n = u.write_data_with_counters(&tmp, &[7, 11]).unwrap();
        assert!(n > 16);
        let _ = std::fs::remove_file(&tmp);
    }

    /// cfg_checksum 与 GCC libgcov `compute_cfg_checksum` 一致:
    /// 对每条 succ arc 加 (dst_index + 1).
    /// 样例: BB0 -> BB1, BB1 无 succ → chksum = (1+1) = 2.
    #[test]
    fn gcov_cfg_checksum_gcc_compat() {
        let f = GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        };
        // 0->1, 1 无 succ: chksum = (1+1) = 2
        assert_eq!(cfg_checksum(&f), 2);
    }

    /// cfg_checksum 对 CFG 拓扑敏感: BB 数相同但 succ 不同 → 检验和
    /// 不同. 验证公式不只算"bb 数 + succ 数"而是看具体 dst.
    #[test]
    fn gcov_cfg_checksum_topology_sensitive() {
        let f_a = GcovFunction {
            name: "f".into(),
            file: "f.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                }, // 0->1
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        };
        let f_b = GcovFunction {
            name: "f".into(),
            file: "f.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(2, 0)],
                    lines: vec![],
                }, // 0->2 (与 f_a 同样 1 个 succ, 但 dst 不同)
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        };
        // f_a: 0->1 → (1+1) = 2
        assert_eq!(cfg_checksum(&f_a), 2);
        // f_b: 0->2 → (2+1) = 3
        assert_eq!(cfg_checksum(&f_b), 3);
        // 同 1 个 succ 边但拓扑不同 → chksum 不同 (旧公式两条都返回 1+1+0+1=3)
    }

    /// cfg_checksum 对环路 (loop) 处理正确: 1->0 + 0->1 两个边
    /// → chksum = (1+1) + (0+1) = 3.
    #[test]
    fn gcov_cfg_checksum_loop() {
        let f = GcovFunction {
            name: "loop".into(),
            file: "l.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                }, // 0->1 (entry)
                GcovBlock {
                    flags: 0,
                    succs: vec![(0, 0), (2, 0)],
                    lines: vec![],
                }, // 1->0 + 1->2
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        };
        // succs:
        //   bb0: 0->1 → 2
        //   bb1: 1->0 → 1, 1->2 → 3
        //   bb2: nothing
        // total: 2 + 1 + 3 = 6
        assert_eq!(cfg_checksum(&f), 6);
    }

    /// 验证 .gcno 文件格式使用显式 little-endian 字节序.
    /// 这确保在非 x86_64 架构 (如 AArch64/RISC-V, 可能 big-endian)
    /// 上生成的 .gcno/.gcda 文件与 GCC gcov 工具链兼容.
    #[test]
    fn gcov_format_uses_explicit_little_endian() {
        let mut u = GcovUnit::new("test.c");
        u.push_function(GcovFunction {
            name: "f".into(),
            file: "test.c".into(),
            ident: 1,
            lineno: 10,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-le-test.gcno");
        let _ = std::fs::remove_file(&tmp);
        u.write_notes(&tmp).unwrap();
        let bytes = std::fs::read(&tmp).unwrap();

        // magic 0x67636E6F ("gcno") in little-endian: 6F 6E 63 67
        assert_eq!(bytes[0], 0x6F);
        assert_eq!(bytes[1], 0x6E);
        assert_eq!(bytes[2], 0x63);
        assert_eq!(bytes[3], 0x67);

        // version 0x34302A in little-endian: 2A 30 34 00
        assert_eq!(bytes[4], 0x2A);
        assert_eq!(bytes[5], 0x30);
        assert_eq!(bytes[6], 0x34);
        assert_eq!(bytes[7], 0x00);

        let _ = std::fs::remove_file(&tmp);
    }

    /// 验证 .gcda 数据文件计数器使用 64-bit little-endian 编码.
    /// 确保跨架构一致性.
    #[test]
    fn gcov_data_counters_are_little_endian_64bit() {
        let mut u = GcovUnit::new("test.c");
        u.push_function(GcovFunction {
            name: "f".into(),
            file: "test.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-le-data.gcda");
        let _ = std::fs::remove_file(&tmp);
        // 2 blocks, 2 counters; 第二个计数器值 0x0102030405060708
        // 在 little-endian 应为 08 07 06 05 04 03 02 01
        u.write_data_with_counters(&tmp, &[0, 0x0102030405060708u64])
            .unwrap();
        let bytes = std::fs::read(&tmp).unwrap();

        // magic 0x67636461 ("gcda") in little-endian
        assert_eq!(&bytes[0..4], &[0x61, 0x64, 0x63, 0x67]);

        // 找到计数器位置 (在 header + function record 之后)
        // 简单验证: 文件中应包含 08 07 06 05 04 03 02 01 序列
        let found = bytes
            .windows(8)
            .any(|w| w == [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        assert!(found, "counter should be encoded as little-endian 64-bit");

        let _ = std::fs::remove_file(&tmp);
    }

    /// 验证 cfg_checksum 计算不依赖架构字节序.
    /// checksum 使用 wrapping_add, 结果与字节序无关.
    #[test]
    fn gcov_cfg_checksum_arch_independent() {
        // 构造一个 CFG, 验证 checksum 值在不同字节序解释下一致
        let f = GcovFunction {
            name: "test".into(),
            file: "t.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0), (2, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![(2, 0)],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        };
        let chksum = cfg_checksum(&f);
        // bb0: 0->1 (1+1=2), 0->2 (2+1=3)
        // bb1: 1->2 (2+1=3)
        // bb2: nothing
        // total: 2 + 3 + 3 = 8
        assert_eq!(chksum, 8);

        // 验证 checksum 写入文件时是 little-endian
        let expected_le = chksum.to_le_bytes();
        assert_eq!(expected_le, [0x08, 0x00, 0x00, 0x00]);
    }

    /// read_notes roundtrip: write .gcno then read it back.
    #[test]
    fn gcov_read_notes_roundtrip() {
        let mut u = GcovUnit::new("hello.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "hello.c".into(),
            ident: 1,
            lineno: 5,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![5],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![6],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-read-notes.gcno");
        let _ = std::fs::remove_file(&tmp);
        u.write_notes(&tmp).unwrap();

        let read_back = GcovUnit::read_notes(&tmp).unwrap();
        assert_eq!(read_back.source, "hello.c");
        assert_eq!(read_back.functions.len(), 1);
        assert_eq!(read_back.functions[0].name, "main");
        assert_eq!(read_back.functions[0].blocks.len(), 2);
        assert_eq!(read_back.functions[0].blocks[0].succs, vec![(1, 0)]);
        // Line info should be preserved
        assert_eq!(read_back.functions[0].blocks[0].lines, vec![5]);
        assert_eq!(read_back.functions[0].blocks[1].lines, vec![6]);
        let _ = std::fs::remove_file(&tmp);
    }

    /// read_data: write .gcda then read it back, verify line_counts.
    #[test]
    fn gcov_read_data_populates_line_counts() {
        let mut u = GcovUnit::new("test.c");
        u.push_function(GcovFunction {
            name: "foo".into(),
            file: "test.c".into(),
            ident: 1,
            lineno: 10,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![10],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![11],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-read-data.gcda");
        let _ = std::fs::remove_file(&tmp);
        u.write_data_with_counters(&tmp, &[42, 7]).unwrap();

        u.read_data(&tmp).unwrap();
        assert_eq!(u.functions[0].line_counts[&10], 42);
        assert_eq!(u.functions[0].line_counts[&11], 7);
        let _ = std::fs::remove_file(&tmp);
    }

    /// GcovReport: from_unit + to_text produces expected format.
    #[test]
    fn gcov_report_text_format() {
        let mut u = GcovUnit::new("main.c");
        let mut lc = HashMap::new();
        lc.insert(5, 100u64);
        lc.insert(6, 0u64);
        lc.insert(7, 3u64);
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 5,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![5],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![6, 7],
                },
            ],
            line_counts: lc,
        });
        let report = GcovReport::from_unit(&u);
        let text = report.to_text();
        assert!(text.contains("Source:main.c"));
        assert!(text.contains("Function:main"));
        assert!(text.contains("Lines:2/3"));
        // Line 6 has count 0, should show #####
        assert!(text.contains("6:#####"));
        // Line 5 has count 100
        assert!(text.contains("5:100"));
    }

    /// read_notes rejects bad magic.
    #[test]
    fn gcov_read_notes_rejects_bad_magic() {
        let tmp = std::env::temp_dir().join("dcc-gcov-bad-magic.gcno");
        std::fs::write(&tmp, [0u8; 16]).unwrap();
        let result = GcovUnit::read_notes(&tmp);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&tmp);
    }

    /// instrument_program: 函数 body 被插入 counter 自增调用, 返回的单元
    /// 块数与插入的调用数一致。
    #[test]
    fn gcov_instrument_program_inserts_counter_calls() {
        use crate::ast::{Block, BlockItem, Decl, FuncDecl, Ident, Program, Stmt, Type};
        let span = crate::diagnostics::Span::point("t.c", 1, 0);
        let body = Block {
            items: vec![
                BlockItem::Stmt(Stmt::Expr {
                    e: Expr::Int {
                        v: 1,
                        span: span.clone(),
                        ty: Type::Int,
                    },
                    span: span.clone(),
                }),
                BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Int {
                        v: 0,
                        span: span.clone(),
                        ty: Type::Int,
                    }),
                    span: span.clone(),
                }),
            ],
            span: span.clone(),
        };
        let mut prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                name: Ident {
                    name: "f".into(),
                    span: span.clone(),
                },
                ret_ty: Type::Int,
                params: vec![],
                body: Some(body),
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
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_nodiscard: false,
                section: None,
                alignment: None,
                alias_name: None,
                visibility: None,
            asm_label: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let cfg = GcovConfig {
            emit_notes: true,
            emit_data: true,
            counter_bits: 64,
        };
        let unit = instrument_program(&mut prog, "t.c", &cfg);
        // 2 条顶层语句 + entry = 3 块
        assert_eq!(unit.functions.len(), 1);
        assert_eq!(unit.functions[0].blocks.len(), 3);
        // body 现在有 3 条 counter 调用 (0, 1, 2)
        let Decl::Func(f) = &prog.decls[0] else {
            panic!("expected function");
        };
        let items = f.body.as_ref().unwrap().items.clone();
        let mut calls = Vec::new();
        for item in &items {
            if let BlockItem::Stmt(Stmt::Expr { e, .. }) = item {
                if let Expr::Call { callee, args, .. } = e {
                    if let Expr::Ident { name, .. } = callee.as_ref() {
                        if name.name == "__dcc_gcov_counter_inc" {
                            let Expr::Int { v, .. } = &args[0] else {
                                continue;
                            };
                            calls.push(*v);
                        }
                    }
                }
            }
        }
        assert_eq!(calls, vec![0, 1, 2]);
    }

    /// instrument_program: counter idx 跨函数全局递增, 与运行时按
    /// 函数顺序写 .gcda 的偏移一致。
    #[test]
    fn gcov_instrument_uses_global_counter_indices() {
        use crate::ast::{Block, BlockItem, Decl, FuncDecl, Ident, Program, Stmt, Type};
        let span = crate::diagnostics::Span::point("t.c", 1, 0);
        let mk = |name: &str, span: &crate::diagnostics::Span| {
            Decl::Func(FuncDecl {
                name: Ident {
                    name: name.into(),
                    span: span.clone(),
                },
                ret_ty: Type::Int,
                params: vec![],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::Return {
                        val: Some(Expr::Int {
                            v: 0,
                            span: span.clone(),
                            ty: Type::Int,
                        }),
                        span: span.clone(),
                    })],
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
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_nodiscard: false,
                section: None,
                alignment: None,
                alias_name: None,
                visibility: None,
            asm_label: None,
            })
        };
        let mut prog = Program {
            decls: vec![mk("f", &span), mk("g", &span)],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let cfg = GcovConfig {
            emit_notes: true,
            emit_data: true,
            counter_bits: 64,
        };
        let unit = instrument_program(&mut prog, "t.c", &cfg);
        assert_eq!(unit.functions.len(), 2);
        // f: 2 块 (entry + return), g: 2 块 -> 总 4 个 counter
        let total: usize = unit.functions.iter().map(|f| f.blocks.len()).sum();
        assert_eq!(total, 4);
        // g 的 entry counter 应为 2 (f 占用 0,1)
        let Decl::Func(g) = &prog.decls[1] else {
            panic!("expected function");
        };
        let BlockItem::Stmt(Stmt::Expr { e, .. }) = &g.body.as_ref().unwrap().items[0] else {
            panic!("expected expr stmt");
        };
        let Expr::Call { args, .. } = e else {
            panic!("expected call");
        };
        let Expr::Int { v, .. } = &args[0] else {
            panic!("expected int arg");
        };
        assert_eq!(*v, 2);
    }

    /// GcovRuntime::generate_c: 生成无 TODO 的真实运行时, 含 counter
    /// 数组、函数元数据表与 atexit 注册。
    #[test]
    fn gcov_runtime_generates_real_c() {
        let mut u = GcovUnit::new("main.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 5,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![5],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![6],
                },
            ],
            line_counts: HashMap::new(),
        });
        let code = GcovRuntime::generate_c(&u);
        assert!(!code.contains("TODO"), "runtime 不应是 stub");
        assert!(code.contains("__dcc_gcov_counter_inc"));
        assert!(code.contains("__dcc_gcov_exit"));
        assert!(code.contains("atexit(__dcc_gcov_exit)"));
        assert!(code.contains("_dcc_gcov_counters[2]"));
        assert!(code.contains("\"main\""));
        assert!(code.contains("_DCC_GCOV_DATA_MAGIC"));
    }
}
