// src/preproc.rs - C 预处理器
// 第一性原理: 预处理 = 文本变换 + 宏展开。在词法分析之前运行。
// suckless 准则: 行级处理 #include/#define/#if, 然后 token 级宏展开。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::fs;
use std::process::Command;
use std::sync::OnceLock;
use crate::diagnostics::{Diagnostic, Span};
use crate::cli::StdMode;

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
    /// 当前文件名 (用于诊断 + __FILE__)
    file: String,
    /// 搜索路径基准目录
    base_dir: PathBuf,
    /// 当前行号 (用于 __LINE__)
    cur_line: u32,
    /// 编译日期/时间 (用于 __DATE__ / __TIME__)
    compile_date: String,
    compile_time: String,
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
    pending_pragmas: RefCell<Vec<String>>,
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

#[derive(Clone)]
struct MacroDef {
    params: Option<Vec<String>>,  // None = object-like, Some = function-like
    body: String,
    /// 可变参数: true 表示末尾有 ...
    variadic: bool,
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
}

pub fn preprocess(src: &[u8], file: &str, include_paths: &[PathBuf],
                  system_paths: &[PathBuf],
                  defines: &[(String, Option<String>)],
                  std_mode: StdMode,
                  force_includes: &[String],
                  freestanding: bool) -> PreprocessOutput {
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
    let base = Path::new(file).parent().unwrap_or(Path::new(".")).to_path_buf();
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
        pending_pragmas: RefCell::new(Vec::new()),
    };
    // 预定义宏
    pp.define_builtin("__DCC__", "1");
    pp.define_builtin("__x86_64__", "1");
    pp.define_builtin("__linux__", "1");
    pp.define_builtin("__LP64__", "1");
    pp.define_builtin("__STDC__", "1");
    // __STDC_VERSION__ 根据选择的 C 标准设置
    let stc_version = match std_mode {
        StdMode::C89 | StdMode::Gnu89 => None,  // C89/C90 不定义 __STDC_VERSION__
        StdMode::C99 | StdMode::Gnu99 => Some("199901"),
        StdMode::C11 | StdMode::Gnu11 => Some("201112"),
        StdMode::C17 | StdMode::Gnu17 => Some("201710"),
        StdMode::C23 | StdMode::Gnu23 => Some("202311"),
    };
    if let Some(v) = stc_version {
        pp.define_builtin("__STDC_VERSION__", v);
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
    pp.macros.insert("kill_dependency".to_string(), MacroDef {
        params: Some(vec!["y".to_string()]), body: "(y)".to_string(), variadic: false,
    });
    // atomic_thread_fence / atomic_signal_fence: 编译屏障 (代码生成忽略)
    pp.macros.insert("atomic_thread_fence".to_string(), MacroDef {
        params: Some(vec!["order".to_string()]), body: "".to_string(), variadic: false,
    });
    pp.macros.insert("atomic_signal_fence".to_string(), MacroDef {
        params: Some(vec!["order".to_string()]), body: "".to_string(), variadic: false,
    });
    // 注意: 不预置 atomic_store/atomic_fetch_add 等 C11 stdatomic 宏。
    // Linux 内核 atomic-instrumented.h 等同名 inline 函数会被误展开
    // （如 atomic_fetch_add(i,v) → __atomic_fetch_add(i,v,5) 破坏声明）。
    // __atomic_* 内建与 __ATOMIC_* 常量仍保留，供显式调用。
    // hosted 模式（非 -ffreestanding）预置常用 C11 atomic 宏：
    if !freestanding {
        let atomic_macros: &[(&str, &[&str], &str)] = &[
            ("atomic_store", &["obj", "val"], "__atomic_store_n(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_load", &["obj"], "__atomic_load_n(obj, __ATOMIC_SEQ_CST)"),
            ("atomic_exchange", &["obj", "val"], "__atomic_exchange_n(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_fetch_add", &["obj", "val"], "__atomic_fetch_add(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_fetch_sub", &["obj", "val"], "__atomic_fetch_sub(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_fetch_and", &["obj", "val"], "__atomic_fetch_and(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_fetch_or", &["obj", "val"], "__atomic_fetch_or(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_fetch_xor", &["obj", "val"], "__atomic_fetch_xor(obj, val, __ATOMIC_SEQ_CST)"),
            ("atomic_compare_exchange_strong", &["obj", "expected", "desired"],
                "__atomic_compare_exchange_n(obj, expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)"),
        ];
        for (name, params, body) in atomic_macros {
            pp.macros.insert(name.to_string(), MacroDef {
                params: Some(params.iter().map(|s| s.to_string()).collect()),
                body: body.to_string(),
                variadic: false,
            });
        }
    }
    // 内存序别名 (memory_order_xxx 已在上面定义)
    // __attribute__: 不定义为宏, 让 token 透传到解析器处理
    // 解析器会识别 __attribute__ 并处理 vector_size 等属性
    // __has_attribute / __has_builtin / __has_feature: 不在宏表中定义, 在展开时特殊处理
    // 这样可以在宏展开路径和 #if 路径都返回正确值
    // likely/unlikely 分支提示
    pp.macros.insert("likely".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "(x)".to_string(), variadic: false,
    });
    pp.macros.insert("unlikely".to_string(), MacroDef {
        params: Some(vec!["x".to_string()]), body: "(x)".to_string(), variadic: false,
    });
    // barrier
    pp.macros.insert("barrier".to_string(), MacroDef {
        params: Some(vec![]), body: "".to_string(), variadic: false,
    });
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
    pp.macros.insert("DEFINE_PER_CPU".to_string(), MacroDef {
        params: Some(vec!["type".to_string(), "name".to_string()]),
        body: "__attribute__((section(\".data..percpu\"))) type name".to_string(),
        variadic: false,
    });
    pp.macros.insert("DEFINE_PER_CPU_SHARED_ALIGNED".to_string(), MacroDef {
        params: Some(vec!["type".to_string(), "name".to_string()]),
        body: "__attribute__((section(\".data..percpu\"), __aligned__(64))) type name".to_string(),
        variadic: false,
    });
    pp.macros.insert("DECLARE_PER_CPU".to_string(), MacroDef {
        params: Some(vec!["type".to_string(), "name".to_string()]),
        body: "extern __attribute__((section(\".data..percpu\"))) type name".to_string(),
        variadic: false,
    });
    // 用户 -D 定义
    for (name, val) in defines {
        let v = val.as_deref().unwrap_or("1");
        pp.define_builtin(name, v);
    }

    // 处理 -include 强制包含文件
    for fi in force_includes {
        pp.force_include(fi);
    }

    pp.process_text(&text, file);
    PreprocessOutput {
        text: pp.out,
        diags: pp.diags,
        deps: pp.deps,
        pack_history: pp.pack_history,
        weak_symbols: pp.weak_symbols,
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
                    if p.is_dir() { dirs.push(p); }
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
                if p.exists() { found = Some(p); break; }
            }
            if found.is_none() {
                for dir in &self.system_paths {
                    let p = dir.join(file_path);
                    if p.exists() { found = Some(p); break; }
                }
            }
            found.unwrap_or(path)
        };
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
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
        self.macros.insert(name.to_string(), MacroDef {
            params: None, body: val.to_string(), variadic: false,
        });
    }

    fn compile_datetime() -> (String, String) {
        // 简化: 使用固定格式, 实际项目可用 chrono
        let date = "Jul  9 2026";
        let time = "00:00:00";
        (date.to_string(), time.to_string())
    }

    fn process_text(&mut self, text: &str, file: &str) {
        let saved_file = self.file.clone();
        let saved_in_comment = self.in_comment;
        self.file = file.to_string();
        // Each included source file undergoes translation phase 3 independently.
        // A malformed comment must not consume text from its includer.
        self.in_comment = false;

        // 行续接: 合并以 \ 结尾的行
        let joined = Self::join_continued_lines(text);

        let mut active_source = String::new();
        for (line_no, line) in joined.lines().enumerate() {
            let line_no = (line_no + 1) as u32;
            self.cur_line = line_no;
            // C translation phase 3 removes comments before preprocessing
            // directives are recognized and before macro expansion.  Doing
            // this afterwards lets quotes in comments alter lexical state.
            let phase3 = self.strip_comments_line(line);
            let trimmed = phase3.trim();

            if trimmed.starts_with('#') {
                if !active_source.is_empty() {
                    self.flush_active_source(&mut active_source);
                }
                self.process_directive(trimmed, line_no);
            } else if self.is_active() {
                active_source.push_str(&phase3);
                active_source.push('\n');
            }
        }
        if !active_source.is_empty() {
            self.flush_active_source(&mut active_source);
        }

        if self.in_comment {
            self.diags.push(Diagnostic::err(
                "E0207",
                "unterminated block comment",
                Span::point(&self.file, self.cur_line.max(1), 1),
            ));
        }
        self.in_comment = saved_in_comment;
        self.file = saved_file;
    }

    /// 对累积的活跃源码块做宏展开并写入输出
    fn flush_active_source(&mut self, src: &mut String) {
        let expanded = self.expand_macros(src);
        if !expanded.trim().is_empty() {
            self.out.push_str(&expanded);
            if !expanded.ends_with('\n') {
                self.out.push('\n');
            }
        }
        // 把展开阶段累积的 _Pragma 字符串批量应用
        // (顺序与源中出现顺序一致, 即 _Pragma(...) 在某一行后, 立即生效)
        let pending: Vec<String> = std::mem::take(&mut *self.pending_pragmas.borrow_mut());
        for p in pending {
            self.apply_pragma_string(&p, self.cur_line);
        }
        src.clear();
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
                if chars[i] == '*' && i + 1 < len && chars[i+1] == '/' {
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
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('"'); i += 1; }
                continue;
            }
            // 字符字面量
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('\''); i += 1; }
                continue;
            }
            // /* ... */ 注释 (支持跨行)
            if i + 1 < len && chars[i] == '/' && chars[i+1] == '*' {
                out.push(' ');
                i += 2;
                self.in_comment = true;
                while i + 1 < len {
                    if chars[i] == '*' && chars[i+1] == '/' {
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
            if i + 1 < len && chars[i] == '/' && chars[i+1] == '/' {
                out.push(' ');
                while i < len && chars[i] != '\n' { i += 1; }
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
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('"'); i += 1; }
                continue;
            }
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('\''); i += 1; }
                continue;
            }
            if i + 1 < len && chars[i] == '/' && chars[i+1] == '*' {
                out.push(' ');
                i += 2;
                while i + 1 < len && !(chars[i] == '*' && chars[i+1] == '/') { i += 1; }
                if i + 1 < len { i += 2; }
                continue;
            }
            if i + 1 < len && chars[i] == '/' && chars[i+1] == '/' {
                out.push(' ');
                while i < len && chars[i] != '\n' { i += 1; }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// 行续接: 将行尾 \ 与下一行合并
    fn join_continued_lines(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut continued = String::new();
        for line in text.lines() {
            if line.ends_with('\\') {
                continued.push_str(&line[..line.len()-1]);
                continued.push(' '); // \ 替换为空格
            } else {
                if !continued.is_empty() {
                    continued.push_str(line);
                    result.push_str(&continued);
                    result.push('\n');
                    continued.clear();
                } else {
                    result.push_str(line);
                    result.push('\n');
                }
            }
        }
        if !continued.is_empty() { result.push_str(&continued); result.push('\n'); }
        result
    }

    fn process_directive(&mut self, line: &str, line_no: u32) {
        let after_hash = line[1..].trim_start();
        // 处理行续接 (\ at end)
        let directive = after_hash;

        if directive.starts_with("include_next") {
            // 必须在 "include" 之前匹配，否则 include_next 会被当成 include
            if self.is_active() {
                self.do_include_next(&directive["include_next".len()..].trim(), line_no);
            }
        } else if directive.starts_with("include") {
            if self.is_active() {
                self.do_include(&directive[7..].trim(), line_no);
            }
        } else if directive.starts_with("define") {
            if self.is_active() {
                self.do_define(&directive[6..].trim());
            }
        } else if directive.starts_with("undef") {
            if self.is_active() {
                let name = directive[5..].trim();
                self.macros.remove(name);
            }
        } else if directive.starts_with("ifdef") {
            let name = directive[5..].trim();
            let active = self.is_active() && self.macros.contains_key(name);
            self.cond_stack.push(CondState {
                any_true: active, active, saw_else: false,
            });
        } else if directive.starts_with("ifndef") {
            let name = directive[6..].trim();
            let active = self.is_active() && !self.macros.contains_key(name);
            self.cond_stack.push(CondState {
                any_true: active, active, saw_else: false,
            });
        } else if directive.starts_with("if ") || directive.starts_with("if\t") {
            let expr = directive[2..].trim();
            let val = if self.is_active() { self.eval_const_expr(expr) } else { false };
            self.cond_stack.push(CondState {
                any_true: val, active: val, saw_else: false,
            });
        } else if directive.starts_with("elif ") || directive.starts_with("elif\t") {
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 { true }
                    else { self.cond_stack[..len-1].iter().all(|c| c.active) };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags.push(Diagnostic::err("E0200", "#elif after #else",
                        Span::point(&self.file, line_no, 1)));
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
                    if val { state.any_true = true; }
                    state.active = val;
                }
            }
        } else if directive.starts_with("elifdef ") || directive.starts_with("elifdef\t") {
            let name = directive[8..].trim();
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 { true }
                    else { self.cond_stack[..len-1].iter().all(|c| c.active) };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags.push(Diagnostic::err("E0200", "#elifdef after #else",
                        Span::point(&self.file, line_no, 1)));
                    return;
                }
                let state = &mut self.cond_stack[len - 1];
                if any_true {
                    state.active = false;
                } else {
                    let val = parent_active && self.macros.contains_key(name);
                    let state = &mut self.cond_stack[len - 1];
                    if val { state.any_true = true; }
                    state.active = val;
                }
            }
        } else if directive.starts_with("elifndef ") || directive.starts_with("elifndef\t") {
            let name = directive[9..].trim();
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 { true }
                    else { self.cond_stack[..len-1].iter().all(|c| c.active) };
                let saw_else = self.cond_stack[len - 1].saw_else;
                let any_true = self.cond_stack[len - 1].any_true;
                if saw_else {
                    self.diags.push(Diagnostic::err("E0200", "#elifndef after #else",
                        Span::point(&self.file, line_no, 1)));
                    return;
                }
                let state = &mut self.cond_stack[len - 1];
                if any_true {
                    state.active = false;
                } else {
                    let val = parent_active && !self.macros.contains_key(name);
                    let state = &mut self.cond_stack[len - 1];
                    if val { state.any_true = true; }
                    state.active = val;
                }
            }
        } else if directive.starts_with("else") {
            let len = self.cond_stack.len();
            if len > 0 {
                let parent_active = if len <= 1 { true }
                    else { self.cond_stack[..len-1].iter().all(|c| c.active) };
                let state = &mut self.cond_stack[len - 1];
                if state.saw_else {
                    self.diags.push(Diagnostic::err("E0201", "duplicate #else",
                        Span::point(&self.file, line_no, 1)));
                    return;
                }
                state.saw_else = true;
                state.active = parent_active && !state.any_true;
                if state.active { state.any_true = true; }
            }
        } else if directive.starts_with("endif") {
            if self.cond_stack.pop().is_none() {
                self.diags.push(Diagnostic::err("E0202", "#endif without #if",
                    Span::point(&self.file, line_no, 1)));
            }
        } else if directive.starts_with("pragma") {
            let pragma_text = directive[6..].trim();
            if pragma_text == "once" {
                // 记录当前文件为 pragma once
                let canonical = Path::new(&self.file).canonicalize()
                    .unwrap_or_else(|_| Path::new(&self.file).to_path_buf());
                self.pragma_once.insert(canonical.to_string_lossy().to_string());
            } else if pragma_text.starts_with("GCC diagnostic") {
                self.handle_gcc_diagnostic(&pragma_text[14..].trim(), line_no);
            } else if pragma_text.starts_with("pack") {
                // #pragma pack() / pack(1) / pack(2) / pack(4) / pack(8) / pack(16)
                // #pragma pack(push[, n]) / #pragma pack(pop[, n])
                // #pragma pack(push) / #pragma pack(pop)
                self.handle_pragma_pack(&pragma_text[4..].trim());
            } else if pragma_text.starts_with("weak") {
                // #pragma weak name
                // 记录弱符号名 (后续在 codegen 中以 weak 链接生成)
                let rest = pragma_text[4..].trim();
                if !rest.is_empty() {
                    // 支持 #pragma weak name 或 #pragma weak(name)
                    let name = rest.trim_start_matches('(').trim_end_matches(')').trim();
                    // 可能有别名: #pragma weak name = other
                    let name = name.split('=').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        self.weak_symbols.insert(name.to_string());
                    }
                }
            } else if pragma_text.starts_with("STDC") {
                // #pragma STDC FENV_ACCESS ON/OFF/DEFAULT
                // #pragma STDC FP_CONTRACT ON/OFF/DEFAULT
                // #pragma STDC CX_LIMITED_RANGE ON/OFF/DEFAULT
                let rest = pragma_text[4..].trim();
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() >= 2 {
                    let kind = parts[0];
                    let state = parts[1].to_uppercase();
                    if kind.eq_ignore_ascii_case("FENV_ACCESS") {
                        self.fenv_access = matches!(state.as_str(), "ON" | "DEFAULT");
                    }
                    // 其他 STDC 子句忽略, 仅用于兼容
                }
            } else if pragma_text.starts_with("comment") {
                // #pragma comment(...) — 链接器注释, 忽略
            }
            // 其他 #pragma 忽略
        } else if directive.starts_with("error") {
            if self.is_active() {
                let msg = directive[5..].trim();
                self.diags.push(Diagnostic::err("E0203", format!("#error {}", msg),
                    Span::point(&self.file, line_no, 1)));
            }
        } else if directive.starts_with("warning") {
            if self.is_active() {
                let msg = directive[7..].trim();
                self.diags.push(Diagnostic::warn("W0200", format!("#warning {}", msg),
                    Span::point(&self.file, line_no, 1)));
            }
        } else if directive.starts_with("embed") {
            if self.is_active() {
                self.do_embed(&directive[5..].trim(), line_no);
            }
        }
        // 忽略未知指令 (line markers 等)
    }

    fn do_define(&mut self, rest: &str) {
        // #define NAME body  或  #define NAME(args, ...) body
        let rest = rest.trim_start();
        // 提取宏名
        let name_end = rest.find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if name_end == 0 { return; }
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
                if s == "..." { variadic = true; }
                else if s.starts_with("...") {
                    // ...name 形式
                    variadic = true;
                    let va_name = &s[3..];
                    if !va_name.is_empty() { params.push(va_name.to_string()); }
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
            let body = after_name[paren_end+1..].trim().to_string();
            self.macros.insert(name.to_string(), MacroDef {
                params: Some(params), body, variadic,
            });
        } else {
            // object-like macro
            let body = after_name.trim().to_string();
            self.macros.insert(name.to_string(), MacroDef {
                params: None, body, variadic: false,
            });
        }
    }

    fn needs_linux_stddef_bool_fixup(path: &str) -> bool {
        path.ends_with("linux/stddef.h")
    }

    /// GCC 扩展：`#include /absolute/path`（无引号）。
    fn is_absolute_include_path(s: &str) -> bool {
        let s = s.trim();
        s.starts_with('/')
            || s.starts_with('\\')
            || (s.len() >= 2 && s.as_bytes()[1] == b':')
    }

    /// 从 `#include` 行剩余部分提取路径（截断空白与 `//` 注释）。
    fn parse_include_path_token(rest: &str) -> &str {
        let rest = rest.trim();
        let end = rest.find("//").unwrap_or(rest.len());
        rest[..end].trim().split_whitespace().next().unwrap_or(rest)
    }

    fn do_include(&mut self, rest: &str, line_no: u32) {
        let rest = rest.trim();
        let (path, is_system) = if rest.starts_with('"') {
            let end = rest[1..].find('"').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], false)
        } else if rest.starts_with('<') {
            let end = rest[1..].find('>').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], true)
        } else if Self::is_absolute_include_path(rest) {
            (Self::parse_include_path_token(rest), false)
        } else {
            self.diags.push(Diagnostic::err("E0204", "expected \"file\" or <file>",
                Span::point(&self.file, line_no, 1)));
            return;
        };

        // 搜索文件
        let found = self.find_include(path, is_system);
        match found {
            Some(full_path) => self.include_resolved_file(full_path, path, line_no),
            None => {
                self.diags.push(Diagnostic::err("E0206",
                    format!("include file not found: {}", path),
                    Span::point(&self.file, line_no, 1)));
            }
        }
    }

    /// `#include_next`：从当前头文件所在目录之后继续搜索。
    fn do_include_next(&mut self, rest: &str, line_no: u32) {
        let rest = rest.trim();
        let (path, is_system) = if rest.starts_with('"') {
            let end = rest[1..].find('"').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], false)
        } else if rest.starts_with('<') {
            let end = rest[1..].find('>').map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], true)
        } else if Self::is_absolute_include_path(rest) {
            (Self::parse_include_path_token(rest), false)
        } else {
            self.diags.push(Diagnostic::err("E0204", "expected \"file\" or <file>",
                Span::point(&self.file, line_no, 1)));
            return;
        };

        match self.find_include_next(path, is_system) {
            Some(full_path) => self.include_resolved_file(full_path, path, line_no),
            None => {
                self.diags.push(Diagnostic::err("E0206",
                    format!("include_next file not found: {}", path),
                    Span::point(&self.file, line_no, 1)));
            }
        }
    }

    /// 将已解析到的头文件内容并入预处理输出。
    fn include_resolved_file(&mut self, full_path: PathBuf, path: &str, line_no: u32) {
                let canonical = full_path.canonicalize()
                    .unwrap_or(full_path.clone());
                let key = canonical.to_string_lossy().to_string();
                if self.pragma_once.contains(&key) { return; }
                self.deps.push(full_path.to_string_lossy().into_owned());
                match fs::read_to_string(&full_path) {
                    Ok(text) => {
                        // 全局类型注册表: 检查缓存
                        let content_hash = Self::hash_content(&text);
                        let macro_hash = self.hash_macro_state();
                        let cache_path = self.get_cache_path(&canonical, content_hash, macro_hash);

                        let bool_fixup = Self::needs_linux_stddef_bool_fixup(path);
                        // linux/stddef.h 定义 enum { false = 0, true = 1 }；若 stdbool.h
                        // 已将 false/true 宏化，预处理会把它变成非法的 0 = 0。
                        let skip_cache = bool_fixup
                            && (self.macros.contains_key("false") || self.macros.contains_key("true"));

                        if let Some(ref cp) = cache_path {
                            if !skip_cache {
                            if let Some((cached_output, cached_macros)) = self.load_from_cache(cp) {
                                // 缓存命中: 直接使用缓存的预处理输出和宏
                                self.out.push_str(&format!("/* #include \"{}\" (cached) */\n", path));
                                self.deps.push(full_path.to_string_lossy().into_owned());
                                self.out.push_str(&cached_output);
                                for (name, def) in cached_macros {
                                    self.macros.insert(name, def);
                                }
                                return;
                            }
                            }
                        }

                        // 缓存未命中: 正常处理
                        self.out.push_str(&format!("/* #include \"{}\" */\n", path));
                        let saved_base = self.base_dir.clone();
                        let saved_out_len = self.out.len();
                        // 记录当前宏表快照, 用于后续提取该头文件新增的宏
                        let saved_macros: std::collections::HashSet<String> = self.macros.keys().cloned().collect();
                        let saved_false = if bool_fixup { self.macros.remove("false") } else { None };
                        let saved_true = if bool_fixup { self.macros.remove("true") } else { None };
                        self.base_dir = full_path.parent()
                            .unwrap_or(Path::new(".")).to_path_buf();
                        self.process_text(&text, &full_path.to_string_lossy());
                        if let Some(def) = saved_false {
                            self.macros.insert("false".to_string(), def);
                        }
                        if let Some(def) = saved_true {
                            self.macros.insert("true".to_string(), def);
                        }
                        self.base_dir = saved_base;

                        // 保存到缓存 (仅保存该头文件新增的宏, 避免冗余和状态污染)
                        if let Some(ref cp) = cache_path {
                            if !skip_cache {
                            let new_output = self.out[saved_out_len..].to_string();
                            let header_macros: HashMap<String, MacroDef> = self.macros.iter()
                                .filter(|(k, _)| !saved_macros.contains(*k))
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect();
                            self.save_to_cache(cp, &new_output, &header_macros);
                            }
                        }
                    }
                    Err(e) => {
                        self.diags.push(Diagnostic::err("E0205",
                            format!("cannot read {}: {}", path, e),
                            Span::point(&self.file, line_no, 1)));
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
        for d in ["/usr/include/linux", "/usr/include/asm", "/usr/include/asm-generic"] {
            dirs.push(PathBuf::from(d));
        }
        dirs
    }

    /// `#include_next`：跳过当前文件所在目录及之前的搜索路径。
    fn find_include_next(&self, name: &str, is_system: bool) -> Option<PathBuf> {
        let name = name.trim();
        let dirs = self.include_search_dirs(is_system);
        let cur = self.base_dir.canonicalize().unwrap_or_else(|_| self.base_dir.clone());

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
            if p.exists() { return Some(p); }
        }
        // 2. 用户 -I 路径
        for dir in &self.include_paths {
            let p = dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 3. 系统 -isystem 路径
        for dir in &self.system_paths {
            let p = dir.join(name);
            if p.exists() { return Some(p); }
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
            if p.exists() { return Some(p); }
        }
        // 5. GCC 内建 include（stddef.h/stdbool.h 等；优先于 linux UAPI 同名头）
        for dir in host_gcc_include_dirs() {
            let p = dir.join(name);
            if p.exists() { return Some(p); }
        }
        // 6. Linux 内核 UAPI 路径（仅当标准/GCC 头不存在时）
        let kernel_dirs = [
            "/usr/include/linux",
            "/usr/include/asm",
            "/usr/include/asm-generic",
        ];
        for dir in &kernel_dirs {
            let p = Path::new(dir).join(name);
            if p.exists() { return Some(p); }
        }
        None
    }

    /// C23 #embed: 嵌入文件内容为逗号分隔的整数字节值
    fn do_embed(&mut self, args: &str, line_no: u32) {
        let args = args.trim();
        // 提取文件名: 以 "file" 或 <file> 开头
        let (path, rest) = if args.starts_with('"') {
            let end = args[1..].find('"').map(|i| i + 1).unwrap_or(args.len());
            (&args[1..end], args[end+1..].trim())
        } else if args.starts_with('<') {
            let end = args[1..].find('>').map(|i| i + 1).unwrap_or(args.len());
            (&args[1..end], args[end+1..].trim())
        } else {
            self.diags.push(Diagnostic::err("E0207", "expected \"file\" or <file> for #embed",
                Span::point(&self.file, line_no, 1)));
            return;
        };
        // 解析可选参数: 支持标准 (limit(N)) 和裸 limit(N) 两种形式
        let mut limit = None;
        let mut rest = rest;
        while !rest.is_empty() {
            // 跳过前导括号
            if rest.starts_with('(') { rest = rest[1..].trim(); }
            // 检查 limit(N)
            if let Some(lim_str) = rest.strip_prefix("limit(") {
                if let Some(lim_end) = lim_str.find(')') {
                    limit = lim_str[..lim_end].trim().parse::<usize>().ok();
                    rest = lim_str[lim_end+1..].trim();
                    // 跳过结尾括号
                    if rest.starts_with(')') { rest = rest[1..].trim(); }
                    continue;
                }
            }
            // 其他参数 (prefix, suffix, if-empty) 或非参数内容: 跳过
            if rest.starts_with('(') {
                let end = rest[1..].find(')').map(|i| i + 2).unwrap_or(rest.len());
                rest = rest[end..].trim();
            } else {
                break;
            }
        }
        // 搜索文件 (与 #include 相同逻辑)
        match self.find_include(path, args.starts_with('<')) {
            Some(full_path) => {
                match std::fs::read(&full_path) {
                    Ok(bytes) => {
                        let max = limit.unwrap_or(bytes.len()).min(bytes.len());
                        for (i, &b) in bytes[..max].iter().enumerate() {
                            if i > 0 { self.out.push_str(", "); }
                            self.out.push_str(&b.to_string());
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
                self.diags.push(Diagnostic::err("E0208",
                    format!("embed file not found: {}", path),
                    Span::point(&self.file, line_no, 1)));
            }
        }
    }

    /// 处理 #pragma pack() / pack(n) / pack(push[,n]) / pack(pop[,n])
    ///
    /// 栈顶元素代表当前生效的 packing 对齐 (None=默认 8)。
    /// 后续记录的 packing 值可被 struct 布局查询 (cdecl/ext_abi 兼容)。
    fn handle_pragma_pack(&mut self, text: &str) {
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
        if self.pack_history.last().map(|(_, p)| *p) != Some(new_pack) {
            self.pack_history.push((self.cur_line, new_pack));
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

    /// 把 `_Pragma("...")` 中的字符串当作一行 #pragma 处理
    /// (不在源码中插入新行, 而是直接调用对应的 handler)
    pub fn apply_pragma_string(&mut self, pragma_text: &str, line_no: u32) {
        // 构造 "pragma ..." 形式的伪指令, 复用 process_directive
        let pseudo = format!("pragma {}", pragma_text);
        self.process_directive(&pseudo, line_no);
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
                    b'n' => { out.push('\n'); i += 2; continue; }
                    b't' => { out.push('\t'); i += 2; continue; }
                    b'r' => { out.push('\r'); i += 2; continue; }
                    b'\\' => { out.push('\\'); i += 2; continue; }
                    b'"' => { out.push('"'); i += 2; continue; }
                    b'\'' => { out.push('\''); i += 2; continue; }
                    b'?' => { out.push('?'); i += 2; continue; }
                    b'0'..=b'7' => {
                        // 八进制: 最多 3 位
                        let mut n = 0u32;
                        let mut k = 0;
                        while k < 3 && i + 1 + k < bytes.len() && (b'0'..=b'7').contains(&bytes[i + 1 + k]) {
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
                        while k < 2 && i + 2 + k < bytes.len() && (bytes[i + 2 + k].is_ascii_hexdigit()) {
                            let d = (bytes[i + 2 + k] as char).to_digit(16).unwrap_or(0);
                            n = n * 16 + d;
                            k += 1;
                        }
                        out.push(n as u8 as char);
                        i += 2 + k;
                        continue;
                    }
                    _ => { out.push(b as char); i += 1; continue; }
                }
            }
            out.push(b as char);
            i += 1;
        }
        out
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
        } else if text.starts_with("ignored") {
            let diag_name = text[7..].trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.insert(diag_name.to_string());
                self.diag_state.as_error.remove(diag_name);
            }
        } else if text.starts_with("warning") {
            let diag_name = text[7..].trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.remove(diag_name);
                self.diag_state.as_error.remove(diag_name);
            }
        } else if text.starts_with("error") {
            let diag_name = text[5..].trim().trim_start_matches("-W");
            if !diag_name.is_empty() {
                self.diag_state.ignored.remove(diag_name);
                self.diag_state.as_error.insert(diag_name.to_string());
            }
        }
    }

    // ===== 宏展开 =====
    /// 顶层入口: 对一行文本做完全宏展开
    fn expand_macros(&self, line: &str) -> String {
        if self.macros.is_empty() { return line.to_string(); }
        let disabled = HashSet::new();
        self.expand_with_disabled(line, &disabled)
    }

    /// 用禁用集展开文本
    fn expand_with_disabled(&self, text: &str, disabled: &HashSet<String>) -> String {
        // 递归深度保护：宏互相引用时避免栈溢出
        thread_local! {
            static EXPAND_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        let depth = EXPAND_DEPTH.with(|d| {
            let v = d.get();
            d.set(v + 1);
            v
        });
        if depth > 256 {
            EXPAND_DEPTH.with(|d| d.set(depth));
            return text.to_string();
        }
        let result = self.expand_with_disabled_inner(text, disabled);
        EXPAND_DEPTH.with(|d| d.set(depth));
        result
    }

    fn expand_with_disabled_inner(&self, text: &str, disabled: &HashSet<String>) -> String {
        let mut out = String::with_capacity(text.len());
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            // 跳过字符串字面量
            if chars[i] == '"' {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('"'); i += 1; }
                continue;
            }
            // 跳过字符字面量
            if chars[i] == '\'' {
                out.push('\'');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < len { out.push(chars[i]); i += 1; }
                    out.push(chars[i]); i += 1;
                }
                if i < len { out.push('\''); i += 1; }
                continue;
            }
            // 标识符
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();

                // 特殊内建宏
                if ident == "__FILE__" {
                    out.push_str(&format!("\"{}\"", self.file));
                    continue;
                }
                if ident == "__LINE__" {
                    out.push_str(&self.cur_line.to_string());
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

                // C99 _Pragma("...") 操作符: 把字符串字面量当作 #pragma 处理
                // 把 _Pragma(...) 整体吃掉, 然后调用 process_directive 内部逻辑
                if ident == "_Pragma" {
                    let mut j = i;
                    while j < len && chars[j].is_whitespace() { j += 1; }
                    if j < len && chars[j] == '(' {
                        j += 1;
                        while j < len && chars[j].is_whitespace() { j += 1; }
                        if j < len && chars[j] == '"' {
                            // 提取字符串内容
                            j += 1;
                            let content_start = j;
                            while j < len && chars[j] != '"' {
                                if chars[j] == '\\' && j + 1 < len { j += 2; continue; }
                                j += 1;
                            }
                            let raw: String = chars[content_start..j].iter().collect();
                            let pragma_text = Self::unescape_string(&raw);
                            if j < len { j += 1; } // 跳过 "
                            while j < len && chars[j] != ')' { j += 1; }
                            if j < len { j += 1; } // 跳过 )
                            i = j;
                            // _Pragma 是 expr, 在展开阶段不能改状态。
                            // 我们把它的字符串收集起来, 待外层 &mut self 方法批量应用
                            if let Ok(mut v) = self.pending_pragmas.try_borrow_mut() {
                                v.push(pragma_text);
                            }
                            continue;
                        }
                    }
                    // 解析失败, 跳过整个 _Pragma(...) 表达式
                    let mut j = i;
                    while j < len && chars[j] != ')' { j += 1; }
                    if j < len { j += 1; }
                    i = j;
                    continue;
                }

                // __has_attribute(xxx) / __has_builtin(xxx) / __has_feature(xxx)
                if ident == "__has_attribute" || ident == "__has_builtin" || ident == "__has_feature" {
                    let mut j = i;
                    while j < len && chars[j].is_whitespace() { j += 1; }
                    if j < len && chars[j] == '(' {
                        j += 1;
                        let mut depth = 1;
                        let arg_start = j;
                        while j < len && depth > 0 {
                            if chars[j] == '(' { depth += 1; }
                            else if chars[j] == ')' { depth -= 1; }
                            if depth > 0 { j += 1; }
                        }
                        let arg: String = chars[arg_start..j].iter().collect();
                        let arg = arg.trim().to_string();
                        let val = if ident == "__has_attribute" {
                            if Self::is_known_attribute(&arg) { "1" } else { "0" }
                        } else if ident == "__has_builtin" {
                            if Self::is_known_builtin(&arg) { "1" } else { "0" }
                        } else { "0" }; // __has_feature: always 0 for now
                        out.push_str(val);
                        if j < len { j += 1; } // skip )
                        i = j;
                        continue;
                    }
                }

                // 检查是否在禁用集中 (paint blue)
                if disabled.contains(&ident) {
                    out.push_str(&ident);
                    continue;
                }

                if let Some(def) = self.macros.get(&ident).cloned() {
                    if let Some(ref params) = def.params {
                        // function-like: 需要后跟 (
                        let mut j = i;
                        while j < len && chars[j].is_whitespace() { j += 1; }
                        if j < len && chars[j] == '(' {
                            j += 1;
                            let raw_args = self.parse_macro_args(&chars, &mut j);
                            // 确定哪些参数被 # 或 ## 操作 (这些参数不预展开)
                            let (stringify_params, paste_params) = self.find_special_params(&def.body, params);
                            // 参数预展开: 未被 # / ## 引用的参数先展开。
                            // 必须继承当前 disabled（涂蓝）集，否则 F(F(1))
                            // 这类嵌套会在参数预展开时丢掉外层禁令而无限递归。
                            let mut expanded_args: Vec<String> = Vec::with_capacity(raw_args.len());
                            for (idx, arg) in raw_args.iter().enumerate() {
                                if stringify_params.contains(&idx) || paste_params.contains(&idx) {
                                    expanded_args.push(arg.clone());
                                } else {
                                    expanded_args.push(self.expand_with_disabled(arg, disabled));
                                }
                            }
                            // 可变参数处理
                            let mut final_args = expanded_args.clone();
                            if def.variadic {
                                let has_va_ident = params.last().map(|p| p == "__VA_ARGS__").unwrap_or(false);
                                let fixed = if has_va_ident {
                                    params.len().saturating_sub(1)
                                } else {
                                    0
                                };
                                if raw_args.len() >= fixed {
                                    if has_va_ident {
                                        let mut normal = expanded_args[..fixed].to_vec();
                                        let va_text = if raw_args.len() > fixed {
                                            raw_args[fixed..].iter().map(|a| {
                                                self.expand_with_disabled(a, disabled)
                                            }).collect::<Vec<_>>().join(", ")
                                        } else {
                                            String::new()
                                        };
                                        normal.push(va_text);
                                        final_args = normal;
                                    } else if !raw_args.is_empty() {
                                        let va_text = raw_args.iter().map(|a| {
                                            self.expand_with_disabled(a, disabled)
                                        }).collect::<Vec<_>>().join(", ");
                                        final_args = vec![va_text];
                                    } else {
                                        final_args = vec![String::new()];
                                    }
                                }
                            }
                            if final_args.len() == params.len() {
                                let substituted = self.substitute_params(&def.body, params, &final_args);
                                // 创建新禁用集 = 原禁用集 + 当前宏名
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                // 重新扫描替换结果
                                let rescanned = self.expand_with_disabled(&substituted, &new_disabled);
                                out.push_str(&rescanned);
                            } else if raw_args.len() == params.len() {
                                let substituted = self.substitute_params(&def.body, params, &raw_args);
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                let rescanned = self.expand_with_disabled(&substituted, &new_disabled);
                                out.push_str(&rescanned);
                            } else if params.is_empty() && raw_args.is_empty() {
                                // PREFETCH() 这类无参函数宏
                                let mut new_disabled = disabled.clone();
                                new_disabled.insert(ident.clone());
                                let rescanned = self.expand_with_disabled(&def.body, &new_disabled);
                                out.push_str(&rescanned);
                            } else {
                                out.push_str(&ident);
                            }
                            i = j;
                        } else {
                            out.push_str(&ident);
                        }
                    } else {
                        // object-like 别名后接 (args) 时须与替换体一起重扫描，
                        // 例如 #define DIV_ROUND_UP __KERNEL_DIV_ROUND_UP
                        let mut new_disabled = disabled.clone();
                        new_disabled.insert(ident.clone());
                        let mut j = i;
                        while j < len && chars[j].is_whitespace() { j += 1; }
                        let tail_call = j < len && chars[j] == '(';
                        let rescanned = if tail_call {
                            let remainder: String = chars[i..].iter().collect();
                            self.expand_with_disabled(
                                &format!("{}{}", def.body, remainder),
                                &new_disabled,
                            )
                        } else {
                            self.expand_with_disabled(&def.body, &new_disabled)
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
                    if !paste.contains(&idx) { paste.push(idx); }
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

    fn substitute_params(&self, body: &str, params: &[String], args: &[String]) -> String {
        // 空 __VA_ARGS__ 时移除 GCC 扩展 ", ## __VA_ARGS__"
        let mut body = body.to_string();
        if let Some(va_idx) = params.iter().position(|p| p == "__VA_ARGS__") {
            if args.get(va_idx).map(|s| s.is_empty()).unwrap_or(false) {
                for pat in &[", ## __VA_ARGS__", ",## __VA_ARGS__", ", ##__VA_ARGS__", ",##__VA_ARGS__"] {
                    body = body.replace(pat, "");
                }
            }
        }
        // 第一步: 处理 # (字符串化) — 必须在 ## 之前, 否则参数名被替换后 # 找不到模式
        let result = self.handle_stringify(&body, params, args);
        // 第二步: 处理 ## (令牌拼接)
        let result = self.handle_paste(&result, params, args);
        // 第三步: 普通参数替换（单次扫描，实参视为不透明，避免 *d 被二次替换成 **d）
        let mut result = result;
        result = Self::replace_all_params(&result, params, args);
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
    fn handle_paste(&self, body: &str, params: &[String], args: &[String]) -> String {
        if !body.contains("##") {
            return body.to_string();
        }
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
            let pasted = format!("{}{}", left_sub, right_sub);
            result = format!("{}{}{}", &before[..left_start], pasted, &after[right_end..]);
        }
        result
    }

    /// # 字符串化: # param → "param_value"
    fn handle_stringify(&self, body: &str, params: &[String], args: &[String]) -> String {
        let mut result = body.to_string();
        for (i, param) in params.iter().enumerate() {
            if i >= args.len() { continue; }
            // 查找 # param (注意不是 ##)
            let pattern = format!("# {}", param);
            let pattern2 = format!("#{}", param);
            for pat in &[pattern, pattern2] {
                while let Some(pos) = result.find(pat.as_str()) {
                    // 确保不是 ##
                    if pos > 0 && result.as_bytes()[pos-1] == b'#' { break; }
                    let stringified = Self::stringify_arg(&args[i]);
                    let end = pos + pat.len();
                    result = format!("{}{}{}", &result[..pos], stringified, &result[end..]);
                }
            }
        }
        result
    }

    /// 将参数值转为字符串字面量 (转义 " 和 \；换行/连续空白折叠为单空格，符合 C # 规则)
    fn stringify_arg(arg: &str) -> String {
        let collapsed: String = arg
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
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
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();
                if ident == from { result.push_str(to); }
                else { result.push_str(&ident); }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    // ===== 常量表达式求值 (用于 #if) =====
    fn eval_const_expr(&self, expr: &str) -> bool {
        let expr = expr.trim();
        // 处理 defined(X) 和 defined X
        let expr = self.replace_defined(expr);
        // 处理 __has_include/__has_attribute/__has_builtin/__has_feature
        let expr = self.replace_has_macros(&expr);
        // 替换已知宏
        let expr = self.expand_macros(&expr);
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
                if prev.is_ascii_alphanumeric() || prev == b'_' { break; }
            }
            let after = &result[pos + 7..];
            let after = after.trim_start();
            if after.starts_with('(') {
                let end = after.find(')').unwrap_or(0);
                let name = after[1..end].trim();
                let val = if self.macros.contains_key(name) { "1" } else { "0" };
                result = format!("{}{}{}", &result[..pos], val, &after[end+1..]);
            } else {
                let name_end = after.find(|c: char| !c.is_alphanumeric() && c != '_')
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
        let expr = self.replace_unknown_idents(&expr);
        // 递归下降求值
        let mut pos = 0;
        let bytes = expr.as_bytes();
        let val = Self::parse_ternary(bytes, &mut pos);
        val
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
                        result.push(chars[i]); i += 1;
                    }
                    result.push(chars[i]); i += 1;
                }
                if i < chars.len() { result.push('\''); i += 1; }
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
                    while i < chars.len()
                        && (chars[i].is_ascii_hexdigit() || chars[i] == '\'')
                    {
                        i += 1;
                    }
                } else {
                    while i < chars.len()
                        && (chars[i].is_ascii_digit() || chars[i] == '\'')
                    {
                        i += 1;
                    }
                }
                // 保留整型后缀 U/L/ull 等
                while i < chars.len()
                    && (chars[i] == 'u'
                        || chars[i] == 'U'
                        || chars[i] == 'l'
                        || chars[i] == 'L')
                {
                    i += 1;
                }
                result.extend(chars[start..i].iter().copied());
                continue;
            }
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let ident: String = chars[start..i].iter().collect();
                match ident.as_str() {
                    "defined" | "true" => result.push_str("1"),
                    "false" => result.push_str("0"),
                    _ => { result.push_str("0"); }
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
            if cond != 0 { then_val } else { else_val }
        } else {
            cond
        }
    }

    fn parse_or_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_and_expr(bytes, pos);
        Self::skip_ws(bytes, pos);
        while *pos + 1 < bytes.len() && bytes[*pos] == b'|' && bytes[*pos+1] == b'|' {
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
        while *pos + 1 < bytes.len() && bytes[*pos] == b'&' && bytes[*pos+1] == b'&' {
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
            if *pos < bytes.len() && bytes[*pos] == b'|'
                && (*pos + 1 >= bytes.len() || bytes[*pos+1] != b'|') {
                *pos += 1;
                val |= Self::parse_bit_xor(bytes, pos);
            } else { break; }
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
            } else { break; }
        }
        val
    }

    fn parse_bit_and(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_eq_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            // 排除 &&
            if *pos < bytes.len() && bytes[*pos] == b'&'
                && (*pos + 1 >= bytes.len() || bytes[*pos+1] != b'&') {
                *pos += 1;
                val &= Self::parse_eq_expr(bytes, pos);
            } else { break; }
        }
        val
    }

    fn parse_eq_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_cmp_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos + 1 < bytes.len() {
                if bytes[*pos] == b'=' && bytes[*pos+1] == b'=' {
                    *pos += 2; val = (val == Self::parse_cmp_expr(bytes, pos)) as i64;
                } else if bytes[*pos] == b'!' && bytes[*pos+1] == b'=' {
                    *pos += 2; val = (val != Self::parse_cmp_expr(bytes, pos)) as i64;
                } else { break; }
            } else { break; }
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
                } else { break; }
            } else { break; }
        }
        val
    }

    fn parse_shift_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_add_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos + 1 < bytes.len() {
                if bytes[*pos] == b'<' && bytes[*pos+1] == b'<' {
                    *pos += 2; val <<= Self::parse_add_expr(bytes, pos);
                } else if bytes[*pos] == b'>' && bytes[*pos+1] == b'>' {
                    *pos += 2; val >>= Self::parse_add_expr(bytes, pos);
                } else { break; }
            } else { break; }
        }
        val
    }

    fn parse_add_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_mul_expr(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                if bytes[*pos] == b'+' { *pos += 1; val += Self::parse_mul_expr(bytes, pos); }
                else if bytes[*pos] == b'-' { *pos += 1; val -= Self::parse_mul_expr(bytes, pos); }
                else { break; }
            } else { break; }
        }
        val
    }

    fn parse_mul_expr(bytes: &[u8], pos: &mut usize) -> i64 {
        let mut val = Self::parse_unary(bytes, pos);
        loop {
            Self::skip_ws(bytes, pos);
            if *pos < bytes.len() {
                if bytes[*pos] == b'*' { *pos += 1; val *= Self::parse_unary(bytes, pos); }
                else if bytes[*pos] == b'/' {
                    *pos += 1;
                    let d = Self::parse_unary(bytes, pos);
                    val = if d != 0 { val / d } else { 0 };
                }
                else if bytes[*pos] == b'%' {
                    *pos += 1;
                    let d = Self::parse_unary(bytes, pos);
                    val = if d != 0 { val % d } else { 0 };
                }
                else { break; }
            } else { break; }
        }
        val
    }

    fn parse_unary(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        if *pos < bytes.len() {
            if bytes[*pos] == b'!' { *pos += 1; return if Self::parse_unary(bytes, pos) == 0 { 1 } else { 0 }; }
            if bytes[*pos] == b'-' { *pos += 1; return -Self::parse_unary(bytes, pos); }
            if bytes[*pos] == b'+' { *pos += 1; return Self::parse_unary(bytes, pos); }
            if bytes[*pos] == b'~' { *pos += 1; return !Self::parse_unary(bytes, pos); }
            if bytes[*pos] == b'(' {
                *pos += 1;
                let val = Self::parse_ternary(bytes, pos);
                Self::skip_ws(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b')' { *pos += 1; }
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
                let c = bytes[*pos]; *pos += 1;
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
                                } else { break; }
                            }
                        }
                        val
                    }
                    d if d.is_ascii_digit() => {
                        // \NNN 八进制
                        let mut val = (d - b'0') as i64;
                        for _ in 0..2 {
                            if *pos < bytes.len() && bytes[*pos].is_ascii_digit() && bytes[*pos] < b'8' {
                                val = val * 8 + (bytes[*pos] - b'0') as i64;
                                *pos += 1;
                            } else { break; }
                        }
                        val
                    }
                    _ => c as i64,
                }
            } else { 0 }
        } else if *pos < bytes.len() {
            let c = bytes[*pos] as i64; *pos += 1; c
        } else { 0 };
        // skip closing '
        if *pos < bytes.len() && bytes[*pos] == b'\'' { *pos += 1; }
        ch
    }

    fn parse_number(bytes: &[u8], pos: &mut usize) -> i64 {
        Self::skip_ws(bytes, pos);
        let start = *pos;
        if *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            while *pos < bytes.len() && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'\'') {
                *pos += 1;
            }
            let s = std::str::from_utf8(&bytes[start..*pos]).unwrap_or("0");
            // 去掉后缀 (L, U, LL 等)
            let s = s.trim_end_matches(|c: char| c == 'L' || c == 'l' || c == 'U' || c == 'u');
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).unwrap_or(0)
            } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
                i64::from_str_radix(bin, 2).unwrap_or(0)
            } else if s.starts_with('0') && s.len() > 1 && s.chars().all(|c| c.is_ascii_digit()) {
                i64::from_str_radix(s, 8).unwrap_or(0)
            } else {
                s.parse::<i64>().unwrap_or(0)
            }
        } else { 0 }
    }

    fn skip_ws(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
            *pos += 1;
        }
    }

    // ===== __has_* 特殊宏求值 (在宏展开之前执行) =====
    /// 替换 __has_include/__has_attribute/__has_builtin/__has_feature 为实际值
    fn replace_has_macros(&self, expr: &str) -> String {
        let mut result = expr.to_string();
        result = self.replace_has_include(&result);
        result = self.replace_has_attr_like(&result, "__has_attribute", |attr| Self::is_known_attribute(attr));
        result = self.replace_has_attr_like(&result, "__has_builtin", |name| Self::is_known_builtin(name));
        result = self.replace_has_attr_like(&result, "__has_feature", |_| false);
        result
    }

    fn replace_has_include(&self, expr: &str) -> String {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // 检查 __has_include
            if i + 14 <= bytes.len() && &bytes[i..i+14] == b"__has_include"
                && (i == 0 || !bytes[i-1].is_ascii_alphanumeric() && bytes[i-1] != b'_')
                && (i + 14 >= bytes.len() || !bytes[i+14].is_ascii_alphanumeric() && bytes[i+14] != b'_')
            {
                let mut j = i + 14;
                while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                    let path_start = j;
                    // 跳过到 )
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' { depth += 1; }
                        else if bytes[j] == b')' { depth -= 1; }
                        if depth > 0 { j += 1; }
                    }
                    let path = expr[path_start..j].trim()
                        .trim_matches(|c| c == '<' || c == '>' || c == '"');
                    let found = self.find_has_include(path);
                    result.push_str(if found { "1" } else { "0" });
                    if j < bytes.len() { j += 1; } // skip )
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
    where F: Fn(&str) -> bool {
        let mut result = String::with_capacity(expr.len());
        let bytes = expr.as_bytes();
        let name_len = macro_name.len();
        let mut i = 0;
        while i < bytes.len() {
            if i + name_len <= bytes.len()
                && &bytes[i..i+name_len] == macro_name.as_bytes()
                && (i == 0 || !bytes[i-1].is_ascii_alphanumeric() && bytes[i-1] != b'_')
                && (i + name_len >= bytes.len() || !bytes[i+name_len].is_ascii_alphanumeric() && bytes[i+name_len] != b'_')
            {
                let mut j = i + name_len;
                while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                if j < bytes.len() && bytes[j] == b'(' {
                    j += 1;
                    while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                    let arg_start = j;
                    let mut depth = 1;
                    while j < bytes.len() && depth > 0 {
                        if bytes[j] == b'(' { depth += 1; }
                        else if bytes[j] == b')' { depth -= 1; }
                        if depth > 0 { j += 1; }
                    }
                    let arg = expr[arg_start..j].trim();
                    let val = if check(arg) { "1" } else { "0" };
                    result.push_str(val);
                    if j < bytes.len() { j += 1; }
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
        if p.exists() { return true; }
        // -I 路径
        for dir in &self.include_paths {
            if dir.join(path).exists() { return true; }
        }
        // -isystem 路径
        for dir in &self.system_paths {
            if dir.join(path).exists() { return true; }
        }
        // 标准系统路径
        let system_dirs = [
            "/usr/local/include", "/usr/include",
            "/usr/include/x86_64-linux-gnu", "/usr/include/aarch64-linux-gnu",
            "/usr/include/linux", "/usr/include/asm", "/usr/include/asm-generic",
        ];
        for dir in &system_dirs {
            if Path::new(dir).join(path).exists() { return true; }
        }
        false
    }

    fn is_known_attribute(attr: &str) -> bool {
        matches!(attr,
            "packed" | "aligned" | "noreturn" | "unused" | "used"
            | "section" | "visibility" | "always_inline" | "noinline"
            | "format" | "nonnull" | "cleanup" | "deprecated"
            | "fallthrough" | "weak" | "constructor" | "destructor"
            | "vector_size" | "may_alias" | "transparent_union"
            | "malloc" | "returns_twice" | "pure" | "const"
            | "warn_unused_result" | "sentinel" | "format_arg"
        )
    }

    fn is_known_builtin(name: &str) -> bool {
        let name = name.trim();
        matches!(name,
            "__builtin_expect" | "__builtin_memcpy" | "__builtin_memset"
            | "__builtin_memmove" | "__builtin_memcmp"
            | "__builtin_malloc" | "__builtin_free"
            | "__builtin_offsetof" | "__builtin_va_start" | "__builtin_va_end"
            | "__builtin_va_arg" | "__builtin_va_copy"
            | "__builtin_add_overflow" | "__builtin_sub_overflow"
            | "__builtin_mul_overflow"
            | "__builtin_types_compatible_p"
            | "__builtin_choose_expr" | "__builtin_constant_p"
            | "__builtin_abs" | "__builtin_unreachable" | "__builtin_trap"
            | "__builtin_strcpy" | "__builtin_strncpy"
            | "__builtin_strcmp" | "__builtin_strlen"
            | "__builtin_prefetch"
            | "__builtin_object_size"
            | "__builtin_expect_with_probability"
            | "__builtin_clz" | "__builtin_clzl" | "__builtin_clzll"
            | "__builtin_ctz" | "__builtin_ctzl" | "__builtin_ctzll"
            | "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll"
            | "__builtin_parity" | "__builtin_parityl" | "__builtin_parityll"
            | "__builtin_bswap16" | "__builtin_bswap32" | "__builtin_bswap64"
            | "__builtin_bitreverse8" | "__builtin_bitreverse16"
            | "__builtin_bitreverse32" | "__builtin_bitreverse64"
            | "__builtin_frame_address" | "__builtin_return_address"
            | "__builtin_overflow_p" | "__builtin_add_overflow_p"
            | "__builtin_sub_overflow_p" | "__builtin_mul_overflow_p"
            | "__builtin_apply" | "__builtin_return"
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
        let mut hash = Self::hash_content("DCC_PREPROCESSOR_CACHE_V6");
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

    fn get_cache_path(&self, file_path: &Path, content_hash: u64, macro_hash: u64) -> Option<PathBuf> {
        let cache_dir = self.cache_dir.as_ref()?;
        let file_hash = Self::hash_content(&file_path.to_string_lossy());
        let cache_name = format!("v6_{:x}_{:x}_{:x}", file_hash, content_hash, macro_hash);
        Some(cache_dir.join(cache_name))
    }

    fn load_from_cache(&self, cache_path: &Path) -> Option<(String, Vec<(String, MacroDef)>)> {
        let content = fs::read_to_string(cache_path).ok()?;
        let mut lines = content.lines();

        // 第一行: 标记
        if lines.next()? != "DCC_CACHE_V3" {
            return None;
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
            if line.is_empty() { continue; }
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
                macros.push((name, MacroDef { params, body, variadic }));
            }
        }

        Some((output, macros))
    }

    fn save_to_cache(&self, cache_path: &Path, output: &str, macros: &HashMap<String, MacroDef>) {
        let mut content = String::from("DCC_CACHE_V3\n");
        content.push_str(output);
        content.push_str("END_OUTPUT\n");

        for (name, def) in macros {
            // 格式: NAME\tPARAMS\tVARIADIC\tBODY
            let params_str = match &def.params {
                None => "-".to_string(),
                Some(p) => p.join(","),
            };
            let variadic_str = if def.variadic { "1" } else { "0" };
            content.push_str(&format!("{}\t{}\t{}\t{}\n", name, params_str, variadic_str, def.body));
        }

        let _ = fs::write(cache_path, content);
    }
}

#[cfg(test)]
mod tests {
    use super::preprocess;
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
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/preproc_comment_force.h");
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
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(output.contains("asm goto(\"1:\""), "output: {output}");
        assert!(output.contains("\"jmp\\n\""), "output: {output}");
        assert!(output.contains(": : \"i\" (0) : : lbl)"), "output: {output}");
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
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            !output.contains("__KERNEL_DIV_ROUND_UP"),
            "output: {output}"
        );
        assert!(output.contains("unsigned long tags[3]["), "output: {output}");
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
        );
        let output = out.text;
        let diags = out.diags;
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            output.contains("\"malloc(sizeof(int) + 1)\""),
            "output: {output}"
        );
    }
}
