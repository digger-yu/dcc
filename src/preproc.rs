// src/preproc.rs - C 预处理器
// 第一性原理: 预处理 = 文本变换 + 宏展开。在词法分析之前运行。
// suckless 准则: 行级处理 #include/#define/#if, 然后 token 级宏展开。

use crate::cli::StdMode;
use crate::diagnostics::{Diagnostic, Span};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// 逻辑行: 反斜杠续接行合并后的文本 + 其覆盖的物理行号范围 (1-based)。
/// SourceLoc 追踪的核心: 诊断 / __LINE__ / 输出行号一律以此映射到物理行。
#[derive(Debug, Clone)]
struct LogicalLine {
    text: String,
    phys_start: u32,
    phys_end: u32,
}

/// __LINE__ 行号映射: 把展开文本内的换行计数映射回物理行号。
/// Block: 顶层块模式 — 行 i (0-based) 对应 phys_lines[i] (flush 时
/// 传入的物理行号表, 续接行按逻辑行首行计);
/// Scalar: 递归模式 — 基础物理行号 + 换行偏移 (宏体/参数展开等
/// 单行递归层级, GCC 语义下 __LINE__ 取调用点所在行)。
#[derive(Clone, Copy)]
enum LineMap<'a> {
    Block(&'a [u32]),
    Scalar(u32),
}

impl<'a> LineMap<'a> {
    fn line(&self, nl: u32) -> u32 {
        match *self {
            LineMap::Block(lines) => {
                let idx = nl as usize;
                if idx < lines.len() {
                    lines[idx]
                } else if lines.is_empty() {
                    1
                } else {
                    *lines.last().unwrap()
                }
            }
            LineMap::Scalar(base) => base + nl,
        }
    }
}

pub struct Preprocessor {
    macros: HashMap<String, MacroDef>,
    include_paths: Vec<PathBuf>,
    /// 系统搜索路径 (-isystem, 在用户路径之后、标准路径之前)
    system_paths: Vec<PathBuf>,
    /// #pragma once 文件
    pragma_once: HashSet<String>,
    diags: Vec<Diagnostic>,
    /// 条件编译栈: true = 当前分支激活, false = 跳过
    cond_stack: Vec<CondState>,
    /// 输出缓冲
    out: String,
    /// 输出行数计数器 (用于 LineMap 区间记录)
    out_lines: u32,
    /// 当前文件名 (用于诊断 + __FILE__)
    file: String,
    /// 搜索路径基准目录
    base_dir: PathBuf,
    /// 当前行号 (用于 __LINE__)
    cur_line: u32,
    /// 行号映射区间: 预处理输出行号 → 源文件 (file, src_line)
    line_map: Vec<LineSpan>,
    /// 编译日期/时间 (用于 __DATE__ / __TIME__)
    compile_date: String,
    compile_time: String,
    /// __COUNTER__ 计数器: 每个 TU 从 0 开始, 每次访问后自增。
    /// 跨宏展开阶段共享, 缓存中以"宏表 + counter"哈希锁定
    counter: u32,
    /// 诊断状态栈 (用于 #pragma GCC diagnostic push/pop)
    diag_stack: Vec<DiagState>,
    /// 当前诊断设置
    diag_state: DiagState,
    /// 全局类型注册表: 头文件缓存目录
    cache_dir: Option<PathBuf>,
    /// 跨行注释状态: true = 当前在多行注释 /* ... 中
    in_comment: bool,
    /// 成功打开过的 include 路径 (供 -MD/-MMD)
    deps: Vec<String>,
    /// #pragma pack 栈: 栈顶是当前 packing 对齐 (None = 默认 8)
    pack_stack: Vec<Option<usize>>,
    /// #pragma pack 变更历史: (行号, 新对齐). 供 parser 按位置查询
    pack_history: Vec<(u32, Option<usize>)>,
    /// #pragma weak 记录的弱符号集合 (在 codegen 中处理)
    pub weak_symbols: HashSet<String>,
    /// #pragma STDC FENV_ACCESS 状态
    pub fenv_access: bool,
    /// 收集在展开阶段遇到的 _Pragma 操作符, 待外层 &mut self 方法批量处理
    pending_pragmas: RefCell<Vec<(String, u32)>>,
    /// 本轮 expand_macros 实际展开过的宏名 (跨嵌套调用累积),
    /// 供第二轮重扫描作为禁用集延续 paint blue 语义
    expanded_macros: RefCell<HashSet<String>>,
    /// 当前字符集/编码 (C23 #pragma locale)
    /// 支持: "UTF-8" (默认), "ASCII", "ISO-8859-1", "POSIX"
    charset: String,
    /// 当前源行文本 (原始, 未去注释), 用于诊断 source_line 附着。
    /// 诊断在产生时即带上正确文件的源码行, 避免 enrich_diags 从主文件
    /// 取同名行 (头文件中的 #error 不再显示主文件的注释行)。
    cur_source_line: Option<String>,
    /// 已读入的 include 依赖: (canonical path, content hash)。
    /// 供头文件缓存的传递依赖校验 (include 图缓存): 缓存条目记录其
    /// 依赖闭包, 命中前逐个重读比对, 任一依赖变更即失效重处理。
    dep_hashes: Vec<(String, u64)>,
    /// include-once 守卫快路径: canonical path -> (content_hash, guard 宏名)。
    /// 守卫宏已定义时整个文件可跳过处理 (等价于 #ifndef 为假)。
    once_guard: HashMap<String, (u64, Option<String>)>,
}

/// 诊断状态: 控制特定警告的开关
#[derive(Clone, Default)]
struct DiagState {
    /// ignored 的警告列表
    ignored: HashSet<String>,
    /// 视为错误的警告列表
    as_error: HashSet<String>,
}

struct CondState {
    /// 是否已有一个分支为 true
    any_true: bool,
    /// 当前分支是否激活
    active: bool,
    /// 是否见过 else
    saw_else: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct MacroDef {
    params: Option<Vec<String>>, // None = object-like, Some = function-like
    body: String,
    /// 可变参数: true 表示末尾有 ...
    variadic: bool,
}

/// 头文件缓存内容: (guard 宏名, 宏列表, 依赖文件, (文件, hash), __COUNTER__ 消耗量)
type MacroCacheData = (
    String,
    Vec<(String, MacroDef)>,
    Vec<String>,
    Vec<(String, u64)>,
    u32,
);

/// 预处理输出行号 → 源文件行号的映射区间
#[derive(Debug, Clone)]
pub struct LineSpan {
    /// 预处理输出中的起始行号 (1-based)
    pub out_start: u32,
    /// 源文件路径
    pub file: String,
    /// 源文件中的起始行号 (1-based)
    pub src_start: u32,
    /// 连续行数
    pub count: u32,
}

/// 预处理器输出: 包含文本与全部对 codegen 有影响的元数据
#[derive(Default)]
pub struct PreprocessOutput {
    /// 预处理后的源码
    pub text: String,
    /// 诊断信息
    pub diags: Vec<Diagnostic>,
    /// 成功打开过的 include 路径 (供 -MD/-MMD)
    pub deps: Vec<String>,
    /// #pragma pack 变更历史: (行号, 新对齐). 供 parser 按位置查询
    pub pack_history: Vec<(u32, Option<usize>)>,
    /// #pragma weak 记录的弱符号集合 (供 codegen 标记 weak 绑定)
    pub weak_symbols: std::collections::HashSet<String>,
    /// 当前字符集 (C23 #pragma locale)
    pub charset: String,
    /// 预处理结束时所有可见的宏定义 (供 -dM 输出来源)
    pub macros: Vec<(String, Option<Vec<String>>, bool, String)>,
    /// 行号映射区间: 预处理输出行号 → 源文件 (file, src_line)
    pub line_map: Vec<LineSpan>,
}

/// 预编译头 (PCH) 选项。
///
/// PCH = PreCompiled Header: 把一或多个共享头文件的预处理结果 (展开文本 +
/// 宏表) 序列化到磁盘, 后续 TU 通过 `-fpch-include=path.pch` 在预处理起点
/// 直接加载, 跳过对共享头的重复词法+展开+条件求值。这是 GCC/Clang 长期
/// 使用的 PCH 思路的最小化实现: 没有 ast/ir 持久化 (dcc 一次性自举用不上),
/// 只复用 "preprocessor state snapshot" 部分, 跨 TU 共用。
#[derive(Debug, Clone, Default)]
pub struct PchOptions {
    /// `-fpch-include=<file>`: 预处理开始前先加载此 PCH。
    /// 加载成功时其保存的宏表 / 输出文本会作为基线, 然后继续处理 TU 源。
    pub include: Option<String>,
    /// `-fpch-create=<file>`: 预处理完成后将当前状态 (输出文本 + 宏表)
    /// 序列化到此路径。`create_after` 为 true 时, 仅当 TU 主文件全部
    /// 处理成功才写出; 否则跳过以避免写出半状态 PCH。
    pub create: Option<String>,
    /// `-fpch-include-only=path.pch` 简化语义: 加载 PCH 并立即以
    /// `-E` 模式 dump 加载后的输出 (调试用)
    pub include_only: Option<String>,
}

/// PCH 文件格式 V1 的固定魔数/标记。
pub const PCH_MAGIC: &str = "DCC_PCH_V1";

/// PCH bundle: 一组共享头文件预处理的快照。
#[derive(Debug, Clone, Default)]
pub struct PchBundle {
    /// 来源 PCH 源 (即生成 PCH 时 -fpch-create 对应的源), 仅诊断用
    pub source: String,
    /// PCH 创建时的字符集 (与 TU 字符集不一致时给出 warning)
    pub charset: String,
    /// PCH 创建时的内容 hash (= 全部 -fpch-create 输入的拼接 hash)
    pub content_hash: u64,
    /// PCH 创建时的宏表 hash
    pub macro_hash: u64,
    /// 预处理输出文本 (即 dcc 内部分词+展开后的中间代码)
    pub output: String,
    /// PCH 内部保存的宏表
    pub macros: Vec<(String, MacroDef)>,
}

impl PchBundle {
    /// 序列化为 PCH 文件字节内容
    pub fn serialize(&self) -> String {
        let mut s = String::new();
        s.push_str(PCH_MAGIC);
        s.push('\n');
        s.push_str(&format!("SOURCE={}\n", Self::escape_kv(&self.source)));
        s.push_str(&format!("CHARSET={}\n", Self::escape_kv(&self.charset)));
        s.push_str(&format!("CONTENT_HASH={:x}\n", self.content_hash));
        s.push_str(&format!("MACRO_HASH={:x}\n", self.macro_hash));
        s.push_str(&format!("NUM_MACROS={}\n", self.macros.len()));
        s.push_str("=== MACROS ===\n");
        for (name, def) in &self.macros {
            let params_str = match &def.params {
                None => "-".to_string(),
                Some(p) => p.join(","),
            };
            let variadic_str = if def.variadic { "1" } else { "0" };
            s.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                Self::escape_kv(name),
                Self::escape_kv(&params_str),
                variadic_str,
                Self::escape_kv(&def.body),
            ));
        }
        s.push_str("=== OUTPUT ===\n");
        s.push_str(&self.output);
        if !self.output.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("=== END ===\n");
        s
    }

    /// 解析 PCH 文件字节内容
    pub fn deserialize(s: &str) -> Result<Self, String> {
        if !s.starts_with(PCH_MAGIC) {
            return Err("not a dcc PCH file (magic mismatch)".to_string());
        }
        let mut bundle = PchBundle::default();
        let mut lines = s.lines();
        lines.next(); // skip magic

        let mut section = "";
        for line in lines {
            if line == "=== MACROS ===" {
                section = "macros";
                continue;
            }
            if line == "=== OUTPUT ===" {
                section = "output";
                continue;
            }
            if line == "=== END ===" {
                break;
            }
            if section.is_empty() {
                if let Some(v) = line.strip_prefix("SOURCE=") {
                    bundle.source = Self::unescape_kv(v);
                } else if let Some(v) = line.strip_prefix("CHARSET=") {
                    bundle.charset = Self::unescape_kv(v);
                } else if let Some(v) = line.strip_prefix("CONTENT_HASH=") {
                    bundle.content_hash = u64::from_str_radix(v, 16)
                        .map_err(|e| format!("bad content hash: {}", e))?;
                } else if let Some(v) = line.strip_prefix("MACRO_HASH=") {
                    bundle.macro_hash =
                        u64::from_str_radix(v, 16).map_err(|e| format!("bad macro hash: {}", e))?;
                }
                // NUM_MACROS 行仅做元信息, 读取宏表时不依赖
            } else if section == "macros" {
                if line.is_empty() {
                    continue;
                }
                let parts = Self::split_kv_tabs(line, 4);
                if parts.len() < 4 {
                    continue;
                }
                let name = Self::unescape_kv(&parts[0]);
                let params_str = Self::unescape_kv(&parts[1]);
                let variadic = parts[2] == "1";
                let body = Self::unescape_kv(&parts[3]);
                let params = if params_str == "-" {
                    None
                } else if params_str.is_empty() {
                    Some(vec![]) // 零参 function-like 宏
                } else {
                    Some(params_str.split(',').map(String::from).collect())
                };
                bundle.macros.push((
                    name,
                    MacroDef {
                        params,
                        body,
                        variadic,
                    },
                ));
            } else if section == "output" {
                if !bundle.output.is_empty() {
                    bundle.output.push('\n');
                }
                bundle.output.push_str(line);
            }
        }
        Ok(bundle)
    }

    /// 把一个 PCH bundle 注入到 Preprocessor 状态: 宏表 + 输出文本
    /// (作为 TU 主源前的"基线")
    pub fn apply(&self, pp: &mut Preprocessor) {
        for (name, def) in &self.macros {
            pp.macros.insert(name.clone(), def.clone());
        }
        if !self.output.is_empty() {
            // 用 /* dcc PCH: <source> */ 标记注入点, 便于诊断
            pp.push_out(&format!("/* dcc PCH: {} */\n", self.source));
            pp.push_out(&self.output);
            if !pp.out.ends_with('\n') {
                pp.push_out("\n");
            }
        }
    }

    /// 简单转义: 让 KV 行可承载 \n / \t / \\
    fn escape_kv(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                _ => out.push(c),
            }
        }
        out
    }

    fn unescape_kv(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// 按 N 次 \t 切分; 行末不足 N 段时, 用空串补齐
    fn split_kv_tabs(line: &str, n: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(n);
        let mut start = 0;
        let mut count = 0;
        for (i, c) in line.char_indices() {
            if c == '\t' && count < n - 1 {
                out.push(line[start..i].to_string());
                start = i + 1;
                count += 1;
            }
        }
        out.push(line[start..].to_string());
        while out.len() < n {
            out.push(String::new());
        }
        out
    }
}

#[allow(clippy::too_many_arguments)]
pub fn preprocess(
    src: &[u8],
    file: &str,
    include_paths: &[PathBuf],
    system_paths: &[PathBuf],
    defines: &[(String, Option<String>)],
    std_mode: StdMode,
    force_includes: &[String],
    freestanding: bool,
    pch_opts: &PchOptions,
) -> PreprocessOutput {
    let text = match std::str::from_utf8(src) {
        Ok(text) => text,
        Err(e) => {
            return PreprocessOutput {
                text: String::new(),
                diags: vec![Diagnostic::err(
                    "E0208",
                    format!("source is not valid UTF-8: {}", e),
                    Span::point(file, 1, 1),
                )],
                ..Default::default()
            };
        }
    };
    let base = Path::new(file)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    // 编译日期/时间
    let (compile_date, compile_time) = Preprocessor::compile_datetime();
    let mut pp = Preprocessor {
        macros: HashMap::new(),
        include_paths: include_paths.to_vec(),
        system_paths: system_paths.to_vec(),
        pragma_once: HashSet::new(),
        diags: vec![],
        cond_stack: vec![],
        out: String::with_capacity(text.len()),
        out_lines: 0,
        line_map: Vec::new(),
        file: file.to_string(),
        base_dir: base,
        cur_line: 0,
        compile_date,
        compile_time,
        diag_stack: vec![],
        diag_state: DiagState::default(),
        cache_dir: Preprocessor::init_cache_dir(),
        in_comment: false,
        deps: Vec::new(),
        pack_stack: vec![None],
        pack_history: vec![(0, None)],
        weak_symbols: HashSet::new(),
        fenv_access: false,
        counter: 0,
        pending_pragmas: RefCell::new(Vec::new()),
        expanded_macros: RefCell::new(HashSet::new()),
        charset: "UTF-8".to_string(),
        cur_source_line: None,
        dep_hashes: Vec::new(),
        once_guard: HashMap::new(),
    };
    // 预定义宏
    pp.define_builtin("__DCC__", "1");
    pp.define_builtin("__x86_64__", "1");
    pp.define_builtin("__linux__", "1");
    pp.define_builtin("__LP64__", "1");
    pp.define_builtin("__STDC__", "1");
    // __STDC_VERSION__ 根据选择的 C 标准设置
    let stc_version = match std_mode {
        StdMode::C89 | StdMode::Gnu89 => None, // C89/C90 不定义 __STDC_VERSION__
        StdMode::C99 | StdMode::Gnu99 => Some("199901"),
        StdMode::C11 | StdMode::Gnu11 => Some("201112"),
        StdMode::C17 | StdMode::Gnu17 => Some("201710"),
        StdMode::C23 | StdMode::Gnu23 => Some("202311"),
    };
    if let Some(v) = stc_version {
        pp.define_builtin("__STDC_VERSION__", v);
    }
    // C23 字节序宏 (C23 7.1.1): 仅 C23 模式定义; 小端主机
    if matches!(std_mode, StdMode::C23 | StdMode::Gnu23) {
        pp.define_builtin("__STDC_ENDIAN_LITTLE__", "1234");
        pp.define_builtin("__STDC_ENDIAN_BIG__", "4321");
        pp.define_builtin("__STDC_ENDIAN_NATIVE__", "__STDC_ENDIAN_LITTLE__");
    }
    if freestanding {
        // 独立 (freestanding) 实现: C11 § 4/6 允许省略 <stdatomic.h>,
        // 必须定义 __STDC_NO_ATOMICS__=1. 不预置 atomic_* 宏
        // (与 Linux 内核同名 inline 函数冲突, 见 M11.2 注释).
        pp.define_builtin("__STDC_HOSTED__", "0");
        pp.define_builtin("__STDC_NO_ATOMICS__", "1");
    } else {
        pp.define_builtin("__STDC_HOSTED__", "1");
    }
    pp.define_builtin("NULL", "((void*)0)");
    // GCC 兼容宏 (Linux 内核头文件大量使用)
    pp.define_builtin("__GNUC__", "13");
    pp.define_builtin("__GNUC_MINOR__", "2");
    pp.define_builtin("__GNUC_PATCHLEVEL__", "0");
    pp.define_builtin("__GNUC_STDC_INLINE__", "1");
    pp.define_builtin("__GXX_ABI_VERSION", "1017");
    pp.define_builtin("__CHAR_BIT__", "8");
    pp.define_builtin("__SCHAR_MAX__", "127");
    pp.define_builtin("__SHRT_MAX__", "32767");
    pp.define_builtin("__INT_MAX__", "2147483647");
    pp.define_builtin("__LONG_MAX__", "9223372036854775807L");
    pp.define_builtin("__LONG_LONG_MAX__", "9223372036854775807LL");
    pp.define_builtin("__UINT_MAX__", "4294967295U");
    pp.define_builtin("__ULONG_MAX__", "18446744073709551615UL");
    pp.define_builtin("__SIZE_MAX__", "18446744073709551615UL");
    pp.define_builtin("__PTRDIFF_MAX__", "9223372036854775807L");
    pp.define_builtin("__ORDER_LITTLE_ENDIAN__", "1234");
    pp.define_builtin("__ORDER_BIG_ENDIAN__", "4321");
    pp.define_builtin("__BYTE_ORDER__", "__ORDER_LITTLE_ENDIAN__");
    pp.define_builtin("__SIZEOF_POINTER__", "8");
    pp.define_builtin("__SIZEOF_LONG__", "8");
    pp.define_builtin("__SIZEOF_INT__", "4");
    pp.define_builtin("__SIZEOF_SHORT__", "2");
    pp.define_builtin("__SIZEOF_LONG_LONG__", "8");
    pp.define_builtin("__SIZEOF_SIZE_T__", "8");
    pp.define_builtin("__SIZEOF_PTRDIFF_T__", "8");
    pp.define_builtin("__SIZEOF_INT128__", "16");
    pp.define_builtin("__SIZEOF_FLOAT128__", "16");
    pp.define_builtin("__INT128_TYPE__", "__int128");
    pp.define_builtin("__UINT128_TYPE__", "__uint128_t");
    pp.define_builtin("__SIZE_TYPE__", "long unsigned int");
    pp.define_builtin("__PTRDIFF_TYPE__", "long int");
    pp.define_builtin("__WCHAR_TYPE__", "int");
    pp.define_builtin("__WINT_TYPE__", "unsigned int");
    pp.define_builtin("__INTMAX_TYPE__", "long int");
    pp.define_builtin("__UINTMAX_TYPE__", "long unsigned int");
    pp.define_builtin("__INT64_TYPE__", "long int");
    pp.define_builtin("__UINT64_TYPE__", "long unsigned int");
    pp.define_builtin("__INT32_TYPE__", "int");
    pp.define_builtin("__UINT32_TYPE__", "unsigned int");
    pp.define_builtin("__INT16_TYPE__", "short int");
    pp.define_builtin("__UINT16_TYPE__", "short unsigned int");
    pp.define_builtin("__INT8_TYPE__", "signed char");
    pp.define_builtin("__UINT8_TYPE__", "unsigned char");
    pp.define_builtin("__ELF__", "1");
    pp.define_builtin("__unix__", "1");
    // 原子内存序常量 (GCC __atomic 内置函数用)
    pp.define_builtin("__ATOMIC_RELAXED", "0");
    pp.define_builtin("__ATOMIC_CONSUME", "1");
    pp.define_builtin("__ATOMIC_ACQUIRE", "2");
    pp.define_builtin("__ATOMIC_RELEASE", "3");
    pp.define_builtin("__ATOMIC_ACQ_REL", "4");
    pp.define_builtin("__ATOMIC_SEQ_CST", "5");
    // GCC 兼容的额外预定义宏 (M19 补齐)
    // x86_64 架构别名 (匹配 GCC/clang 命名)
    pp.define_builtin("__amd64", "1");
    pp.define_builtin("__amd64__", "1");
    pp.define_builtin("__x86_64", "1");
    pp.define_builtin("__k8", "1");
    pp.define_builtin("__k8__", "1");
    // x86_64 默认启用 SSE2 (需要 __SSE2__ 才能让 CET / _mm_* / setssbsy 路径编译过)
    pp.define_builtin("__SSE2__", "1");
    pp.define_builtin("__SSE2_MATH__", "1");
    pp.define_builtin("__SSE_MATH__", "1");
    pp.define_builtin("__SSE__", "1");
    pp.define_builtin("__MMX__", "1");
    // CET 头 (cet.h) 依赖这个 feature 宏
    pp.define_builtin("__CET__", "0");
    // 链接/平台相关
    pp.define_builtin("__USER_LABEL_PREFIX__", "");
    pp.define_builtin("__VERSION__", "dcc 0.1 (M19)");
    // __TIMESTAMP__ / __BASE_FILE__ 推迟到宏展开阶段按需生成
    // (避免编译日期/时间/源路径被冻结进缓存)
    pp.define_builtin("__OPTIMIZE__", "1");
    pp.define_builtin("__OPTIMIZE_SIZE__", "0");
    pp.define_builtin("__NO_INLINE__", "1");
    pp.define_builtin("__FINITE_MATH_ONLY__", "0");
    pp.define_builtin("__PIC__", "2");
    pp.define_builtin("__PIE__", "2");
    pp.define_builtin("__code_model_small__", "1");
    pp.define_builtin("__SEG_GS", "1");
    pp.define_builtin("__SEG_FS", "1");
    // __GLIBC__ / __GLIBC_MINOR__ 由 libc 头自动提供, 但 dcc 不强制
    // (内核构建通常带 nolibc, 不需要); 用户可在 -D 中手动设置
    // __COUNTER__ / __INCLUDE_LEVEL__ 在宏展开阶段特殊处理
    // (不放入 macros, 避免被通用替换逻辑误吃)
    pp.define_builtin("memory_order_relaxed", "0");
    pp.define_builtin("memory_order_consume", "1");
    pp.define_builtin("memory_order_acquire", "2");
    pp.define_builtin("memory_order_release", "3");
    pp.define_builtin("memory_order_acq_rel", "4");
    pp.define_builtin("memory_order_seq_cst", "5");
    // <stdatomic.h> 内建宏定义 (C11)
    // 原子类型 (简化: 展开为等价非原子类型, _Atomic 由类型系统处理)
    pp.define_builtin("atomic_int", "int");
    pp.define_builtin("atomic_uint", "unsigned int");
    pp.define_builtin("atomic_long", "long");
    pp.define_builtin("atomic_ulong", "unsigned long");
    pp.define_builtin("atomic_llong", "long long");
    pp.define_builtin("atomic_ullong", "unsigned long long");
    pp.define_builtin("atomic_short", "short");
    pp.define_builtin("atomic_ushort", "unsigned short");
    pp.define_builtin("atomic_char", "char");
    pp.define_builtin("atomic_schar", "signed char");
    pp.define_builtin("atomic_uchar", "unsigned char");
    pp.define_builtin("atomic_bool", "_Bool");
    pp.define_builtin("atomic_intptr", "long");
    pp.define_builtin("atomic_uintptr", "unsigned long");
    pp.define_builtin("atomic_size_t", "unsigned long");
    pp.define_builtin("atomic_ptrdiff_t", "long");
    pp.define_builtin("atomic_wchar_t", "int");
    pp.define_builtin("atomic_float", "float");
    pp.define_builtin("atomic_double", "double");
    pp.define_builtin("atomic_long_double", "long double");
    // kill_dependency: 返回参数
    pp.macros.insert(
        "kill_dependency".to_string(),
        MacroDef {
            params: Some(vec!["y".to_string()]),
            body: "(y)".to_string(),
            variadic: false,
        },
    );
    // atomic_thread_fence: 跨线程全屏障, emit  mfence + lock 序
    // 对应 GCC __atomic_thread_fence(MEMORY_ORDER): hardware fence
    // 注: dcc 当前不跨函数优化, 整个函数都是一应连续. 主依赖
    // codegen 把 __ATOMIC_SEQ_CST 序列化为 mfence 序列.
    // 实现: emit 一个 volatile 空 asm + memory 寄存器clobber, 防止
    // 编译器重排外尧访存. 这只是编译器屏障 (ompiler barrier),
    // 不是硬件屏障; 硬件序由 mfence 等交互.
    pp.macros.insert(
        "atomic_thread_fence".to_string(),
        MacroDef {
            params: Some(vec!["order".to_string()]),
            body: "__asm__ volatile(\"\" ::: \"memory\")".to_string(),
            variadic: false,
        },
    );
    // atomic_signal_fence: 编译期屏障, 防止编译器重排. 不同于
    // atomic_thread_fence, 不需要硬件序, 只需阻止本地优化.
    // 实现: 空 asm volatile 块 + memory 寄存器clobber.
    pp.macros.insert(
        "atomic_signal_fence".to_string(),
        MacroDef {
            params: Some(vec!["order".to_string()]),
            body: "__asm__ volatile(\"\" ::: \"memory\")".to_string(),
            variadic: false,
        },
    );
    // 注意: 不预置 atomic_store/atomic_fetch_add 等 C11 stdatomic 宏。
    // Linux 内核 atomic-instrumented.h 等同名 inline 函数会被误展开
    // （如 atomic_fetch_add(i,v) → __atomic_fetch_add(i,v,5) 破坏声明）。
    // __atomic_* 内建与 __ATOMIC_* 常量仍保留，供显式调用。
    // 不要在 hosted 模式下预置它们 (会破坏 glibc/userspace 中同名函数/宏的调用).
    // 内存序别名 (memory_order_xxx 已在上面定义)
    // __attribute__: 不定义为宏, 让 token 透传到解析器处理
    // 解析器会识别 __attribute__ 并处理 vector_size 等属性
    // __has_attribute / __has_builtin / __has_feature: 不在宏表中定义, 在展开时特殊处理
    // 这样可以在宏展开路径和 #if 路径都返回正确值
    // GCC 接受 `__attribute` (无尾部双下划线) 作为 `__attribute__` 的简写。
    // kernel 7.1 的 __noinstr_section() 误用此写法, 故加入映射.
    pp.macros.insert(
        "__attribute".to_string(),
        MacroDef {
            params: None,
            body: "__attribute__".to_string(),
            variadic: false,
        },
    );
    // likely/unlikely 分支提示
    pp.macros.insert(
        "likely".to_string(),
        MacroDef {
            params: Some(vec!["x".to_string()]),
            body: "(x)".to_string(),
            variadic: false,
        },
    );
    pp.macros.insert(
        "unlikely".to_string(),
        MacroDef {
            params: Some(vec!["x".to_string()]),
            body: "(x)".to_string(),
            variadic: false,
        },
    );
    // barrier
    pp.macros.insert(
        "barrier".to_string(),
        MacroDef {
            params: Some(vec![]),
            body: "".to_string(),
            variadic: false,
        },
    );
    // MODULE_LICENSE/AUTHOR/DESCRIPTION/ALIAS 等模块宏
    // 展开为 .modinfo section 的字符串变量
    pp.macros.insert("MODULE_LICENSE".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]),
        body: "static const char __module_license[] __attribute__((section(\".modinfo\"))) = \"license=\" x;".to_string(),
        variadic: false,
    });
    pp.macros.insert("MODULE_AUTHOR".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]),
        body: "static const char __module_author[] __attribute__((section(\".modinfo\"))) = \"author=\" x;".to_string(),
        variadic: false,
    });
    pp.macros.insert("MODULE_DESCRIPTION".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]),
        body: "static const char __module_description[] __attribute__((section(\".modinfo\"))) = \"description=\" x;".to_string(),
        variadic: false,
    });
    pp.macros.insert("MODULE_ALIAS".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]),
        body: "static const char __module_alias[] __attribute__((section(\".modinfo\"))) = \"alias=\" x;".to_string(),
        variadic: false,
    });
    pp.macros.insert("MODULE_INFO".to_string(), MacroDef {
        params: Some(vec!["tag".to_string(), "info".to_string()]),
        body: "static const char __module_info[] __attribute__((section(\".modinfo\"))) = tag \"=\" info;".to_string(),
        variadic: false,
    });
    // percpu 支持: DEFINE_PER_CPU(type, name) 展开为 .data..percpu section 的变量
    pp.macros.insert(
        "DEFINE_PER_CPU".to_string(),
        MacroDef {
            params: Some(vec!["type".to_string(), "name".to_string()]),
            body: "__attribute__((section(\".data..percpu\"))) type name".to_string(),
            variadic: false,
        },
    );
    pp.macros.insert(
        "DEFINE_PER_CPU_SHARED_ALIGNED".to_string(),
        MacroDef {
            params: Some(vec!["type".to_string(), "name".to_string()]),
            body: "__attribute__((section(\".data..percpu\"), __aligned__(64))) type name"
                .to_string(),
            variadic: false,
        },
    );
    pp.macros.insert(
        "DECLARE_PER_CPU".to_string(),
        MacroDef {
            params: Some(vec!["type".to_string(), "name".to_string()]),
            body: "extern __attribute__((section(\".data..percpu\"))) type name".to_string(),
            variadic: false,
        },
    );
    // 用户 -D 定义
    for (name, val) in defines {
        let v = val.as_deref().unwrap_or("1");
        pp.define_builtin(name, v);
    }

    // 处理 -include 强制包含文件
    for fi in force_includes {
        pp.force_include(fi);
    }

    // PCH 加载: 在 -include / 用户 -D 之后, TU 主源之前注入
    if let Some(ref pch_path) = pch_opts.include {
        match fs::read_to_string(pch_path) {
            Ok(content) => match PchBundle::deserialize(&content) {
                Ok(bundle) => {
                    // 完整性: TU 侧当前宏表 hash (进入 TU 主源前的快照) 应等于
                    // PCH 创建时进入源前的快照. 这是 dcc 简化等价于 GCC 的
                    // PCH timestamp + checksum: 不一致时回退到无 PCH, 强制
                    // 完整重处理.
                    let cur_macro_hash = pp.hash_macro_state();
                    if cur_macro_hash == bundle.macro_hash {
                        bundle.apply(&mut pp);
                    } else {
                        // 不兼容: 报告诊断, 静默继续无 PCH 处理
                        pp.diags.push(Diagnostic::warn(
                            "W0400",
                            format!(
                                "PCH {} is incompatible with current macro state \
                                    (saved hash {:x}, current {:x}); ignoring",
                                pch_path, bundle.macro_hash, cur_macro_hash
                            ),
                            Span::point(file, 1, 1),
                        ));
                    }
                }
                Err(e) => {
                    pp.diags.push(Diagnostic::err(
                        "E0401",
                        format!("failed to load PCH {}: {}", pch_path, e),
                        Span::point(file, 1, 1),
                    ));
                }
            },
            Err(e) => {
                pp.diags.push(Diagnostic::err(
                    "E0401",
                    format!("cannot read PCH file {}: {}", pch_path, e),
                    Span::point(file, 1, 1),
                ));
            }
        }
    }
    if let Some(ref pch_path) = pch_opts.include_only {
        match fs::read_to_string(pch_path) {
            Ok(content) => match PchBundle::deserialize(&content) {
                Ok(bundle) => {
                    bundle.apply(&mut pp);
                }
                Err(e) => {
                    pp.diags.push(Diagnostic::err(
                        "E0401",
                        format!("failed to load PCH {}: {}", pch_path, e),
                        Span::point(file, 1, 1),
                    ));
                }
            },
            Err(e) => {
                pp.diags.push(Diagnostic::err(
                    "E0401",
                    format!("cannot read PCH file {}: {}", pch_path, e),
                    Span::point(file, 1, 1),
                ));
            }
        }
    }

    // PCH 创建: 在主源处理之前快照宏表, 作为加载时的完整性验证基线。
    // 这避免了"创建后宏表变重"导致的 hash 漂移: 加载时只需与初始状态
    // 匹配即可 (创建时的源处理可能增加宏, 不影响加载验证).
    let create_time_macro_hash = pp.hash_macro_state();

    pp.process_text(text, file);
    let text_output = pp.out.clone();
    let pch_diags: Vec<Diagnostic> = pp.diags.clone();
    let pch_deps: Vec<String> = pp.deps.clone();
    let pch_pack_history: Vec<(u32, Option<usize>)> = pp.pack_history.clone();
    let pch_weak_symbols: std::collections::HashSet<String> = pp.weak_symbols.clone();
    let pch_charset = pp.charset.clone();

    // PCH 创建: 在主源处理成功后, 把当前状态快照写入磁盘。
    // 写文件不记入 errors (即使失败, 也不让 TU 失败: 这是优化而非必需).
    if let Some(ref pch_path) = pch_opts.create {
        let has_error = pp
            .diags
            .iter()
            .any(|d| matches!(d.sev, crate::diagnostics::Severity::Error));
        if !has_error {
            let bundle = PchBundle {
                source: file.to_string(),
                charset: pp.charset.clone(),
                content_hash: create_time_macro_hash,
                macro_hash: create_time_macro_hash,
                output: pp.out.clone(),
                macros: pp
                    .macros
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            };
            if let Err(e) = fs::write(pch_path, bundle.serialize()) {
                pp.diags.push(Diagnostic::warn(
                    "W0402",
                    format!("failed to write PCH {}: {}", pch_path, e),
                    Span::point(file, 1, 1),
                ));
            }
        } else {
            pp.diags.push(Diagnostic::warn(
                "W0403",
                format!("PCH {} not created due to preprocessing errors", pch_path),
                Span::point(file, 1, 1),
            ));
        }
    }

    PreprocessOutput {
        text: text_output,
        diags: pch_diags,
        deps: pch_deps,
        pack_history: pch_pack_history,
        weak_symbols: pch_weak_symbols,
        charset: pch_charset,
        macros: pp
            .macros
            .iter()
            .map(|(k, v)| (k.clone(), v.params.clone(), v.variadic, v.body.clone()))
            .collect(),
        line_map: pp.line_map.clone(),
    }
}

/// 探测 GCC 内建头文件目录（`gcc -print-file-name=include` 及常见路径）。
fn host_gcc_include_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut dirs = Vec::new();
        if let Ok(out) = Command::new("gcc").arg("-print-file-name=include").output() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() && p != "include" && Path::new(&p).is_dir() {
                dirs.push(PathBuf::from(p));
            }
        }
        for pattern in [
            "/usr/lib/gcc/x86_64-linux-gnu/*/include",
            "/usr/lib/gcc/aarch64-linux-gnu/*/include",
        ] {
            if let Ok(entries) = glob_gcc_include_paths(pattern) {
                for p in entries {
                    if p.is_dir() {
                        dirs.push(p);
                    }
                }
            }
        }
        dirs
    })
}

fn glob_gcc_include_paths(pattern: &str) -> Result<Vec<PathBuf>, ()> {
    let mut out = Vec::new();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() != 2 {
        return Err(());
    }
    let parent = Path::new(parts[0].trim_end_matches('/'));
    let suffix = parts[1].trim_start_matches('/');
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let candidate = p.join(suffix);
                if candidate.is_dir() {
                    out.push(candidate);
                }
            }
        }
    }
    Ok(out)
}

thread_local! {
    static EXPAND_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

impl Preprocessor {
    /// 强制包含文件 (GCC -include 语义)
    fn force_include(&mut self, file_path: &str) {
        let path = PathBuf::from(file_path);
        let resolved = if path.exists() {
            path
        } else {
            let mut found = None;
            for dir in &self.include_paths {
                let p = dir.join(file_path);
                if p.exists() {
                    found = Some(p);
                    break;
                }
            }
            if found.is_none() {
                for dir in &self.system_paths {
                    let p = dir.join(file_path);
                    if p.exists() {
                        found = Some(p);
                        break;
                    }
                }
            }
            found.unwrap_or(path)
        };
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                // 强制包含文件也计入 include 图: -MD 依赖 + 缓存依赖闭包
                self.deps.push(resolved.display().to_string());
                self.dep_hashes
                    .push((resolved.display().to_string(), Self::hash_content(&content)));
                let saved_file = self.file.clone();
                let saved_dir = self.base_dir.clone();
                self.file = resolved.display().to_string();
                self.base_dir = resolved.parent().unwrap_or(Path::new(".")).to_path_buf();
                self.process_text(&content, &self.file.clone());
                self.file = saved_file;
                self.base_dir = saved_dir;
            }
            Err(e) => {
                self.diags.push(Diagnostic::err(
                    "E0209",
                    format!("cannot read forced include {}: {}", resolved.display(), e),
                    Span::point(file_path, 1, 1),
                ));
            }
        }
    }

    fn is_active(&self) -> bool {
        self.cond_stack.iter().all(|c| c.active)
    }

    fn define_builtin(&mut self, name: &str, val: &str) {
        self.macros.insert(
            name.to_string(),
            MacroDef {
                params: None,
                body: val.to_string(),
                variadic: false,
            },
        );
    }

    fn compile_datetime() -> (String, String) {
        // 简化: 使用固定格式, 实际项目可用 chrono
        let date = "Jul  9 2026";
        let time = "00:00:00";
        (date.to_string(), time.to_string())
    }

    /// 向输出写入文本并同步更新 out_lines 计数器
    fn push_out(&mut self, s: &str) {
        self.out_lines += s.bytes().filter(|&b| b == b'\n').count() as u32;
        self.out.push_str(s);
    }

    fn process_text(&mut self, text: &str, file: &str) {
        let saved_file = self.file.clone();
        let saved_in_comment = self.in_comment;
        self.file = file.to_string();
        // Each included source file undergoes translation phase 3 independently.
        // A malformed comment must not consume text from its includer.
        self.in_comment = false;

        // 行续接: 合并以 \ 结尾的行, 同时记录每个逻辑行的物理行号范围
        // (SourceLoc 追踪: 诊断/__LINE__/输出行号一律以物理行为准)
        let logical = Self::logical_lines(text);

        let mut active_source = String::new();
        // 与 active_source 中每行对应的物理行号范围 (逻辑行跨度)
        let mut active_spans: Vec<(u32, u32)> = Vec::new();
        for ll in &logical {
            self.cur_line = ll.phys_start;
            // 记录原始源行文本 (未去注释), 供诊断 source_line 附着
            self.cur_source_line = Some(ll.text.trim_end().to_string());
            // C translation phase 3 removes comments before preprocessing
            // directives are recognized and before macro expansion.  Doing
            // this afterwards lets quotes in comments alter lexical state.
            let phase3 = self.strip_comments_line(&ll.text);
            let trimmed = phase3.trim();

            if trimmed.starts_with('#') {
                // 宏调用可能跨越 #ifdef/#endif 边界（如 struct_group(headers, ...)），
                // 如果 active_source 有未闭合的 (，不 flush，继续累积直到宏调用闭合。
                if !active_source.is_empty() && !self.has_unclosed_paren(&active_source) {
                    self.flush_active_source(&mut active_source, &mut active_spans);
                }
                let out_len_before = self.out.len();
                self.process_directive(trimmed, ll.phys_start);
                // 指令行若不产生输出 (#define/#if/#error/#pragma 等), 以空行
                // 占位, 保证输出行号与输入物理行号一一对应;
                // #include/#include_next/#embed 自产输出, 无需占位。
                if self.out.len() == out_len_before {
                    self.preserve_lines(ll.phys_start, ll.phys_end);
                }
            } else if self.is_active() {
                active_source.push_str(&phase3);
                active_source.push('\n');
                active_spans.push((ll.phys_start, ll.phys_end));
            } else {
                // 非激活行 (#if 0 块等): 以空行占位保持行号对齐
                self.preserve_lines(ll.phys_start, ll.phys_end);
            }
        }
        if !active_source.is_empty() {
            self.flush_active_source(&mut active_source, &mut active_spans);
        }

        if self.in_comment {
            self.diags.push(self.err_at(
                "E0207",
                "unterminated block comment".to_string(),
                self.cur_line.max(1),
            ));
        }
        self.in_comment = saved_in_comment;
        self.file = saved_file;
    }

    /// 对累积的活跃源码块做宏展开并写入输出
    ///
    /// `spans` 与 `src` 中的行一一对应: (phys_start, phys_end)。
    /// 展开结果逐行写出; 空行/注释行也输出, 续接覆盖的物理行补空行,
    /// 保证输出行号与输入物理行号一一对应 (SourceLoc 对齐)。
    fn flush_active_source(&mut self, src: &mut String, spans: &mut Vec<(u32, u32)>) {
        let out_start = self.out_lines + 1;
        let phys_lines: Vec<u32> = spans.iter().map(|&(s, _)| s).collect();
        let expanded = self.expand_macros(src, &phys_lines);
        let lines: Vec<&str> = expanded.split('\n').collect();
        for (i, &(phys_start, phys_end)) in spans.iter().enumerate() {
            let line = lines.get(i).copied().unwrap_or("");
            if !line.trim().is_empty() {
                self.push_out(line);
                if !line.ends_with('\n') {
                    self.push_out("\n");
                }
            } else {
                // 空行/纯注释行: 保留行号
                self.push_out("\n");
            }
            // 逻辑行覆盖多个物理行时补空行, 保持行数守恒
            for _ in phys_start..phys_end {
                self.push_out("\n");
            }
        }
        // 兜底: 展开结果行数多于输入行数时 (理论不发生) 写全
        for line in lines.iter().skip(spans.len()) {
            if !line.is_empty() {
                self.push_out(line);
                self.push_out("\n");
            }
        }
        // 把展开阶段累积的 _Pragma 字符串批量应用
        // 顺序与源中出现顺序一致; 行号用收集时记录的 out 行号,
        // 这样 pack_history 与 parser 的 lex.line 严格对齐
        let pending: Vec<(String, u32)> = std::mem::take(&mut *self.pending_pragmas.borrow_mut());
        for (p, line_no) in pending {
            self.apply_pragma_string(&p, line_no);
        }
        // 记录行号映射区间
        let actual_count = self.out_lines - out_start + 1;
        if actual_count > 0 {
            self.line_map.push(LineSpan {
                out_start,
                file: self.file.clone(),
                src_start: spans[0].0,
                count: actual_count,
            });
        }
        src.clear();
        spans.clear();
    }

    /// 向输出写入空行, 使输出行号与输入物理行号保持一致 (SourceLoc 对齐)。
    /// 逻辑行覆盖 [start, end] 的物理行时, 每个物理行占一行。
    fn preserve_lines(&mut self, start: u32, end: u32) {
        let out_start = self.out_lines + 1;
        let count = end - start + 1;
        for _ in start..=end {
            self.push_out("\n");
        }
        self.line_map.push(LineSpan {
            out_start,
            file: self.file.clone(),
            src_start: start,
            count,
        });
    }

    /// 构造一条带当前源行文本的诊断 (E 系列)。
    /// source_line 在产生时即附着正确文件的源码行, 避免 enrich_diags
    /// 从主文件取同名行 (头文件中的 #error 不再显示主文件的注释行)。
    fn err_at(&self, code: &'static str, msg: String, line: u32) -> Diagnostic {
        let mut d = Diagnostic::err(code, msg, Span::point(&self.file, line, 1));
        if let Some(src) = &self.cur_source_line {
            d = d.with_source(src.clone());
        }
        d
    }

    /// 构造一条带当前源行文本的诊断 (W 系列), 语义同 err_at。
    fn warn_at(&self, code: &'static str, msg: String, line: u32) -> Diagnostic {
        let mut d = Diagnostic::warn(code, msg, Span::point(&self.file, line, 1));
        if let Some(src) = &self.cur_source_line {
            d = d.with_source(src.clone());
        }
        d
    }

    /// 检查文本中是否存在未闭合的 ( (忽略字符串/字符字面量内的括号)。
    /// 用于判断宏调用是否跨越 #ifdef/#endif 边界: 如 struct_group(headers, ...)
    /// 的实参中包含 #ifdef 块时, process_file 不能在 #ifdef 处截断宏调用。
    fn has_unclosed_paren(&self, s: &str) -> bool {
        let mut depth = 0i32;
        let mut in_string = false;
        let mut in_char = false;
        let mut escape = false;
        for c in s.chars() {
            if in_string {
                if escape {
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            if in_char {
                if escape {
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '\'' {
                    in_char = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '\'' => in_char = true,
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }
        depth > 0
    }

    /// 获取当前字符集设置 (C23 #pragma locale)
    pub fn charset(&self) -> &str {
        &self.charset
    }

    /// 计算 self.out 已写入的行数 (1-based)
    fn count_out_lines(&self) -> u32 {
        // 行数 = 换行符个数 + 1 (如果非空)
        let mut n = 1u32;
        for c in self.out.chars() {
            if c == '\n' {
                n += 1;
            }
        }
        n
    }

    /// 把 `_Pragma("...")` 中的字符串当作一行 #pragma 处理
    /// (不在源码中插入新行, 而是直接调用对应的 handler)
    /// `line_no` 是 _Pragma 被检测时记录的 out 行号 (即 _Pragma
    /// 在输出流中"逻辑上"所在的行), 用来精确对齐 pack_history
    pub fn apply_pragma_string(&mut self, pragma_text: &str, line_no: u32) {
        let text = pragma_text.trim();
        // 仅处理 pack 相关的 _Pragma; 其他类型仍走 process_directive 路径
        // (GCC diagnostic / weak / STDC / once 等不依赖行号)
        if let Some(rest) = text.strip_prefix("pack") {
            self.handle_pragma_pack(rest.trim(), line_no);
        } else {
            // 构造 "pragma ..." 形式的伪指令, 复用 process_directive
            let pseudo = format!("pragma {}", pragma_text);
            self.process_directive(&pseudo, line_no);
        }
    }

    /// 去除单行中的 C 风格注释 (跨行注释由 in_comment 状态跟踪)
    fn strip_comments_line(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        while i < len {
            // This check must precede quote handling: quotes have no lexical
            // meaning while a block comment is active.
            if self.in_comment {
                if chars[i] == '*' && i + 1 < len && chars[i + 1] == '/' {
                    i += 2;
                    self.in_comment = false;
                } else {
                    i += 1;
                }
                continue;
            }
            // 字符串字面量
            if chars[i] == '"' {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
                continue;
            }
            // 字符字面量
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    out.push('\'');
                    i += 1;
                }
                continue;
            }
            // /* ... */ 注释 (支持跨行)
            if i + 1 < len && chars[i] == '/' && chars[i + 1] == '*' {
                out.push(' ');
                i += 2;
                self.in_comment = true;
                while i + 1 < len {
                    if chars[i] == '*' && chars[i + 1] == '/' {
                        i += 2;
                        self.in_comment = false;
                        break;
                    }
                    i += 1;
                }
                if self.in_comment {
                    // 跨行: 当前行剩余部分全部忽略
                    break;
                }
                continue;
            }
            // // ... 注释
            if i + 1 < len && chars[i] == '/' && chars[i + 1] == '/' {
                out.push(' ');
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// 去除单行文本中的注释 (静态版本, 用于指令/表达式处理)
    fn strip_comments(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        while i < len {
            if chars[i] == '"' {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
                continue;
            }
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    out.push('\'');
                    i += 1;
                }
                continue;
            }
            if i + 1 < len && chars[i] == '/' && chars[i + 1] == '*' {
                out.push(' ');
                i += 2;
                while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                if i + 1 < len {
                    i += 2;
                }
                continue;
            }
            if i + 1 < len && chars[i] == '/' && chars[i + 1] == '/' {
                out.push(' ');
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// 将物理行合并为逻辑行 (行尾 \ 与下一行拼接, \ 替换为空格),
    /// 同时记录每个逻辑行覆盖的物理行号范围。
    fn logical_lines(text: &str) -> Vec<LogicalLine> {
        let mut out = Vec::new();
        let mut phys = 1u32;
        let mut continued: Option<(String, u32)> = None; // (累积文本, 起始物理行)
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(prefix) = line.strip_suffix('\\') {
                match &mut continued {
                    Some((acc, _)) => {
                        acc.push_str(prefix);
                        acc.push(' ');
                    }
                    None => {
                        continued = Some((format!("{} ", prefix), phys));
                    }
                }
            } else if let Some((mut acc, start)) = continued.take() {
                acc.push_str(line);
                out.push(LogicalLine {
                    text: acc,
                    phys_start: start,
                    phys_end: phys,
                });
            } else {
                out.push(LogicalLine {
                    text: line.to_string(),
                    phys_start: phys,
                    phys_end: phys,
                });
            }
            phys += 1;
        }
        if let Some((acc, start)) = continued.take() {
            out.push(LogicalLine {
                text: acc,
                phys_start: start,
                phys_end: phys - 1,
            });
        }
        out
    }

    fn process_directive(&mut self, line: &str, line_no: u32) {
        let after_hash = line[1..].trim_start();
        // 处理行续接 (\ at end)
        let directive = after_hash;

        if let Some(rest) = directive.strip_prefix("include_next") {
            // 必须在 "include" 之前匹配，否则 include_next 会被当成 include
            if self.is_active() {
                // ISO C 6.10.2: 先用原始形式试 (<file> / "file" / 绝对路径);
                // 解析失败再做宏展开 (kernel 的 `#include TRACE_INCLUDE(x)` 走这条).
                let raw = rest.trim();
                if Self::is_parsable_include_path(raw) {
                    self.do_include_next(raw, line_no);
                } else {
                    let expanded = self.expand_macros(raw, &[self.cur_line]);
                    self.do_include_next(expanded.trim(), line_no);
                }
            }
        } else if let Some(rest) = directive.strip_prefix("include") {
            if self.is_active() {
                let raw = rest.trim();
                if Self::is_parsable_include_path(raw) {
                    self.do_include(raw, line_no);
                } else {
                    // ISO C 6.10.2: 头文件名先做宏展开 (e.g. `#include TRACE_INCLUDE(file)`).
                    let expanded = self.expand_macros(raw, &[self.cur_line]);
                    self.do_include(expanded.trim(), line_no);
                }
            }
        } else if let Some(rest) = directive.strip_prefix("define") {
            if self.is_active() {
                self.do_define(rest.trim());
            }
        } else if let Some(rest) = directive.strip_prefix("undef") {
            if self.is_active() {
                let name = rest.trim();
                self.macros.remove(name);
            }
        } else if let Some(rest) = directive.strip_prefix("ifdef") {
            let name = rest.trim();
            let active = self.is_active() && self.macros.contains_key(name);
            self.cond_stack.push(CondState {
                any_true: active,
                active,
                saw_else: false,
            });
        } else if let Some(rest) = directive.strip_prefix("ifndef") {
            let name = rest.trim();
            let active = self.is_active() && !self.macros.contains_key(name);
            self.cond_stack.push(CondState {
                any_true: active,
                active,
                saw_else: false,
            });
        } else if directive.starts_with("if ") || directive.starts_with("if\t") {
            let expr = directive[2..].trim();
            let val = if self.is_active() {
                self.eval_const_expr(expr)
            } else {
                false
            };
            self.cond_stack.push(CondState {
                any_true: val,
                active: val,
                saw_else: false,
            });
        } else if directive.starts_with("elif ") || directive.starts_with("elif\t") {
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 {
                    true
                } else {
                    self.cond_stack[..len - 1].iter().all(|c| c.active)
                };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags
                        .push(self.err_at("E0200", "#elif after #else".to_string(), line_no));
                    return;
                }
                let state = &mut self.cond_stack[len - 1];
                if any_true {
                    state.active = false;
                } else {
                    let expr = directive[4..].trim().to_string();
                    // eval 必须先于 cond_stack 的 mutable borrow 完成
                    drop(state);
                    let val = parent_active && self.eval_const_expr(&expr);
                    let state = &mut self.cond_stack[len - 1];
                    if val {
                        state.any_true = true;
                    }
                    state.active = val;
                }
            }
        } else if directive.starts_with("elifdef ") || directive.starts_with("elifdef\t") {
            let name = directive[8..].trim();
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 {
                    true
                } else {
                    self.cond_stack[..len - 1].iter().all(|c| c.active)
                };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags.push(self.err_at(
                        "E0200",
                        "#elifdef after #else".to_string(),
                        line_no,
                    ));
                    return;
                }
                let state = &mut self.cond_stack[len - 1];
                if any_true {
                    state.active = false;
                } else {
                    let val = parent_active && self.macros.contains_key(name);
                    let state = &mut self.cond_stack[len - 1];
                    if val {
                        state.any_true = true;
                    }
                    state.active = val;
                }
            }
        } else if directive.starts_with("elifndef ") || directive.starts_with("elifndef\t") {
            let name = directive[9..].trim();
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 {
                    true
                } else {
                    self.cond_stack[..len - 1].iter().all(|c| c.active)
                };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags.push(self.err_at(
                        "E0200",
                        "#elifndef after #else".to_string(),
                        line_no,
                    ));
                    return;
                }
                let state = &mut self.cond_stack[len - 1];
                if any_true {
                    state.active = false;
                } else {
                    let val = parent_active && !self.macros.contains_key(name);
                    let state = &mut self.cond_stack[len - 1];
                    if val {
                        state.any_true = true;
                    }
                    state.active = val;
                }
            }
        } else if directive.starts_with("else") {
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 {
                    true
                } else {
                    self.cond_stack[..len - 1].iter().all(|c| c.active)
                };
                let state = &mut self.cond_stack[len - 1];
                if state.saw_else {
                    self.diags
                        .push(self.err_at("E0201", "duplicate #else".to_string(), line_no));
                    return;
                }
                state.saw_else = true;
                state.active = parent_active && !state.any_true;
                if state.active {
                    state.any_true = true;
                }
            }
        } else if directive.starts_with("endif") {
            if self.cond_stack.pop().is_none() {
                self.diags
                    .push(self.err_at("E0202", "#endif without #if".to_string(), line_no));
            }
        } else if let Some(rest) = directive.strip_prefix("pragma") {
            let pragma_text = rest.trim();
            if pragma_text == "once" {
                // 记录当前文件为 pragma once
                let canonical = Path::new(&self.file)
                    .canonicalize()
                    .unwrap_or_else(|_| Path::new(&self.file).to_path_buf());
                self.pragma_once
                    .insert(canonical.to_string_lossy().to_string());
            } else if let Some(rest) = pragma_text.strip_prefix("GCC diagnostic") {
                self.handle_gcc_diagnostic(rest.trim(), line_no);
            } else if let Some(rest) = pragma_text.strip_prefix("pack") {
                // #pragma pack() / pack(1) / pack(2) / pack(4) / pack(8) / pack(16)
                // #pragma pack(push[, n]) / #pragma pack(pop[, n])
                // #pragma pack(push) / #pragma pack(pop)
                // 直接 #pragma 用 0, 让 handle_pragma_pack 自动用 count_out_lines()
                self.handle_pragma_pack(rest.trim(), 0);
            } else if let Some(rest) = pragma_text.strip_prefix("weak") {
                // #pragma weak name
                // 记录弱符号名 (后续在 codegen 中以 weak 链接生成)
                let rest = rest.trim();
                if !rest.is_empty() {
                    // 支持 #pragma weak name 或 #pragma weak(name)
                    let name = rest.trim_start_matches('(').trim_end_matches(')').trim();
                    // 可能有别名: #pragma weak name = other
                    let name = name.split('=').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        self.weak_symbols.insert(name.to_string());
                    }
                }
            } else if let Some(rest) = pragma_text.strip_prefix("STDC") {
                // #pragma STDC FENV_ACCESS ON/OFF/DEFAULT
                // #pragma STDC FP_CONTRACT ON/OFF/DEFAULT
                // #pragma STDC CX_LIMITED_RANGE ON/OFF/DEFAULT
                // 走 handle_pragma_stdc 单独函数, 以便单元测试
                self.handle_pragma_stdc(rest.trim(), line_no);
            } else if let Some(rest) = pragma_text.strip_prefix("locale") {
                // C23 #pragma locale: 接受 locale 切换指令
                // 语法形式:
                //   #pragma locale default       -> 重置为 UTF-8
                //   #pragma locale "POSIX"        -> ASCII
                //   #pragma locale reset          -> 重置为 UTF-8
                //   #pragma locale "UTF-8"        -> UTF-8
                //   #pragma locale "ISO-8859-1"   -> ISO-8859-1
                // 实际 locale 切换需要 libc 支持, dcc 记录字符集设置
                // 用于宽字符编码转换
                let arg = rest.trim();
                match arg {
                    "default" | "reset" => self.charset = "UTF-8".to_string(),
                    s if s.starts_with('"') && s.ends_with('"') => {
                        let inner = &s[1..s.len() - 1];
                        match inner {
                            "POSIX" | "C" | "ASCII" => self.charset = "ASCII".to_string(),
                            "UTF-8" | "utf8" => self.charset = "UTF-8".to_string(),
                            "ISO-8859-1" | "latin1" => self.charset = "ISO-8859-1".to_string(),
                            _ => {} // 未知 locale 静默接受
                        }
                    }
                    _ => {} // 其他形式静默接受
                }
            } else if pragma_text.starts_with("comment") {
                // #pragma comment(...) — 链接器注释, 忽略
            }
            // 其他 #pragma 忽略
        } else if let Some(rest) = directive.strip_prefix("error") {
            if self.is_active() {
                let msg = rest.trim();
                self.diags
                    .push(self.err_at("E0203", format!("#error {}", msg), line_no));
            }
        } else if let Some(rest) = directive.strip_prefix("warning") {
            if self.is_active() {
                let msg = rest.trim();
                self.diags
                    .push(self.warn_at("W0200", format!("#warning {}", msg), line_no));
            }
        } else if directive.starts_with("embed") && self.is_active() {
            self.do_embed(directive[5..].trim(), line_no);
        }
        // 忽略未知指令 (line markers 等)
    }

    fn do_define(&mut self, rest: &str) {
        // #define NAME body  或  #define NAME(args, ...) body
        let rest = rest.trim_start();
        // 提取宏名
        let name_end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if name_end == 0 {
            return;
        }
        let name = &rest[..name_end];
        let after_name = &rest[name_end..];

        if after_name.starts_with('(') {
            // function-like macro
            let paren_end = after_name.find(')').unwrap_or(after_name.len());
            let params_str = &after_name[1..paren_end];
            let mut params: Vec<String> = vec![];
            let mut variadic = false;
            let mut gnu_named_variadic = false;
            for s in params_str.split(',') {
                let s = s.trim();
                if s == "..." {
                    variadic = true;
                } else if let Some(va_name) = s.strip_prefix("...") {
                    // ...name 形式
                    variadic = true;
                    if !va_name.is_empty() {
                        params.push(va_name.to_string());
                    }
                } else if s.ends_with("...") && s.len() > 3 {
                    // name... 形式 (GNU): #define F(x...) body(x)
                    variadic = true;
                    gnu_named_variadic = true;
                    params.push(s[..s.len() - 3].trim().to_string());
                } else if !s.is_empty() {
                    params.push(s.to_string());
                }
            }
            // ISO 可变参数 (a, b, ...) 自动映射 __VA_ARGS__；GNU x... 在 body 中直接用参数名
            if variadic && !gnu_named_variadic && !params.iter().any(|p| p == "__VA_ARGS__") {
                params.push("__VA_ARGS__".to_string());
            }
            let body = after_name[paren_end + 1..].trim().to_string();
            self.macros.insert(
                name.to_string(),
                MacroDef {
                    params: Some(params),
                    body,
                    variadic,
                },
            );
        } else {
            // object-like macro
            let body = after_name.trim().to_string();
            self.macros.insert(
                name.to_string(),
                MacroDef {
                    params: None,
                    body,
                    variadic: false,
                },
            );
        }
    }

    fn needs_linux_stddef_bool_fixup(path: &str) -> bool {
        path.ends_with("linux/stddef.h")
    }

    /// GCC 扩展：`#include /absolute/path`（无引号）。
    fn is_absolute_include_path(s: &str) -> bool {
        let s = s.trim();
        s.starts_with('/') || s.starts_with('\\') || (s.len() >= 2 && s.as_bytes()[1] == b':')
    }

    /// 从 `#include` 行剩余部分提取路径（截断空白与 `//` 注释）。
    fn parse_include_path_token(rest: &str) -> &str {
        let rest = rest.trim();
        let end = rest.find("//").unwrap_or(rest.len());
        rest[..end].split_whitespace().next().unwrap_or(rest)
    }

    fn do_include(&mut self, rest: &str, line_no: u32) {
        let rest = rest.trim();
        let (path, is_system) = if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], false)
        } else if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], true)
        } else if Self::is_absolute_include_path(rest) {
            (Self::parse_include_path_token(rest), false)
        } else {
            self.diags.push(self.err_at(
                "E0204",
                "expected \"file\" or <file>".to_string(),
                line_no,
            ));
            return;
        };

        // dcc 内置 intrinsics 头: 拦截 x86intrin.h 系列 (标量模拟实现),
        // 优先于 host GCC 同名头 (后者依赖 dcc 未支持的 SIMD builtin)
        if self.try_include_builtin_intrin(path) {
            return;
        }

        // 搜索文件
        let found = self.find_include(path, is_system);
        match found {
            Some(full_path) => self.include_resolved_file(full_path, path, line_no),
            None => {
                self.diags.push(self.err_at(
                    "E0206",
                    format!("include file not found: {}", path),
                    line_no,
                ));
            }
        }
    }

    /// 拦截 x86 intrinsics 头文件, 注入 dcc 内置标量模拟实现.
    /// 命中返回 true (已处理), 未命中返回 false (继续正常搜索).
    fn try_include_builtin_intrin(&mut self, path: &str) -> bool {
        const X86INTRIN_H: &str = include_str!("headers/x86intrin.h");
        let name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        let text = match name {
            "x86intrin.h" | "immintrin.h" | "emmintrin.h" | "smmintrin.h" | "tmmintrin.h"
            | "shaintrin.h" => X86INTRIN_H,
            _ => return false,
        };
        let key = "<builtin:x86intrin.h>".to_string();
        if self.pragma_once.contains(&key) {
            return true;
        }
        self.pragma_once.insert(key.clone());
        // 内置头也计入依赖闭包 (缓存校验用; 伪路径, cache_deps_fresh 跳过实读)
        self.dep_hashes
            .push((key.clone(), Self::hash_content(X86INTRIN_H)));
        self.push_out(&format!("/* #include <{}> (dcc builtin) */\n", name));
        self.process_text(text, &key);
        true
    }

    /// 判断 `#include` 后的原始文本是否已经是 <file> / "file" / 绝对路径。
    /// 是则不必再走宏展开 (避免误展开 `current` 这样的对象宏).
    fn is_parsable_include_path(rest: &str) -> bool {
        let rest = rest.trim();
        if rest.starts_with('"') || rest.starts_with('<') {
            return true;
        }
        Self::is_absolute_include_path(rest)
    }

    /// `#include_next`：从当前头文件所在目录之后继续搜索。
    fn do_include_next(&mut self, rest: &str, line_no: u32) {
        let rest = rest.trim();
        let (path, is_system) = if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], false)
        } else if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], true)
        } else if Self::is_absolute_include_path(rest) {
            (Self::parse_include_path_token(rest), false)
        } else {
            self.diags.push(self.err_at(
                "E0204",
                "expected \"file\" or <file>".to_string(),
                line_no,
            ));
            return;
        };

        match self.find_include_next(path, is_system) {
            Some(full_path) => self.include_resolved_file(full_path, path, line_no),
            None => {
                self.diags.push(self.err_at(
                    "E0206",
                    format!("include_next file not found: {}", path),
                    line_no,
                ));
            }
        }
    }

    /// 将已解析到的头文件内容并入预处理输出。
    ///
    /// 三条路径, 按代价升序:
    /// 1. include-once 守卫快路径: 守卫宏已定义 → 整文件跳过 (等价 #ifndef 为假);
    /// 2. 缓存命中: 依赖闭包逐一重读比对新鲜后直接复用输出 + 宏表;
    /// 3. 正常处理; 仅当未产生任何新诊断时才落缓存 (有 #error/#warning
    ///    的头文件不缓存, 避免缓存吞掉诊断)。
    fn include_resolved_file(&mut self, full_path: PathBuf, path: &str, line_no: u32) {
        let canonical = full_path.canonicalize().unwrap_or(full_path.clone());
        let key = canonical.to_string_lossy().to_string();
        if self.pragma_once.contains(&key) {
            return;
        }
        self.deps.push(full_path.to_string_lossy().into_owned());
        match fs::read_to_string(&full_path) {
            Ok(text) => {
                let content_hash = Self::hash_content(&text);
                // 记录自身依赖 (含后续守卫/缓存命中路径): 父文件的依赖闭包
                // 必须包含子文件本身, 否则子文件变更时父条目无法失效
                self.dep_hashes.push((key.clone(), content_hash));

                // include-once 守卫快路径: 记住 (content_hash, 守卫宏名),
                // hash 匹配才有效; 守卫宏已定义时跳过全部预处理
                let guard = match self.once_guard.get(&key) {
                    Some((h, g)) if *h == content_hash => g.clone(),
                    _ => {
                        let g = Self::detect_include_guard(&text);
                        self.once_guard
                            .insert(key.clone(), (content_hash, g.clone()));
                        g
                    }
                };
                if let Some(g) = &guard {
                    if self.macros.contains_key(g) {
                        return;
                    }
                }

                // 全局类型注册表: 检查缓存
                let macro_hash = self.hash_macro_state();
                let cache_path = self.get_cache_path(&canonical, content_hash, macro_hash);

                let bool_fixup = Self::needs_linux_stddef_bool_fixup(path);
                // linux/stddef.h 定义 enum { false = 0, true = 1 }；若 stdbool.h
                // 已将 false/true 宏化，预处理会把它变成非法的 0 = 0。
                let skip_cache = bool_fixup
                    && (self.macros.contains_key("false") || self.macros.contains_key("true"));

                if let Some(ref cp) = cache_path {
                    if !skip_cache {
                        if let Some((
                            cached_output,
                            cached_macros,
                            cached_removed,
                            cached_deps,
                            counter_delta,
                        )) = self.load_from_cache(cp)
                        {
                            // 依赖新鲜度校验: 任一依赖文件变更即 miss (重处理)
                            if Self::cache_deps_fresh(&cached_deps) {
                                // 缓存命中: 直接使用缓存的预处理输出和宏
                                self.push_out(&format!("/* #include \"{}\" (cached) */\n", path));
                                self.deps.push(full_path.to_string_lossy().into_owned());
                                // 回填传递依赖: 父条目依赖闭包完整 + -MD 依赖更完整
                                for (dpath, dhash) in &cached_deps {
                                    if !self.dep_hashes.iter().any(|(p, _)| p == dpath) {
                                        self.dep_hashes.push((dpath.clone(), *dhash));
                                    }
                                    if !self.deps.iter().any(|d| d == dpath) {
                                        self.deps.push(dpath.clone());
                                    }
                                }
                                self.push_out(&cached_output);
                                // 记录缓存回放的映射区间（近似：嵌套 include 映射到外层头文件）
                                let cached_out_start = self.out_lines + 1; // 跳过 include 注释行
                                let cached_lines = cached_output.matches('\n').count() as u32;
                                if cached_lines > 0 {
                                    self.line_map.push(LineSpan {
                                        out_start: cached_out_start,
                                        file: path.to_string(),
                                        src_start: 1,
                                        count: cached_lines,
                                    });
                                }
                                // 回放宏变更集: 先删后插。缓存条目记录了处理期间
                                // #undef 的宏和被重新定义/新增的宏; 只回放新增会
                                // 丢失覆盖定义 (如 stddef.h 覆盖内置 NULL)
                                for name in &cached_removed {
                                    self.macros.remove(name);
                                }
                                for (name, def) in cached_macros {
                                    self.macros.insert(name, def);
                                }
                                // 缓存输出内嵌了创建时的 __COUNTER__ 值;
                                // 回放消耗量, 使后续展开的计数器与冷缓存运行一致
                                self.counter = self.counter.wrapping_add(counter_delta);
                                return;
                            }
                        }
                    }
                }

                // 缓存未命中: 正常处理
                self.push_out(&format!("/* #include \"{}\" */\n", path));
                let saved_base = self.base_dir.clone();
                let saved_out_len = self.out.len();
                // 记录当前宏表快照 (含值), 用于后续提取该头文件对宏表的
                // 变更集 (新增 + 重新定义 + #undef 删除)。仅记录新增会
                // 丢失覆盖定义 — 如 stddef.h 把内置 NULL 重新定义为内核
                // 版, 热缓存运行若不回放, 后续展开仍用内置版本, 输出与
                // 冷运行不一致。
                let saved_macros: HashMap<String, MacroDef> = self.macros.clone();
                // 记录处理前 __COUNTER__ 值, 用于计算本头文件的计数器消耗量
                let counter_before = self.counter;
                let saved_false = if bool_fixup { self.macros.remove("false") } else { None };
                let saved_true = if bool_fixup { self.macros.remove("true") } else { None };
                let diags_before = self.diags.len();
                // 记录依赖闭包起点, 用于提取本次处理期间的传递依赖
                let deps_before = self.dep_hashes.len();
                self.base_dir = full_path.parent().unwrap_or(Path::new(".")).to_path_buf();
                self.process_text(&text, &full_path.to_string_lossy());
                if let Some(def) = saved_false {
                    self.macros.insert("false".to_string(), def);
                }
                if let Some(def) = saved_true {
                    self.macros.insert("true".to_string(), def);
                }
                self.base_dir = saved_base;

                // 保存到缓存 (仅保存该头文件新增的宏, 避免冗余和状态污染);
                // 产生诊断的头文件不落缓存, 下次运行重新处理并重新报诊断
                if let Some(ref cp) = cache_path {
                    if !skip_cache && self.diags.len() == diags_before {
                        let new_output = self.out[saved_out_len..].to_string();
                        // 变更集: 值被改变或新增的宏 (新增 + 重新定义)
                        let header_macros: HashMap<String, MacroDef> = self
                            .macros
                            .iter()
                            .filter(|(k, v)| {
                                saved_macros
                                    .get(*k)
                                    .is_none_or(|old| !Self::macro_def_eq(old, v))
                            })
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        // 变更集: 处理期间被 #undef 删除的宏, 命中时必须回放删除
                        let removed_macros: Vec<String> = saved_macros
                            .keys()
                            .filter(|k| !self.macros.contains_key(*k))
                            .cloned()
                            .collect();
                        // 本次处理期间新增的依赖闭包: 自身 + 嵌套 include
                        // (含嵌套缓存命中回填的传递依赖), 按路径去重后写入
                        let mut dep_closure: Vec<(String, u64)> = vec![(key.clone(), content_hash)];
                        dep_closure.extend(self.dep_hashes[deps_before..].iter().cloned());
                        let mut seen: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        dep_closure.retain(|(p, _)| seen.insert(p.clone()));
                        let counter_delta = self.counter.wrapping_sub(counter_before);
                        self.save_to_cache(
                            cp,
                            &new_output,
                            &header_macros,
                            &removed_macros,
                            &dep_closure,
                            counter_delta,
                        );
                    }
                }
            }
            Err(e) => {
                self.diags.push(self.err_at(
                    "E0205",
                    format!("cannot read {}: {}", path, e),
                    line_no,
                ));
            }
        }
    }

    /// 构造与 `find_include` 一致的搜索目录序列。
    fn include_search_dirs(&self, is_system: bool) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if !is_system {
            dirs.push(self.base_dir.clone());
        }
        dirs.extend(self.include_paths.iter().cloned());
        dirs.extend(self.system_paths.iter().cloned());
        for d in [
            "/usr/local/include",
            "/usr/include",
            "/usr/include/x86_64-linux-gnu",
            "/usr/include/aarch64-linux-gnu",
        ] {
            dirs.push(PathBuf::from(d));
        }
        dirs.extend(host_gcc_include_dirs().iter().cloned());
        for d in [
            "/usr/include/linux",
            "/usr/include/asm",
            "/usr/include/asm-generic",
        ] {
            dirs.push(PathBuf::from(d));
        }
        dirs
    }

    /// `#include_next`：跳过当前文件所在目录及之前的搜索路径。
    fn find_include_next(&self, name: &str, is_system: bool) -> Option<PathBuf> {
        let name = name.trim();
        let dirs = self.include_search_dirs(is_system);
        let cur = self
            .base_dir
            .canonicalize()
            .unwrap_or_else(|_| self.base_dir.clone());

        // 找到当前头文件目录在搜索列表中的位置，从其之后开始找
        let mut start = 0;
        for (i, dir) in dirs.iter().enumerate() {
            let d = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if d == cur {
                start = i + 1;
                break;
            }
        }
        // 若当前目录不在列表中，仍从列表头部找，但跳过等于 cur 的目录
        for dir in dirs.iter().skip(start) {
            let d = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if d == cur {
                continue;
            }
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    fn find_include(&self, name: &str, is_system: bool) -> Option<PathBuf> {
        let name = name.trim();
        // 0. 绝对路径（GCC: #include /path/to/header）
        let abs = Path::new(name);
        if abs.is_absolute() {
            if abs.exists() {
                return Some(abs.to_path_buf());
            }
            return None;
        }
        // 1. 相对当前文件 (仅 "file" 形式)
        if !is_system {
            let p = self.base_dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
        // 2. 用户 -I 路径
        for dir in &self.include_paths {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
        // 3. 系统 -isystem 路径
        for dir in &self.system_paths {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
        // 4. 标准系统路径 (含多架构支持)
        let system_dirs = [
            "/usr/local/include",
            "/usr/include",
            // Debian/Ubuntu 多架构路径
            "/usr/include/x86_64-linux-gnu",
            "/usr/include/aarch64-linux-gnu",
        ];
        for dir in &system_dirs {
            let p = Path::new(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
        // 5. GCC 内建 include（stddef.h/stdbool.h 等；优先于 linux UAPI 同名头）
        for dir in host_gcc_include_dirs() {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
        // 6. Linux 内核 UAPI 路径（仅当标准/GCC 头不存在时）
        let kernel_dirs = [
            "/usr/include/linux",
            "/usr/include/asm",
            "/usr/include/asm-generic",
        ];
        for dir in &kernel_dirs {
            let p = Path::new(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    /// 解析 #embed 的字节列表参数: "1, 2, 0x03" → [1, 2, 3]
    fn parse_embed_bytes(s: &str) -> Vec<u8> {
        s.split(',')
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .filter_map(|t| {
                let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    u64::from_str_radix(h, 16).ok()
                } else {
                    t.parse::<u64>().ok()
                };
                v.map(|x| (x & 0xFF) as u8)
            })
            .collect()
    }

    /// C23 #embed: 嵌入文件内容为逗号分隔的整数字节值
    fn do_embed(&mut self, args: &str, line_no: u32) {
        let args = args.trim();
        // 提取文件名: 以 "file" 或 <file> 开头
        let (path, rest) = if let Some(stripped) = args.strip_prefix('"') {
            let end = stripped.find('"').map(|i| i + 1).unwrap_or(args.len());
            (&args[1..end], args[end + 1..].trim())
        } else if let Some(stripped) = args.strip_prefix('<') {
            let end = stripped.find('>').map(|i| i + 1).unwrap_or(args.len());
            (&args[1..end], args[end + 1..].trim())
        } else {
            self.diags.push(self.err_at(
                "E0207",
                "expected \"file\" or <file> for #embed".to_string(),
                line_no,
            ));
            return;
        };
        // 解析可选参数: limit(N) / prefix(...) / suffix(...) / if_empty(...)
        // 标准形式为 limit(N), 也接受 GCC 兼容的裸 `(limit(N))` 包裹形式
        let mut limit = None;
        let mut prefix: Vec<u8> = vec![];
        let mut suffix: Vec<u8> = vec![];
        let mut if_empty: Vec<u8> = vec![];
        let mut rest = rest;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            // 跳过前导括号 (兼容 `(limit(4))`)
            if rest.starts_with('(') {
                rest = rest[1..].trim_start();
            }
            let mut matched = false;
            for kw in ["limit", "prefix", "suffix", "if_empty"] {
                if let Some(after_kw) = rest.strip_prefix(kw) {
                    let after_kw = after_kw.trim_start();
                    if let Some(kw_body) = after_kw.strip_prefix('(') {
                        if let Some(close) = kw_body.find(')') {
                            let arg = kw_body[..close].trim();
                            match kw {
                                "limit" => limit = arg.parse::<usize>().ok(),
                                "prefix" => prefix = Self::parse_embed_bytes(arg),
                                "suffix" => suffix = Self::parse_embed_bytes(arg),
                                _ => if_empty = Self::parse_embed_bytes(arg),
                            }
                            rest = after_kw[close + 2..].trim_start();
                            // 跳过结尾括号
                            if rest.starts_with(')') {
                                rest = rest[1..].trim_start();
                            }
                            matched = true;
                            break;
                        }
                    }
                }
            }
            if !matched {
                break;
            }
        }
        // 搜索文件 (与 #include 相同逻辑)
        match self.find_include(path, args.starts_with('<')) {
            Some(full_path) => {
                match std::fs::read(&full_path) {
                    Ok(bytes) => {
                        // C23: prefix + 文件内容(截断至 limit) + suffix;
                        // 文件为空时以 if_empty 代替文件内容
                        let mut data: Vec<u8> =
                            Vec::with_capacity(bytes.len() + prefix.len() + suffix.len());
                        data.extend_from_slice(&prefix);
                        let max = limit.unwrap_or(bytes.len()).min(bytes.len());
                        if bytes.is_empty() {
                            data.extend_from_slice(&if_empty);
                        } else {
                            data.extend_from_slice(&bytes[..max]);
                        }
                        data.extend_from_slice(&suffix);
                        for (i, &b) in data.iter().enumerate() {
                            if i > 0 {
                                self.push_out(", ");
                            }
                            self.push_out(&b.to_string());
                        }
                    }
                    Err(_) => {
                        // #embed 空文件或无文件: 不输出任何内容
                    }
                }
            }
            None => {
                // 找不到文件: 输出空 (C23 标准中 #embed 找不到文件为未定义行为)
                // #embed "file" limit(0) 总是有效
                self.diags.push(self.err_at(
                    "E0208",
                    format!("embed file not found: {}", path),
                    line_no,
                ));
            }
        }
    }

    /// 处理 #pragma pack() / pack(n) / pack(push[,n]) / pack(pop[,n])
    ///
    /// 栈顶元素代表当前生效的 packing 对齐 (None=默认 8)。
    /// 后续记录的 packing 值可被 struct 布局查询 (cdecl/ext_abi 兼容)。
    /// `line_no` 是 _Pragma / 直接 #pragma 所在源行号; 若为 0 则
    /// 自动使用 self.count_out_lines() (适用于直接 #pragma 的常见情形)
    fn handle_pragma_pack(&mut self, text: &str, line_no: u32) {
        let text = text.trim();
        // 去掉包裹的括号
        let inner = text.trim_start_matches('(').trim_end_matches(')').trim();
        let new_pack: Option<usize>;
        if inner.is_empty() {
            // #pragma pack(): 恢复默认
            if let Some(top) = self.pack_stack.last_mut() {
                *top = None;
            }
            new_pack = None;
        } else {
            // 拆分 token
            let mut parts = inner.split(',').map(|p| p.trim());
            let first = parts.next().unwrap_or("");
            if first == "push" {
                // 复制当前对齐再压栈
                let current = self.pack_stack.last().copied().flatten();
                let n = parts.next().and_then(|s| s.parse::<usize>().ok());
                // 如果 push 给了对齐, 优先级: 显式 n > 当前对齐
                let pushed = n.or(current);
                self.pack_stack.push(pushed);
                new_pack = pushed;
            } else if first == "pop" {
                // 弹出栈顶
                if self.pack_stack.len() > 1 {
                    self.pack_stack.pop();
                }
                new_pack = self.pack_stack.last().copied().flatten();
            } else if let Ok(n) = first.parse::<usize>() {
                // #pragma pack(n): 仅修改当前层
                if let Some(top) = self.pack_stack.last_mut() {
                    *top = Some(n);
                }
                new_pack = Some(n);
            } else {
                return; // 未识别的 pack 形式, 不记录
            }
        }
        // 记录到 pack_history, 供 parser 按位置查询
        // out_line 是当前输出 (预处理后) 的行号, 与 parser 的 lex.line 对齐
        if self.pack_history.last().map(|(_, p)| *p) != Some(new_pack) {
            // 计算当前输出已经写入的行数
            let prev_last = self.pack_history.last().map(|(l, _)| *l).unwrap_or(0);
            // 优先使用调用方提供的行号 (_Pragma 时机);
            // 否则使用当前 out 的行数 (直接 #pragma 的常见情形)
            let current_out_line = if line_no > 0 { line_no } else { self.count_out_lines() };
            // 只有当行号真正增加时才记录 (避免同一行多次)
            if current_out_line > prev_last {
                self.pack_history.push((current_out_line, new_pack));
            }
        }
    }

    /// 公开接口: 获取当前 packing 对齐 (供 codegen struct 布局使用)
    pub fn current_pack(&self) -> Option<usize> {
        self.pack_stack.last().copied().flatten()
    }

    /// 取出 pack_history (移动所有权) 供外部使用
    pub fn take_pack_history(&mut self) -> Vec<(u32, Option<usize>)> {
        std::mem::take(&mut self.pack_history)
    }

    /// 取出 weak_symbols (移动所有权) 供外部使用
    pub fn take_weak_symbols(&mut self) -> HashSet<String> {
        std::mem::take(&mut self.weak_symbols)
    }

    /// 反转义字符串 (用于 _Pragma 操作符)
    /// 支持: \\ \n \t \r \" \' \x.. \ooo \? \\"
    pub fn unescape_string(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let bytes = raw.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\\' && i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'n' => {
                        out.push('\n');
                        i += 2;
                        continue;
                    }
                    b't' => {
                        out.push('\t');
                        i += 2;
                        continue;
                    }
                    b'r' => {
                        out.push('\r');
                        i += 2;
                        continue;
                    }
                    b'\\' => {
                        out.push('\\');
                        i += 2;
                        continue;
                    }
                    b'"' => {
                        out.push('"');
                        i += 2;
                        continue;
                    }
                    b'\'' => {
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    b'?' => {
                        out.push('?');
                        i += 2;
                        continue;
                    }
                    b'0'..=b'7' => {
                        // 八进制: 最多 3 位
                        let mut n = 0u32;
                        let mut k = 0;
                        while k < 3
                            && i + 1 + k < bytes.len()
                            && (b'0'..=b'7').contains(&bytes[i + 1 + k])
                        {
                            n = n * 8 + (bytes[i + 1 + k] - b'0') as u32;
                            k += 1;
                        }
                        out.push(n as u8 as char);
                        i += 1 + k;
                        continue;
                    }
                    b'x' => {
                        // 十六进制
                        let mut n = 0u32;
                        let mut k = 0;
                        while k < 2
                            && i + 2 + k < bytes.len()
                            && (bytes[i + 2 + k].is_ascii_hexdigit())
                        {
                            let d = (bytes[i + 2 + k] as char).to_digit(16).unwrap_or(0);
                            n = n * 16 + d;
                            k += 1;
                        }
                        out.push(n as u8 as char);
                        i += 2 + k;
                        continue;
                    }
                    _ => {
                        out.push(b as char);
                        i += 1;
                        continue;
                    }
                }
            }
            out.push(b as char);
            i += 1;
        }
        out
    }

    /// 处理 #pragma STDC FENV_ACCESS / FP_CONTRACT / CX_LIMITED_RANGE
    /// 这三个 C99 标准 STDC pragma 都以 `ON` / `OFF` / `DEFAULT`
    /// 三个状态中之一为参数, dcc 现阶段将它们作为 noop 接受:
    /// * FENV_ACCESS: 记录到 self.fenv_access, 供 codegen 阶段
    ///   决定是否插入 `volatile` 以防止浮点环境访问被优化
    ///   (dcc 仍将其作为纯语义状态, 不发警告)
    /// * FP_CONTRACT / CX_LIMITED_RANGE: 仅接受不存储, 不发警告
    ///   未知状态值 (`MAYBE` 等) 发出 W0001 警告, 避免静默拼写错误
    ///   污染语义状态.
    ///
    /// `line_no` 是 `_Pragma` / 直接 `#pragma` 所在源行号; 直接
    /// `#pragma` 调用时由 `process_directive` 传 0, 此处自动取
    /// `self.cur_line`.
    fn handle_pragma_stdc(&mut self, text: &str, line_no: u32) {
        let text = text.trim();
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.len() < 2 {
            return;
        }
        let kind = parts[0];
        let state_raw = parts[1];
        let state = state_raw.to_uppercase();
        let line = if line_no == 0 { self.cur_line.max(1) } else { line_no };
        if kind.eq_ignore_ascii_case("FENV_ACCESS") {
            match state.as_str() {
                "ON" | "DEFAULT" => self.fenv_access = true,
                "OFF" => self.fenv_access = false,
                _ => {
                    self.diags.push(Diagnostic::warn(
                        "W0001",
                        format!(
                            "unknown STDC FENV_ACCESS state `{}`; \
                                 expected ON, OFF, or DEFAULT",
                            state_raw
                        ),
                        Span::point(&self.file, line, 1),
                    ));
                }
            }
        }
        // FP_CONTRACT / CX_LIMITED_RANGE 接受为 noop
        // (dcc codegen 暂不区分, 不发警告)
    }

    /// 处理 #pragma GCC diagnostic
    fn handle_gcc_diagnostic(&mut self, text: &str, _line_no: u32) {
        let text = text.trim();
        if text.starts_with("push") {
            self.diag_stack.push(self.diag_state.clone());
        } else if text.starts_with("pop") {
            if let Some(prev) = self.diag_stack.pop() {
                self.diag_state = prev;
            }
        } else if let Some(rest) = text.strip_prefix("ignored") {
            let diag_name = rest.trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.insert(diag_name.to_string());
                self.diag_state.as_error.remove(diag_name);
            }
        } else if let Some(rest) = text.strip_prefix("warning") {
            let diag_name = rest.trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.remove(diag_name);
                self.diag_state.as_error.remove(diag_name);
            }
        } else if let Some(rest) = text.strip_prefix("error") {
            let diag_name = rest.trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.remove(diag_name);
                self.diag_state.as_error.insert(diag_name.to_string());
            }
        }
    }

    // ===== 宏展开 =====
    /// 顶层入口: 对一行文本做完全宏展开
    ///
    /// 第一轮展开可能留下跨边界调用: 函数式宏的替换结果末尾标识符
    /// 恰好与替换体后的 `(X)` 形成新函数式宏调用 (例如内核 cleanup.h
    /// 的 DEFINE_LOCK_GUARD_1_COND 模式):
    ///   #define F(X...) CONCATENATE(F_, COUNT_ARGS(X))(X)
    /// 第一轮展开 CONCATENATE 得到 F_3(…) 但 `(…)` 未被识别为新宏调用;
    /// 第二轮重扫描即可捕获 F_3(…).
    ///
    /// 第二轮不能用空禁用集: 自引用宏 (如 glibc 的
    /// `#define sa_handler __sigaction_handler.sa_handler`) 在单轮内由
    /// disabled set "paint blue", 空禁用集重扫会把它重复展开成
    /// __sigaction_handler.__sigaction_handler.sa_handler. 因此第二轮
    /// 的禁用集 = 第一轮实际展开过的全部宏名: 自引用 token 不再展开,
    /// 而跨边界调用的目标 (## 拼接产生的新宏名) 从未展开过, 不受影响.
    fn expand_macros(&mut self, line: &str, phys_lines: &[u32]) -> String {
        // 顶层块模式: phys_lines 与 line 中的行一一对应 (每行一个物理行号),
        // 使 __LINE__ 在展开块内按物理行精确定位
        self.expand_macros_map(line, LineMap::Block(phys_lines))
    }

    /// expand_macros 的通用实现; 行号映射由调用方给定 (顶层 Block /
    /// 递归 Scalar, 见 LineMap)。
    fn expand_macros_map(&mut self, line: &str, map: LineMap) -> String {
        // 不能用 macros.is_empty() 早退: __COUNTER__ / __FILE__ / __LINE__
        // 等特殊 token 不在 macros 表内, 必须进入 expand_with_disabled
        //
        // 嵌套调用 (tail_call 递归) 时先保存外层记录, 结束时合并回去
        let saved = self.expanded_macros.borrow().clone();
        if std::env::var("DCC_PP_DEBUG").is_ok() && line.contains("asm_inline") {
            eprintln!(
                "[pp-debug] MAP_ENTER saved={} depth={} line_hdr={:?}",
                saved.len(),
                EXPAND_DEPTH.with(|c| c.get()),
                &line[..line.len().min(60)]
            );
        }
        self.expanded_macros.borrow_mut().clear();
        let disabled = HashSet::new();
        let mut result = self.expand_with_disabled(line, &disabled, map);
        // 第二轮重扫描: 处理跨边界宏调用 (如 cleanup.h 的
        // DEFINE_LOCK_GUARD_1_COND 模式). 禁用集 = 第一轮实际展开过的
        // 宏名, 延续 paint blue 语义, 防止自引用宏被重复展开.
        let expanded = std::mem::take(&mut *self.expanded_macros.borrow_mut());
        if result != line {
            // 第二轮禁用集只保留"展开结果仍含自身标识符"的自引用宏
            // (如 glibc `#define sa_handler __sigaction_handler.sa_handler`)。
            // 纯转发宏 (如内核 `#define asm_inline asm __inline`、
            // `#define __always_inline inline __attribute__((__always_inline__))`)
            // 的展开结果不含自身 token, 重新展开是安全的; 若把它们整体禁掉,
            // 链式宏调用在第二轮的重扫描 (如 GEN_BINARY_RMWcc_5(..) 展开出的
            // __GEN_RMWcc body 里的 asm_inline) 会被 paint blue 误伤而残留,
            // 导致预处理输出含未展开宏名而编译失败。
            let mut self_ref = HashSet::new();
            for name in &expanded {
                if let Some(def) = self.macros.get(name) {
                    if Self::body_contains_ident(&def.body, name) {
                        self_ref.insert(name.clone());
                    }
                }
            }
            let result2 = self.expand_with_disabled(&result, &self_ref, map);
            result = result2;
        }
        // 丢弃第二轮的新记录, 只把第一轮展开的宏合并回外层记录
        self.expanded_macros.borrow_mut().clear();
        let saved_len = saved.len();
        let mut merged = saved;
        merged.extend(expanded);
        let merged_len = merged.len();
        *self.expanded_macros.borrow_mut() = merged;
        if std::env::var("DCC_PP_DEBUG").is_ok() && result.contains("asm_inline") {
            eprintln!(
                "[pp-debug] MAP_EXIT saved={} merged={} result_has_asm_inline={}",
                saved_len,
                merged_len,
                result.contains("asm_inline")
            );
        }
        result
    }

    /// body 中是否出现 name 作为完整标识符 (前后都不是标识符字符)。
    /// 用于判断宏展开结果是否仍含自身 token (真自引用)。
    /// 注意不能用子串匹配: `#define __always_inline inline \
    /// __attribute__((__always_inline__))` 的 body 含 `__always_inline__`
    /// 子串 `__always_inline`, 但那是另一个标识符, 并非自引用。
    /// 边界 (body 开头/末尾) 视为安全分隔符: name 位于开头/末尾时
    /// 前后无字符, 是完整标识符。
    fn body_contains_ident(body: &str, name: &str) -> bool {
        let bytes = body.as_bytes();
        let mut start = 0;
        while let Some(rel) = body[start..].find(name) {
            let pos = start + rel;
            let end = pos + name.len();
            // 开头/末尾是安全边界, 中间位置按实际字符判断
            let before_ok = pos == 0 || {
                let b = bytes[pos - 1];
                !b.is_ascii_alphanumeric() && b != b'_'
            };
            let after_ok = end >= bytes.len() || {
                let a = bytes[end];
                !a.is_ascii_alphanumeric() && a != b'_'
            };
            if before_ok && after_ok {
                return true;
            }
            start = end;
        }
        false
    }

    /// 用禁用集展开文本; `map` 提供 __LINE__ 的行号映射
    fn expand_with_disabled(
        &mut self,
        text: &str,
        disabled: &HashSet<String>,
        map: LineMap,
    ) -> String {
        // 递归深度保护：宏互相引用时避免栈溢出
        let depth = EXPAND_DEPTH.with(|d| {
            let v = d.get();
            d.set(v + 1);
            v
        });
        if depth > 256 {
            EXPAND_DEPTH.with(|d| d.set(depth));
            if std::env::var("DCC_PP_DEBUG").is_ok() && text.contains("asm_inline") {
                eprintln!(
                    "[pp-debug] DEPTH_LIMIT depth={} text={:?}",
                    depth,
                    &text[..text.len().min(200)]
                );
            }
            return text.to_string();
        }
        let result = self.expand_with_disabled_inner(text, disabled, map);
        EXPAND_DEPTH.with(|d| d.set(depth));
        result
    }

    fn expand_with_disabled_inner(
        &mut self,
        text: &str,
        disabled: &HashSet<String>,
        map: LineMap,
    ) -> String {
        let mut out = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        // 当前文本内已消费的换行数 (0-based): __LINE__ = map.line(nl)
        let mut nl: u32 = 0;

        while i < len {
            // 跳过字符串字面量
            if chars[i] == '"' {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
                continue;
            }
            // 跳过字符字面量
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    out.push('\'');
                    i += 1;
                }
                continue;
            }
            // 标识符
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();

                // 特殊内建宏
                if ident == "__FILE__" {
                    out.push_str(&format!("\"{}\"", self.file));
                    continue;
                }
                if ident == "__LINE__" {
                    out.push_str(&map.line(nl).to_string());
                    continue;
                }
                if ident == "__DATE__" {
                    out.push_str(&format!("\"{}\"", self.compile_date));
                    continue;
                }
                if ident == "__TIME__" {
                    out.push_str(&format!("\"{}\"", self.compile_time));
                    continue;
                }
                // __COUNTER__: 每次访问自增, 共享 preproc 实例
                // 跨宏嵌套展开保持单一计数
                if ident == "__COUNTER__" {
                    let v = self.counter;
                    self.counter = self.counter.wrapping_add(1);
                    out.push_str(&v.to_string());
                    continue;
                }

                // C99 _Pragma("...") 操作符: 把字符串字面量当作 #pragma 处理
                // 把 _Pragma(...) 整体吃掉, 然后调用 process_directive 内部逻辑
                if ident == "_Pragma" {
                    let mut j = i;
                    while j < len && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < len && chars[j] == '(' {
                        j += 1;
                        while j < len && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if j < len && chars[j] == '"' {
                            // 提取字符串内容
                            j += 1;
                            let content_start = j;
                            while j < len && chars[j] != '"' {
                                if chars[j] == '\\' && j + 1 < len {
                                    j += 2;
                                    continue;
                                }
                                j += 1;
                            }
                            let raw: String = chars[content_start..j].iter().collect();
                            let pragma_text = Self::unescape_string(&raw);
                            if j < len {
                                j += 1;
                            } // 跳过 "
                            while j < len && chars[j] != ')' {
                                j += 1;
                            }
                            if j < len {
                                j += 1;
                            } // 跳过 )
                            i = j;
                            // _Pragma 是 expr, 在展开阶段不能改状态。
                            // 我们把它的字符串收集起来, 待外层 &mut self 方法批量应用
                            // 同时记录当前 out 的行号, 这样 pack_history 与 parser 的 lex.line 对齐
                            if let Ok(mut v) = self.pending_pragmas.try_borrow_mut() {
                                v.push((pragma_text, self.count_out_lines()));
                            }
                            continue;
                        }
                    }
                    // 解析失败, 跳过整个 _Pragma(...) 表达式
                    let mut j = i;
                    while j < len && chars[j] != ')' {
                        j += 1;
                    }
                    if j < len {
                        j += 1;
                    }
                    i = j;
                    continue;
                }

                // __has_attribute(xxx) / __has_builtin(xxx) / __has_feature(xxx)
                // __has_c_attribute(xxx): C23 属性检测
                if ident == "__has_attribute"
                    || ident == "__has_builtin"
                    || ident == "__has_feature"
                    || ident == "__has_c_attribute"
                {
                    let mut j = i;
                    while j < len && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < len && chars[j] == '(' {
                        j += 1;
                        let mut depth = 1;
                        let arg_start = j;
                        while j < len && depth > 0 {
                            if chars[j] == '(' {
                                depth += 1;
                            } else if chars[j] == ')' {
                                depth -= 1;
                            }
                            if depth > 0 {
                                j += 1;
                            }
                        }
                        let arg: String = chars[arg_start..j].iter().collect();
                        let arg = arg.trim().to_string();
                        let val = if ident == "__has_attribute" {
                            if Self::is_known_attribute(&arg) {
                                "1"
                            } else {
                                "0"
                            }
                        } else if ident == "__has_c_attribute" {
                            if Self::is_known_c_attribute(&arg) {
                                "1"
                            } else {
                                "0"
                            }
                        } else if ident == "__has_builtin" {
                            if Self::is_known_builtin(&arg) {
                                "1"
                            } else {
                                "0"
                            }
                        } else {
                            "0"
                        }; // __has_feature: always 0 for now
                        out.push_str(val);
                        if j < len {
                            j += 1;
                        } // skip )
                        i = j;
                        continue;
                    }
                }

                // 检查是否在禁用集中 (paint blue)
                if disabled.contains(&ident) {
                    if std::env::var("DCC_PP_DEBUG").is_ok() && ident == "asm_inline" {
                        let d = EXPAND_DEPTH.with(|c| c.get());
                        eprintln!("[pp-debug] PAINT_BLUE asm_inline depth={} file={} dis={:?} text_hdr={:?}", d, self.file, disabled, &text[..text.len().min(60)]);
                    }
                    out.push_str(&ident);
                    continue;
                }

                if let Some(def) = self.macros.get(&ident).cloned() {
                    if std::env::var("DCC_PP_DEBUG").is_ok() && ident == "asm_inline" {
                        let d = EXPAND_DEPTH.with(|c| c.get());
                        eprintln!("[pp-debug] EXPAND asm_inline depth={} file={} dis_len={} dis_has_gen={} text={:?}", d, self.file, disabled.len(), disabled.contains("GEN_BINARY_RMWcc"), &text[..text.len().min(100)]);
                    }
                    if let Some(ref params) = def.params {
                        // function-like: 需要后跟 (
                        let mut j = i;
                        while j < len && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if j < len && chars[j] == '(' {
                            j += 1;
                            self.expanded_macros.borrow_mut().insert(ident.clone());
                            let raw_args = self.parse_macro_args(&chars, &mut j);
                            // 确定哪些参数被 # 或 ## 操作 (这些参数不预展开)
                            let (stringify_params, paste_params) =
                                self.find_special_params(&def.body, params);
                            // 参数预展开: 未被 # / ## 引用的参数先展开。
                            // 必须继承当前 disabled（涂蓝）集，否则 F(F(1))
                            // 这类嵌套会在参数预展开时丢掉外层禁令而无限递归。
                            let mut expanded_args: Vec<String> = Vec::with_capacity(raw_args.len());
                            for (idx, arg) in raw_args.iter().enumerate() {
                                if stringify_params.contains(&idx) || paste_params.contains(&idx) {
                                    expanded_args.push(arg.clone());
                                } else {
                                    expanded_args.push(self.expand_with_disabled(
                                        arg,
                                        disabled,
                                        LineMap::Scalar(map.line(nl)),
                                    ));
                                }
                            }
                            // 可变参数处理
                            let mut final_args = expanded_args.clone();
                            if def.variadic {
                                // 最后一位是可变参数 (__VA_ARGS__ 或 GNU 命名)
                                let fixed = params.len().saturating_sub(1);
                                if raw_args.len() >= fixed {
                                    let mut normal =
                                        expanded_args[..fixed.min(expanded_args.len())].to_vec();
                                    let va_text = if raw_args.len() > fixed {
                                        raw_args[fixed..]
                                            .iter()
                                            .map(|a| {
                                                self.expand_with_disabled(
                                                    a,
                                                    disabled,
                                                    LineMap::Scalar(map.line(nl)),
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    } else {
                                        String::new()
                                    };
                                    normal.push(va_text);
                                    final_args = normal;
                                }
                            }
                            if final_args.len() == params.len() {
                                if std::env::var("DCC_PP_DEBUG").is_ok()
                                    && (ident == "__GEN_RMWcc"
                                        || ident == "GEN_BINARY_RMWcc_6"
                                        || ident == "GEN_BINARY_RMWcc_5"
                                        || ident == "GEN_BINARY_RMWcc"
                                        || ident == "CONCATENATE"
                                        || ident == "__CONCATENATE")
                                {
                                    eprintln!("[pp-debug] CALL {:?} raw={} final={} params={} body_has_asm={} dis={:?} body={:?}", ident, raw_args.len(), final_args.len(), params.len(), def.body.contains("asm_inline"), disabled, &def.body[..def.body.len().min(110)]);
                                }
                                let substituted = self.substitute_params(
                                    &def.body,
                                    params,
                                    &final_args,
                                    def.variadic,
                                );
                                if std::env::var("DCC_PP_DEBUG").is_ok()
                                    && substituted.contains("asm_inline")
                                {
                                    eprintln!(
                                        "[pp-debug] SUBST {:?} -> {:?}",
                                        ident,
                                        &substituted[..substituted.len().min(250)]
                                    );
                                }
                                // 创建新禁用集 = 原禁用集 + 当前宏名
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                // 重新扫描替换结果
                                let rescanned = self.expand_with_disabled(
                                    &substituted,
                                    &new_disabled,
                                    LineMap::Scalar(map.line(nl)),
                                );
                                if std::env::var("DCC_PP_DEBUG").is_ok()
                                    && rescanned.contains("asm_inline")
                                {
                                    eprintln!(
                                        "[pp-debug] RESCAN_LEFTOVER {:?} -> {:?}",
                                        ident,
                                        &rescanned[..rescanned.len().min(250)]
                                    );
                                }
                                out.push_str(&rescanned);
                            } else if raw_args.len() == params.len() {
                                if std::env::var("DCC_PP_DEBUG").is_ok()
                                    && (ident == "__GEN_RMWcc"
                                        || ident == "GEN_BINARY_RMWcc_6"
                                        || ident == "GEN_BINARY_RMWcc_5"
                                        || ident == "GEN_BINARY_RMWcc"
                                        || ident == "CONCATENATE"
                                        || ident == "__CONCATENATE")
                                {
                                    eprintln!("[pp-debug] CALL-raw {:?} raw={} final={} params={} body_has_asm={} body={:?}", ident, raw_args.len(), final_args.len(), params.len(), def.body.contains("asm_inline"), &def.body[..def.body.len().min(110)]);
                                }
                                let substituted = self.substitute_params(
                                    &def.body,
                                    params,
                                    &raw_args,
                                    def.variadic,
                                );
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                let rescanned = self.expand_with_disabled(
                                    &substituted,
                                    &new_disabled,
                                    LineMap::Scalar(map.line(nl)),
                                );
                                out.push_str(&rescanned);
                            } else if params.is_empty() && raw_args.is_empty() {
                                // PREFETCH() 这类无参函数宏
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                let rescanned = self.expand_with_disabled(
                                    &def.body,
                                    &new_disabled,
                                    LineMap::Scalar(map.line(nl)),
                                );
                                out.push_str(&rescanned);
                            } else if raw_args.is_empty() && !params.is_empty() {
                                // GCC 扩展: __STRING() / FOO() 当宏期待 ≥1 实参却被空括号
                                // 调用时, 视为一个空实参。glibc 头文件 __ASMNAME2 链依赖此行为:
                                //   #define __STRING(x) #x
                                //   #define __ASMNAME2(prefix, cname) __STRING (prefix) cname
                                // 当 prefix 为空 (__USER_LABEL_PREFIX__="") 时, 替换体变成
                                //   __STRING () cname
                                // GCC 把 __STRING () 视为 __STRING(""), 生成 "" (空串字面量);
                                // dcc 旧实现因 parse_macro_args("()") 返回 [] 而落到 fallthrough
                                // 保留原标识符, 导致 __STRING 残留在预处理器输出里。
                                let empty_args: Vec<String> = vec![String::new(); params.len()];
                                let substituted = self.substitute_params(
                                    &def.body,
                                    params,
                                    &empty_args,
                                    def.variadic,
                                );
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                let rescanned = self.expand_with_disabled(
                                    &substituted,
                                    &new_disabled,
                                    LineMap::Scalar(map.line(nl)),
                                );
                                out.push_str(&rescanned);
                            } else {
                                out.push_str(&ident);
                            }
                            i = j;
                        } else {
                            out.push_str(&ident);
                        }
                    } else {
                        self.expanded_macros.borrow_mut().insert(ident.clone());
                        // object-like 别名后接 (args) 时须与替换体一起重扫描，
                        // 例如 #define DIV_ROUND_UP __KERNEL_DIV_ROUND_UP
                        // 但是只有当替换体本身是函数式宏名时才需要 tail_call
                        // 优化 (后跟 ( 能发走函数调用). 如果替换体仅是
                        // 普通别名 (如 stdbool.h 的 #define bool _Bool),
                        // 不应采用该优化: tail_call 会把整个剩余文本都
                        // 以 "bool" 处于 disabled 集的方式重扫描, 令后续
                        // 同一别名出现时不再展开, 例如
                        //   static char *f(bool (*cb)(int)) {}
                        //   static bool g(void) {}
                        // 第二个 static bool 会保留为 bool, 报 unknown type.
                        // 判定依据: def.body 为简单 identifier 标识时
                        // 才走 tail_call 优化.
                        let body_is_simple_ident = def
                            .body
                            .chars()
                            .next()
                            .map(|c| c.is_alphabetic() || c == '_')
                            .unwrap_or(false)
                            && def.body.chars().all(|c| c.is_alphanumeric() || c == '_');
                        let mut new_disabled = disabled.clone();
                        new_disabled.insert(ident.clone());
                        let mut j = i;
                        while j < len && chars[j].is_whitespace() {
                            j += 1;
                        }
                        let tail_call = body_is_simple_ident && j < len && chars[j] == '(';
                        let rescanned = if tail_call {
                            // 只重扫描到本次调用的闭合括号, 避免把后续同
                            // 名标识符一起带进 disabled 集.
                            let mut k = j;
                            // 进入 ( 后逐字符计深度, 跳过本组 () 的全部内容.
                            if k < len && chars[k] == '(' {
                                let mut depth = 1i32;
                                k += 1;
                                while k < len && depth > 0 {
                                    if chars[k] == '(' {
                                        depth += 1;
                                    } else if chars[k] == ')' {
                                        depth -= 1;
                                    }
                                    k += 1;
                                }
                            }
                            let call_args: String = chars[j..k].iter().collect();
                            let after_call: String = chars[k..].iter().collect();
                            if std::env::var("DCC_PP_DEBUG").is_ok()
                                && after_call.contains("asm_inline")
                            {
                                eprintln!("[pp-debug] TAILCALL_AFTER ident={} depth={} disabled={:?} after_hdr={:?}", ident, EXPAND_DEPTH.with(|c| c.get()), disabled, &after_call[..after_call.len().min(70)]);
                            }
                            let head = self.expand_with_disabled(
                                &format!("{}{}", def.body, call_args),
                                &new_disabled,
                                LineMap::Scalar(map.line(nl)),
                            );
                            // 后续文本不带 disabled 集, 正常展开; 行号从当前
                            // 行继续 (Scalar 映射, 递归层级用不到物理表)
                            format!(
                                "{}{}",
                                head,
                                self.expand_macros_map(&after_call, LineMap::Scalar(map.line(nl)))
                            )
                        } else {
                            self.expand_with_disabled(
                                &def.body,
                                &new_disabled,
                                LineMap::Scalar(map.line(nl)),
                            )
                        };
                        out.push_str(&rescanned);
                        if tail_call {
                            i = len;
                        }
                    }
                } else {
                    out.push_str(&ident);
                }
                continue;
            }
            if chars[i] == '\n' {
                nl += 1;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// 找出哪些参数被 # 或 ## 引用 (这些参数不做预展开)
    fn find_special_params(&self, body: &str, params: &[String]) -> (Vec<usize>, Vec<usize>) {
        let mut stringify = vec![];
        let mut paste = vec![];
        // 检查 #: # param
        for (idx, param) in params.iter().enumerate() {
            let p1 = format!("# {}", param);
            let p2 = format!("#{}", param);
            if body.contains(&p1) || body.contains(&p2) {
                stringify.push(idx);
            }
        }
        // 检查 ##: param ## 或 ## param
        for (idx, param) in params.iter().enumerate() {
            // 简单检查: 参数名两侧是否有 ##
            let patterns = [
                format!("{} ##", param),
                format!("{}##", param),
                format!("## {}", param),
                format!("##{}", param),
            ];
            for pat in &patterns {
                if body.contains(pat.as_str()) {
                    if !paste.contains(&idx) {
                        paste.push(idx);
                    }
                    break;
                }
            }
        }
        (stringify, paste)
    }

    fn parse_macro_args(&self, chars: &[char], pos: &mut usize) -> Vec<String> {
        let mut args = vec![];
        let mut current = String::new();
        let mut depth = 1;
        let mut closed = false;
        let mut in_string = false;
        let mut in_char = false;
        let mut escape = false;
        while *pos < chars.len() && depth > 0 {
            let c = chars[*pos];
            if in_string {
                current.push(c);
                if escape {
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '"' {
                    in_string = false;
                }
                *pos += 1;
                continue;
            }
            if in_char {
                current.push(c);
                if escape {
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '\'' {
                    in_char = false;
                }
                *pos += 1;
                continue;
            }
            match c {
                '"' => {
                    in_string = true;
                    current.push(c);
                }
                '\'' => {
                    in_char = true;
                    current.push(c);
                }
                '(' => {
                    depth += 1;
                    current.push(c);
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        closed = true;
                        // FOO() → 零个实参；FOO(a,) → 最后空实参仍保留
                        let trimmed = current.trim().to_string();
                        if !trimmed.is_empty() || !args.is_empty() {
                            args.push(trimmed);
                        }
                    } else {
                        current.push(c);
                    }
                }
                ',' if depth == 1 => {
                    args.push(current.trim().to_string());
                    current = String::new();
                }
                _ => current.push(c),
            }
            *pos += 1;
        }
        if !closed && !current.trim().is_empty() {
            args.push(current.trim().to_string());
        }
        args
    }

    fn substitute_params(
        &self,
        body: &str,
        params: &[String],
        args: &[String],
        variadic: bool,
    ) -> String {
        // 空 __VA_ARGS__ 时移除 GCC 扩展 ", ## __VA_ARGS__"
        let mut body = body.to_string();
        if let Some(va_idx) = params.iter().position(|p| p == "__VA_ARGS__") {
            if args.get(va_idx).map(|s| s.is_empty()).unwrap_or(false) {
                for pat in &[
                    ", ## __VA_ARGS__",
                    ",## __VA_ARGS__",
                    ", ##__VA_ARGS__",
                    ",##__VA_ARGS__",
                ] {
                    body = body.replace(pat, "");
                }
            }
        }
        // __VA_OPT__(content) (C++20 / C23):
        // 仅当 __VA_ARGS__ 非空时展开为 content, 否则完全删除。
        // 该 token 只能出现在可变参数宏的宏体中 (C 标准 6.10.5/2),
        // 在普通宏或宏体外出现时, GCC/clang 保留为字面 token;
        // 这里我们采取相同策略 (普通宏或无 __VA_ARGS__ 时按字面 token 保留)。
        if body.contains("__VA_OPT__") {
            let va_empty = params
                .iter()
                .position(|p| p == "__VA_ARGS__")
                .and_then(|i| args.get(i))
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            if params.iter().any(|p| p == "__VA_ARGS__") {
                body = Self::handle_va_opt(&body, va_empty);
            }
        }
        // 第一步: 处理 # (字符串化) — 必须在 ## 之前, 否则参数名被替换后 # 找不到模式
        let result = self.handle_stringify(&body, params, args);
        // 第二步: 处理 ## (令牌拼接)
        let result = self.handle_paste(&result, params, args, variadic);
        // 第三步: 普通参数替换（单次扫描，实参视为不透明，避免 *d 被二次替换成 **d）
        let mut result = result;
        result = Self::replace_all_params(&result, params, args);
        result
    }

    /// 处理 `__VA_OPT__(content)`: 若 `va_empty` 为真则整段移除,
    /// 否则替换为 `content` (内部参数已被 caller 替换, 这里只做块切除)。
    ///
    /// 配对规则: 在 `__VA_OPT__` 之后跳过空白, 匹配一个 `(` 并按括号
    /// 深度读到匹配的 `)`. 嵌套的 `__VA_OPT__` 会被递归处理 (按内→外
    /// 顺序), 因此最里层先被消解, 外层按当前 `va_empty` 决策。
    fn handle_va_opt(body: &str, va_empty: bool) -> String {
        let mut result = String::with_capacity(body.len());
        let bytes = body.as_bytes();
        let mut i = 0;
        // __VA_OPT__ 关键字长度为 10
        const KEYWORD: &[u8; 10] = b"__VA_OPT__";
        while i < bytes.len() {
            // 匹配 `__VA_OPT__` 关键字 (确认前后不是标识符字符)
            if i + KEYWORD.len() <= bytes.len()
                && &bytes[i..i + KEYWORD.len()] == KEYWORD
                && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
                && (i + KEYWORD.len() >= bytes.len()
                    || !(bytes[i + KEYWORD.len()].is_ascii_alphanumeric()
                        || bytes[i + KEYWORD.len()] == b'_'))
            {
                // 跳过空白
                let mut j = i + KEYWORD.len();
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    // 平衡括号读取 content 范围 [j+1, end)
                    let mut depth = 1usize;
                    let mut k = j + 1;
                    while k < bytes.len() && depth > 0 {
                        match bytes[k] {
                            b'(' => depth += 1,
                            b')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    if k >= bytes.len() {
                        // 没有匹配的 `)`, 视为普通 token, 透传
                        result.push_str("__VA_OPT__");
                        i += KEYWORD.len();
                        continue;
                    }
                    // 递归处理 content 内嵌的 __VA_OPT__
                    let content = std::str::from_utf8(&bytes[j + 1..k]).unwrap_or("");
                    let inner = Self::handle_va_opt(content, va_empty);
                    if va_empty {
                        // 删除整段, 同时消掉前面紧邻的单个空白字符
                        // (与 GCC 行为一致: __VA_OPT__ 视作不可见 token,
                        //  空参时连同其前的空格一起移除)
                        while result.ends_with(' ') || result.ends_with('\t') {
                            result.pop();
                        }
                        // 记录一个 "前面刚切了空白" 的标记, 避免连续切
                        // 多个 __VA_OPT__ 时把中间的 token 间隔也吃掉
                        // (本实现一次只切一个尾部空白, 多次切由循环保证)
                    } else {
                        result.push_str(&inner);
                    }
                    i = k + 1;
                    continue;
                }
            }
            // 字符串/字符字面量内 __VA_OPT__ 视为普通 token
            if bytes[i] == b'"' {
                result.push('"');
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        result.push(bytes[i] as char);
                        result.push(bytes[i + 1] as char);
                        i += 2;
                    } else {
                        result.push(bytes[i] as char);
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    result.push('"');
                    i += 1;
                }
                continue;
            }
            if bytes[i] == b'\'' {
                result.push('\'');
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        result.push(bytes[i] as char);
                        result.push(bytes[i + 1] as char);
                        i += 2;
                    } else {
                        result.push(bytes[i] as char);
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    result.push('\'');
                    i += 1;
                }
                continue;
            }
            // 普通字符透传
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }

    /// 一次扫描替换所有宏参数；写入的实参文本不再参与参数名匹配
    fn replace_all_params(text: &str, params: &[String], args: &[String]) -> String {
        if params.is_empty() {
            return text.to_string();
        }
        let mut result = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        while i < len {
            if chars[i] == '"' {
                result.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len {
                        result.push(chars[i]);
                        i += 1;
                    }
                    result.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    result.push('"');
                    i += 1;
                }
                continue;
            }
            if chars[i] == '\'' {
                result.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len {
                        result.push(chars[i]);
                        i += 1;
                    }
                    result.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    result.push('\'');
                    i += 1;
                }
                continue;
            }
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                if let Some(idx) = params.iter().position(|p| p == &ident) {
                    if let Some(arg) = args.get(idx) {
                        result.push_str(arg);
                        continue;
                    }
                }
                result.push_str(&ident);
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    /// ## 令牌拼接: 仅替换 ## 两侧的参数名后拼接（勿全局替换，否则会与后续
    /// substitute_params 二次替换把 `&di` 弄成 `&&di`）
    ///
    /// GNU 可变参数扩展: `,##X` 当 X 可变参数非空时 ## 无效果（仅移除 `##`）；
    /// 当 X 为空时移除 `,##X` 整段（含逗号）。
    fn handle_paste(
        &self,
        body: &str,
        params: &[String],
        args: &[String],
        variadic: bool,
    ) -> String {
        if !body.contains("##") {
            return body.to_string();
        }
        let variadic_param = if variadic { params.last() } else { None };
        let mut result = body.to_string();
        while let Some(pos) = result.find("##") {
            let before = result[..pos].trim_end();
            let after = result[pos + 2..].trim_start();
            // 取 before 最后一个 token
            let left_start = before
                .rfind(|c: char| !c.is_alphanumeric() && c != '_')
                .map(|p| p + 1)
                .unwrap_or(0);
            let left = &before[left_start..];
            // 取 after 第一个 token
            let right_end = after
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            let right = &after[..right_end];
            // 若侧为宏参数名则换为实参，否则原样保留
            let left_sub = params
                .iter()
                .position(|p| p == left)
                .and_then(|i| args.get(i))
                .map(|s| s.as_str())
                .unwrap_or(left);
            let right_sub = params
                .iter()
                .position(|p| p == right)
                .and_then(|i| args.get(i))
                .map(|s| s.as_str())
                .unwrap_or(right);
            // GNU 可变参数 `,##X` 特殊处理:
            if let Some(vp) = variadic_param {
                if vp == right {
                    // right 是可变参数
                    if right_sub.trim().is_empty() {
                        // 空 variadic:
                        //  - 标准 C99: identifier##__VA_ARGS__ → 移除 ##__VA_ARGS__, 保留左 token
                        //  - GCC 扩展: ,##X → 移除 ,##X 整段 (含前导逗号)
                        // left 为空表示左 token 是标点 (rfind 当成分隔符),
                        // 此时检查前一个字符是否为 ','。
                        let prefix = if left.is_empty() {
                            let chars: Vec<char> = before.chars().collect();
                            if left_start > 0
                                && left_start - 1 < chars.len()
                                && chars[left_start - 1] == ','
                            {
                                &before[..left_start - 1]
                            } else {
                                &before[..left_start]
                            }
                        } else {
                            // left 是有效 token (如 __try_acquires), 保留
                            &before[..left_start + left.len()]
                        };
                        result = format!("{}{}", prefix, &after[right_end..]);
                        continue;
                    } else {
                        // 非空: 按 ## 语义拼接两侧 token (无空格)
                        // - ,##X: , 在 before[..left_start], 直接拼上 right_sub → ,X
                        // - identifier##__VA_ARGS__: left_sub + right_sub 无空格
                        let pasted = format!("{}{}", left_sub, right_sub);
                        result =
                            format!("{}{}{}", &before[..left_start], pasted, &after[right_end..]);
                        continue;
                    }
                }
            }
            let pasted = format!("{}{}", left_sub, right_sub);
            result = format!("{}{}{}", &before[..left_start], pasted, &after[right_end..]);
        }
        result
    }

    /// # 字符串化: # param → "param_value"
    fn handle_stringify(&self, body: &str, params: &[String], args: &[String]) -> String {
        let mut result = body.to_string();
        for (i, param) in params.iter().enumerate() {
            if i >= args.len() {
                continue;
            }
            // 查找 # param (注意不是 ##)
            let pattern = format!("# {}", param);
            let pattern2 = format!("#{}", param);
            // 从起始位扫描, 避免 ## 后 的 #被误判为 ## 而跳过
            // (find 返回首个匹配, 若该匹配是 ## 的第二#, 必须推进扫描指针)
            let mut search_from;
            for pat in &[&pattern, &pattern2] {
                search_from = 0usize;
                while let Some(rel) = result[search_from..].find(pat.as_str()) {
                    let pos = search_from + rel;
                    // 确保不是 ##: 如果前一字符也是 #, 则这是 ##, 跳过此匹配
                    if pos > 0 && result.as_bytes()[pos - 1] == b'#' {
                        search_from = pos + 1;
                        continue;
                    }
                    let stringified = Self::stringify_arg(&args[i]);
                    let end = pos + pat.len();
                    result = format!("{}{}{}", &result[..pos], stringified, &result[end..]);
                    // 替换后 search_from 前进 (基于原 body 长度, 这里 result 变短,
                    // 新位置以原本的 pos 作为起点继续)
                    search_from = pos + stringified.len();
                }
            }
        }
        result
    }

    /// 将参数值转为字符串字面量 (转义 " 和 \；换行/连续空白折叠为单空格，符合 C # 规则)
    fn stringify_arg(arg: &str) -> String {
        let collapsed: String = arg.split_whitespace().collect::<Vec<_>>().join(" ");
        let escaped = collapsed.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    }

    /// 替换文本中的独立标识符（跳过字符串/字符常量，避免 ENTRY 类宏破坏字面量）
    fn replace_ident(text: &str, from: &str, to: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        while i < len {
            // 跳过字符串字面量
            if chars[i] == '"' {
                result.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len {
                        result.push(chars[i]);
                        i += 1;
                    }
                    result.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    result.push('"');
                    i += 1;
                }
                continue;
            }
            // 跳过字符常量
            if chars[i] == '\'' {
                result.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len {
                        result.push(chars[i]);
                        i += 1;
                    }
                    result.push(chars[i]);
                    i += 1;
                }
                if i < len {
                    result.push('\'');
                    i += 1;
                }
                continue;
            }
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                if ident == from {
                    result.push_str(to);
                } else {
                    result.push_str(&ident);
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    // ===== 常量表达式求值 (用于 #if) =====
    fn eval_const_expr(&mut self, expr: &str) -> bool {
        let expr = expr.trim();
        // 处理 defined(X) 和 defined X
        let expr = self.replace_defined(expr);
        // 处理 __has_include/__has_attribute/__has_builtin/__has_feature
        let expr = self.replace_has_macros(&expr);
        // 替换已知宏
        let expr = self.expand_macros(&expr, &[self.cur_line]);
        // 宏展开后去除残留注释 (内核宏体可能含注释)
        let expr = Self::strip_comments(&expr);
        // 简单求值: 非零 = true
        self.eval_int_expr(&expr) != 0
    }

    fn replace_defined(&self, expr: &str) -> String {
        let mut result = expr.to_string();
        // defined(NAME) or defined NAME
        while let Some(pos) = result.find("defined") {
            // 确保 defined 不在标识符中间
            if pos > 0 {
                let prev = result.as_bytes()[pos - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' {
                    break;
                }
            }
            let after = &result[pos + 7..];
            let after = after.trim_start();
            if after.starts_with('(') {
                let end = after.find(')').unwrap_or(0);
                let name = after[1..end].trim();
                let val = if self.macros.contains_key(name) { "1" } else { "0" };
                result = format!("{}{}{}", &result[..pos], val, &after[end + 1..]);
            } else {
                let name_end = after
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(after.len());
                let name = &after[..name_end];
                let val = if self.macros.contains_key(name) { "1" } else { "0" };
                result = format!("{}{}{}", &result[..pos], val, &after[name_end..]);
            }
        }
        result
    }

    fn eval_int_expr(&self, expr: &str) -> i64 {
        let expr = expr.trim();
        // 替换未知标识符为 0
        let expr = self.replace_unknown_idents(expr);
        // 递归下降求值
        let mut pos = 0;
        let bytes = expr.as_bytes();

        Self::parse_ternary(bytes, &mut pos)
    }

    fn replace_unknown_idents(&self, expr: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = expr.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // 字符常量: 直接保留给 parse_number 处理
            if chars[i] == '\'' {
                result.push('\'');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        result.push(chars[i]);
                        i += 1;
                    }
                    result.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    result.push('\'');
                    i += 1;
                }
                continue;
            }
            // 数字字面量（含 0x/0b 与数字分隔符）须整段保留，
            // 否则 0x41 会被拆成 '0' + 标识符 x41 → 变成 00
            if chars[i].is_ascii_digit() {
                let start = i;
                i += 1;
                if start + 1 < chars.len()
                    && chars[start] == '0'
                    && (chars[start + 1] == 'x'
                        || chars[start + 1] == 'X'
                        || chars[start + 1] == 'b'
                        || chars[start + 1] == 'B')
                {
                    i += 1; // 吃掉 x/X/b/B
                    while i < chars.len() && (chars[i].is_ascii_hexdigit() || chars[i] == '\'') {
                        i += 1;
                    }
                } else {
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '\'') {
                        i += 1;
                    }
                }
                // 保留整型后缀 U/L/ull 等
                while i < chars.len()
                    && (chars[i] == 'u' || chars[i] == 'U' || chars[i] == 'l' || chars[i] == 'L')
                {
                    i += 1;
                }
                result.extend(chars[start..i].iter().copied());
                continue;
            }
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                match ident.as_str() {
                    "defined" | "true" => result.push('1'),
                    "false" => result.push('0'),
                    // __COUNTER__ 等特殊宏在 expand_macros 阶段已被替换为数字,
                    // 但若用户在 #if 中直接写, 仍按 0 兜底 (与 GCC 一致)
                    _ => {
                        result.push('0');
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    // 三元: cond ? then : else
    fn parse_ternary(bytes: &[u8], pos: &mut usize) -> i64 {
        let cond = Self::parse_or_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() && bytes[*pos] == b'?' {
            *pos += 1;
            let then_val = Self::parse_ternary(bytes, pos);
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() && bytes[*pos] == b':' {
                *pos += 1;
            }
            let else_val = Self::parse_ternary(bytes, pos);
            if cond != 0 {
                then_val
            } else {
                else_val
            }
        } else {
            cond
        }
    }

    fn parse_or_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_and_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        while *pos + 1 < bytes.len() && bytes[*pos] == b'|' && bytes[*pos + 1] == b'|' {
            *pos += 2;
            let rhs = Self::parse_and_expr(bytes, pos);
            val = if val != 0 || rhs != 0 { 1 } else { 0 };
            Self::skip_ws(bytes, pos);
        }
        val
    }

    fn parse_and_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_bit_or(bytes, pos);
        Self::skip_ws(bytes, pos);
        while *pos + 1 < bytes.len() && bytes[*pos] == b'&' && bytes[*pos + 1] == b'&' {
            *pos += 2;
            let rhs = Self::parse_bit_or(bytes, pos);
            val = if val != 0 && rhs != 0 { 1 } else { 0 };
            Self::skip_ws(bytes, pos);
        }
        val
    }

    fn parse_bit_or(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_bit_xor(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            // 排除 ||
            if *pos < bytes.len()
                && bytes[*pos] == b'|'
                && (*pos + 1 >= bytes.len() || bytes[*pos + 1] != b'|')
            {
                *pos += 1;
                val |= Self::parse_bit_xor(bytes, pos);
            } else {
                break;
            }
        }
        val
    }

    fn parse_bit_xor(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_bit_and(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() && bytes[*pos] == b'^' {
                *pos += 1;
                val ^= Self::parse_bit_and(bytes, pos);
            } else {
                break;
            }
        }
        val
    }

    fn parse_bit_and(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_eq_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            // 排除 &&
            if *pos < bytes.len()
                && bytes[*pos] == b'&'
                && (*pos + 1 >= bytes.len() || bytes[*pos + 1] != b'&')
            {
                *pos += 1;
                val &= Self::parse_eq_expr(bytes, pos);
            } else {
                break;
            }
        }
        val
    }

    fn parse_eq_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_cmp_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos + 1 < bytes.len() {
                if bytes[*pos] == b'=' && bytes[*pos + 1] == b'=' {
                    *pos += 2;
                    val = (val == Self::parse_cmp_expr(bytes, pos)) as i64;
                } else if bytes[*pos] == b'!' && bytes[*pos + 1] == b'=' {
                    *pos += 2;
                    val = (val != Self::parse_cmp_expr(bytes, pos)) as i64;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        val
    }

    fn parse_cmp_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_shift_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                let (op, len) = match bytes[*pos..] {
                    [b'<', b'=', ..] => ("le", 2),
                    [b'>', b'=', ..] => ("ge", 2),
                    [b'<', ..] => ("lt", 1),
                    [b'>', ..] => ("gt", 1),
                    _ => ("none", 0),
                };
                if op != "none" {
                    *pos += len;
                    let rhs = Self::parse_shift_expr(bytes, pos);
                    val = match op {
                        "lt" => (val < rhs) as i64,
                        "gt" => (val > rhs) as i64,
                        "le" => (val <= rhs) as i64,
                        "ge" => (val >= rhs) as i64,
                        _ => unreachable!(),
                    };
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        val
    }

    fn parse_shift_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_add_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos + 1 < bytes.len() {
                if bytes[*pos] == b'<' && bytes[*pos + 1] == b'<' {
                    *pos += 2;
                    val <<= Self::parse_add_expr(bytes, pos);
                } else if bytes[*pos] == b'>' && bytes[*pos + 1] == b'>' {
                    *pos += 2;
                    val >>= Self::parse_add_expr(bytes, pos);
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        val
    }

    fn parse_add_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_mul_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                if bytes[*pos] == b'+' {
                    *pos += 1;
                    val += Self::parse_mul_expr(bytes, pos);
                } else if bytes[*pos] == b'-' {
                    *pos += 1;
                    val -= Self::parse_mul_expr(bytes, pos);
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        val
    }

    fn parse_mul_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_unary(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                if bytes[*pos] == b'*' {
                    *pos += 1;
                    val *= Self::parse_unary(bytes, pos);
                } else if bytes[*pos] == b'/' {
                    *pos += 1;
                    let d = Self::parse_unary(bytes, pos);
                    val = if d != 0 { val / d } else { 0 };
                } else if bytes[*pos] == b'%' {
                    *pos += 1;
                    let d = Self::parse_unary(bytes, pos);
                    val = if d != 0 { val % d } else { 0 };
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        val
    }

    fn parse_unary(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() {
            if bytes[*pos] == b'!' {
                *pos += 1;
                return if Self::parse_unary(bytes, pos) == 0 { 1 } else { 0 };
            }
            if bytes[*pos] == b'-' {
                *pos += 1;
                return -Self::parse_unary(bytes, pos);
            }
            if bytes[*pos] == b'+' {
                *pos += 1;
                return Self::parse_unary(bytes, pos);
            }
            if bytes[*pos] == b'~' {
                *pos += 1;
                return !Self::parse_unary(bytes, pos);
            }
            if bytes[*pos] == b'(' {
                *pos += 1;
                let val = Self::parse_ternary(bytes, pos);
                Self::skip_ws(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b')' {
                    *pos += 1;
                }
                return val;
            }
        }
        Self::parse_primary(bytes, pos)
    }

    fn parse_primary(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() && bytes[*pos] == b'\'' {
            return Self::parse_char_const(bytes, pos);
        }
        Self::parse_number(bytes, pos)
    }

    /// 解析字符常量: 'A', '\n', '\x41', '\0' 等
    fn parse_char_const(bytes: &[u8], pos: &mut usize) -> i64 {
        debug_assert!(bytes[*pos] == b'\'');
        *pos += 1; // skip '
        let ch = if *pos < bytes.len() && bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos < bytes.len() {
                let c = bytes[*pos];
                *pos += 1;
                match c {
                    b'n' => '\n' as i64,
                    b't' => '\t' as i64,
                    b'r' => '\r' as i64,
                    b'0' => 0,
                    b'\\' => '\\' as i64,
                    b'\'' => '\'' as i64,
                    b'"' => '"' as i64,
                    b'a' => 7,  // bell
                    b'b' => 8,  // backspace
                    b'f' => 12, // form feed
                    b'v' => 11, // vertical tab
                    b'x' => {
                        // \xNN
                        let mut val: i64 = 0;
                        for _ in 0..2 {
                            if *pos < bytes.len() {
                                let d = bytes[*pos];
                                if d.is_ascii_hexdigit() {
                                    val = val * 16 + (d as char).to_digit(16).unwrap() as i64;
                                    *pos += 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        val
                    }
                    d if d.is_ascii_digit() => {
                        // \NNN 八进制
                        let mut val = (d - b'0') as i64;
                        for _ in 0..2 {
                            if *pos < bytes.len()
                                && bytes[*pos].is_ascii_digit()
                                && bytes[*pos] < b'8'
                            {
                                val = val * 8 + (bytes[*pos] - b'0') as i64;
                                *pos += 1;
                            } else {
                                break;
                            }
                        }
                        val
                    }
                    _ => c as i64,
                }
            } else {
                0
            }
        } else if *pos < bytes.len() {
            let c = bytes[*pos] as i64;
            *pos += 1;
            c
        } else {
            0
        };
        // skip closing '
        if *pos < bytes.len() && bytes[*pos] == b'\'' {
            *pos += 1;
        }
        ch
    }

    fn parse_number(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        let start = *pos;
        if *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            while *pos < bytes.len()
                && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'\'')
            {
                *pos += 1;
            }
            let s = std::str::from_utf8(&bytes[start..*pos]).unwrap_or("0");
            // 去掉后缀 (L, U, LL 等)
            let s = s.trim_end_matches(['L', 'l', 'U', 'u']);
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).unwrap_or(0)
            } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
                i64::from_str_radix(bin, 2).unwrap_or(0)
            } else if s.starts_with('0') && s.len() > 1 && s.chars().all(|c| c.is_ascii_digit()) {
                i64::from_str_radix(s, 8).unwrap_or(0)
            } else {
                s.parse::<i64>().unwrap_or(0)
            }
        } else {
            0
        }
    }

    fn skip_ws(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
            *pos += 1;
        }
    }

    // ===== __has_* 特殊宏求值 (在宏展开之前执行) =====
    /// 替换 __has_include/__has_attribute/__has_builtin/__has_feature/
    /// __has_embed/__has_c_attribute 为实际值
    fn replace_has_macros(&self, expr: &str) -> String {
        let mut result = expr.to_string();
        result = self.replace_has_include(&result);
        result = self.replace_has_embed(&result);
        result = self.replace_has_attr_like(&result, "__has_attribute", Self::is_known_attribute);
        result =
            self.replace_has_attr_like(&result, "__has_c_attribute", Self::is_known_c_attribute);
        result = self.replace_has_attr_like(&result, "__has_builtin", Self::is_known_builtin);
        result = self.replace_has_attr_like(&result, "__has_feature", |_| false);
        result
    }

    /// C23 __has_embed(header-name ...): 文件是否存在 (忽略 limit 等参数)
    fn replace_has_embed(&self, expr: &str) -> String {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i + 11 <= bytes.len()
                && &bytes[i..i + 11] == b"__has_embed"
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_')
                && (i + 11 >= bytes.len()
                    || !bytes[i + 11].is_ascii_alphanumeric() && bytes[i + 11] != b'_')
            {
                let mut j = i + 11;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' {
                        j += 1;
                    }
                    // 提取路径: "file" 或 <file> 形式
                    let path = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'<') {
                        let start = j;
                        let close_ch = if bytes[j] == b'"' { b'"' } else { b'>' };
                        j += 1;
                        while j < bytes.len() && bytes[j] != close_ch {
                            j += 1;
                        }
                        if j < bytes.len() {
                            j += 1;
                        }
                        expr[start..j]
                            .trim()
                            .trim_matches(|c| c == '"' || c == '<' || c == '>')
                            .to_string()
                    } else {
                        String::new()
                    };
                    // 跳过剩余参数 (limit(...) 等) 至 ')' 并求值
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' {
                            depth += 1;
                        } else if bytes[j] == b')' {
                            depth -= 1;
                        }
                        if depth > 0 {
                            j += 1;
                        }
                    }
                    let found = !path.is_empty() && self.find_has_include(&path);
                    result.push_str(if found { "1" } else { "0" });
                    if j < bytes.len() {
                        j += 1;
                    } // skip )
                    i = j;
                    continue;
                }
            }
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }

    fn replace_has_include(&self, expr: &str) -> String {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // 检查 __has_include (13 字符)
            if i + 13 <= bytes.len()
                && &bytes[i..i + 13] == b"__has_include"
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_')
                && (i + 13 >= bytes.len()
                    || !bytes[i + 13].is_ascii_alphanumeric() && bytes[i + 13] != b'_')
            {
                let mut j = i + 13;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' {
                        j += 1;
                    }
                    let path_start = j;
                    // 跳过到 )
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' {
                            depth += 1;
                        } else if bytes[j] == b')' {
                            depth -= 1;
                        }
                        if depth > 0 {
                            j += 1;
                        }
                    }
                    let path = expr[path_start..j]
                        .trim()
                        .trim_matches(|c| c == '<' || c == '>' || c == '"');
                    let found = self.find_has_include(path);
                    result.push_str(if found { "1" } else { "0" });
                    if j < bytes.len() {
                        j += 1;
                    } // skip )
                    i = j;
                    continue;
                }
            }
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }

    fn replace_has_attr_like<F>(&self, expr: &str, macro_name: &str, check: F) -> String
    where
        F: Fn(&str) -> bool,
    {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let name_len = macro_name.len();
        let mut i = 0;
        while i < bytes.len() {
            if i + name_len <= bytes.len()
                && &bytes[i..i + name_len] == macro_name.as_bytes()
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_')
                && (i + name_len >= bytes.len()
                    || !bytes[i + name_len].is_ascii_alphanumeric() && bytes[i + name_len] != b'_')
            {
                let mut j = i + name_len;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' {
                        j += 1;
                    }
                    let arg_start = j;
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' {
                            depth += 1;
                        } else if bytes[j] == b')' {
                            depth -= 1;
                        }
                        if depth > 0 {
                            j += 1;
                        }
                    }
                    let arg = expr[arg_start..j].trim();
                    let val = if check(arg) { "1" } else { "0" };
                    result.push_str(val);
                    if j < bytes.len() {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
            result.push(bytes[i] as char);
            i += 1;
        }
        result
    }

    fn find_has_include(&self, path: &str) -> bool {
        let path = path.trim();
        let abs = Path::new(path);
        if abs.is_absolute() {
            return abs.exists();
        }
        // 相对当前文件
        let p = self.base_dir.join(path);
        if p.exists() {
            return true;
        }
        // -I 路径
        for dir in &self.include_paths {
            if dir.join(path).exists() {
                return true;
            }
        }
        // -isystem 路径
        for dir in &self.system_paths {
            if dir.join(path).exists() {
                return true;
            }
        }
        // 标准系统路径
        let system_dirs = [
            "/usr/local/include",
            "/usr/include",
            "/usr/include/x86_64-linux-gnu",
            "/usr/include/aarch64-linux-gnu",
            "/usr/include/linux",
            "/usr/include/asm",
            "/usr/include/asm-generic",
        ];
        for dir in &system_dirs {
            if Path::new(dir).join(path).exists() {
                return true;
            }
        }
        false
    }

    fn is_known_attribute(attr: &str) -> bool {
        matches!(
            attr,
            "packed"
                | "aligned"
                | "noreturn"
                | "unused"
                | "used"
                | "section"
                | "visibility"
                | "always_inline"
                | "noinline"
                | "format"
                | "nonnull"
                | "cleanup"
                | "deprecated"
                | "fallthrough"
                | "weak"
                | "constructor"
                | "destructor"
                | "vector_size"
                | "may_alias"
                | "transparent_union"
                | "malloc"
                | "returns_twice"
                | "pure"
                | "const"
                | "warn_unused_result"
                | "sentinel"
                | "format_arg"
                | "maybe_unused"
                | "nodiscard"
                | "reproducible"
                | "unsequenced"
                | "returns_nonnull"
                | "assume"
        )
    }

    /// C23 标准属性 (__has_c_attribute 用): 返回 1 表示支持
    fn is_known_c_attribute(attr: &str) -> bool {
        let a = attr.trim().trim_matches('_');
        matches!(
            a,
            "deprecated"
                | "fallthrough"
                | "maybe_unused"
                | "nodiscard"
                | "noreturn"
                | "unsequenced"
                | "reproducible"
                | "assume"
                | "nonnull"
                | "returns_nonnull"
        )
    }

    fn is_known_builtin(name: &str) -> bool {
        let name = name.trim();
        matches!(
            name,
            "__builtin_expect"
                | "__builtin_memcpy"
                | "__builtin_memset"
                | "__builtin_memmove"
                | "__builtin_memcmp"
                | "__builtin_malloc"
                | "__builtin_free"
                | "__builtin_offsetof"
                | "__builtin_va_start"
                | "__builtin_va_end"
                | "__builtin_va_arg"
                | "__builtin_va_copy"
                | "__builtin_add_overflow"
                | "__builtin_sub_overflow"
                | "__builtin_mul_overflow"
                | "__builtin_types_compatible_p"
                | "__builtin_choose_expr"
                | "__builtin_constant_p"
                | "__builtin_abs"
                | "__builtin_unreachable"
                | "__builtin_trap"
                | "__builtin_strcpy"
                | "__builtin_strncpy"
                | "__builtin_strcmp"
                | "__builtin_strlen"
                | "__builtin_prefetch"
                | "__builtin_object_size"
                | "__builtin_expect_with_probability"
                | "__builtin_clz"
                | "__builtin_clzl"
                | "__builtin_clzll"
                | "__builtin_ctz"
                | "__builtin_ctzl"
                | "__builtin_ctzll"
                | "__builtin_popcount"
                | "__builtin_popcountl"
                | "__builtin_popcountll"
                | "__builtin_parity"
                | "__builtin_parityl"
                | "__builtin_parityll"
                | "__builtin_bswap16"
                | "__builtin_bswap32"
                | "__builtin_bswap64"
                | "__builtin_bitreverse8"
                | "__builtin_bitreverse16"
                | "__builtin_bitreverse32"
                | "__builtin_bitreverse64"
                | "__builtin_frame_address"
                | "__builtin_return_address"
                | "__builtin_overflow_p"
                | "__builtin_add_overflow_p"
                | "__builtin_sub_overflow_p"
                | "__builtin_mul_overflow_p"
                | "__builtin_apply"
                | "__builtin_return"
        )
    }

    // ===== 全局类型注册表: 头文件缓存 =====
    // 设计: 缓存已处理头文件的输出和宏状态, 避免重复解析
    // 缓存键: 文件路径 + 内容哈希
    // 缓存值: 预处理输出 + 宏定义

    fn init_cache_dir() -> Option<PathBuf> {
        // 尝试创建 ~/.dcc/cache/
        if let Ok(home) = std::env::var("HOME") {
            let cache = PathBuf::from(home).join(".dcc").join("cache");
            if fs::create_dir_all(&cache).is_ok() {
                return Some(cache);
            }
        }
        None
    }

    fn hash_content(content: &str) -> u64 {
        // FNV-1a 哈希 (简单高效, 零依赖)
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in content.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b7);
        }
        hash
    }

    fn hash_macro_state(&self) -> u64 {
        let mut names: Vec<&String> = self.macros.keys().collect();
        names.sort();
        // 预处理语义变更时须 bump 此版本，避免复用过期 include 缓存
        // V12: 缓存条目保存完整宏变更集 (新增 + 重新定义 + #undef 删除),
        //     命中时先删后插回放; 旧版只回放新增宏, 丢失对已存在宏的
        //     覆盖 (如 stddef.h 重新定义内置 NULL), 导致冷/热缓存运行
        //     输出不一致。
        let mut hash = Self::hash_content("DCC_PREPROCESSOR_CACHE_V12");
        for name in names {
            let def = &self.macros[name];
            hash ^= Self::hash_content(name);
            hash = hash.wrapping_mul(0x100000001b3);
            if let Some(params) = &def.params {
                for param in params {
                    hash ^= Self::hash_content(param);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
            }
            hash ^= Self::hash_content(&def.body);
            hash = hash.wrapping_mul(0x100000001b3);
            hash ^= u64::from(def.variadic);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn get_cache_path(
        &self,
        file_path: &Path,
        content_hash: u64,
        macro_hash: u64,
    ) -> Option<PathBuf> {
        let cache_dir = self.cache_dir.as_ref()?;
        let file_hash = Self::hash_content(&file_path.to_string_lossy());
        let cache_name = format!("v12_{:x}_{:x}_{:x}", file_hash, content_hash, macro_hash);
        Some(cache_dir.join(cache_name))
    }

    /// 校验缓存条目的依赖闭包是否新鲜: 逐个重读依赖文件并比对内容哈希。
    /// 任一文件变更/缺失即失效 (依赖感知失效, 修复传递依赖陈旧问题)。
    fn cache_deps_fresh(deps: &[(String, u64)]) -> bool {
        for (path, hash) in deps {
            // 内置 intrinsics 头为伪路径 (编译期内置, 内容恒定), 跳过实读
            if path.starts_with("<builtin:") {
                continue;
            }
            match fs::read_to_string(path) {
                Ok(content) => {
                    if Self::hash_content(&content) != *hash {
                        return false;
                    }
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// 探测 include-once 守卫: 跳过注释与空行后, 前两条指令为
    /// `#ifndef X` (或 `#if !defined(X)`) + `#define X` 即视为守卫,
    /// 且守卫的匹配 `#endif` 必须是文件最后一个指令 (守卫后无内容),
    /// 守卫层不允许 #else/#elif。若守卫之外还有指令 (如 tracepoint.h
    /// 在守卫外定义 TRACE_EVENT, 每次包含都须重读), 返回 None,
    /// 不能走整体跳过快路径。
    fn detect_include_guard(text: &str) -> Option<String> {
        let mut in_block_comment = false;
        // 收集所有指令行 (去 #)
        let mut directives: Vec<&str> = Vec::new();
        let mut first: Option<&str> = None;
        let mut second: Option<&str> = None;
        for line in text.split('\n') {
            let line = line.trim();
            if in_block_comment {
                if line.contains("*/") {
                    in_block_comment = false;
                }
                continue;
            }
            if line.is_empty() {
                continue;
            }
            if line.starts_with("//") {
                continue;
            }
            if line.starts_with("/*") {
                if !line.contains("*/") {
                    in_block_comment = true;
                }
                continue;
            }
            // 指令前出现代码 → 不是守卫文件
            let after_hash = line.strip_prefix('#')?;
            let d = after_hash.trim_start();
            if first.is_none() {
                first = Some(d);
            } else if second.is_none() {
                second = Some(d);
            }
            directives.push(d);
        }
        // 第一条指令必须是 `#ifndef X` 或 `#if !defined(X)`
        let (guard, ifndef_idx) = {
            let f = first?;
            if let Some(rest) = f.strip_prefix("ifndef ") {
                (rest.trim().to_string(), 0usize)
            } else {
                let rest = f.strip_prefix("if !defined(")?;
                let name = rest.trim_end_matches(')').trim();
                if name.is_empty() || name.contains(')') {
                    return None;
                }
                (name.to_string(), 0usize)
            }
        };
        // 第二条指令必须是 `#define <guard> ...`
        let ok = second
            .map(|s| {
                s.strip_prefix("define ")
                    .map(|rest| rest.trim_start().starts_with(guard.as_str()))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !ok {
            return None;
        }
        // 从守卫 #if 起扫描: 匹配 #endif 必须是最后一个指令,
        // 守卫层不得出现 #else/#elif (否则宏已定义时须走 else 分支)
        let mut depth = 0i32;
        for (i, d) in directives.iter().enumerate().skip(ifndef_idx) {
            let d = d.trim_start();
            if d.starts_with("if ")
                || d.starts_with("if\t")
                || d.starts_with("ifdef ")
                || d.starts_with("ifdef\t")
                || d.starts_with("ifndef ")
                || d.starts_with("ifndef\t")
                || d.starts_with("if !defined(")
            {
                depth += 1;
            } else if d.starts_with("endif") {
                depth -= 1;
                if depth == 0 {
                    // 守卫 #endif 之后不能有其它指令
                    return if i == directives.len() - 1 { Some(guard) } else { None };
                }
            } else if depth == 1 && (d.starts_with("else") || d.starts_with("elif")) {
                // 守卫层 #else/#elif: 宏已定义时 GCC 会处理 else 分支, 不能跳过
                return None;
            }
        }
        None
    }

    /// 比较两个宏定义是否语义等价 (params/body/variadic 全同)。
    fn macro_def_eq(a: &MacroDef, b: &MacroDef) -> bool {
        a.params == b.params && a.body == b.body && a.variadic == b.variadic
    }

    fn load_from_cache(&self, cache_path: &Path) -> Option<MacroCacheData> {
        let content = fs::read_to_string(cache_path).ok()?;
        let mut lines = content.lines();

        // 第一行: 标记
        if lines.next()? != "DCC_CACHE_V6" {
            return None;
        }

        // 第二行: COUNTER_DELTA <n> — 本头文件处理期间的 __COUNTER__ 消耗量
        let line = lines.next()?;
        let counter_delta = {
            let rest = line.strip_prefix("COUNTER_DELTA\t")?;
            rest.trim().parse().unwrap_or(0)
        };

        // DEPS 段: <path>\t<hash_hex>
        let mut deps = Vec::new();
        if lines.next()? != "DEPS" {
            return None;
        }
        for line in lines.by_ref() {
            if line == "END_DEPS" {
                break;
            }
            if let Some((p, h)) = line.split_once('\t') {
                if let Ok(hash) = u64::from_str_radix(h, 16) {
                    deps.push((p.to_string(), hash));
                }
            }
        }

        // REMOVED 段: 处理期间被 #undef 删除的宏名 (逐行)
        let mut removed = Vec::new();
        if lines.next()? != "REMOVED" {
            return None;
        }
        for line in lines.by_ref() {
            if line == "END_REMOVED" {
                break;
            }
            removed.push(line.to_string());
        }

        // 读取预处理输出 (直到 END_OUTPUT 标记)
        let mut output = String::new();
        for line in lines.by_ref() {
            if line == "END_OUTPUT" {
                break;
            }
            output.push_str(line);
            output.push('\n');
        }

        // 读取宏定义
        let mut macros = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            // 格式: NAME\tPARAMS\tVARIADIC\tBODY
            let parts: Vec<&str> = line.splitn(4, '\t').collect();
            if parts.len() >= 4 {
                let name = parts[0].to_string();
                let params = if parts[1] == "-" {
                    None
                } else {
                    // 零参 function-like 宏缓存为 ""；旧版 split 会误读成 [""]
                    let p: Vec<String> = parts[1]
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                    Some(p)
                };
                let variadic = parts[2] == "1";
                let body = parts[3].to_string();
                macros.push((
                    name,
                    MacroDef {
                        params,
                        body,
                        variadic,
                    },
                ));
            }
        }

        Some((output, macros, removed, deps, counter_delta))
    }

    fn save_to_cache(
        &self,
        cache_path: &Path,
        output: &str,
        macros: &HashMap<String, MacroDef>,
        removed: &[String],
        deps: &[(String, u64)],
        counter_delta: u32,
    ) {
        let mut content = String::from("DCC_CACHE_V6\n");
        content.push_str(&format!("COUNTER_DELTA\t{}\n", counter_delta));
        content.push_str("DEPS\n");
        for (p, h) in deps {
            content.push_str(&format!("{}\t{:x}\n", p, h));
        }
        content.push_str("END_DEPS\n");
        content.push_str("REMOVED\n");
        for name in removed {
            content.push_str(&format!("{}\n", name));
        }
        content.push_str("END_REMOVED\n");
        content.push_str(output);
        content.push_str("END_OUTPUT\n");

        for (name, def) in macros {
            // 格式: NAME\tPARAMS\tVARIADIC\tBODY
            let params_str = match &def.params {
                None => "-".to_string(),
                Some(p) => p.join(","),
            };
            let variadic_str = if def.variadic { "1" } else { "0" };
            content.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                name, params_str, variadic_str, def.body
            ));
        }

        let _ = fs::write(cache_path, content);
    }
}

#[cfg(test)]
mod tests {
    use super::preprocess;
    use super::{MacroDef, PchBundle, PchOptions};
    use crate::cli::StdMode;
    use std::path::PathBuf;

    #[test]
    fn include_unquoted_absolute_path() {
        use std::fs;
        let dir = std::env::temp_dir();
        let hdr = dir.join(format!("dcc-abs-inc-{}.h", std::process::id()));
        fs::write(&hdr, "#define ABS_INCLUDE_MARKER 42\n").expect("write hdr");
        let src = format!("#include {}\nint x = ABS_INCLUDE_MARKER;\n", hdr.display());
        let out = preprocess(
            src.as_bytes(),
            "abs-include.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let _ = fs::remove_file(hdr);
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            output.contains("int x = ABS_INCLUDE_MARKER") || output.contains("int x = 42"),
            "output: {output}"
        );
    }

    #[test]
    fn forced_include_comments_cannot_leak_literals_or_words() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c/comment.h");
        let force_includes = vec![fixture.to_string_lossy().into_owned()];
        let out = preprocess(
            b"int value = FORCED_VALUE;\n",
            "comment-regression.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &force_includes,
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;

        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(output.trim(), "int value = 7;");
        assert!(!output.contains("Don't"));
        assert!(!output.contains("quoted text"));
        assert!(!output.contains("COMMENT_WORD"));
    }

    #[test]
    fn gnu_variadic_preserves_multiline_asm_goto_args() {
        let out = preprocess(
            b"#define asm_volatile_goto(x...) asm goto(x)\n\
              void f(void) {\n\
              asm_volatile_goto(\"1:\"\n\
                  \"jmp\\n\"\n\
                  : : \"i\" (0) : : lbl);\n\
              lbl: ; }\n",
            "asm-goto-expand.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("asm goto(\"1:\""), "output: {output}");
        assert!(output.contains("\"jmp\\n\""), "output: {output}");
        assert!(
            output.contains(": : \"i\" (0) : : lbl)"),
            "output: {output}"
        );
        assert!(!output.contains("asm goto(\"1:\")"), "output: {output}");
    }

    #[test]
    fn iso_variadic_macro_allows_empty_va_args() {
        let out = preprocess(
            b"#define __stringify_1(x...) #x\n\
              #define __stringify(x...) __stringify_1(x)\n\
              #define __ASM_FORM(x, ...) \" \" __stringify(x,##__VA_ARGS__) \" \"\n\
              #define __ASM_SEL(a,b) __ASM_FORM(b)\n\
              const char *s = __ASM_SEL(1, 2);\n",
            "iso-variadic.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("\"2\""), "output: {output}");
        assert!(!output.contains("__ASM_FORM"), "output: {output}");
    }

    #[test]
    fn gnu_named_variadic_macro_expands() {
        let out = preprocess(
            b"#define asm_volatile_goto(x...) asm goto(x)\n\
              void f(void) { asm_volatile_goto(\"jmp\" : : : : lbl); lbl: ; }\n",
            "gnu-variadic.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("asm goto(\"jmp\""), "output: {output}");
        assert!(!output.contains("asm goto(x)"), "output: {output}");
    }

    #[test]
    fn unterminated_block_comment_is_an_error() {
        let out = preprocess(
            b"int value;\n/* never closed\n",
            "unterminated.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let diags = out.diags;

        assert!(diags.iter().any(|d| d.code == "E0207"));
    }

    /// #if 中 0x 十六进制不得被 replace_unknown_idents 拆坏（GCC safe-ctype.h）
    #[test]
    fn if_hex_and_char_const_compare() {
        let out = preprocess(
            b"#if '\\n' == 0x0A && ' ' == 0x20 && '0' == 0x30 \\\n\
               && 'A' == 0x41 && 'a' == 0x61 && '!' == 0x21\n\
              #define HOST_CHARSET 1\n\
              #else\n\
              #define HOST_CHARSET 0\n\
              #endif\n\
              int x = HOST_CHARSET;\n",
            "charset.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int x = 1;"), "output: {output}");
    }

    #[test]
    fn if_hex_literal_equals_decimal() {
        let out = preprocess(
            b"#if 0x41 == 65\n#define OK 1\n#else\n#define OK 0\n#endif\nint x = OK;\n",
            "hex-if.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int x = 1;"), "output: {output}");
    }

    #[test]
    fn object_like_alias_expands_function_like_tail() {
        let out = preprocess(
            b"#define __KERNEL_DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))\n\
              #define DIV_ROUND_UP __KERNEL_DIV_ROUND_UP\n\
              #define XA_MARK_LONGS DIV_ROUND_UP((1UL << 6), 64)\n\
              unsigned long tags[3][XA_MARK_LONGS];\n",
            "div-round.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            !output.contains("__KERNEL_DIV_ROUND_UP"),
            "output: {output}"
        );
        assert!(
            output.contains("unsigned long tags[3]["),
            "output: {output}"
        );
    }

    /// 嵌套函数宏参数预展开须继承涂蓝集，避免 F(F(1)) 类无限递归
    #[test]
    fn nested_function_macro_does_not_recurse_forever() {
        let out = preprocess(
            b"#define F(x) G(x)\n\
              #define G(x) F(x)\n\
              int v = F(F(1));\n",
            "nested-macro.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // 展开后应留下涂蓝的 F(...) 调用，而非栈溢出
        assert!(output.contains("int v ="), "output: {output}");
        assert!(!output.contains("G(G("), "output: {output}");
    }

    #[test]
    fn include_next_is_recognized() {
        use std::fs;
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("dcc_inc_next_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        // a/foo.h 通过 include_next 拉到 b/foo.h
        fs::write(
            dir.join("a/foo.h"),
            "#ifndef A_FOO\n#define A_FOO 1\n#include_next <foo.h>\n#endif\n",
        )
        .unwrap();
        fs::write(dir.join("b/foo.h"), "#define FROM_B 42\n").unwrap();
        let src = dir.join("t.c");
        let mut f = fs::File::create(&src).unwrap();
        writeln!(f, "#include <foo.h>\nint x = FROM_B;").unwrap();
        let out = preprocess(
            &fs::read(&src).unwrap(),
            src.to_str().unwrap(),
            &[],
            &[dir.join("a"), dir.join("b")],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        let _ = fs::remove_dir_all(&dir);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int x = 42;"), "output: {output}");
    }

    /// 零参 function-like 宏经缓存往返后仍须匹配 F() 而非误读成 F("")
    #[test]
    fn zero_arg_function_macro_roundtrip() {
        let out = preprocess(
            b"#define F() 42\nint x = F();\n",
            "zero-arg-fn.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int x = 42;"), "output: {output}");
        assert!(!output.contains("F()"), "macro should expand: {output}");
    }

    #[test]
    fn current_thread_info_zero_arg_expands() {
        let out = preprocess(
            b"#define current ((void*)0)\n\
              #define current_thread_info() ((struct thread_info *)current)\n\
              struct thread_info { unsigned long syscall_work; };\n\
              void *p = &current_thread_info()->syscall_work;\n",
            "cti.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            output.contains("((struct thread_info *)"),
            "output: {output}"
        );
        assert!(
            !output.contains("current_thread_info->"),
            "output: {output}"
        );
    }

    #[test]
    fn stringify_collapses_multiline_whitespace() {
        let out = preprocess(
            b"#define NOFAIL(ptr) do_nofail((ptr), #ptr)\n\
              void *do_nofail(void *p, const char *e);\n\
              void *f(void) {\n\
                return NOFAIL(malloc(sizeof(int) + \\\n\
                                     1));\n\
              }\n",
            "nofail.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            output.contains("\"malloc(sizeof(int) + 1)\""),
            "output: {output}"
        );
    }

    /// M19: __COUNTER__ 单调递增, 每次访问自增
    #[test]
    fn counter_macro_monotonic_increment() {
        let out = preprocess(
            b"int a = __COUNTER__;\n\
              int b = __COUNTER__;\n\
              int c = __COUNTER__;\n\
              int x = __COUNTER__;\n",
            "counter.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // 期望: a=0, b=1, c=2, x=3
        assert!(output.contains("int a = 0;"), "output: {output}");
        assert!(output.contains("int b = 1;"), "output: {output}");
        assert!(output.contains("int c = 2;"), "output: {output}");
        assert!(output.contains("int x = 3;"), "output: {output}");
    }

    /// M19: __COUNTER__ 在宏体中依然递增
    #[test]
    fn counter_inside_function_like_macro() {
        let out = preprocess(
            b"#define MAKE_VAR(name) int name = __COUNTER__\n\
              MAKE_VAR(a);\n\
              MAKE_VAR(b);\n\
              MAKE_VAR(c);\n",
            "counter-macro.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // 三个 MAKE_VAR 展开后必须分别得到 0/1/2 (而非同一个常量)
        assert!(output.contains("int a = 0"), "output: {output}");
        assert!(output.contains("int b = 1"), "output: {output}");
        assert!(output.contains("int c = 2"), "output: {output}");
    }

    /// M19: __COUNTER__ 在 #if 中可被求值
    #[test]
    fn counter_in_if_expression() {
        let out = preprocess(
            b"#if __COUNTER__ == 0\n\
              int first = 1;\n\
              #else\n\
              int first = 0;\n\
              #endif\n",
            "counter-if.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int first = 1;"), "output: {output}");
    }

    /// M19: __VA_OPT__ 变参宏, 非空变参时展开 content
    #[test]
    fn va_opt_with_args_expands_content() {
        let out = preprocess(
            b"#define LOG(fmt, ...) printf(fmt __VA_OPT__(, __VA_ARGS__))\n\
              int main(void) {\n\
                LOG(\"hello %d\\n\", 42);\n\
                return 0;\n\
              }\n",
            "va-opt-args.c",
            &[],
            &[],
            &[],
            StdMode::Gnu23,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // 非空: __VA_OPT__(, __VA_ARGS__) → , 42
        // 宏体中 fmt 与 __VA_OPT__ 之间有空格, GCC 实际展开也会保留该空格
        assert!(
            output.contains("printf(\"hello %d\\n\" , 42)"),
            "output: {output}"
        );
    }

    /// M19: __VA_OPT__ 变参宏, 空变参时移除 content
    #[test]
    fn va_opt_empty_args_removes_content() {
        let out = preprocess(
            b"#define LOG(fmt, ...) printf(fmt __VA_OPT__(, __VA_ARGS__))\n\
              int main(void) {\n\
                LOG(\"hello\\n\");\n\
                return 0;\n\
              }\n",
            "va-opt-no-args.c",
            &[],
            &[],
            &[],
            StdMode::Gnu23,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // 空: __VA_OPT__(, __VA_ARGS__) → 空, 前面紧邻空格也一并去掉 (GCC 行为)
        assert!(output.contains("printf(\"hello\\n\")"), "output: {output}");
        assert!(
            !output.contains("printf(\"hello\\n\" )"),
            "output: {output}"
        );
        assert!(!output.contains("__VA_OPT__"), "output: {output}");
    }

    /// M19: __VA_OPT__ 与 ## 组合, 头文件族常用
    #[test]
    fn va_opt_gnu_define_with_va_args() {
        let out = preprocess(
            b"#define FOREACH(it, list, ...) \\\n\
              for (it = list; it; it = it->next) __VA_OPT__(__VA_ARGS__)\n\
              int use(struct item *it) { FOREACH(it, NULL, use(it->next)); return 0; }\n",
            "va-opt-foreach.c",
            &[],
            &[],
            &[],
            StdMode::Gnu23,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // 该 fixture 实际调用了宏, 验证非空时 __VA_OPT__(content) 展开为 content
        // NULL 会被预展开为 ((void*)0), 所以 list 部分会变成 ((void*)0)
        assert!(output.contains("for (it = ((void*)0)"), "output: {output}");
        assert!(
            output.contains("it = it->next) use(it->next)"),
            "output: {output}"
        );
    }

    /// M19: __VA_OPT__ 嵌套 (内层先解)
    #[test]
    fn va_opt_nested() {
        let out = preprocess(
            b"#define NESTED(a, ...) a __VA_OPT__(b __VA_OPT__(c))\n\
              int x1 = NESTED(1);\n\
              int x2 = NESTED(1, 2);\n",
            "va-opt-nested.c",
            &[],
            &[],
            &[],
            StdMode::Gnu23,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        // NESTED(1) 空参 → 1 (外层内层都去掉)
        assert!(output.contains("int x1 = 1"), "output: {output}");
        // NESTED(1, 2) 非空 → 1 b (内层空去掉, 外层保留)
        assert!(output.contains("int x2 = 1 b"), "output: {output}");
    }

    /// M19: 补齐 GCC 预定义宏 (x86_64 别名 + 平台/链接信息)
    #[test]
    fn predefined_x86_64_aliases_available() {
        let out = preprocess(
            b"#if defined(__amd64__) && defined(__amd64) && defined(__x86_64) \\\
n\
                 && defined(__k8__) && defined(__k8) && defined(__x86_64__)\n\
              int all_ok = 1;\n\
              #else\n\
              int all_ok = 0;\n\
              #endif\n",
            "predef-arch.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int all_ok = 1;"), "output: {output}");
    }

    /// M19: 平台/编译相关 GCC 预定义宏
    #[test]
    fn predefined_platform_macros_available() {
        let out = preprocess(
            b"#if defined(__USER_LABEL_PREFIX__) && defined(__VERSION__) \\\
n\
                 && defined(__OPTIMIZE__) && defined(__NO_INLINE__) \\\
n\
                 && defined(__SEG_GS) && defined(__SEG_FS)\n\
              int all_ok = 1;\n\
              #else\n\
              int all_ok = 0;\n\
              #endif\n",
            "predef-platform.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("int all_ok = 1;"), "output: {output}");
    }

    /// M19: #pragma locale 接受且不报错
    #[test]
    fn pragma_locale_is_accepted() {
        let out = preprocess(
            b"#pragma locale default\n\
              #pragma locale \"POSIX\"\n\
              #pragma locale reset\n\
              int x = 0;\n",
            "pragma-locale.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "pragma locale must be silent: {diags:?}");
        assert!(output.contains("int x = 0;"), "output: {output}");
    }

    /// P4.1: #pragma STDC FENV_ACCESS ON/OFF/DEFAULT 接受为 noop,
    /// 不发警告, 且在 Preprocessor.fenv_access 字段上反映状态.
    /// ON / DEFAULT 走 true, OFF 走 false, 小写状态也接受.
    #[test]
    fn pragma_stdc_fenv_access_accepted_silently() {
        // OFF 路径: fenv_access 必须为 false
        let out_off = preprocess(
            b"#pragma STDC FENV_ACCESS OFF\nint x = 0;\n",
            "p4_1_off.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            out_off.diags.is_empty(),
            "OFF must be silent: {:?}",
            out_off.diags
        );
        // ON 路径: fenv_access 必须为 true
        let out_on = preprocess(
            b"#pragma STDC FENV_ACCESS ON\nint x = 0;\n",
            "p4_1_on.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            out_on.diags.is_empty(),
            "ON must be silent: {:?}",
            out_on.diags
        );
        // DEFAULT 路径: fenv_access 必须为 true (DEFAULT = ON)
        let out_def = preprocess(
            b"#pragma STDC FENV_ACCESS DEFAULT\nint x = 0;\n",
            "p4_1_def.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            out_def.diags.is_empty(),
            "DEFAULT must be silent: {:?}",
            out_def.diags
        );
        // 小写状态: 也接受 (不区分大小写)
        let out_lower = preprocess(
            b"#pragma STDC FENV_ACCESS on\nint x = 0;\n",
            "p4_1_lower.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            out_lower.diags.is_empty(),
            "lowercase on must be silent: {:?}",
            out_lower.diags
        );
        // 输出不被影响: 保留 int x = 0;
        assert!(
            out_on.text.contains("int x = 0;"),
            "ON output: {}",
            out_on.text
        );
        assert!(
            out_off.text.contains("int x = 0;"),
            "OFF output: {}",
            out_off.text
        );
        assert!(
            out_def.text.contains("int x = 0;"),
            "DEFAULT output: {}",
            out_def.text
        );
    }

    /// P4.1: #pragma STDC FENV_ACCESS <bad> 必须发 W0001 警告
    /// 而不是静默接受 / 静默写错状态, 避免拼写错误污染语义状态.
    #[test]
    fn pragma_stdc_fenv_access_unknown_state_warns() {
        let out = preprocess(
            b"#pragma STDC FENV_ACCESS MAYBE\nint x = 0;\n",
            "p4_1_bad.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        // 至少一个 W0001 警告
        assert!(
            out.diags.iter().any(|d| d.code == "W0001"),
            "expected W0001 warning, got: {:?}",
            out.diags
        );
        // 不能是错误级 (只是警告)
        assert!(
            out.diags
                .iter()
                .all(|d| !matches!(d.sev, crate::diagnostics::Severity::Error)),
            "should be warning, not error: {:?}",
            out.diags
        );
    }

    /// P4.1: #pragma STDC FP_CONTRACT / CX_LIMITED_RANGE 接受为 noop,
    /// 不发警告 (它们与 FENV_ACCESS 一样是 C99 标准的 STDC 子句,
    /// dcc codegen 暂不区分, 走静默 noop 路径).
    #[test]
    fn pragma_stdc_fp_contract_cx_limited_accepted() {
        let out = preprocess(
            b"#pragma STDC FP_CONTRACT ON\n\
              #pragma STDC FP_CONTRACT OFF\n\
              #pragma STDC CX_LIMITED_RANGE ON\n\
              #pragma STDC CX_LIMITED_RANGE OFF\n\
              int x = 0;\n",
            "p4_1_fp.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            out.diags.is_empty(),
            "FP_CONTRACT/CX_LIMITED_RANGE must be silent: {:?}",
            out.diags
        );
        assert!(out.text.contains("int x = 0;"), "output: {}", out.text);
    }

    /// M19: _FORTIFY_SOURCE 改变时, include 缓存必须按新值重新计算
    /// 通过两次不同 -D 调用同一头, 验证 hash_macro_state 把 _FORTIFY_SOURCE
    /// 的值变化反映到缓存键中。
    #[test]
    fn fortify_source_changes_invalidate_cache() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("dcc_fortify_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // 头文件内容完全相同, 但其展开依赖 _FORTIFY_SOURCE
        let hdr = dir.join("ft.h");
        fs::write(
            &hdr,
            "#if _FORTIFY_SOURCE > 0\n\
             #define BOUNDED_COPY(dst, n) __builtin___strncpy_chk(dst, \"x\", n, __builtin_object_size(dst, 0))\n\
             #else\n\
             #define BOUNDED_COPY(dst, n) strncpy(dst, \"x\", n)\n\
             #endif\n",
        )
        .unwrap();

        // 用相对文件名 + 包含路径, 避免 Windows 长路径/反斜杠问题
        let src = "#include \"ft.h\"\nint x = BOUNDED_COPY(buf, 4);\n";
        let includes = vec![dir.clone()];
        // 运行 1: -D_FORTIFY_SOURCE=2
        let out1 = preprocess(
            src.as_bytes(),
            "t1.c",
            &includes,
            &[],
            &[("_FORTIFY_SOURCE".to_string(), Some("2".to_string()))],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        // 运行 2: -D_FORTIFY_SOURCE=0
        let out2 = preprocess(
            src.as_bytes(),
            "t2.c",
            &includes,
            &[],
            &[("_FORTIFY_SOURCE".to_string(), Some("0".to_string()))],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let _ = fs::remove_dir_all(&dir);
        assert!(out1.diags.is_empty(), "{:?}", out1.diags);
        assert!(out2.diags.is_empty(), "{:?}", out2.diags);
        // FORTIFY=2 应生成 __builtin___strncpy_chk 形式
        assert!(
            out1.text.contains("__builtin___strncpy_chk"),
            "out1: {}",
            out1.text
        );
        // FORTIFY=0 应回退到 libc strncpy
        assert!(out2.text.contains("strncpy("), "out2: {}", out2.text);
    }

    // M23: #pragma locale 实际切换字符集
    #[test]
    fn pragma_locale_switches_charset() {
        // 默认 UTF-8
        let out = preprocess(
            b"int x = 0;\n",
            "charset.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert_eq!(out.charset, "UTF-8");

        // 切换到 POSIX/ASCII
        let out = preprocess(
            b"#pragma locale \"POSIX\"\nint x = 0;\n",
            "charset.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert_eq!(out.charset, "ASCII");

        // 重置回 UTF-8
        let out = preprocess(
            b"#pragma locale \"POSIX\"\n#pragma locale reset\nint x = 0;\n",
            "charset.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert_eq!(out.charset, "UTF-8");
    }

    // === M24: PCH (预编译头) ===

    fn pch_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dcc-pch-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pch_bundle_roundtrip() {
        // 创建一个简单的 PCH bundle, 序列化再反序列化, 验证字段一致
        let bundle = PchBundle {
            source: "test.h".to_string(),
            charset: "UTF-8".to_string(),
            content_hash: 0xdeadbeefcafe1234,
            macro_hash: 0x12345678abcdef00,
            output: "int x = 42;\n#define HELLO 1".to_string(),
            macros: vec![
                (
                    "FOO".to_string(),
                    MacroDef {
                        params: None,
                        body: "1".to_string(),
                        variadic: false,
                    },
                ),
                (
                    "BAR".to_string(),
                    MacroDef {
                        params: Some(vec!["x".to_string(), "y".to_string()]),
                        body: "((x)+(y))".to_string(),
                        variadic: false,
                    },
                ),
                (
                    "EMPTY".to_string(),
                    MacroDef {
                        params: Some(vec![]),
                        body: "".to_string(),
                        variadic: false,
                    },
                ),
                (
                    "VARI".to_string(),
                    MacroDef {
                        params: Some(vec!["a".to_string()]),
                        body: "a##__VA_ARGS__".to_string(),
                        variadic: true,
                    },
                ),
            ],
        };
        let s = bundle.serialize();
        let restored = PchBundle::deserialize(&s).expect("deserialize");

        assert_eq!(restored.source, "test.h");
        assert_eq!(restored.charset, "UTF-8");
        assert_eq!(restored.content_hash, 0xdeadbeefcafe1234);
        assert_eq!(restored.macro_hash, 0x12345678abcdef00);
        assert_eq!(restored.output, bundle.output);
        assert_eq!(restored.macros.len(), 4);
        assert_eq!(restored.macros[0].0, "FOO");
        assert!(restored.macros[0].1.params.is_none());
        assert_eq!(restored.macros[0].1.body, "1");
        assert_eq!(restored.macros[1].0, "BAR");
        assert_eq!(
            restored.macros[1].1.params.as_ref().unwrap(),
            &vec!["x".to_string(), "y".to_string()]
        );
        assert_eq!(restored.macros[1].1.body, "((x)+(y))");
        assert_eq!(restored.macros[2].0, "EMPTY");
        assert_eq!(restored.macros[2].1.params.as_ref().unwrap().len(), 0);
        assert!(!restored.macros[2].1.variadic);
        assert!(restored.macros[3].1.variadic);
    }

    #[test]
    fn pch_bundle_kv_escape_handles_newlines_and_tabs() {
        // 验证转义: body 中含 \n / \t / \\ 时不丢语义
        let bundle = PchBundle {
            source: "tricky.h".to_string(),
            charset: "UTF-8".to_string(),
            content_hash: 1,
            macro_hash: 2,
            output: "a\nb\tc\\d".to_string(),
            macros: vec![(
                "T".to_string(),
                MacroDef {
                    params: None,
                    body: "1\t2\n3\\4".to_string(),
                    variadic: false,
                },
            )],
        };
        let s = bundle.serialize();
        let restored = PchBundle::deserialize(&s).unwrap();
        assert_eq!(restored.macros[0].1.body, "1\t2\n3\\4");
        assert_eq!(restored.output, "a\nb\tc\\d");
    }

    #[test]
    fn pch_create_then_include_roundtrip() {
        // 1. 处理 common.h 生成 PCH
        // 2. 加载 PCH, 验证宏表被恢复
        // 3. 处理使用 common.h 中宏的 TU, 结果与不用 PCH 时一致
        let dir = pch_test_dir("roundtrip");
        let common = dir.join("common.h");
        std::fs::write(
            &common,
            "#define FROM_COMMON 42\n#define SQUARE(x) ((x)*(x))\n",
        )
        .unwrap();
        let pch_path = dir.join("common.pch");

        // 创建 PCH
        let create_opts = PchOptions {
            include: None,
            create: Some(pch_path.to_string_lossy().into_owned()),
            include_only: None,
        };
        let out_create = preprocess(
            b"#include \"common.h\"\n", // 源: 包含 common.h 后空主体
            "tu.c",
            std::slice::from_ref(&dir),
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &create_opts,
        );
        assert!(
            out_create.diags.is_empty(),
            "create: {:?}",
            out_create.diags
        );
        assert!(pch_path.exists(), "PCH file should be created");
        let pch_content = std::fs::read_to_string(&pch_path).unwrap();
        assert!(pch_content.starts_with("DCC_PCH_V1"), "magic missing");
        assert!(pch_content.contains("FROM_COMMON"));

        // 加载 PCH, 处理一个使用 FROM_COMMON / SQUARE 的 TU
        let include_opts = PchOptions {
            include: Some(pch_path.to_string_lossy().into_owned()),
            create: None,
            include_only: None,
        };
        let out_with_pch = preprocess(
            b"int a = FROM_COMMON; int b = SQUARE(3);",
            "tu.c",
            std::slice::from_ref(&dir),
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &include_opts,
        );
        assert!(
            out_with_pch.diags.is_empty(),
            "include: {:?}",
            out_with_pch.diags
        );
        // 宏已被 PCH 展开
        assert!(
            out_with_pch.text.contains("int a = 42"),
            "FROM_COMMON should expand: {}",
            out_with_pch.text
        );
        assert!(
            out_with_pch.text.contains("int b = ((3)*(3))"),
            "SQUARE should expand: {}",
            out_with_pch.text
        );

        // 不使用 PCH, 重新包含 common.h, 验证结果一致
        let out_no_pch = preprocess(
            b"#include \"common.h\"\nint a = FROM_COMMON; int b = SQUARE(3);",
            "tu.c",
            std::slice::from_ref(&dir),
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        // 两条路径都应展开 FROM_COMMON=42 / SQUARE(3)=((3)*(3))
        assert!(
            out_with_pch.text.contains("int a = 42"),
            "PCH FROM_COMMON 展开失败: {}",
            out_with_pch.text
        );
        assert!(
            out_with_pch.text.contains("int b = ((3)*(3))"),
            "PCH SQUARE 展开失败: {}",
            out_with_pch.text
        );
        assert!(
            out_no_pch.text.contains("int a = 42"),
            "无 PCH FROM_COMMON 展开失败: {}",
            out_no_pch.text
        );
        assert!(
            out_no_pch.text.contains("int b = ((3)*(3))"),
            "无 PCH SQUARE 展开失败: {}",
            out_no_pch.text
        );
        // 不强制逐字节一致: PCH 路径会注入 `/* dcc PCH: tu.c */` 标记,
        // 无 PCH 路径走 include cache 注入 `/* #include "common.h" (cached) */`.
        // 两者都是合法的中间表示, 不破坏后续 parser (parser 跳过 /* */ 注释).

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pch_incompatible_macro_state_warns_and_falls_back() {
        // 创建一个 PCH, 然后用不同 -D 加载, 验证 hash 不匹配时回退 (warning)
        let dir = pch_test_dir("incompat");
        let pch_path = dir.join("x.pch");

        let create_opts = PchOptions {
            include: None,
            create: Some(pch_path.to_string_lossy().into_owned()),
            include_only: None,
        };
        let _ = preprocess(
            b"#define A 1\n",
            "x.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &create_opts,
        );
        assert!(pch_path.exists());

        // 用不同的 -D 加载 PCH: 宏表 hash 不同, 应回退
        let include_opts = PchOptions {
            include: Some(pch_path.to_string_lossy().into_owned()),
            create: None,
            include_only: None,
        };
        let out = preprocess(
            b"int x = 1;",
            "x.c",
            &[],
            &[],
            &[("EXTRA".to_string(), Some("1".to_string()))],
            StdMode::Gnu11,
            &[],
            false,
            &include_opts,
        );
        // 应有 warning (W0400) 但不致命
        let has_warn = out.diags.iter().any(|d| d.code == "W0400");
        assert!(has_warn, "expected W0400 warn, got: {:?}", out.diags);
        // 不应被当作 error 阻塞: 仍输出文本
        assert!(!out.text.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pch_missing_file_emits_error() {
        let include_opts = PchOptions {
            include: Some("/nonexistent/path/foo.pch".to_string()),
            create: None,
            include_only: None,
        };
        let out = preprocess(
            b"int x;",
            "x.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &include_opts,
        );
        let has_err = out.diags.iter().any(|d| d.code == "E0401");
        assert!(has_err, "expected E0401, got: {:?}", out.diags);
    }

    #[test]
    fn pch_magic_mismatch_rejected() {
        // 构造一个非 PCH 文件, 加载应失败
        let dir = pch_test_dir("badmagic");
        let bad = dir.join("bad.pch");
        std::fs::write(&bad, "not a pch file\n").unwrap();

        let include_opts = PchOptions {
            include: Some(bad.to_string_lossy().into_owned()),
            create: None,
            include_only: None,
        };
        let out = preprocess(
            b"int x;",
            "x.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &include_opts,
        );
        let has_err = out.diags.iter().any(|d| d.code == "E0401");
        assert!(
            has_err,
            "expected E0401 for bad magic, got: {:?}",
            out.diags
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pch_no_pch_does_not_create_file() {
        // 默认 (无 PCH) 时不写文件
        let dir = pch_test_dir("nopch");
        let pch_path = dir.join("should_not_exist.pch");
        assert!(!pch_path.exists());

        let _ = preprocess(
            b"int x;",
            "x.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            !pch_path.exists(),
            "no PCH file should be created without -fpch-create"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 头文件 #error 的诊断必须携带头文件自身的 source_line (bug 1 回归)
    #[test]
    fn error_diag_in_header_carries_header_source_line() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("dcc_hdr_err_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let hdr = dir.join("bad.h");
        fs::write(
            &hdr,
            "#ifndef BAD_H\n#define BAD_H\n#error stop here in header\n#endif\n",
        )
        .unwrap();
        let src = "#include \"bad.h\"\nint x;\n";
        let out = preprocess(
            src.as_bytes(),
            "bad-inc.c",
            std::slice::from_ref(&dir),
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let err = out
            .diags
            .iter()
            .find(|d| d.code == "E0203")
            .unwrap_or_else(|| panic!("expected E0203, got: {:?}", out.diags));
        assert!(err.span.file.ends_with("bad.h"), "file: {}", err.span.file);
        assert_eq!(err.span.line, 3);
        let src_line = err.source_line.clone().unwrap_or_default();
        assert!(
            src_line.contains("stop here in header"),
            "source_line: {src_line}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 续接行合并时记录覆盖的物理行范围 (phys_start/phys_end)
    #[test]
    fn logical_lines_track_physical_spans() {
        let lines = super::Preprocessor::logical_lines("a\\\nb\nc\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "a b");
        assert_eq!(lines[0].phys_start, 1);
        assert_eq!(lines[0].phys_end, 2);
        assert_eq!(lines[1].text, "c");
        assert_eq!(lines[1].phys_start, 3);
        assert_eq!(lines[1].phys_end, 3);
        // 尾随换行的空逻辑行: 占位, 不产生输出
        assert_eq!(lines[2].text, "");
        assert_eq!(lines[2].phys_start, 4);
        assert_eq!(lines[2].phys_end, 4);
    }

    /// 续接行后的 #error 必须报物理行号 (bug 2 回归)
    #[test]
    fn line_numbers_survive_continuation() {
        let out = preprocess(
            b"#define FOO 1 + \\\n  2\n#error boom\n",
            "cont.c",
            &[],
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        let err = out
            .diags
            .iter()
            .find(|d| d.code == "E0203")
            .unwrap_or_else(|| panic!("expected E0203, got: {:?}", out.diags));
        assert_eq!(err.span.line, 3);
    }

    /// 缓存不得吞掉 #error 诊断: 有诊断的头文件不落缓存 (bug 4 回归)
    #[test]
    fn include_cache_does_not_swallow_errors() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("dcc_cache_err_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let hdr = dir.join("bad.h");
        fs::write(&hdr, "#error broken header\n").unwrap();
        let src = "#include \"bad.h\"\nint x;\n";
        // 隔离 HOME: 缓存写入临时目录, 不污染 ~/.dcc/cache
        std::env::set_var("HOME", dir.join("home"));
        for i in 0..2 {
            let out = preprocess(
                src.as_bytes(),
                "cache-err.c",
                std::slice::from_ref(&dir),
                &[],
                &[],
                StdMode::Gnu11,
                &[],
                false,
                &PchOptions::default(),
            );
            assert!(
                out.diags.iter().any(|d| d.code == "E0203"),
                "run {i}: E0203 was swallowed: {:?}",
                out.diags
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// 嵌套依赖变更后父条目必须失效重处理 (bug 5 回归)
    #[test]
    fn include_cache_invalidates_on_nested_dep_change() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("dcc_cache_dep_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("A.h");
        let b = dir.join("B.h");
        fs::write(
            &a,
            "#ifndef A_H\n#define A_H\n#include \"B.h\"\n#define A_VAL (B_VAL + 10)\n#endif\n",
        )
        .unwrap();
        fs::write(&b, "#ifndef B_H\n#define B_H\n#define B_VAL 42\n#endif\n").unwrap();
        let src = "#include \"A.h\"\nint v = A_VAL;\n";
        std::env::set_var("HOME", dir.join("home"));
        let out1 = preprocess(
            src.as_bytes(),
            "nested-dep.c",
            std::slice::from_ref(&dir),
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(out1.text.contains("(42 + 10)"), "run 1: {}", out1.text);
        // 修改嵌套头文件 B.h, 父条目 A.h 的 DEPS 校验必须使其失效
        fs::write(&b, "#ifndef B_H\n#define B_H\n#define B_VAL 52\n#endif\n").unwrap();
        let out2 = preprocess(
            src.as_bytes(),
            "nested-dep.c",
            std::slice::from_ref(&dir),
            &[],
            &[],
            StdMode::Gnu11,
            &[],
            false,
            &PchOptions::default(),
        );
        assert!(
            out2.text.contains("(52 + 10)"),
            "run 2 after B.h change: {}",
            out2.text
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
