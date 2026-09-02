// src/parser.rs - 语法分析
// 第一性原理: 解析器把 token 流构造成 AST。
// suckless: 递归下降 + Pratt 表达式解析。

use crate::ast::*;
use crate::const_eval::{self, ConstEvalError};
use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Lexer, Tok, TokKind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 解析后的 GNU 属性
#[derive(Default, Clone)]
struct ParsedAttrs {
    section: Option<String>,
    alignment: Option<usize>,
    is_weak: bool,
    visibility: Option<String>,
    is_unused: bool,
    is_cold: bool,
    is_hot: bool,
    is_noinline: bool,
    is_always_inline: bool,
    is_noreturn: bool,
    register_name: Option<String>,
    cleanup_fn: Option<String>,
    is_constructor: bool,
    is_destructor: bool,
    alias_name: Option<String>,
    format_kind: Option<String>,
    format_string_idx: Option<u32>,
    format_first_arg: Option<u32>,
    /// __attribute__((no_sanitize("address"|"undefined"|"thread"|"kernel-address")))
    no_sanitize: Vec<String>,
    /// __attribute__((target_clones("default,avx2,...")))
    target_clones: Vec<String>,
    /// __attribute__((copy("fn")))
    copy_fn: Option<String>,
    /// __attribute__((returns_nonnull))
    returns_nonnull: bool,
    /// __attribute__((constructor(N))) / __attribute__((destructor(N)))
    ctor_priority: Option<u16>,
    dtor_priority: Option<u16>,
    /// __attribute__((retain))
    is_retain: bool,
    /// __attribute__((used))
    is_used: bool,
    /// __attribute__((flatten))
    is_flatten: bool,
    /// __attribute__((error("msg")))
    error_msg: Option<String>,
    /// __attribute__((warning("msg")))
    warning_msg: Option<String>,
    /// __attribute__((deprecated("msg")))
    deprecated_msg: Option<String>,
    /// __attribute__((ifunc("resolver")))
    ifunc_resolver: Option<String>,
    /// __attribute__((asm_output("template"))) - GCC15 提案
    /// 指定函数返回值的 asm 输出模板 (目前仅解析存储, 无 codegen 影响)
    asm_output_template: Option<String>,
    /// __attribute__((vector_size(N)))
    vector_size: Option<usize>,
    /// CUDA/HIP 函数限定符: __global__ / __device__ / __host__
    /// (仅 -fcuda / -fhip 时解析为限定符, 否则是普通标识符)
    is_global: bool,
    is_device: bool,
    is_host: bool,
    /// __attribute__((naked)): 不生成 prologue/epilogue
    is_naked: bool,
    /// __attribute__((may_alias)): 类型属性，指示该类型的指针可以别名任何其他类型
    /// 禁用基于类型的别名分析 (TBAA)。内核中 __bitwise 类型（如 __le32、__be32）
    /// 依赖此属性进行安全的类型双关。
    may_alias: bool,
    /// __attribute__((packed/__packed__))
    is_packed: bool,
    /// __attribute__((tls_model("model")))
    /// TLS 访问模型: "global-dynamic", "local-dynamic", "initial-exec", "local-exec"
    tls_model: Option<String>,
    /// __asm__("label"): GNU asm 标签, 覆盖 ELF 符号名
    asm_label: Option<String>,
    /// C23 constexpr: 变量必须常量初始化, 函数等价 inline + always_inline
    is_constexpr: bool,
    /// __attribute__((pure)): 函数无副作用, 结果仅依赖参数和/或全局变量
    is_pure: bool,
    /// __attribute__((const)): 函数结果仅依赖参数, 不读全局内存
    is_const_attr: bool,
    /// __attribute__((returns_twice)): 函数可能多次返回 (如 setjmp)
    returns_twice: bool,
    /// __attribute__((no_instrument_function)): 禁止 profiling 插桩
    no_instrument: bool,
    /// __attribute__((malloc)): 返回指针不别名已有内存
    is_malloc: bool,
    /// __attribute__((malloc (dealloc_fn, argno))): GCC11+ 释放函数提示
    malloc_dealloc: Option<String>,
    malloc_dealloc_argno: Option<u32>,
    /// __attribute__((alloc_size(N[,M]))): 参数 N(,M) 指定分配大小
    alloc_size: Option<(u32, Option<u32>)>,
    /// __attribute__((assume_aligned(N))): 返回指针对齐至少 N 字节
    assume_aligned: Option<usize>,
    /// __attribute__((warn_unused_result)): 返回值不应被忽略
    warn_unused_result: bool,
    /// __attribute__((sentinel)): 可变参函数需 NULL 结尾
    is_sentinel: bool,
    /// __attribute__((format_arg(N))): 第 N 个参数是格式字符串
    format_arg_idx: Option<u32>,
    /// C23 [[fallthrough]]: 标记 switch case 有意贯穿
    is_fallthrough: bool,
    /// C23 [[nodiscard]]: 函数返回值不应被忽略
    is_nodiscard: bool,
    /// __attribute__((transparent_union)): struct 字段透明联合
    transparent_union: bool,
    /// __attribute__((externally_visible)): 阻止 LTO 隐藏符号
    externally_visible: bool,
    /// __attribute__((mode(__DI__))): 类型模式 (SI/DI/HI/QI/word/SF/DF/TF/pointer)
    mode_name: Option<String>,
    /// C `inline` 关键字: GNU89 语义, 非 static inline 函数发射为 weak 符号
    is_inline: bool,
}

/// 解析选项（Driver 传入）
#[derive(Debug, Default, Clone)]
pub struct ParseOptions {
    /// `-fpermissive`：未知 typedef 降级为 int 并告警（默认严格 E0009）
    pub permissive: bool,
    /// #pragma pack 变更历史: (行号, 新对齐). 供 struct 布局按位置查询
    pub pack_history: Vec<(u32, Option<usize>)>,
    /// #pragma weak 记录的弱符号集合
    pub weak_symbols: std::collections::HashSet<String>,
    /// `-Wint-truncation`: 整数截断警告
    pub warn_int_truncation: bool,
    /// `-Wint-conversion`: 整数↔指针隐式转换警告
    pub warn_int_conversion: bool,
    /// C23 模式 (-std=c23/gnu23): bool/true/false/alignas/thread_local/
    /// typeof_unqual 等 C23 关键字生效; 非 C23 模式保持普通标识符语义
    /// (兼容 `typedef _Bool bool;` 与 gnu11 内核代码)
    pub is_c23: bool,
    /// C89/GNU89 或 `-fpermissive`: 允许隐式函数声明降级为 int。
    pub allow_implicit_function_decl: bool,
    /// `-Wimplicit-declaration`: 未声明函数调用警告
    pub warn_implicit_decl: bool,
    /// `-fcuda`: 启用 CUDA 函数限定符 (__global__/__device__/__host__)
    pub cuda: bool,
    /// `-fhip`: 启用 HIP 函数限定符 (同 CUDA 拼写)
    pub hip: bool,
    /// #pragma STDC FENV_ACCESS 状态: ON/DEFAULT 为 true.
    /// 供 codegen 对浮点操作插入 volatile 语义.
    pub fenv_access: bool,
    /// #pragma STDC FP_CONTRACT 状态: ON 为 true.
    pub fp_contract: bool,
    /// #pragma STDC CX_LIMITED_RANGE 状态: ON 为 true.
    pub cx_limited_range: bool,
    /// `-fshort-wchar`: wchar_t 为 2 字节 (UTF-16LE), 而非默认 4 字节 (UTF-32LE)
    pub short_wchar: bool,
    /// LP64 数据模型 (64-bit long/pointer): x86_64/AArch64/RISC-V 64/MIPS64.
    /// 供编译器内置 typedef (size_t/ptrdiff_t 等) 选择正确底层类型.
    pub is_lp64: bool,
}

impl ParsedAttrs {
    fn merge(&mut self, other: ParsedAttrs) {
        /// 布尔属性: other 为 true 则置位 (永不清除)
        macro_rules! mb { ($f:ident) => { if other.$f { self.$f = true; } } }
        /// Option/String 属性: other 为 Some 则覆盖
        macro_rules! mo { ($f:ident) => { if other.$f.is_some() { self.$f = other.$f; } } }
        /// Vec 属性: other 非空则追加
        macro_rules! mv { ($f:ident) => { if !other.$f.is_empty() { self.$f.extend(other.$f); } } }

        mo!(section); mo!(alignment);
        mb!(is_weak); mo!(visibility);
        mb!(is_unused); mb!(is_cold); mb!(is_hot); mb!(is_noinline);
        mb!(is_always_inline); mb!(is_noreturn); mb!(is_constructor); mb!(is_destructor);
        mb!(returns_nonnull); mb!(is_retain); mb!(is_used); mb!(is_flatten);
        mb!(is_global); mb!(is_device); mb!(is_host); mb!(is_naked);
        mb!(may_alias); mb!(is_packed); mb!(is_constexpr); mb!(is_pure);
        mb!(is_const_attr); mb!(returns_twice); mb!(no_instrument); mb!(is_malloc);
        mb!(warn_unused_result); mb!(is_sentinel); mb!(is_fallthrough);
        mb!(is_nodiscard); mb!(transparent_union); mb!(externally_visible);
        // is_inline: 由 C `inline` 关键字在 parse_type_mode 中设置,
        // 需要从 decl_attrs 合并 (但不从 __attribute__ 传播).
        // merge_attrs(leading_attrs, decl_attrs) 中 decl_attrs 是 second arg,
        // 所以这里合并 is_inline 是安全的.
        mb!(is_inline);
        mo!(register_name); mo!(cleanup_fn); mo!(alias_name);
        mo!(format_kind); mo!(format_string_idx); mo!(format_first_arg);
        mo!(copy_fn); mo!(error_msg); mo!(warning_msg); mo!(deprecated_msg);
        mo!(ifunc_resolver); mo!(tls_model); mo!(asm_label);
        mo!(mode_name);
        mo!(asm_output_template);
        mo!(alloc_size); mo!(malloc_dealloc); mo!(malloc_dealloc_argno);
        mo!(assume_aligned); mo!(ctor_priority); mo!(dtor_priority);
        mo!(format_arg_idx); mo!(vector_size);
        mv!(no_sanitize); mv!(target_clones);
    }
}

#[derive(Default, Clone, Copy)]
struct StorageClass {
    is_extern: bool,
    is_static: bool,
    is_register: bool,
    is_thread_local: bool,
}

/// 枚举定义表项: (常量名, 值) 列表 + C23 可选底层类型
type EnumDef = (Arc<Vec<(String, i64)>>, Option<Box<QualType>>);

/// 分层作用域链: 替代每次块边界 clone 整个 HashMap 的高开销方案.
/// 进入块时 push 空层, 退出时 pop; 查找从内向外搜索.
struct ScopeChain {
    scopes: Vec<HashMap<String, Type>>,
}

impl ScopeChain {
    fn new() -> Self {
        Self { scopes: vec![HashMap::new()] }
    }
    fn insert(&mut self, key: String, val: Type) {
        self.scopes.last_mut().unwrap().insert(key, val);
    }
    fn get(&self, key: &str) -> Option<&Type> {
        self.scopes.iter().rev().find_map(|s| s.get(key))
    }
    fn contains_key(&self, key: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains_key(key))
    }
    /// 检查名字是否存在于外层作用域 (不含当前最内层)
    fn exists_in_outer_scopes(&self, key: &str) -> bool {
        if self.scopes.len() < 2 { return false; }
        self.scopes[..self.scopes.len()-1].iter().any(|s| s.contains_key(key))
    }
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }
    fn clear(&mut self) {
        self.scopes.clear();
        self.scopes.push(HashMap::new());
    }
    fn all_keys(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        for s in &self.scopes {
            for k in s.keys() {
                set.insert(k.clone());
            }
        }
        set
    }
    fn retain_globals(&mut self, globals: &HashMap<String, Type>) {
        for scope in &mut self.scopes {
            scope.retain(|k, _| globals.contains_key(k));
        }
    }
    fn load_globals(&mut self, globals: &HashMap<String, Type>) {
        self.scopes.clear();
        self.scopes.push(globals.clone());
    }
    fn snapshot(&self) -> Vec<HashMap<String, Type>> {
        self.scopes.clone()
    }
    fn restore(&mut self, snapshot: Vec<HashMap<String, Type>>) {
        self.scopes = snapshot;
    }
    /// 将所有层合并为单个 HashMap (内层覆盖外层)
    fn flatten(&self) -> HashMap<String, Type> {
        let mut map = HashMap::new();
        for scope in &self.scopes {
            for (k, v) in scope {
                map.insert(k.clone(), v.clone());
            }
        }
        map
    }
}

pub struct Parser<'a> {
    lex: Lexer<'a>,
    cur: Tok,
    peek: Tok,
    diags: Vec<Diagnostic>,
    finished: bool,
    typedefs: HashMap<String, Type>,
    struct_defs: HashMap<String, (Arc<Vec<Field>>, bool, usize)>,
    enum_defs: HashMap<String, EnumDef>,
    enum_consts: HashMap<String, (i64, Type)>, // 枚举常量: 值 + C23 底层类型
    locals: ScopeChain,                      // 分层作用域链 (替代单 HashMap + clone)
    var_quals: HashMap<String, QualifierFlags>, // 变量名 → 限定符 (const检查用)
    globals: HashMap<String, Type>,            // 全局变量名 → 类型 (永不清除)
    nodiscard_fns: HashSet<String>,            // C23 [[nodiscard]] 函数名集合
    in_function: bool,                         // 是否在函数体内
    current_function: Option<String>,          // 当前函数名 (供 __func__/__builtin_FUNCTION 等使用)
    pub last_variadic: bool,                   // parse_params 是否遇到 ...
    decl_storage: StorageClass,                // 最近一次声明说明符中的存储类
    decl_attrs: ParsedAttrs,                   // 类型说明符之间出现的 GNU 属性
    decl_auto_type: bool,                      // __auto_type：类型由初始化表达式推导
    allow_named_type: bool,                    // typeof 等上下文允许 Type::Named
    spec_quals: QualifierFlags,                // 当前声明说明符中捕获的限定符
    decl_quals: QualifierFlags,                // 声明符中捕获的限定符 (如 * const 中的 const)
    spec_addr_space: AddrSpace,                // 当前声明说明符中捕获的地址空间
    opts: ParseOptions,
    shadow_counter: i32, // 嵌套块遮蔽重命名计数器
}

pub fn parse(src: &[u8], file: &str) -> (Program, Vec<Diagnostic>) {
    parse_with_opts(src, file, ParseOptions::default())
}

pub fn parse_with_opts(src: &[u8], file: &str, opts: ParseOptions) -> (Program, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file, opts);
    p.program()
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8], file: &str, opts: ParseOptions) -> Self {
        let mut lex = Lexer::new(src, file);
        let cur = lex.next();
        let peek = lex.next();
        let mut p = Self {
            lex,
            cur,
            peek,
            diags: vec![],
            finished: false,
            typedefs: HashMap::new(),
            struct_defs: HashMap::new(),
            enum_defs: HashMap::new(),
            enum_consts: HashMap::new(),
            locals: ScopeChain::new(),
            var_quals: HashMap::new(),
            globals: HashMap::new(),
            nodiscard_fns: HashSet::new(),
            in_function: false,
            current_function: None,
            last_variadic: false,
            decl_storage: StorageClass::default(),
            decl_attrs: ParsedAttrs::default(),
            decl_auto_type: false,
            allow_named_type: false,
            spec_quals: QualifierFlags::NONE,
            decl_quals: QualifierFlags::NONE,
            spec_addr_space: AddrSpace::None,
            opts,
            shadow_counter: 0,
        };
        // C23 nullptr_t: 内置类型 (等价 nullptr 表达式所对应的 (void*)0)
        // 系统头 (glibc stddef.h) 若重复定义, register_typedef 按
        // types_compatible 允许兼容重定义, 不会报错.
        p.typedefs
            .insert("nullptr_t".to_string(), Type::Ptr(Box::new(Type::Void.into())));
        // 编译器内置 typedef: size_t/ptrdiff_t/intptr_t/uintptr_t.
        // GCC/Clang 在编译器层提供这些类型, 不依赖系统头文件.
        // 系统头 (glibc stddef.h) 若重复定义, register_typedef 按
        // is_integer_typedef 允许整型别名覆盖, 不会报错.
        if p.opts.is_lp64 {
            // LP64: long = 64-bit, size_t = unsigned long
            p.typedefs.insert("size_t".to_string(), Type::ULong);
            p.typedefs.insert("ptrdiff_t".to_string(), Type::Long);
            p.typedefs.insert("intptr_t".to_string(), Type::Long);
            p.typedefs.insert("uintptr_t".to_string(), Type::ULong);
        } else {
            // ILP32: int = 32-bit, size_t = unsigned int
            p.typedefs.insert("size_t".to_string(), Type::UInt);
            p.typedefs.insert("ptrdiff_t".to_string(), Type::Int);
            p.typedefs.insert("intptr_t".to_string(), Type::Int);
            p.typedefs.insert("uintptr_t".to_string(), Type::UInt);
        }
        p
    }
    fn advance(&mut self) {
        self.cur = std::mem::replace(&mut self.peek, self.lex.next());
    }

    /// 解包指针/数组外层后比较 struct/union/enum 标签是否兼容
    /// 同标签且任一方不完整 (或字段完全相同) 即视为兼容类型
    fn tag_compatible(a: &Type, b: &Type) -> bool {
        fn unwrap(t: &Type) -> &Type {
            match t {
                Type::Ptr(inner) => unwrap(&inner.base),
                Type::Array(inner, _) => unwrap(&inner.base),
                other => other,
            }
        }
        match (unwrap(a), unwrap(b)) {
            (Type::Struct(an, af, _), Type::Struct(bn, bf, _)) => {
                an == bn && (af.is_empty() || bf.is_empty() || af == bf)
            }
            (Type::Union(an, af, _), Type::Union(bn, bf, _)) => {
                an == bn && (af.is_empty() || bf.is_empty() || af == bf)
            }
            // enum 同标签兼容: 前向声明 (空 values) 与完整定义视为同一类型
            (Type::Enum(an, av, _), Type::Enum(bn, bv, _)) => {
                an == bn && (av.is_empty() || bv.is_empty() || av == bv)
            }
            _ => false,
        }
    }

    /// 给定源行号, 查找 #pragma pack 历史中的当前 packing 对齐
    /// pack_history 按行号升序, 找到最后一个 l <= line 的条目即生效值
    pub fn pack_at_line(&self, line: u32) -> Option<usize> {
        let mut current: Option<usize> = None;
        for &(l, pack) in &self.opts.pack_history {
            if l <= line {
                current = pack;
            } else {
                break;
            }
        }
        current
    }

    /// 当前行处是否标记为弱符号 (#pragma weak name)
    pub fn is_weak_symbol(&self, name: &str) -> bool {
        self.opts.weak_symbols.contains(name)
    }

    /// 保存解析器/词法位置，供试探性解析后回退
    fn bookmark(&self) -> (usize, u32, u32, Tok, Tok) {
        (
            self.lex.pos,
            self.lex.line,
            self.lex.col,
            self.cur.clone(),
            self.peek.clone(),
        )
    }
    fn restore(&mut self, (pos, line, col, cur, peek): (usize, u32, u32, Tok, Tok)) {
        self.lex.pos = pos;
        self.lex.line = line;
        self.lex.col = col;
        self.cur = cur;
        self.peek = peek;
    }

    fn err_here(&mut self, code: &'static str, msg: impl Into<String>, hint: Option<&str>) {
        let mut d = Diagnostic::err(code, msg, self.cur.span.clone());
        if let Some(h) = hint {
            d.hint = Some(h.into());
        }
        self.diags.push(d);
    }
    fn warn_here(&mut self, code: &'static str, msg: impl Into<String>, hint: Option<&str>) {
        let mut d = Diagnostic::warn(code, msg, self.cur.span.clone());
        if let Some(h) = hint {
            d.hint = Some(h.into());
        }
        self.diags.push(d);
    }
    /// C23 [[nodiscard]] 检查: 如果表达式是对 nodiscard 函数的调用,
    /// 且返回值被丢弃 (作为语句表达式), 发射警告.
    fn check_nodiscard_call(&mut self, e: &Expr) {
        if let Expr::Call { callee, span, .. } = e {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if self.nodiscard_fns.contains(&name.name) {
                    self.diags.push(Diagnostic::warn(
                        "W0401",
                        format!(
                            "ignoring return value of '{}' declared with [[nodiscard]]",
                            name.name
                        ),
                        span.clone(),
                    ));
                }
            }
        }
    }
    /// 严格类型转换检查: 返回 Some(diag) 当出现以下情况:
    ///   1. 整型截断 (例如 (char)0x100, 由 warn_int_truncation 控制)
    ///   2. 整数 ↔ 指针隐式转换 (由 warn_int_conversion 控制)
    ///
    /// 仅在显式 (type)expr 强制转换路径触发; 隐式 cast 走 IR。
    fn check_strict_conversion(
        &self,
        from: &Type,
        to: &Type,
        sp: crate::diagnostics::Span,
    ) -> Option<Diagnostic> {
        use crate::ast::Type::*;
        let from_int_bits = match from {
            Bool | Char | SChar | UChar => Some(8),
            Short | UShort => Some(16),
            Int | UInt | Enum(_, _, _) => Some(32),
            Long | ULong => Some(64),
            LongLong | ULongLong => Some(64),
            Int128 | UInt128 => Some(128),
            _ => None,
        };
        let to_int_bits = match to {
            Bool | Char | SChar | UChar => Some(8),
            Short | UShort => Some(16),
            Int | UInt | Enum(_, _, _) => Some(32),
            Long | ULong => Some(64),
            LongLong | ULongLong => Some(64),
            Int128 | UInt128 => Some(128),
            _ => None,
        };
        // 整数截断: 仅当 const_eval 已可算 + 超出范围
        if let (Some(from_b), Some(to_b)) = (from_int_bits, to_int_bits) {
            if to_b < from_b && self.opts.warn_int_truncation {
                // 看是否紧跟字面量
                return Some(Diagnostic::warn(
                    "W0301",
                    format!(
                        "integer conversion from {} to {} bits may lose information",
                        from_b, to_b
                    ),
                    sp,
                ));
            }
        }
        // 整数 ↔ 指针转换
        let from_is_ptr = matches!(from, Ptr(_) | Func(_, _));
        let to_is_ptr = matches!(to, Ptr(_) | Func(_, _));
        if from_is_ptr != to_is_ptr && self.opts.warn_int_conversion {
            let dir = if from_is_ptr {
                "pointer to integer"
            } else {
                "integer to pointer"
            };
            return Some(Diagnostic::warn(
                "W0302",
                format!(
                    "implicit conversion from {} (use an explicit cast if intended)",
                    dir
                ),
                sp,
            ));
        }
        None
    }
    /// 登记 typedef；GCC 允许兼容或整型别名的重复 typedef（宿主头文件常见）
    fn register_typedef(&mut self, name: &Ident, ty: Type) -> bool {
        if let Some(existing) = self.typedefs.get(&name.name) {
            if Self::types_compatible(existing, &ty) {
                return true;
            }
            if Self::is_integer_typedef(existing) && Self::is_integer_typedef(&ty) {
                self.typedefs.insert(name.name.clone(), ty);
                return true;
            }
            if Self::typedef_redef_allowed(existing, &ty) {
                self.typedefs.insert(name.name.clone(), ty);
                return true;
            }
            self.diags.push(
                Diagnostic::err(
                    "E0005",
                    format!("duplicate typedef '{}'", name.name),
                    name.span.clone(),
                )
                .with_hint("each typedef name may only be defined once"),
            );
            return false;
        }
        self.typedefs.insert(name.name.clone(), ty);
        true
    }
    /// 变量/参数重声明类型冲突报 E0007
    /// M7 修复：locals 是扁平 HashMap，无法区分同函数内嵌套块的
    /// 重声明（合法 C 语义：内层遮蔽外层）。原实现在嵌套块内
    /// 重声明同名变量会误报 E0007。libdecnumber 的 decBasic.c
    /// 在外层 `uInt div[]` 之内的内层块又声明 `uint64_t div=...`
    /// 就属于这种情况。规则：
    ///   - 同类型: 允许
    ///   - 函数体内: 允许 (嵌套块遮蔽 / 遮蔽全局)
    ///   - 文件作用域且不同类型: 报 E0007
    ///     例外: `extern struct gstr x;` (不完整) + `struct gstr x;` (完整
    ///     定义) 是合法 C (从声明变为定义). 跳过该情形.
    fn check_var_redecl(&mut self, name: &str, ty: &Type, span: Span) -> bool {
        if let Some(existing) = self.locals.get(name).or_else(|| self.globals.get(name)) {
            if existing == ty {
                return true;
            }
            // 函数体内允许遮蔽 (嵌套块 / 遮蔽全局)
            if self.in_function {
                return true;
            }
            // 允许同一 struct/union 标签的重声明: C11 6.2.5p22 规定同标签的
            // struct/union 无论是否完整都是同一类型. dcc 的 Type 携带字段列表,
            // 导致 Ptr(Struct(name,[])) != Ptr(Struct(name,fields)), 例如内核
            // sched.h `extern struct pid *cad_pid;` (标签不完整) 之后源文件里
            // `struct pid *cad_pid;` (标签已完整定义) 就会误报. 解包指针/数组
            // 外层后按标签名比较: 同标签且任一方不完整 (或字段相同) 即兼容.
            if Self::tag_compatible(existing, ty) {
                return true;
            }
            // 文件作用域且类型不匹配: 报 E0007
            self.diags.push(
                Diagnostic::err(
                    "E0007",
                    format!("conflicting types for redeclaration of '{}'", name),
                    span,
                )
                .with_hint("use the same type as the previous declaration"),
            );
            return false;
        }
        true
    }
    /// 声明说明符后是否像声明符（非类型名）
    fn peek_decl_context(&self) -> bool {
        matches!(
            self.peek.kind,
            TokKind::Semi | TokKind::Comma | TokKind::Assign | TokKind::LParen | TokKind::Star
        )
    }
    /// typeof 括号内是类型形式还是表达式形式
    fn typeof_operand_is_type_form(&self) -> bool {
        if self.cur.kind == TokKind::Ident {
            match self.cur.text.as_str() {
                "_Generic"
                | "nullptr"
                | "sizeof"
                | "__builtin_choose_expr"
                | "__builtin_offsetof"
                | "offsetof"
                | "__alignof__"
                | "_Alignof"
                | "__alignof"
                | "__builtin_types_compatible_p" => return false,
                _ => {}
            }
            let n = self.cur.text.clone();
            if self.typedefs.contains_key(&n) {
                return true;
            }
            if self.locals.contains_key(&n) || self.globals.contains_key(&n) {
                return false;
            }
            // Enum constants are values, not types
            if self.enum_consts.contains_key(&n) {
                return false;
            }
        }
        if self.is_type_start() {
            return true;
        }
        if self.cur.kind == TokKind::Ident {
            return true;
        }
        false
    }
    fn has_storage_without_base(&self, base: &Option<Type>) -> bool {
        base.is_none()
            && (self.decl_storage.is_static
                || self.decl_storage.is_extern
                || self.decl_storage.is_register)
    }
    fn expect(&mut self, k: TokKind, what: &str) -> bool {
        if self.cur.kind == k {
            self.advance();
            return true;
        }
        // DEBUG: show actual token for line mapping diagnosis
        if std::env::var("DCC_DEBUG_PARSE").is_ok() {
            eprintln!(
                "DEBUG-PARSE: expected {} got {:?} text={:?} line={} col={} file={}",
                what,
                self.cur.kind,
                self.cur.text,
                self.cur.span.line,
                self.cur.span.col,
                self.cur.span.file
            );
        }
        self.err_here(
            "E0001",
            format!("expected {}, got {:?}", what, self.cur.kind),
            Some("check the syntax near this position"),
        );
        false
    }
    /// 将 __attribute__((mode(...))) 的模式名映射为对应的 Type
    /// SI=32bit, DI=64bit, HI=16bit, QI=8bit, word=pointer-sized
    /// SF=float, DF=double, TF=long double, pointer=uintptr
    fn mode_to_type(&self, mode: &str, is_unsigned: bool) -> Option<Type> {
        match mode {
            "QI" => Some(if is_unsigned { Type::UChar } else { Type::SChar }),
            "HI" => Some(if is_unsigned { Type::UShort } else { Type::Short }),
            "SI" => Some(if is_unsigned { Type::UInt } else { Type::Int }),
            "DI" => Some(if is_unsigned { Type::ULong } else { Type::Long }),
            "word" => Some(if is_unsigned { Type::ULong } else { Type::Long }),
            "SF" => Some(Type::Float),
            "DF" => Some(Type::Double),
            "TF" => Some(Type::LongDouble),
            "pointer" => Some(Type::ULong), // pointer-sized uint
            _ => None,
        }
    }
    /// 静默剥离 __attribute__((...)): GNU 扩展属性不影响代码生成
    fn skip_attributes(&mut self) {
        while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.advance(); // __attribute__
            if self.cur.kind == TokKind::LParen {
                self.advance();
            } // (
            if self.cur.kind == TokKind::LParen {
                self.advance();
            } // (
            let mut depth = 2i32;
            while depth > 0 && self.cur.kind != TokKind::Eof {
                if self.cur.kind == TokKind::LParen {
                    depth += 1;
                } else if self.cur.kind == TokKind::RParen {
                    depth -= 1;
                }
                self.advance();
            }
        }
    }

    /// 跳过属性并提取 aligned 值（如果存在）。
    /// 用于字段级 __attribute__((aligned(N))) 解析。
    fn skip_attributes_extract_aligned(&mut self) -> Option<usize> {
        let mut aligned: Option<usize> = None;
        while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.advance(); // __attribute__
            if self.cur.kind == TokKind::LParen {
                self.advance();
            } // (
            if self.cur.kind == TokKind::LParen {
                self.advance();
            } // (
            // 解析内部属性列表
            let mut depth = 2i32;
            while depth > 0 && self.cur.kind != TokKind::Eof {
                // 在 depth==2 时检测 "aligned"
                if depth == 2
                    && self.cur.kind == TokKind::Ident
                    && self.cur.text == "aligned"
                {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance(); // (
                        // 解析对齐值
                        if self.cur.kind == TokKind::Number {
                            let value = parse_int_lit(&self.cur.text);
                            if value > 0 {
                                aligned = Some(value as usize);
                            }
                            self.advance();
                        }
                        // 跳过到匹配的 )
                        let mut inner_depth = 1i32;
                        while inner_depth > 0 && self.cur.kind != TokKind::Eof {
                            if self.cur.kind == TokKind::LParen {
                                inner_depth += 1;
                            } else if self.cur.kind == TokKind::RParen {
                                inner_depth -= 1;
                            }
                            if inner_depth > 0 {
                                self.advance();
                            }
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance(); // )
                        }
                    } else {
                        // __attribute__((aligned)) 无参数默认为 16
                        aligned = Some(16);
                    }
                } else if self.cur.kind == TokKind::LParen {
                    depth += 1;
                    self.advance();
                } else if self.cur.kind == TokKind::RParen {
                    depth -= 1;
                    self.advance(); // 总是前进, 包括最后一个 )
                } else {
                    self.advance();
                }
            }
        }
        aligned
    }

    /// Parse a constant expression inside `__attribute__((aligned(...)))`.
    /// Supports: numeric literals, `sizeof(type)`, `sizeof(expr)`,
    /// and simple binary ops (`*`, `+`, `-`) between them.
    fn parse_aligned_expr(&mut self, depth: &mut i32) -> Option<usize> {
        let result = self.parse_aligned_term(depth);
        // Handle +, -, <<, >> at this level
        loop {
            if self.cur.kind == TokKind::Plus {
                self.advance();
                let rhs = self.parse_aligned_term(depth);
                if let (Some(l), Some(r)) = (result, rhs) {
                    return self.parse_aligned_rest(depth, l + r);
                }
                return self.parse_aligned_rest(depth, result.unwrap_or(0));
            } else if self.cur.kind == TokKind::Minus {
                self.advance();
                let rhs = self.parse_aligned_term(depth);
                if let (Some(l), Some(r)) = (result, rhs) {
                    return self.parse_aligned_rest(depth, l - r);
                }
                return self.parse_aligned_rest(depth, result.unwrap_or(0));
            } else if self.cur.kind == TokKind::Shl {
                self.advance();
                let rhs = self.parse_aligned_term(depth);
                if let (Some(l), Some(r)) = (result, rhs) {
                    return self.parse_aligned_rest(depth, l << r);
                }
                return self.parse_aligned_rest(depth, result.unwrap_or(0));
            } else if self.cur.kind == TokKind::Shr {
                self.advance();
                let rhs = self.parse_aligned_term(depth);
                if let (Some(l), Some(r)) = (result, rhs) {
                    return self.parse_aligned_rest(depth, l >> r);
                }
                return self.parse_aligned_rest(depth, result.unwrap_or(0));
            } else {
                break;
            }
        }
        result
    }

    fn parse_aligned_rest(&mut self, depth: &mut i32, acc: usize) -> Option<usize> {
        if self.cur.kind == TokKind::Plus {
            self.advance();
            let rhs = self.parse_aligned_term(depth);
            return self.parse_aligned_rest(depth, acc + rhs.unwrap_or(0));
        } else if self.cur.kind == TokKind::Minus {
            self.advance();
            let rhs = self.parse_aligned_term(depth);
            return self.parse_aligned_rest(depth, acc - rhs.unwrap_or(0));
        } else if self.cur.kind == TokKind::Shl {
            self.advance();
            let rhs = self.parse_aligned_term(depth);
            return self.parse_aligned_rest(depth, acc << rhs.unwrap_or(0));
        } else if self.cur.kind == TokKind::Shr {
            self.advance();
            let rhs = self.parse_aligned_term(depth);
            return self.parse_aligned_rest(depth, acc >> rhs.unwrap_or(0));
        }
        Some(acc)
    }

    fn parse_aligned_term(&mut self, depth: &mut i32) -> Option<usize> {
        let mut result = self.parse_aligned_atom(depth);
        while self.cur.kind == TokKind::Star || self.cur.kind == TokKind::Slash {
            let op = self.cur.kind;
            self.advance();
            let rhs = self.parse_aligned_atom(depth);
            if let (Some(l), Some(r)) = (result, rhs) {
                result = if op == TokKind::Star { Some(l * r) } else if r != 0 { Some(l / r) } else { Some(l) };
            }
        }
        result
    }

    fn parse_aligned_atom(&mut self, depth: &mut i32) -> Option<usize> {
        // sizeof(type) or sizeof expr
        if self.cur.kind == TokKind::KwSizeof
            || (self.cur.kind == TokKind::Ident && self.cur.text == "sizeof")
        {
            self.advance();
            if self.cur.kind == TokKind::LParen {
                self.advance();
                *depth += 1;
                // Try to parse as sizeof(type)
                let ty = self.parse_type_name();
                let sz = ty.map(|t| t.size()).unwrap_or(8);
                // Consume closing ')'
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                    *depth -= 1;
                }
                return Some(sz);
            }
            return Some(8); // fallback
        }
        // __alignof__(type) or __alignof__ expr
        if self.cur.kind == TokKind::KwAlignof {
            self.advance();
            if self.cur.kind == TokKind::LParen {
                self.advance();
                *depth += 1;
                let ty = self.parse_type_name();
                let al = ty.map(|t| t.align()).unwrap_or(8);
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                    *depth -= 1;
                }
                return Some(al);
            }
            return Some(8); // fallback
        }
        // Numeric literal
        if self.cur.kind == TokKind::Number {
            let v = parse_int_lit(&self.cur.text);
            self.advance();
            return if v > 0 { Some(v as usize) } else { Some(1) };
        }
        // Parenthesized sub-expression
        if self.cur.kind == TokKind::LParen {
            self.advance();
            *depth += 1;
            let v = self.parse_aligned_expr(depth);
            if self.cur.kind == TokKind::RParen {
                self.advance();
                *depth -= 1;
            }
            return v;
        }
        // Unknown token – skip it
        self.advance();
        None
    }

    /// Try to parse a type name (for sizeof(type) inside attributes).
    fn parse_type_name(&mut self) -> Option<Type> {
        // Simple type keyword matching
        let base = match self.cur.text.as_str() {
            "void" => { self.advance(); Some(Type::Void) }
            "char" => { self.advance(); Some(Type::Char) }
            "signed" => {
                self.advance();
                if self.cur.text == "char" { self.advance(); Some(Type::SChar) }
                else if self.cur.text == "int" || self.cur.text == "long" {
                    // signed int / signed long
                    if self.cur.text == "long" {
                        self.advance();
                        if self.cur.text == "long" { self.advance(); Some(Type::LongLong) }
                        else { Some(Type::Long) }
                    } else { self.advance(); Some(Type::Int) }
                } else { Some(Type::Int) }
            }
            "unsigned" => {
                self.advance();
                if self.cur.text == "char" { self.advance(); Some(Type::UChar) }
                else if self.cur.text == "short" { self.advance(); Some(Type::UShort) }
                else if self.cur.text == "long" {
                    self.advance();
                    if self.cur.text == "long" { self.advance(); Some(Type::ULongLong) }
                    else { Some(Type::ULong) }
                } else if self.cur.text == "int" { self.advance(); Some(Type::UInt) }
                else { Some(Type::UInt) }
            }
            "short" => { self.advance(); Some(Type::Short) }
            "int" => { self.advance(); Some(Type::Int) }
            "long" => {
                self.advance();
                if self.cur.text == "long" { self.advance(); Some(Type::LongLong) }
                else if self.cur.text == "double" { self.advance(); Some(Type::LongDouble) }
                else { Some(Type::Long) }
            }
            "__int128" | "__int128_t" => { self.advance(); Some(Type::Int128) }
            "float" => { self.advance(); Some(Type::Float) }
            "double" => { self.advance(); Some(Type::Double) }
            "long" => { self.advance(); Some(Type::Long) }
            _ => {
                // Try struct/union/enum name
                if self.cur.text == "struct" || self.cur.text == "union" || self.cur.text == "enum" {
                    let is_struct = self.cur.text.clone();
                    self.advance();
                    let name = self.cur.text.clone();
                    self.advance();
                    // Look up the type
                    let key = format!("{} {}", is_struct, name);
                    if let Some((fields, is_u, _align)) = self.struct_defs.get(&key) {
                        if *is_u {
                            Some(Type::Union(key, Arc::clone(fields), 1))
                        } else {
                            Some(Type::Struct(key, Arc::clone(fields), 1))
                        }
                    } else {
                        Some(Type::Named(key))
                    }
                } else {
                    // Try typedef name
                    let name = self.cur.text.clone();
                    if let Some(ty) = self.typedefs.get(&name).cloned() {
                        self.advance();
                        Some(ty)
                    } else {
                        None
                    }
                }
            }
        };
        // Handle pointer suffix
        let mut ty = base?;
        while self.cur.kind == TokKind::Star {
            self.advance();
            ty = Type::Ptr(Box::new(ty.into()));
        }
        Some(ty)
    }
    /// C usual arithmetic conversions: determine common type for integer binops
    fn usual_arith_type(a: &Type, b: &Type) -> Type {
        use Type::*;
        let rank = |t: &Type| -> i32 {
            match t {
                Bool | Char | SChar | UChar => 1,
                Short | UShort => 2,
                Int | UInt => 3,
                Long | ULong => 4,
                LongLong | ULongLong => 5,
                Int128 | UInt128 => 6,
                _ => 0,
            }
        };
        let is_uns =
            |t: &Type| -> bool { matches!(t, UChar | UShort | UInt | ULong | ULongLong | UInt128) };
        // integer promotion first
        let a = match a {
            Char | SChar | UChar | Short | UShort => &Int,
            _ => a,
        };
        let b = match b {
            Char | SChar | UChar | Short | UShort => &Int,
            _ => b,
        };
        if std::mem::discriminant(a) == std::mem::discriminant(b) {
            return a.clone();
        }
        let ra = rank(a);
        let rb = rank(b);
        let (higher, lower) = if ra >= rb { (a, b) } else { (b, a) };
        if is_uns(higher) {
            return higher.clone();
        }
        if !is_uns(lower) {
            return higher.clone();
        }
        if rank(higher) > rank(lower) {
            return higher.clone();
        }
        match higher {
            Long => ULong,
            LongLong => ULongLong,
            _ => higher.clone(),
        }
    }

    /// 去除字符串字面量两端的引号
    fn strip_quotes(s: &str) -> String {
        let s = s.trim();
        if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
            s[1..s.len() - 1].to_string()
        } else {
            s.to_string()
        }
    }
    fn decode_string_literal(s: &str) -> String {
        let mut bytes = parse_str(s);
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
    /// 读取一个或多个相邻字符串字面量并拼接（GCC section 等属性常见）
    fn parse_concatenated_string_lit(&mut self) -> Option<String> {
        if self.cur.kind != TokKind::StringLit {
            return None;
        }
        let mut s = Self::strip_quotes(&self.cur.text);
        self.advance();
        while self.cur.kind == TokKind::StringLit {
            s.push_str(&Self::strip_quotes(&self.cur.text));
            self.advance();
        }
        Some(s)
    }
    /// 解析 __attribute__((...)) 并提取属性信息
    fn parse_attributes(&mut self) -> ParsedAttrs {
        let mut attrs = ParsedAttrs::default();
        while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.advance(); // __attribute__
            if self.cur.kind == TokKind::LParen {
                self.advance();
            } // (
            if self.cur.kind == TokKind::LParen {
                self.advance();
            } // (
              // 解析内部属性列表
            while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                let name = self.cur.text.trim_matches('_').to_string();
                match name.as_str() {
                    "section" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(section) = self.parse_concatenated_string_lit() {
                                attrs.section = Some(section);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "aligned" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen {
                                    depth += 1;
                                } else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 {
                                        break;
                                    }
                                    continue;
                                } else if depth == 1 && self.cur.kind == TokKind::Number {
                                    let value = parse_int_lit(&self.cur.text);
                                    if value > 0 {
                                        attrs.alignment = Some(value as usize);
                                    }
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof {
                                    break;
                                }
                            }
                        } else {
                            attrs.alignment = Some(16); // __attribute__((aligned)) 无参数默认16
                        }
                    }
                    "weak" => {
                        attrs.is_weak = true;
                        self.advance();
                    }
                    "visibility" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(visibility) = self.parse_concatenated_string_lit() {
                                attrs.visibility = Some(visibility);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "unused" => {
                        attrs.is_unused = true;
                        self.advance();
                    }
                    "cold" => {
                        attrs.is_cold = true;
                        self.advance();
                    }
                    "noinline" => {
                        attrs.is_noinline = true;
                        self.advance();
                    }
                    "always_inline" => {
                        attrs.is_always_inline = true;
                        self.advance();
                    }
                    "noreturn" | "Noreturn" => {
                        // noreturn / __noreturn__ 接受 trim 后的形式
                        // _Noreturn 在 trim 后变成 Noreturn (单下划线不匹配)
                        // 因为 stdnoreturn.h 的 noreturn 宏会被预处理器展开为 _Noreturn
                        attrs.is_noreturn = true;
                        self.advance();
                    }
                    "cleanup" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Ident {
                                attrs.cleanup_fn = Some(self.cur.text.clone());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "constructor" => {
                        attrs.is_constructor = true;
                        self.advance();
                        // optional priority: __attribute__((constructor(100)))
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Number {
                                let v = parse_int_lit(&self.cur.text);
                                attrs.ctor_priority = Some(v as u16);
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "destructor" => {
                        attrs.is_destructor = true;
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Number {
                                let v = parse_int_lit(&self.cur.text);
                                attrs.dtor_priority = Some(v as u16);
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "alias" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(alias) = self.parse_concatenated_string_lit() {
                                attrs.alias_name = Some(alias);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "format" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Ident {
                                attrs.format_kind = Some(self.cur.text.clone());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Comma {
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Number {
                                attrs.format_string_idx =
                                    parse_int_lit(&self.cur.text).try_into().ok();
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Comma {
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Number {
                                attrs.format_first_arg =
                                    parse_int_lit(&self.cur.text).try_into().ok();
                                self.advance();
                            }
                            while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof
                            {
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "no_sanitize" | "no_sanitize_address" | "no_address_safety_analysis" => {
                        // __attribute__((no_sanitize("address", "undefined")))
                        // __attribute__((no_sanitize_address))
                        // __attribute__((no_address_safety_analysis))
                        if name == "no_sanitize" {
                            self.advance();
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                while self.cur.kind != TokKind::RParen
                                    && self.cur.kind != TokKind::Eof
                                {
                                    if self.cur.kind == TokKind::StringLit {
                                        let s = Self::strip_quotes(&self.cur.text);
                                        if !s.is_empty() {
                                            attrs.no_sanitize.push(s);
                                        }
                                        self.advance();
                                    } else {
                                        self.advance();
                                    }
                                    if self.cur.kind == TokKind::Comma {
                                        self.advance();
                                    }
                                }
                                if self.cur.kind == TokKind::RParen {
                                    self.advance();
                                }
                            }
                        } else {
                            attrs.no_sanitize.push(name.clone());
                            self.advance();
                        }
                    }
                    "target_clones" => {
                        // __attribute__((target_clones("default,avx2,avx512f")))
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof
                            {
                                if self.cur.kind == TokKind::StringLit {
                                    let s = Self::strip_quotes(&self.cur.text);
                                    if !s.is_empty() {
                                        // split by comma
                                        for part in s.split(',') {
                                            let part = part.trim();
                                            if !part.is_empty() {
                                                attrs.target_clones.push(part.to_string());
                                            }
                                        }
                                    }
                                    self.advance();
                                } else {
                                    self.advance();
                                }
                                if self.cur.kind == TokKind::Comma {
                                    self.advance();
                                }
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "copy" => {
                        // __attribute__((copy("fn"))) — 函数/变量符号别名
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(s) = self.parse_concatenated_string_lit() {
                                attrs.copy_fn = Some(s);
                            } else if self.cur.kind == TokKind::Ident {
                                attrs.copy_fn = Some(self.cur.text.clone());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "returns_nonnull" => {
                        attrs.returns_nonnull = true;
                        self.advance();
                        // optional (): __attribute__((returns_nonnull))
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen {
                                    depth += 1;
                                } else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 {
                                        break;
                                    }
                                    continue;
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof {
                                    break;
                                }
                            }
                        }
                    }
                    "retain" => {
                        attrs.is_retain = true;
                        self.advance();
                    }
                    "used" => {
                        attrs.is_used = true;
                        self.advance();
                    }
                    "flatten" => {
                        attrs.is_flatten = true;
                        self.advance();
                    }
                    "error" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(msg) = self.parse_concatenated_string_lit() {
                                attrs.error_msg = Some(msg);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "warning" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(msg) = self.parse_concatenated_string_lit() {
                                attrs.warning_msg = Some(msg);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "deprecated" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(msg) = self.parse_concatenated_string_lit() {
                                attrs.deprecated_msg = Some(msg);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "ifunc" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(resolver) = self.parse_concatenated_string_lit() {
                                attrs.ifunc_resolver = Some(resolver);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "asm_output" | "__asm_output__" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if let Some(tmpl) = self.parse_concatenated_string_lit() {
                                attrs.asm_output_template = Some(tmpl);
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    "vector_size" | "__vector_size__" => {
                        self.advance(); // vector_size
                        if self.cur.kind == TokKind::LParen {
                            self.advance(); // (
                            if self.cur.kind == TokKind::Number {
                                let v: usize = parse_int_lit(&self.cur.text) as usize;
                                attrs.vector_size = Some(v);
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            } // )
                        }
                    }
                    // __attribute__((naked)): 不生成 prologue/epilogue
                    "naked" => {
                        attrs.is_naked = true;
                        self.advance();
                    }
                    // __attribute__((may_alias)): 类型属性，禁用 TBAA
                    "may_alias" => {
                        attrs.may_alias = true;
                        self.advance();
                    }
                    // __attribute__((packed/__packed__))
                    "packed" => {
                        attrs.is_packed = true;
                        self.advance();
                    }
                    // __attribute__((tls_model("model")))
                    // TLS 访问模型: "global-dynamic", "local-dynamic",
                    // "initial-exec", "local-exec"
                    "tls_model" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance(); // (
                            if self.cur.kind == TokKind::StringLit {
                                attrs.tls_model = Some(
                                    Self::decode_string_literal(&self.cur.text),
                                );
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance(); // )
                            }
                        }
                    }
                    // CUDA/HIP 属性拼写 (__attribute__((global)) 等形式):
                    // 只在 -fcuda / -fhip 时生效, 否则按未知属性跳过
                    "global" | "device" | "host" if self.opts.cuda || self.opts.hip => {
                        match name.as_str() {
                            "global" => attrs.is_global = true,
                            "device" => attrs.is_device = true,
                            _ => attrs.is_host = true,
                        }
                        self.advance();
                    }
                    // __attribute__((hot)): 热函数, 放入 .text.hot 段
                    "hot" => {
                        attrs.is_hot = true;
                        self.advance();
                    }
                    // __attribute__((pure)): 无副作用函数
                    "pure" => {
                        attrs.is_pure = true;
                        self.advance();
                    }
                    // __attribute__((const)): 纯函数 (不读全局内存)
                    "const" => {
                        attrs.is_const_attr = true;
                        self.advance();
                    }
                    // __attribute__((returns_twice)): 可多次返回 (setjmp 等)
                    "returns_twice" => {
                        attrs.returns_twice = true;
                        self.advance();
                    }
                    // __attribute__((no_instrument_function)):
                    // 禁止 profiling/fuzzer 插桩
                    "no_instrument_function" => {
                        attrs.no_instrument = true;
                        self.advance();
                    }
                    // 以下属性仅解析消费, 无 codegen 效果:
                    // noclone / noipa / nocf_check
                    "noclone" | "noipa" | "nocf_check" => {
                        self.advance();
                    }
                    // __attribute__((malloc)) 或 __attribute__((malloc (dealloc, argno))):
                    // 返回指针不别名已有内存; GCC11+ 支持带释放函数参数
                    "malloc" => {
                        attrs.is_malloc = true;
                        self.advance();
                        // 解析可选的 (dealloc_fn, argno) 参数
                        if self.cur.kind == TokKind::LParen {
                            self.advance(); // (
                            // 第一个参数: 释放函数名 (标识符)
                            if self.cur.kind == TokKind::Ident
                                || self.cur.kind == TokKind::Star // 容错
                            {
                                attrs.malloc_dealloc = Some(self.cur.text.clone());
                                self.advance();
                            }
                            // 第二个参数: 参数序号 (数字)
                            if self.cur.kind == TokKind::Comma {
                                self.advance(); // ,
                                if self.cur.kind == TokKind::Number {
                                    attrs.malloc_dealloc_argno =
                                        Some(parse_int_lit(&self.cur.text) as u32);
                                    self.advance();
                                }
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance(); // )
                            }
                        }
                    }
                    // __attribute__((alloc_size(N[,M]))): 分配大小参数索引
                    "alloc_size" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            let mut idx1 = 0u32;
                            let mut idx2: Option<u32> = None;
                            if self.cur.kind == TokKind::Number {
                                idx1 = parse_int_lit(&self.cur.text) as u32;
                                self.advance();
                            }
                            if self.cur.kind == TokKind::Comma {
                                self.advance();
                                if self.cur.kind == TokKind::Number {
                                    idx2 = Some(parse_int_lit(&self.cur.text) as u32);
                                    self.advance();
                                }
                            }
                            attrs.alloc_size = Some((idx1, idx2));
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    // __attribute__((assume_aligned(N))): 返回指针对齐
                    // N can be a number, sizeof(type), __alignof__(type),
                    // or a simple arithmetic expression of these.
                    "assume_aligned" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            let mut depth = 0i32;
                            attrs.assume_aligned = self.parse_aligned_expr(&mut depth);
                            // skip to closing RParen
                            while self.cur.kind != TokKind::RParen
                                && self.cur.kind != TokKind::Eof
                            {
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    // __attribute__((warn_unused_result))
                    "warn_unused_result" => {
                        attrs.warn_unused_result = true;
                        self.advance();
                    }
                    // __attribute__((sentinel)): 可变参需 NULL 结尾
                    "sentinel" => {
                        attrs.is_sentinel = true;
                        self.advance();
                        // optional (N) argument
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen {
                                    depth += 1;
                                } else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 { break; }
                                    continue;
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof { break; }
                            }
                        }
                    }
                    // __attribute__((format_arg(N)))
                    "format_arg" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            if self.cur.kind == TokKind::Number {
                                attrs.format_arg_idx =
                                    Some(parse_int_lit(&self.cur.text) as u32);
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    // __attribute__((transparent_union)): struct 字段
                    // 作为透明联合处理
                    "transparent_union" => {
                        attrs.transparent_union = true;
                        self.advance();
                    }
                    // __attribute__((externally_visible)): 阻止 LTO
                    // 将符号转为 hidden visibility
                    "externally_visible" => {
                        attrs.externally_visible = true;
                        self.advance();
                    }
                    // 以下属性仅消费, 无 codegen 效果:
                    "no_reorder" | "no_split_stack" => {
                        self.advance();
                    }
                    "optimize" | "patchable_function_entry" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen {
                                    depth += 1;
                                } else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 {
                                        break;
                                    }
                                    continue;
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof {
                                    break;
                                }
                            }
                        }
                    }
                    // __attribute__((mode(__DI__))): 类型模式映射
                    "mode" | "__mode__" => {
                        self.advance();
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            // 模式名可以是 __DI__/DI 等标识符
                            if self.cur.kind == TokKind::Ident {
                                attrs.mode_name =
                                    Some(self.cur.text.trim_matches('_').to_string());
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                        }
                    }
                    _ => {
                        self.advance();
                        // 跳过未知属性的完整参数列表，不能把其内部右括号
                        // 误当成 __attribute__((...)) 的结束。
                        if self.cur.kind == TokKind::LParen {
                            let mut depth = 0i32;
                            loop {
                                if self.cur.kind == TokKind::LParen {
                                    depth += 1;
                                } else if self.cur.kind == TokKind::RParen {
                                    depth -= 1;
                                    self.advance();
                                    if depth == 0 {
                                        break;
                                    }
                                    continue;
                                }
                                self.advance();
                                if self.cur.kind == TokKind::Eof {
                                    break;
                                }
                            }
                        }
                    }
                }
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                }
            }
            if self.cur.kind == TokKind::RParen {
                self.advance();
            } // )
            if self.cur.kind == TokKind::RParen {
                self.advance();
            } // )
        }
        attrs
    }
    /// 合并两组属性 (b 覆盖 a 中的同名字段)
    fn merge_attrs(a: ParsedAttrs, b: ParsedAttrs) -> ParsedAttrs {
        let mut r = a;
        r.merge(b);
        r
    }
    /// 解析 C23 标准属性 [[attr]]: 提取语义效果而非静默跳过
    /// 支持: [[deprecated]], [[deprecated("msg")]], [[fallthrough]],
    ///        [[nodiscard]], [[maybe_unused]], [[noreturn]],
    ///        [[unsequenced]], [[reproducible]]
    fn parse_std_attributes(&mut self) {
        while self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
            self.advance(); // [
            self.advance(); // [
            // 收集属性内容
            let mut attr_text = String::new();
            let mut depth = 2i32;
            while depth > 0 && self.cur.kind != TokKind::Eof {
                if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                    depth += 2;
                    self.advance();
                    self.advance();
                    continue;
                }
                if self.cur.kind == TokKind::RBracket && self.peek.kind == TokKind::RBracket {
                    depth -= 2;
                    if depth == 0 {
                        self.advance(); // ]
                        break;
                    }
                    self.advance();
                    self.advance();
                    continue;
                }
                if !attr_text.is_empty() || self.cur.kind != TokKind::Comma {
                    attr_text.push_str(&self.cur.text);
                    attr_text.push(' ');
                }
                self.advance();
            }
            if self.cur.kind == TokKind::RBracket {
                self.advance(); // ]
            }
            // 解析已收集的属性文本并应用效果
            let attr_trimmed = attr_text.trim();
            if attr_trimmed.starts_with("deprecated") {
                // [[deprecated]] or [[deprecated("msg")]]
                let msg = if attr_trimmed.contains('(') {
                    let start = attr_trimmed.find('"').unwrap_or(0) + 1;
                    let end = attr_trimmed.rfind('"').unwrap_or(attr_trimmed.len());
                    if start <= end {
                        attr_trimmed[start..end].to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                self.decl_attrs.deprecated_msg = Some(if msg.is_empty() {
                    "deprecated".to_string()
                } else {
                    msg
                });
            } else if attr_trimmed == "fallthrough" {
                // [[fallthrough]]: 标记 switch case 有意贯穿
                // 记录属性, 用于 switch case 末尾验证
                self.decl_attrs.is_fallthrough = true;
            } else if attr_trimmed == "nodiscard" {
                // [[nodiscard]]: 返回值不应被忽略
                // 映射到 FuncDecl.is_nodiscard, 调用结果被丢弃时发射警告
                self.decl_attrs.is_nodiscard = true;
            } else if attr_trimmed == "maybe_unused" {
                // [[maybe_unused]]: 抑制未使用警告
                self.decl_attrs.is_unused = true;
            } else if attr_trimmed == "noreturn" {
                // [[noreturn]]: 函数不返回
                self.decl_attrs.is_noreturn = true;
            } else if attr_trimmed == "unsequenced" {
                // [[unsequenced]]: C23 函数无副作用且结果仅依赖参数
                // 等价于 always_inline 优化提示
                self.decl_attrs.is_always_inline = true;
            } else if attr_trimmed == "reproducible" {
                // [[reproducible]]: C23 函数结果仅依赖参数, 允许通过指针参数的副作用
                // 等价于 always_inline 优化提示
                self.decl_attrs.is_always_inline = true;
            }
        }
    }

    /// 剥离 __asm__("label"): GNU asm 标签, 不影响代码生成
    fn skip_asm_label(&mut self) -> Option<String> {
        if self.cur.kind == TokKind::Ident
            && (self.cur.text == "__asm__" || self.cur.text == "__asm" || self.cur.text == "asm")
            && self.peek.kind == TokKind::LParen
        {
            self.advance(); // asm
            self.advance(); // (
            let mut label = String::new();
            while self.cur.kind == TokKind::StringLit {
                label.push_str(&Self::decode_string_literal(&self.cur.text));
                self.advance();
            }
            if label.is_empty() {
                if self.cur.kind == TokKind::Ident {
                    // 宏尚未展开为字符串: asm(__ASM_FORM_RAW) / asm(rsp)
                    label = self.cur.text.clone();
                    self.advance();
                } else {
                    // 复杂实参 (如 __stringify(rsp)): 跳过平衡括号
                    let mut depth = 1i32;
                    while depth > 0 && self.cur.kind != TokKind::Eof {
                        if self.cur.kind == TokKind::LParen {
                            depth += 1;
                        }
                        if self.cur.kind == TokKind::RParen {
                            depth -= 1;
                            if depth == 0 {
                                self.advance();
                                break;
                            }
                        }
                        self.advance();
                    }
                }
            }
            if self.cur.kind == TokKind::RParen {
                self.advance();
            }
            return Some(label);
        }
        None
    }

    fn skip_post_decl_modifiers(&mut self) -> ParsedAttrs {
        let mut attrs = ParsedAttrs::default();
        loop {
            if let Some(label) = self.skip_asm_label() {
                attrs.asm_label = Some(label);
                continue;
            }
            if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                attrs = Self::merge_attrs(attrs, self.parse_attributes());
                continue;
            }
            break;
        }
        attrs
    }

    fn note_implicit_function_call(&mut self, name: &Ident) {
        if self.locals.contains_key(&name.name)
            || self.globals.contains_key(&name.name)
            || self.typedefs.contains_key(&name.name)
            || self.enum_consts.contains_key(&name.name)
            || name.name.starts_with("__builtin_")
            || name.name.starts_with("__atomic_")
            || name.name.starts_with("__sync_")
            || name.name.starts_with("__compiletime_assert_")
        {
            return;
        }
        if self.opts.allow_implicit_function_decl {
            if self.opts.warn_implicit_decl {
                self.diags.push(
                    Diagnostic::warn(
                        "W1000",
                        format!("implicit declaration of '{}' (assumed int)", name.name),
                        name.span.clone(),
                    )
                    .with_hint("add a declaration before use"),
                );
            }
        } else {
            self.diags.push(
                Diagnostic::err(
                    "E0017",
                    format!("implicit declaration of function '{}'", name.name),
                    name.span.clone(),
                )
                .with_hint("declare the function before use or pass -fpermissive"),
            );
        }
    }

    fn lookup_ident_type(&self, name: &str) -> Type {
        if let Some(ty) = self.locals.get(name) {
            return ty.clone();
        }
        if let Some(ty) = self.globals.get(name) {
            return ty.clone();
        }
        if let Some(ty) = self.typedefs.get(name) {
            return ty.clone();
        }
        int_t()
    }

    fn call_result_type(&self, callee: &Expr) -> Type {
        // __builtin_fma returns double, __builtin_fmaf returns float, __builtin_fmal returns long double
        if let Expr::Ident { name, .. } = callee {
            match name.name.as_str() {
                "__builtin_fma" => return Type::Double,
                "__builtin_fmaf" => return Type::Float,
                "__builtin_fmal" => return Type::LongDouble,
                _ => {}
            }
        }
        match callee.ty() {
            Type::Func(ret, _) => ret.base.clone(),
            Type::Ptr(inner) => match inner.base {
                Type::Func(ret, _) => ret.base.clone(),
                ref other => other.clone(),
            },
            other => other,
        }
    }

    fn type_of_member(&self, base: &Expr, field: &Ident, is_arrow: bool) -> Type {
        let aggregate = if is_arrow {
            match base.ty() {
                Type::Ptr(inner) => inner.base.clone(),
                other => other,
            }
        } else {
            base.ty()
        };
        match aggregate {
            Type::Struct(ref name, ref fields, _) | Type::Union(ref name, ref fields, _) => {
                // 仅当嵌入字段为空（前向声明）时才查 struct_defs；
                // 匿名 struct 名同为 ""，不能覆盖已嵌入的正确字段。
                let resolved = if fields.is_empty() {
                    self.struct_defs
                        .get(name)
                        .map(|(f, _, _)| f.as_ref())
                        .unwrap_or(fields.as_ref())
                } else {
                    fields.as_ref()
                };
                find_field(resolved, &field.name)
                    .map(|(found, _)| found.ty.clone())
                    .unwrap_or_else(int_t)
            }
            _ => int_t(),
        }
    }

    fn index_result_type(base: &Expr) -> Type {
        match base.ty() {
            Type::Ptr(inner) | Type::Array(inner, _) => inner.base.clone(),
            Type::Vector(inner, _) => inner.base.clone(),
            _ => int_t(),
        }
    }

    /// 探测 `asm [qualifiers]* goto` 而不消耗 token
    fn probe_asm_goto(&mut self) -> bool {
        if !matches!(self.cur.text.as_str(), "__asm__" | "__asm" | "asm") {
            return false;
        }
        let bm = self.bookmark();
        self.advance();
        self.skip_asm_qualifiers();
        let is_goto = self.cur.kind == TokKind::KwGoto
            || (self.cur.kind == TokKind::Ident && self.cur.text == "goto");
        self.restore(bm);
        is_goto
    }

    /// 跳过 asm/__asm__ 与括号之间的限定符（volatile、inline 等）
    /// 返回是否包含 volatile 限定符
    fn skip_asm_qualifiers(&mut self) -> bool {
        let mut is_volatile = false;
        loop {
            match self.cur.kind {
                TokKind::KwVolatile | TokKind::KwConst => {
                    is_volatile = true;
                    self.advance();
                }
                TokKind::KwInline => {
                    self.advance();
                }
                TokKind::Ident
                    if matches!(
                        self.cur.text.as_str(),
                        "__volatile__" | "__volatile" | "__const__" | "__const" | "__inline__" | "__inline" | "inline"
                    ) =>
                {
                    if self.cur.text == "__volatile__" || self.cur.text == "__volatile"
                        || self.cur.text == "__const__" || self.cur.text == "__const" {
                        is_volatile = true;
                    }
                    self.advance();
                }
                _ => break,
            }
        }
        is_volatile
    }

    /// 解析内联汇编表达式: asm("..." : outs : ins : clobbers)
    fn parse_asm_expr(&mut self, sp: Span, is_volatile: bool) -> Option<Expr> {
        if self.cur.kind == TokKind::LParen {
            self.advance();
        } else {
            self.err_here(
                "E0001",
                "expected '(' after asm",
                Some("asm requires parentheses: asm(\"instructions\" : ...)"),
            );
            return None;
        }
        let mut asm_str = String::new();
        while self.cur.kind == TokKind::StringLit {
            asm_str.push_str(&Self::decode_string_literal(&self.cur.text));
            self.advance();
        }
        let mut outputs = vec![];
        let mut output_names = vec![];
        let mut output_constraints = vec![];
        let mut inputs = vec![];
        let mut input_names = vec![];
        let mut input_constraints = vec![];
        let mut clobbers = vec![];
        if self.cur.kind == TokKind::Colon {
            self.advance();
            let (o, n, c) = self.parse_asm_operand_list();
            outputs = o;
            output_names = n;
            output_constraints = c;
        }
        if self.cur.kind == TokKind::Colon {
            self.advance();
            let (o, n, c) = self.parse_asm_operand_list();
            inputs = o;
            input_names = n;
            input_constraints = c;
        }
        if self.cur.kind == TokKind::Colon {
            self.advance();
            clobbers = self.parse_asm_clobber_list();
        }
        if self.cur.kind == TokKind::RParen {
            self.advance();
        }
        self.validate_asm_semantics(
            &asm_str,
            &outputs,
            &output_names,
            &output_constraints,
            &inputs,
            &input_names,
            &input_constraints,
            &sp,
        );
        Some(Expr::AsmExpr {
            asm_str,
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            volatile: is_volatile,
            span: sp,
            ty: Type::Void,
        })
    }
    /// 解析 asm 操作数列表 (逗号分隔的表达式, 到冒号或右括号结束)
    fn parse_asm_operand_list(&mut self) -> (Vec<Expr>, Vec<Option<String>>, Vec<String>) {
        let mut ops = vec![];
        let mut names = vec![];
        let mut constraints = vec![];
        while self.cur.kind != TokKind::Colon
            && self.cur.kind != TokKind::RParen
            && self.cur.kind != TokKind::Eof
        {
            let name = if self.cur.kind == TokKind::LBracket {
                self.advance();
                let mut n = String::new();
                while self.cur.kind != TokKind::RBracket && self.cur.kind != TokKind::Eof {
                    if !n.is_empty() {
                        n.push(' ');
                    }
                    n.push_str(&self.cur.text);
                    self.advance();
                }
                if self.cur.kind == TokKind::RBracket {
                    self.advance();
                }
                Some(n.trim().to_string())
            } else {
                None
            };
            names.push(name);
            // GCC 约束字符串，实际表达式位于随后的括号中。
            let constraint = if self.cur.kind == TokKind::StringLit {
                let c = Self::decode_string_literal(&self.cur.text);
                self.advance();
                c
            } else {
                String::new()
            };
            constraints.push(constraint);
            let expr = if self.cur.kind == TokKind::LParen {
                self.advance();
                let e = self.parse_expr_bp(2);
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                e
            } else {
                self.parse_expr_bp(2)
            };
            if let Some(e) = expr {
                ops.push(e);
            } else {
                break;
            }
            if self.cur.kind == TokKind::Comma {
                self.advance();
            }
        }
        (ops, names, constraints)
    }
    /// 解析 asm clobber 列表 (逗号分隔的字符串)
    fn parse_asm_clobber_list(&mut self) -> Vec<String> {
        let mut clobbers = vec![];
        while self.cur.kind != TokKind::Colon
            && self.cur.kind != TokKind::RParen
            && self.cur.kind != TokKind::Eof
        {
            clobbers.push(Self::strip_quotes(&self.cur.text));
            self.advance();
            if self.cur.kind == TokKind::Comma {
                self.advance();
            }
        }
        clobbers
    }
    /// 解析 asm goto label 列表 (逗号分隔的标识符)
    fn parse_asm_label_list(&mut self) -> Vec<Ident> {
        let mut labels = vec![];
        while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
            if let Some(id) = self.parse_ident() {
                labels.push(id);
            }
            if self.cur.kind == TokKind::Comma {
                self.advance();
            }
        }
        labels
    }

    /// 验证 asm goto 模板中的 `%lN` / `%l[name]` 标签引用。
    ///
    /// 这些诊断必须保持 warning: Linux 内核等代码可能通过配置或
    /// 宏展开产生暂不可解析的 asm goto 标签引用，dcc 应继续解析。
    fn validate_asm_goto_labels(&mut self, asm_str: &str, goto_labels: &[Ident], op_count: usize) {
        use std::collections::HashSet;

        let label_names: HashSet<&str> = goto_labels.iter().map(|l| l.name.as_str()).collect();
        let label_count = goto_labels.len();
        let mut pos = 0;
        let bytes = asm_str.as_bytes();

        while pos < bytes.len() {
            if bytes[pos] != b'%' {
                pos += 1;
                continue;
            }

            pos += 1;
            if pos >= bytes.len() {
                break;
            }
            if bytes[pos] == b'%' {
                pos += 1;
                continue;
            }
            if bytes[pos] != b'l' {
                pos += 1;
                continue;
            }

            pos += 1;
            if pos < bytes.len() && bytes[pos] == b'[' {
                // %l[name]: 命名标签引用
                pos += 1;
                let start = pos;
                while pos < bytes.len() && bytes[pos] != b']' {
                    pos += 1;
                }
                let name = &asm_str[start..pos];
                if pos < bytes.len() {
                    pos += 1; // skip ']'
                }
                if !name.is_empty() && !label_names.contains(name) {
                    self.warn_here(
                        "W0004",
                        format!(
                            "asm goto references label '{}' which is not in the label list",
                            name
                        ),
                        Some("add the label to the goto label list"),
                    );
                }
            } else if pos < bytes.len() && bytes[pos].is_ascii_digit() {
                // %lN: 数字标签引用。支持 dcc 历史兼容的 label-local
                // 索引 N，也支持 GCC 的全操作数索引 op_count + N。
                let start = pos;
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                let idx_text = &asm_str[start..pos];
                let in_range = idx_text.parse::<usize>().ok().is_some_and(|n| {
                    n < label_count || n.checked_sub(op_count).is_some_and(|i| i < label_count)
                });
                if !in_range {
                    self.warn_here(
                        "W0004",
                        format!(
                            "asm goto references label index {} which is out of range (have {} labels)",
                            idx_text, label_count
                        ),
                        Some("check the label index or add missing labels"),
                    );
                }
            }
        }
    }

    /// 解析 GCC asm 约束前缀 (= 输出, + 读写, & early-clobber)
    /// 返回 (is_output, is_inout, early_clobber, match_digit)
    /// match_digit: Some(n) 表示 "与第 n 个操作数共用寄存器", None 表示无
    fn parse_asm_constraint_flags(constraint: &str) -> (bool, bool, bool, Option<u32>) {
        let mut is_output = false;
        let mut is_inout = false;
        let mut early = false;
        let mut match_digit: Option<u32> = None;
        let mut s = constraint.trim();
        while !s.is_empty() {
            match s.chars().next() {
                Some('=') => {
                    is_output = true;
                    s = &s[1..];
                }
                Some('+') => {
                    is_output = true;
                    is_inout = true;
                    s = &s[1..];
                }
                Some('&') => {
                    early = true;
                    s = &s[1..];
                }
                Some('%') => {
                    s = &s[1..];
                }
                Some(c) if c.is_ascii_digit() => {
                    let mut val: u32 = 0;
                    while let Some(d) = s.chars().next() {
                        if !d.is_ascii_digit() {
                            break;
                        }
                        val = val.wrapping_mul(10).wrapping_add(d.to_digit(10).unwrap());
                        s = &s[1..];
                    }
                    match_digit = Some(val);
                }
                _ => break,
            }
        }
        (is_output, is_inout, early, match_digit)
    }

    /// 验证 asm 操作数的语义约束 (约束前缀、重名、类型匹配、占位符数量)
    #[allow(clippy::too_many_arguments)]
    fn validate_asm_semantics(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        sp: &Span,
    ) {
        use std::collections::HashSet;
        let output_count = outputs.len();
        let total_ops = output_count + inputs.len();

        // 0.0 约束数组 vs 表达式数组长度不匹配 (防御性: parse_asm_operand_list 异常分支)
        if output_constraints.len() != output_names.len() || output_names.len() != output_count {
            let expected = output_count;
            if output_constraints.len() != expected {
                self.diags.push(Diagnostic::err(
                    "E0014",
                    format!(
                        "asm output constraint count ({}) != output operand count ({})",
                        output_constraints.len(),
                        expected
                    ),
                    sp.clone(),
                ));
            }
            if output_names.len() != expected {
                self.diags.push(Diagnostic::err(
                    "E0014",
                    format!(
                        "asm output name count ({}) != output operand count ({})",
                        output_names.len(),
                        expected
                    ),
                    sp.clone(),
                ));
            }
        }
        let input_count = inputs.len();
        if input_constraints.len() != input_names.len() || input_names.len() != input_count {
            if input_constraints.len() != input_count {
                self.diags.push(Diagnostic::err(
                    "E0014",
                    format!(
                        "asm input constraint count ({}) != input operand count ({})",
                        input_constraints.len(),
                        input_count
                    ),
                    sp.clone(),
                ));
            }
            if input_names.len() != input_count {
                self.diags.push(Diagnostic::err(
                    "E0014",
                    format!(
                        "asm input name count ({}) != input operand count ({})",
                        input_names.len(),
                        input_count
                    ),
                    sp.clone(),
                ));
            }
        }

        // 0. 匹配约束 (matching constraint) 数字越界检测 (W0008)
        //    输入约束中的数字 (如 "0", "1") 表示与对应编号操作数共用寄存器.
        //    mini-asm 兼容策略: 这些原本由 GCC hard error 的非法操作数
        //    不阻断编译, 交给后端 warning + nop fallback 继续发射.
        for (input_idx, constraint) in input_constraints.iter().enumerate() {
            let (_, _, _, match_digit) = Self::parse_asm_constraint_flags(constraint);
            if let Some(n) = match_digit {
                let n_usize = n as usize;
                if n_usize >= output_count {
                    self.diags.push(Diagnostic::warn(
                        "W0008",
                        format!(
                            "matching constraint references invalid operand number '{}' for input operand {}",
                            n, input_idx
                        ),
                        sp.clone(),
                    ).with_hint(
                        if output_count == 0 {
                            "no output operands defined; matching constraints normally require outputs".into()
                        } else {
                            format!(
                                "matching constraint digits normally reference an output operand (0 to {}); use a regular constraint if you intended an independent input",
                                output_count.saturating_sub(1)
                            )
                        }
                    ));
                }
                if n_usize >= total_ops {
                    self.diags.push(Diagnostic::warn(
                        "W0008",
                        format!(
                            "matching constraint references operand {} but only {} operand(s) provided",
                            n, total_ops
                        ),
                        sp.clone(),
                    ).with_hint(
                        "add the missing operand or fix the matching constraint digit"
                    ));
                }
            }
        }
        // 输出约束中的匹配约束: 输出本身不应带匹配数字.
        for (output_idx, constraint) in output_constraints.iter().enumerate() {
            let (_, _, _, match_digit) = Self::parse_asm_constraint_flags(constraint);
            if let Some(n) = match_digit {
                self.diags.push(Diagnostic::warn(
                    "W0008",
                    format!(
                        "matching constraint digit '{}' not allowed in output operand {} constraint",
                        n, output_idx
                    ),
                    sp.clone(),
                ).with_hint(
                    "output operands already define their own number; use '+' for read-write output instead"
                ));
            }
        }

        // 1. 输出约束必须含 '=' 或 '+' 前缀 (E0014)
        for (i, c) in output_constraints.iter().enumerate() {
            let stripped = c.trim();
            if !stripped.starts_with('=') && !stripped.starts_with('+') {
                self.diags.push(
                    Diagnostic::err(
                        "E0014",
                        format!(
                            "output operand {} constraint '{}' missing '=' or '+' prefix",
                            i, c
                        ),
                        sp.clone(),
                    )
                    .with_hint("add '=' for write-only or '+' for read-write output"),
                );
            }
        }
        // 2. 重复操作数名检测 (W0008, 对齐 mini-asm warning+nop 策略)
        let mut seen_names: HashSet<&str> = HashSet::new();
        for name in output_names.iter().chain(input_names.iter()).flatten() {
            if !name.is_empty() && !seen_names.insert(name.as_str()) {
                self.diags.push(
                    Diagnostic::warn(
                        "W0008",
                        format!(
                            "duplicate asm operand name '{}'",
                            name
                        ),
                        sp.clone(),
                    )
                    .with_hint("each named operand must have a unique name"),
                );
            }
        }
        // 3. 命名/位置操作数混用 (W0007):
        //    GNU C 明确允许混用命名和位置操作数, Linux 内核大量使用此模式
        //    (通过宏展开生成). GCC 自身不警告此行为, dcc 也不应警告.
        //    保留检测逻辑但不再发出诊断.
        // 4. 约束与表达式类型不匹配 (W0005) 在 codegen 层统一完成，
        //    以 matched constraint、多重约束和显式寄存器分类为准。
        // 5. 模板占位符数量与操作数数量不匹配 (W0008)
        let total_ops = outputs.len() + inputs.len();
        let bytes = asm_str.as_bytes();
        let mut pos = 0;
        let mut max_idx: Option<usize> = None;
        while pos < bytes.len() {
            if bytes[pos] == b'%' {
                pos += 1;
                if pos >= bytes.len() {
                    break;
                }
                // 跳过 %% 转义
                if bytes[pos] == b'%' {
                    pos += 1;
                    continue;
                }
                // 跳过修饰符字母 (b/w/h/k/q/a/P)
                let mut had_l_modifier = false;
                while pos < bytes.len()
                    && matches!(
                        bytes[pos],
                        b'b' | b'w' | b'h' | b'k' | b'q' | b'a' | b'P' | b'l'
                    )
                {
                    if bytes[pos] == b'l' {
                        had_l_modifier = true;
                    }
                    pos += 1;
                    if pos >= bytes.len() {
                        break;
                    }
                }
                if pos >= bytes.len() {
                    break;
                }
                if bytes[pos] == b'[' {
                    // %l[name] 是 goto label 引用, 非操作数引用
                    if had_l_modifier {
                        // 跳过整个 %[...] 块
                        while pos < bytes.len() && bytes[pos] != b']' {
                            pos += 1;
                        }
                        if pos < bytes.len() {
                            pos += 1; // skip ']'
                        }
                    } else {
                        // 命名操作数引用 %[name]: 验证名称存在
                        pos += 1;
                        let start = pos;
                        while pos < bytes.len() && bytes[pos] != b']' {
                            pos += 1;
                        }
                        if pos < bytes.len() {
                            pos += 1; // skip ']'
                        }
                        let name = &asm_str[start..pos.saturating_sub(1)];
                        if !name.is_empty() {
                            let all_names_flat: Vec<&str> = output_names
                                .iter()
                                .chain(input_names.iter())
                                .filter_map(|n| n.as_deref())
                                .collect();
                            if !all_names_flat.contains(&name) {
                                self.diags.push(
                                    Diagnostic::warn(
                                        "W0008",
                                        format!(
                                            "asm template references unnamed operand \
                                         '%[{}]' which does not exist; emitted as nop",
                                            name
                                        ),
                                        sp.clone(),
                                    )
                                    .with_hint("check the operand name or add it to the list"),
                                );
                            }
                        }
                    }
                } else if bytes[pos].is_ascii_digit() {
                    let start = pos;
                    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    // %lN 是 goto label 引用, 不计入普通操作数索引
                    if !had_l_modifier {
                        if let Ok(idx) = asm_str[start..pos].parse::<usize>() {
                            max_idx = Some(max_idx.map_or(idx, |m: usize| m.max(idx)));
                        }
                    }
                } else {
                    pos += 1;
                }
            } else {
                pos += 1;
            }
        }
        if let Some(max) = max_idx {
            if max >= total_ops && total_ops > 0 {
                self.diags.push(
                    Diagnostic::warn(
                        "W0008",
                        format!(
                            "asm template references operand %{} but only {} \
                         operand(s) provided; emitted as nop",
                            max, total_ops
                        ),
                        sp.clone(),
                    )
                    .with_hint("add the missing operand or fix the template index"),
                );
            }
        }
    }

    pub fn program(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut decls = vec![];
        while self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::Semi {
                self.advance();
                continue;
            }
            match self.parse_decl() {
                Some(d) => {
                    decls.extend(d);
                }
                None => {
                    if !self.finished {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
        }
        (
            Program {
                decls,
                fenv_access: self.opts.fenv_access,
                fp_contract: self.opts.fp_contract,
                cx_limited_range: self.opts.cx_limited_range,
            },
            std::mem::take(&mut self.diags),
        )
    }

    fn parse_decl(&mut self) -> Option<Vec<Decl>> {
        // __extension__: GNU 扩展前缀，直接跳过
        if self.cur.kind == TokKind::Ident && self.cur.text == "__extension__" {
            self.advance();
        }
        // C23 [[attributes]]: 静默跳过
        self.parse_std_attributes();
        // constexpr: C23 编译期常量声明.
        // 变量: 隐含 const + static, 必须常量初始化; 函数: 等价 inline + always_inline.
        while self.cur.kind == TokKind::Ident && self.cur.text == "constexpr" {
            self.decl_attrs.is_always_inline = true;
            self.decl_attrs.is_constexpr = true;
            self.spec_quals |= QualifierFlags::CONST;
            self.advance();
            self.parse_std_attributes();
        }
        // _Static_assert(expr, msg): C11 编译期断言; C23 static_assert 同义
        if self.cur.kind == TokKind::Ident
            && (self.cur.text == "_Static_assert" || self.cur.text == "static_assert")
        {
            return self.parse_static_assert().map(|_| vec![]);
        }
        // 预处理未完全展开的静态分支 key 声明 → 语义补全为 extern struct static_key_{false,true}
        if self.cur.kind == TokKind::Ident && self.cur.text.starts_with("_DECLARE_STATIC_KEY_") {
            return self.parse_static_key_decl();
        }
        // 解析声明开头的 __attribute__: GCC 扩展, 如 __attribute__((section(".init.text"))) int foo()
        let leading_attrs = self.parse_attributes();
        self.decl_storage = StorageClass::default();
        // 保留 C23 标志: 上面已解析, 重置 decl_attrs 后需恢复
        let was_constexpr = self.decl_attrs.is_constexpr;
        let was_nodiscard = self.decl_attrs.is_nodiscard;
        self.decl_attrs = ParsedAttrs::default();
        self.decl_attrs.is_constexpr = was_constexpr;
        self.decl_attrs.is_nodiscard = was_nodiscard;
        // 文件级 asm: 必须在 skip_asm_label 之前, 因为 __asm__("...") 与 asm 标签共享相同 token 模式
        if self.cur.kind == TokKind::Ident && (self.cur.text == "__asm__" || self.cur.text == "asm")
        {
            let sp = self.cur.span.clone();
            self.advance();
            // 可选的 volatile/const/inline
            let global_asm_volatile = self.skip_asm_qualifiers();
            // 解析带约束的内联汇编表达式
            if self.cur.kind == TokKind::LParen {
                if let Some(Expr::AsmExpr {
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    span,
                    ..
                }) = self.parse_asm_expr(sp.clone(), global_asm_volatile)
                {
                    if !self.expect(TokKind::Semi, "';'") {
                        return None;
                    }
                    return Some(vec![Decl::GlobalAsm {
                        asm_str,
                        outputs,
                        output_names,
                        output_constraints,
                        inputs,
                        input_names,
                        input_constraints,
                        clobbers,
                        span,
                    }]);
                }
                return None;
            } else {
                self.err_here(
                    "E0001",
                    "expected '(' after asm",
                    Some("asm requires parentheses: asm(\"instructions\" : ...)"),
                );
                return None;
            }
        }
        // 跳过声明开头的 __asm__("label") (在文件级 asm 之后, 避免误吞)
        self.skip_asm_label();
        // typedef
        if self.cur.kind == TokKind::KwTypedef {
            self.advance();
            let mut ty = self.parse_type_mode(true)?;
            // __attribute__((mode(...))): 覆盖基类大小/符号性
            // 例如: typedef int __attribute__((mode(DI))) int64_t;
            // 将 int (32-bit) 覆盖为 DI (64-bit long)
            if let Some(ref mode) = self.decl_attrs.mode_name {
                let is_unsigned = matches!(
                    ty,
                    Type::UInt
                        | Type::ULong
                        | Type::ULongLong
                        | Type::UChar
                        | Type::UShort
                );
                if let Some(mode_ty) = self.mode_to_type(mode, is_unsigned) {
                    ty = mode_ty;
                }
            }
            ty = self.parse_ptr_suffix(ty);
            // typedef ret_type (name)(params) — 无 * 的函数 typedef
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Ident {
                self.advance(); // (
                let name = self.parse_typedef_name()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    let func_ty =
                        Type::Func(Box::new(ty.into()), params.iter().map(|p| p.ty.clone().into()).collect());
                    let sp = name.span.clone();
                    if !self.expect(TokKind::Semi, "';'") {
                        return None;
                    }
                    if !self.register_typedef(&name, func_ty.clone()) {
                        return None;
                    }
                    return Some(vec![Decl::Typedef {
                        name,
                        ty: func_ty,
                        span: sp,
                    }]);
                }
            }
            // 函数指针 typedef: typedef ret_type (*name)(params)
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                self.advance(); // (
                self.advance(); // *
                while matches!(
                    self.cur.kind,
                    TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict
                ) {
                    self.advance();
                }
                let name = self.parse_typedef_name()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    let func_ty =
                        Type::Func(Box::new(ty.into()), params.iter().map(|p| p.ty.clone().into()).collect());
                    let ptr_ty = Type::Ptr(Box::new(func_ty.into()));
                    let sp = name.span.clone();
                    if !self.expect(TokKind::Semi, "';'") {
                        return None;
                    }
                    if !self.register_typedef(&name, ptr_ty.clone()) {
                        return None;
                    }
                    return Some(vec![Decl::Typedef {
                        name,
                        ty: ptr_ty,
                        span: sp,
                    }]);
                }
            }
            let name = self.parse_typedef_name()?;
            let sp = name.span.clone();
            // 函数 typedef: typedef ret_type name(params)
            if self.cur.kind == TokKind::LParen {
                self.advance();
                let params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                let func_ty = Type::Func(
                    Box::new(ty.clone().into()),
                    params.iter().map(|p| p.ty.clone().into()).collect(),
                );
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                if !self.register_typedef(&name, func_ty.clone()) {
                    return None;
                }
                return Some(vec![Decl::Typedef {
                    name,
                    ty: func_ty,
                    span: sp,
                }]);
            }
            // 数组 typedef: typedef T name[N] / name[]
            // 多维数组按 C11 6.7.6.2p4 从内向外结合: `T name[2][8]` 是
            // "2 个 T[8]" (最内层元素先与 [8] 结合). 收集全部维度后反转
            // 再逐层包裹, 与函数指针数组的 array_sizes.rev() 实现一致.
            let mut arr_sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    // 跳过 const/static/restrict 限定符
                    while matches!(
                        self.cur.kind,
                        TokKind::KwConst
                            | TokKind::KwVolatile
                            | TokKind::KwRestrict
                            | TokKind::KwStatic
                    ) {
                        self.advance();
                    }
                    if self.cur.kind == TokKind::RBracket {
                        ArraySize::Const(0)
                    } else {
                        let e = self.parse_expr()?;
                        if let Ok(v) = crate::const_eval::eval(&e) {
                            ArraySize::Const(v.max(0) as usize)
                        } else {
                            match &e {
                                Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                                _ => ArraySize::Var(Box::new(e)),
                            }
                        }
                    }
                } else {
                    ArraySize::Const(0)
                };
                if !self.expect(TokKind::RBracket, "']'") {
                    return None;
                }
                arr_sizes.push(arr_size);
            }
            for sz in arr_sizes.into_iter().rev() {
                ty = Type::Array(Box::new(ty.into()), sz);
            }
            let ty = self.try_parse_vector_attr(ty);
            let mut ty = ty;
            // typedef 声明符后的属性: 解析并应用 mode 属性
            if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                let attrs = self.parse_attributes();
                if let Some(ref mode) = attrs.mode_name {
                    let is_unsigned = matches!(
                        ty,
                        Type::UInt
                            | Type::ULong
                            | Type::ULongLong
                            | Type::UChar
                            | Type::UShort
                    );
                    if let Some(mode_ty) = self.mode_to_type(mode, is_unsigned) {
                        ty = mode_ty;
                    }
                }
            }
            if !self.expect(TokKind::Semi, "';'") {
                return None;
            }
            if !self.register_typedef(&name, ty.clone()) {
                return None;
            }
            return Some(vec![Decl::Typedef { name, ty, span: sp }]);
        }
        // struct/union/enum 定义或声明
        if matches!(
            self.cur.kind,
            TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum
        ) {
            // 只用声明说明符（不含 *）：每个声明符各自 parse_ptr_suffix
            // 否则 `struct T *a, *b` 会把第一个 * 吃进 ty，导致 b 变成 T**
            let ty = self.parse_decl_specifiers()?;
            let storage = self.decl_storage;
            let attrs = Self::merge_attrs(leading_attrs, std::mem::take(&mut self.decl_attrs));
            if self.cur.kind == TokKind::Semi {
                self.advance();
                return Some(vec![]);
            }
            // 支持 `struct T *a, *b;` 多声明符
            let mut decls = vec![];
            let saved_auto = self.decl_auto_type;
            while let Some(ds) = self.parse_var_or_func(ty.clone(), attrs.clone(), storage) {
                decls.extend(ds);
                if self.cur.kind != TokKind::Comma {
                    break;
                }
                self.advance();
                // 多声明符 (__auto_type a = 1, b = 2;): 恢复 auto 标志供下一声明符使用
                self.decl_auto_type = saved_auto;
            }
            if decls.is_empty() {
                None
            } else {
                Some(decls)
            }
        } else {
            let ty = self.parse_decl_specifiers()?;
            // 单独的 struct/union/enum 定义: `__declspec(...) struct S { int x; };`
            // 解析说明符后类型是 struct/union/enum, 而当前 token 是分号,
            // 直接吃掉分号返回空 decl, 避免后面误调 parse_var_or_func.
            if matches!(
                ty,
                Type::Struct(_, _, _) | Type::Union(_, _, _) | Type::Enum(_, _, _)
            ) && self.cur.kind == TokKind::Semi
            {
                self.advance();
                return Some(vec![]);
            }
            let storage = self.decl_storage;
            let attrs = Self::merge_attrs(leading_attrs, std::mem::take(&mut self.decl_attrs));
            let mut decls = vec![];
            let saved_auto = self.decl_auto_type;
            while let Some(ds) = self.parse_var_or_func(ty.clone(), attrs.clone(), storage) {
                decls.extend(ds);
                if self.cur.kind != TokKind::Comma {
                    break;
                }
                self.advance();
                // 多声明符 (__auto_type a = 1, b = 2;): 恢复 auto 标志供下一声明符使用
                self.decl_auto_type = saved_auto;
            }
            if decls.is_empty() {
                None
            } else {
                Some(decls)
            }
        }
    }

    fn parse_static_assert(&mut self) -> Option<()> {
        let span = self.cur.span.clone();
        self.advance();
        if !self.expect(TokKind::LParen, "'('") {
            return None;
        }
        let condition = self.parse_expr_bp(2)?;
        // C23 允许省略消息; 默认为 "static assertion failed"
        let mut message = "static assertion failed".to_string();
        if self.cur.kind == TokKind::Comma {
            self.advance();
            if self.cur.kind == TokKind::StringLit {
                let mut buf = String::from_utf8_lossy(&parse_str(&self.cur.text))
                    .trim_end_matches('\0')
                    .to_string();
                self.advance();
                // C 允许相邻字符串字面量拼接: "foo" "bar" → "foobar"
                // 内核的 BUILD_BUG_ON_MSG 宏会用到 "..." " is true" 形式.
                while self.cur.kind == TokKind::StringLit {
                    let more = String::from_utf8_lossy(&parse_str(&self.cur.text))
                        .trim_end_matches('\0')
                        .to_string();
                    buf.push_str(&more);
                    self.advance();
                }
                message = buf;
            } else {
                self.err_here(
                    "E0012",
                    "expected string literal in _Static_assert",
                    Some("the second argument must be a diagnostic string"),
                );
                return None;
            }
        }
        if !self.expect(TokKind::RParen, "')'") {
            return None;
        }
        if !self.expect(TokKind::Semi, "';'") {
            return None;
        }
        // 在求值失败时附加条件表达式文本 (GCC 兼容: 错误消息后追加 "evaluates to <text>")
        let cond_text = format!("{}", const_eval::ExprDebug(&condition));
        match const_eval::eval(&condition) {
            Ok(0) => {
                // _Static_assert 失败降级为警告以支持内核构建
                if !cond_text.is_empty() && cond_text != "<expr>" {
                    self.diags.push(Diagnostic::warn(
                        "W0010",
                        format!("{} (evaluates to: {})", message, cond_text),
                        span,
                    ));
                } else {
                    self.diags.push(Diagnostic::warn("W0010", message, span));
                }
            }
            Ok(_) => {}
            Err(ConstEvalError::DivisionByZero) => self.diags.push(Diagnostic::err(
                "E0011",
                format!(
                    "division by zero in static assertion (evaluates to: {})",
                    cond_text
                ),
                span,
            )),
            Err(_) => self.diags.push(Diagnostic::err(
                "E0011",
                format!(
                    "{} (not an integer constant expression: {})",
                    message, cond_text
                ),
                span,
            )),
        }
        Some(())
    }

    /// 将 `_DECLARE_STATIC_KEY_0(name)` / `_1(name)` 及
    /// `_DECLARE_STATIC_KEY_IS_ENABLED(cfg)(name)` 补全为 Linux jump_label 外部声明。
    fn parse_static_key_decl(&mut self) -> Option<Vec<Decl>> {
        let span = self.cur.span.clone();
        let macro_name = self.cur.text.clone();
        self.advance();
        // __PASTE 未完全展开时残留 _DECLARE_STATIC_KEY_IS_ENABLED(cfg)(name)
        let enabled = if macro_name == "_DECLARE_STATIC_KEY_IS_ENABLED" {
            if self.cur.kind == TokKind::LParen {
                self.advance();
                while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                    self.advance();
                }
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
            }
            false
        } else {
            let suffix = macro_name.strip_prefix("_DECLARE_STATIC_KEY_")?;
            match suffix {
                "0" => false,
                "1" => true,
                _ => {
                    self.err_here(
                        "E0001",
                        format!("unknown static key macro '{macro_name}'"),
                        Some("expected _DECLARE_STATIC_KEY_0 or _DECLARE_STATIC_KEY_1"),
                    );
                    return None;
                }
            }
        };
        let struct_name = if enabled { "static_key_true" } else { "static_key_false" };
        if !self.struct_defs.contains_key(struct_name) {
            self.struct_defs
                .insert(struct_name.to_string(), (Arc::new(vec![]), false, 4));
        }
        if !self.expect(TokKind::LParen, "'('") {
            return None;
        }
        let name = if self.cur.kind == TokKind::Ident {
            let ident = Ident {
                name: self.cur.text.clone(),
                span: self.cur.span.clone(),
            };
            self.advance();
            ident
        } else {
            self.err_here(
                "E0001",
                "expected identifier in static key declaration",
                None,
            );
            return None;
        };
        if !self.expect(TokKind::RParen, "')'") {
            return None;
        }
        if !self.expect(TokKind::Semi, "';'") {
            return None;
        }
        let ty = Type::Struct(struct_name.to_string(), Arc::new(vec![]), 8);
        self.globals.insert(name.name.clone(), ty.clone());
        Some(vec![Decl::Var(VarDecl {
            ty,
            name,
            init: None,
            span,
            is_extern: true,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: vec![],
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: QualifierFlags::NONE,
            addr_space: AddrSpace::None,
            tls_model: None,
            asm_label: None,
        })])
    }

    fn parse_var_or_func(
        &mut self,
        ty: Type,
        leading_attrs: ParsedAttrs,
        storage: StorageClass,
    ) -> Option<Vec<Decl>> {
        // __auto_type/C23 auto: parse_type_mode 设置了 decl_auto_type 标志,
        // 在此立即归属给当前声明符并清除, 防止 init 表达式内嵌套声明
        // (如语句表达式中的局部变量) 误消费该标志
        let auto_owned = self.decl_auto_type;
        self.decl_auto_type = false;
        // 函数指针声明/定义: type (*name)(params)
        // 第一性原理: (* 表示这不是普通变量名, 而是函数指针声明
        if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
            self.advance(); // (
            self.advance(); // *
            return self.parse_func_ptr_inner(ty, leading_attrs, storage);
        }
        // 函数指针带属性: type (__attribute__((...)) *name)(params)
        // `(` 后跟 `__attribute__` 而非 `*`, 需跳过属性后判断
        if self.cur.kind == TokKind::LParen
            && self.peek.kind == TokKind::Ident
            && self.peek.text == "__attribute__"
        {
            self.advance(); // (
            let inner_attrs = self.parse_attributes();
            if self.cur.kind == TokKind::Star {
                // 确认是函数指针: (__attribute__(...) *name)
                let merged = Self::merge_attrs(leading_attrs, inner_attrs);
                self.advance(); // *
                return self.parse_func_ptr_inner(ty, merged, storage);
            }
            // 不是函数指针, 继续正常声明流程 (属性已消费)
        }
        // 函数指针带段寄存器限定符: type (__seg_fs/__seg_gs *name)(params)
        // `(` 后跟段寄存器限定符 (x86 per-cpu 访问) 而非 `*`.
        // 与函数指针带属性同构, 只是限定符在 * 之前.
        if self.cur.kind == TokKind::LParen
            && self.peek.kind == TokKind::Ident
            && matches!(
                self.peek.text.as_str(),
                "__seg_fs"
                    | "__seg_gs"
                    | "__seg_cs"
                    | "__seg_ds"
                    | "__seg_es"
                    | "__seg_ss"
            )
        {
            self.advance(); // (
            self.skip_ptr_qualifiers(); // 跳过 __seg_* 限定符
            if self.cur.kind == TokKind::Star {
                self.advance(); // *
                return self.parse_func_ptr_inner(ty, leading_attrs, storage);
            }
            // 不是函数指针, 继续正常声明流程 (限定符已消费)
        }
        // 限定符在基类型与 * 之间 (如 const, __seg_gs 等)
        // 先捕获基本类型后的属性 (如 void __attribute__((used)) func()),
        // 再跳过指针限定符 (skip_ptr_qualifiers 会丢弃属性, 故须先捕获),
        // 最后再捕获限定符后可能残留的属性 (如 void const __attribute__((used)) *p).
        let mid_attrs = self.parse_attributes();
        // 限定符在基类型与 * 之间: `T const *p` 中 const 属于 pointed-to 类型
        // 保存当前 decl_quals, 在 skip_ptr_qualifiers 后恢复
        let saved_dq = self.decl_quals;
        self.skip_ptr_qualifiers();
        // 如果后面有 *, 则 skip_ptr_qualifiers 消费的限定符属于 pointed-to 类型
        // 如果后面没有 *, 则限定符属于变量本身
        if self.cur.kind == TokKind::Star {
            // 有限定符在 * 之前: 属于 pointed-to 类型, 不属于变量
            self.decl_quals = saved_dq;
        }
        // 否则: skip_ptr_qualifiers 设置的 decl_quals 属于变量, 保留
        let mid_attrs = Self::merge_attrs(mid_attrs, self.parse_attributes());
        let ty = self.parse_ptr_suffix(ty);
        // 函数指针: void * (*name)(params) — * 已被 parse_ptr_suffix 吃掉, 现在是 (
        if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
            self.advance(); // (
            self.advance(); // *
            let merged = Self::merge_attrs(leading_attrs, mid_attrs);
            return self.parse_func_ptr_inner(ty, merged, storage);
        }
        // 解析变量/函数名前的 __attribute__: 如 int __attribute__((unused)) x;
        let pre_attrs = Self::merge_attrs(Self::merge_attrs(leading_attrs, mid_attrs), self.parse_attributes());
        self.skip_asm_label();
        // 括号包函数名防宏展开: size_t (htab_size) (htab_t h)
        let name = if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Ident {
            self.advance(); // (
            let id = self.parse_ident()?;
            if !self.expect(TokKind::RParen, "')'") {
                return None;
            }
            id
        } else if self.cur.kind == TokKind::LParen && self.peek.kind != TokKind::Star {
            // 无名函数定义: int () { ... } — IR 阶段报 E1002
            Ident {
                name: String::new(),
                span: self.cur.span.clone(),
            }
        } else {
            self.parse_ident()?
        };
        // C23 [[attributes]]: 在变量/函数名后出现，静默跳过
        self.parse_std_attributes();
        let sp = name.span.clone();
        let name_for_decl = name.clone();
        if self.cur.kind == TokKind::LParen {
            self.advance();
            let params = self.parse_params()?;
            if !self.expect(TokKind::RParen, "')'") {
                return None;
            }
            // K&R 风格函数: 参数后跟类型声明, 然后是函数体
            let mut is_old_style = false;
            let params = if (params.is_empty() || params.iter().all(|p| p.ty == Type::Int))
                && self.cur.kind != TokKind::LBrace
                && self.cur.kind != TokKind::Semi
                && self.is_type_start()
            {
                is_old_style = true;
                // 解析 K&R 参数类型声明
                let mut kr_params = params;
                while self.is_type_start() && self.cur.kind != TokKind::LBrace {
                    let ty = self.parse_type()?;
                    // 可能有多个参数名共享同一类型
                    while self.cur.kind == TokKind::Ident {
                        let name = self.cur.text.clone();
                        self.advance();
                        // 更新对应参数的类型
                        for p in &mut kr_params {
                            if let Some(n) = &p.name {
                                if n.name == name {
                                    p.ty = ty.clone();
                                }
                            }
                        }
                        if self.cur.kind == TokKind::Comma {
                            self.advance();
                        }
                    }
                    if self.cur.kind == TokKind::Semi {
                        self.advance();
                    }
                }
                kr_params
            } else {
                params
            };
            // 函数声明后: 解析 __attribute__ 和 __asm__ 标签
            let post_attrs = self.skip_post_decl_modifiers();
            let attrs = Self::merge_attrs(pre_attrs, post_attrs);
            // 进入函数: 从全局变量表恢复, 清除局部变量
            let func_ty = Type::Func(
                Box::new(ty.clone().into()),
                params.iter().map(|p| p.ty.clone().into()).collect(),
            );
            // 检测是否是嵌套函数 (在外层函数体内)
            let is_nested_func = self.in_function;
            let parent_locals = if is_nested_func {
                let mut snap = self.locals.snapshot();
                snap.last_mut().unwrap().insert(name_for_decl.name.clone(), func_ty.clone());
                snap
            } else {
                vec![]
            };
            if !is_nested_func {
                self.globals.insert(name_for_decl.name.clone(), func_ty);
            }
            // 非嵌套函数: 不克隆 globals, 让 lookup_ident_type 回退到 globals
            if !is_nested_func {
                self.locals.clear();
            } else {
                self.locals.load_globals(&self.globals);
            }
            self.in_function = true;
            let saved_func = self.current_function.replace(name_for_decl.name.clone());
            for p in &params {
                if let Some(nm) = &p.name {
                    self.locals.insert(nm.name.clone(), p.ty.clone());
                }
            }
            let body = if self.cur.kind == TokKind::LBrace {
                self.advance();
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                Some(b)
            } else {
                if !self.expect(TokKind::Semi, "';' or '{'") {
                    return None;
                }
                None
            };
            // 恢复外层函数的 in_function 状态
            if is_nested_func {
                // 将嵌套函数体内的 locals 展平为单 HashMap (供 collect_upvars 使用)
                let child_flat = {
                    let mut m = HashMap::new();
                    for scope in &self.locals.scopes {
                        for (k, v) in scope { m.insert(k.clone(), v.clone()); }
                    }
                    m
                };
                self.locals.restore(parent_locals.clone());
                self.in_function = true;
                // 将 parent_locals 展平
                let parent_flat = {
                    let mut m = HashMap::new();
                    for scope in &parent_locals {
                        for (k, v) in scope { m.insert(k.clone(), v.clone()); }
                    }
                    m
                };
                // 检测嵌套函数捕获的外层变量
                let upvars = {
                    let mut captured = Vec::new();
                    if let Some(ref b) = body {
                        Self::collect_upvars(
                            &b.items,
                            &child_flat,
                            &parent_flat,
                            &mut captured,
                        );
                    }
                    captured
                };
                // 函数声明后不再解析额外属性 (避免吞掉下一个声明的属性)
                // #pragma weak name 在 codegen 体现为 STB_WEAK 绑定
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                if attrs.is_nodiscard {
                    self.nodiscard_fns.insert(name_for_decl.name.clone());
                }
                Some(vec![Decl::Func(FuncDecl {
                    ret_ty: ty,
                    name: name_for_decl,
                    params,
                    body,
                    span: sp,
                    is_extern: storage.is_extern,
                    is_static: storage.is_static,
                    section: attrs.section,
                    alignment: attrs.alignment,
                    is_weak: attrs.is_weak || pragma_weak,
                    visibility: attrs.visibility,
                    is_unused: attrs.is_unused,
                    is_cold: attrs.is_cold,
                    is_hot: attrs.is_hot,
                    is_noinline: attrs.is_noinline,
                    is_always_inline: attrs.is_always_inline,
                    is_inline: attrs.is_inline,
                    is_variadic: self.last_variadic,
                    is_constructor: attrs.is_constructor,
                    is_destructor: attrs.is_destructor,
                    alias_name: attrs.alias_name.clone().or(attrs.copy_fn.clone()),
                    is_nested: is_nested_func,
                    upvars,
                    is_old_style,
                    format_kind: attrs.format_kind,
                    format_string_idx: attrs.format_string_idx,
                    format_first_arg: attrs.format_first_arg,
                    no_sanitize: attrs.no_sanitize.clone(),
                    target_clones: attrs.target_clones.clone(),
                    returns_nonnull: attrs.returns_nonnull,
                    ctor_priority: attrs.ctor_priority,
                    dtor_priority: attrs.dtor_priority,
                    is_retain: attrs.is_retain,
                    is_used_attr: attrs.is_used,
                    is_flatten: attrs.is_flatten,
                    error_msg: attrs.error_msg.clone(),
                    warning_msg: attrs.warning_msg.clone(),
                    deprecated_msg: attrs.deprecated_msg.clone(),
                    ifunc_resolver: attrs.ifunc_resolver.clone(),
                    asm_output_template: attrs.asm_output_template.clone(),
                    is_global: attrs.is_global,
                    is_device: attrs.is_device,
                    is_host: attrs.is_host,
                    is_naked: attrs.is_naked,
                    is_noreturn: attrs.is_noreturn,
                    is_pure: attrs.is_pure,
                    is_const_attr: attrs.is_const_attr,
                    returns_twice: attrs.returns_twice,
                    no_instrument: attrs.no_instrument,
                    is_malloc: attrs.is_malloc,
                    malloc_dealloc: attrs.malloc_dealloc.clone(),
                    malloc_dealloc_argno: attrs.malloc_dealloc_argno,
                    alloc_size: attrs.alloc_size,
                    assume_aligned: attrs.assume_aligned,
                    warn_unused_result: attrs.warn_unused_result,
                    is_sentinel: attrs.is_sentinel,
                    format_arg_idx: attrs.format_arg_idx,
                    is_nodiscard: attrs.is_nodiscard,
                    asm_label: attrs.asm_label.clone(),
                })])
            } else {
                // 非嵌套函数: 用 retain 移除函数体内新增的局部条目, 避免 O(n) 克隆 globals
                self.locals.retain_globals(&self.globals);
                self.in_function = false;
                // 恢复外层 current_function
                self.current_function = saved_func;
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                if attrs.is_nodiscard {
                    self.nodiscard_fns.insert(name_for_decl.name.clone());
                }
                Some(vec![Decl::Func(FuncDecl {
                    ret_ty: ty,
                    name: name_for_decl,
                    params,
                    body,
                    span: sp,
                    is_extern: storage.is_extern,
                    is_static: storage.is_static,
                    section: attrs.section,
                    alignment: attrs.alignment,
                    is_weak: attrs.is_weak || pragma_weak,
                    visibility: attrs.visibility,
                    is_unused: attrs.is_unused,
                    is_cold: attrs.is_cold,
                    is_hot: attrs.is_hot,
                    is_noinline: attrs.is_noinline,
                    is_always_inline: attrs.is_always_inline,
                    is_inline: attrs.is_inline,
                    is_variadic: self.last_variadic,
                    is_constructor: attrs.is_constructor,
                    is_destructor: attrs.is_destructor,
                    alias_name: attrs.alias_name.clone().or(attrs.copy_fn.clone()),
                    is_nested: false,
                    upvars: vec![],
                    is_old_style,
                    format_kind: attrs.format_kind,
                    format_string_idx: attrs.format_string_idx,
                    format_first_arg: attrs.format_first_arg,
                    no_sanitize: attrs.no_sanitize.clone(),
                    target_clones: attrs.target_clones.clone(),
                    returns_nonnull: attrs.returns_nonnull,
                    ctor_priority: attrs.ctor_priority,
                    dtor_priority: attrs.dtor_priority,
                    is_retain: attrs.is_retain,
                    is_used_attr: attrs.is_used,
                    is_flatten: attrs.is_flatten,
                    error_msg: attrs.error_msg.clone(),
                    warning_msg: attrs.warning_msg.clone(),
                    deprecated_msg: attrs.deprecated_msg.clone(),
                    ifunc_resolver: attrs.ifunc_resolver.clone(),
                    asm_output_template: attrs.asm_output_template.clone(),
                    is_global: attrs.is_global,
                    is_device: attrs.is_device,
                    is_host: attrs.is_host,
                    is_naked: attrs.is_naked,
                    is_noreturn: attrs.is_noreturn,
                    is_pure: attrs.is_pure,
                    is_const_attr: attrs.is_const_attr,
                    returns_twice: attrs.returns_twice,
                    no_instrument: attrs.no_instrument,
                    is_malloc: attrs.is_malloc,
                    malloc_dealloc: attrs.malloc_dealloc.clone(),
                    malloc_dealloc_argno: attrs.malloc_dealloc_argno,
                    alloc_size: attrs.alloc_size,
                    assume_aligned: attrs.assume_aligned,
                    warn_unused_result: attrs.warn_unused_result,
                    is_sentinel: attrs.is_sentinel,
                    format_arg_idx: attrs.format_arg_idx,
                    is_nodiscard: attrs.is_nodiscard,
                    asm_label: attrs.asm_label.clone(),
                })])
            }
        } else if self.cur.kind == TokKind::LBracket {
            let mut arr_ty = ty;
            // 多维数组从内向外结合 (C11 6.7.6.2p4): `T a[2][8]` 是
            // "2 个 T[8]", 收集维度后反转包裹, 与函数指针数组一致.
            let mut arr_sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr()?;
                    // 整型常量表达式按标准折叠为编译期常量, 否则保留为 VLA
                    if let Ok(v) = crate::const_eval::eval(&e) {
                        ArraySize::Const(v.max(0) as usize)
                    } else {
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    }
                } else {
                    ArraySize::Const(0)
                };
                if !self.expect(TokKind::RBracket, "']'") {
                    return None;
                }
                arr_sizes.push(arr_size);
            }
            for sz in arr_sizes.into_iter().rev() {
                arr_ty = Type::Array(Box::new(arr_ty.into()), sz);
            }
            let mut arr_attrs = pre_attrs.clone();
            arr_attrs.merge(self.parse_attributes());
            self.skip_asm_label(); // asm 标签
            let init = if matches!(self.cur.kind, TokKind::Assign | TokKind::LBrace) {
                // C23 允许省略 '=' 的大括号初始化: int a[] {1,2,3}
                // 注意: '{' 不能在这里吃掉, 必须交给 parse_expr 的 InitList 分支处理
                if self.cur.kind == TokKind::Assign {
                    self.advance();
                }
                Some(self.parse_expr_bp(2)?)
            } else {
                None
            };
            // 从 InitList 推断最外层数组大小: data[] = {1,2,3} → data[3]
            if let (Type::Array(elem, ArraySize::Const(0)), Some(Expr::InitList { inits, .. })) =
                (&arr_ty, &init)
            {
                arr_ty = Type::Array(elem.clone(), ArraySize::Const(inits.len()));
            }
            self.locals
                .insert(name_for_decl.name.clone(), arr_ty.clone());
            if !self.in_function {
                self.globals
                    .insert(name_for_decl.name.clone(), arr_ty.clone());
            }
            if self.cur.kind != TokKind::Comma && !self.expect(TokKind::Semi, "';'") {
                return None;
            }
            Some(vec![Decl::Var(VarDecl {
                ty: arr_ty,
                name: name_for_decl,
                init,
                span: sp,
                is_extern: storage.is_extern,
                is_static: storage.is_static,
                section: arr_attrs.section,
                alignment: arr_attrs.alignment,
                is_weak: arr_attrs.is_weak,
                visibility: arr_attrs.visibility,
                is_unused: arr_attrs.is_unused,
                register_name: arr_attrs.register_name,
                cleanup_fn: arr_attrs.cleanup_fn,
                is_thread_local: storage.is_thread_local,
                no_sanitize: arr_attrs.no_sanitize.clone(),
                alias_name: arr_attrs.alias_name.clone(),
                copy_fn: arr_attrs.copy_fn.clone(),
                is_retain: arr_attrs.is_retain,
                is_used_attr: arr_attrs.is_used,
                quals: self.spec_quals.iso_c(),
                addr_space: self.spec_addr_space,
                tls_model: arr_attrs.tls_model.clone(),
                asm_label: arr_attrs.asm_label.clone(),
            })])
        } else {
            // extern 变量可以是 void 类型: 内核使用 `extern const void __nosave_begin, __nosave_end;`
            // 作为链接器 section 标记, 实际类型/大小由链接脚本决定.
            let allow_void = storage.is_extern;
            if matches!(ty, Type::Void) && !allow_void {
                self.err_here(
                    "E0008",
                    "variable declared with void type",
                    Some("variables must have a complete object type; use 'void *' for generic pointers"),
                );
                return None;
            }
            if !name_for_decl.name.is_empty()
                && !self.check_var_redecl(&name_for_decl.name, &ty, sp.clone())
            {
                return None;
            }
            // 变量名后可能有 __attribute__((unused)) 和 __asm__("label") 等
            let var_attrs = self.parse_attributes();
            let mut attrs = Self::merge_attrs(pre_attrs, var_attrs);
            // __asm__("label"): 可能是 asm 标签或 register asm("rN")
            if let Some(asm_label) = self.skip_asm_label() {
                if storage.is_register && attrs.register_name.is_none() {
                    attrs.register_name = Some(asm_label);
                } else {
                    // GNU asm 标签, 覆盖 ELF 符号名
                    attrs.asm_label = Some(asm_label);
                }
            }
            if matches!(self.cur.kind, TokKind::Assign | TokKind::LBrace) {
                // C23 允许省略 '=' 的大括号初始化: int x {5}
                // '{' 由 parse_expr 的 InitList 分支处理 (标量也可用花括号初始化)
                if self.cur.kind == TokKind::Assign {
                    self.advance();
                }
                // 初始化器可引用当前声明符（sizeof(*v)、typeof(v) 等）
                if !name_for_decl.name.is_empty() {
                    self.locals.insert(name_for_decl.name.clone(), ty.clone());
                    // For pointer types, spec_quals applies to pointee, not the pointer.
                    // Use decl_quals (from after *) for pointer types.
                    let var_q = if matches!(ty, Type::Ptr(_)) {
                        self.decl_quals
                    } else {
                        self.spec_quals | self.decl_quals
                    };
                    self.var_quals.insert(name_for_decl.name.clone(), var_q);
                }
                let init = self.parse_expr_bp(2)?;
                // C23 constexpr 变量: 初始化器必须是编译期常量
                if attrs.is_constexpr {
                    match const_eval::eval_full(&init) {
                        Ok(_) => {} // 常量表达式, OK
                        Err(_) => {
                            self.err_here(
                                "E0011",
                                "constexpr variable requires constant initializer",
                                Some("C23 constexpr variables must be initialized with a constant expression"),
                            );
                        }
                    }
                }
                let decl_ty = if auto_owned { init.ty() } else { ty.clone() };
                // 允许多声明符: T *a = 0, *b = 0;
                if self.cur.kind != TokKind::Comma && !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                self.locals
                    .insert(name_for_decl.name.clone(), decl_ty.clone());
                if !self.in_function {
                    self.globals
                        .insert(name_for_decl.name.clone(), decl_ty.clone());
                }
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                Some(vec![Decl::Var(VarDecl {
                    ty: decl_ty,
                    name: name_for_decl,
                    init: Some(init),
                    span: sp,
                    is_extern: storage.is_extern,
                    is_static: storage.is_static,
                    section: attrs.section,
                    alignment: attrs.alignment,
                    is_weak: attrs.is_weak || pragma_weak,
                    visibility: attrs.visibility,
                    is_unused: attrs.is_unused,
                    register_name: attrs.register_name,
                    cleanup_fn: attrs.cleanup_fn,
                    is_thread_local: storage.is_thread_local,
                    no_sanitize: attrs.no_sanitize.clone(),
                    alias_name: attrs.alias_name.clone(),
                    copy_fn: attrs.copy_fn.clone(),
                    is_retain: attrs.is_retain,
                    is_used_attr: attrs.is_used,
                    quals: self.spec_quals.iso_c(),
                    addr_space: self.spec_addr_space,
                    tls_model: attrs.tls_model.clone(),
                    asm_label: attrs.asm_label.clone(),
                })])
            } else if self.cur.kind == TokKind::Semi {
                self.locals.insert(name_for_decl.name.clone(), ty.clone());
                let var_q = if matches!(ty, Type::Ptr(_)) {
                    self.decl_quals
                } else {
                    self.spec_quals | self.decl_quals
                };
                self.var_quals.insert(name_for_decl.name.clone(), var_q);
                if !self.in_function {
                    self.globals.insert(name_for_decl.name.clone(), ty.clone());
                }
                self.advance();
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                Some(vec![Decl::Var(VarDecl {
                    ty,
                    name: name_for_decl,
                    init: None,
                    span: sp,
                    is_extern: storage.is_extern,
                    is_static: storage.is_static,
                    section: attrs.section,
                    alignment: attrs.alignment,
                    is_weak: attrs.is_weak || pragma_weak,
                    visibility: attrs.visibility,
                    is_unused: attrs.is_unused,
                    register_name: attrs.register_name,
                    cleanup_fn: attrs.cleanup_fn,
                    is_thread_local: storage.is_thread_local,
                    no_sanitize: attrs.no_sanitize.clone(),
                    alias_name: attrs.alias_name.clone(),
                    copy_fn: attrs.copy_fn.clone(),
                    is_retain: attrs.is_retain,
                    is_used_attr: attrs.is_used,
                    quals: self.spec_quals.iso_c(),
                    addr_space: self.spec_addr_space,
                    tls_model: attrs.tls_model.clone(),
                    asm_label: attrs.asm_label.clone(),
                })])
            } else if self.cur.kind == TokKind::Comma {
                self.locals.insert(name_for_decl.name.clone(), ty.clone());
                let var_q = if matches!(ty, Type::Ptr(_)) {
                    self.decl_quals
                } else {
                    self.spec_quals | self.decl_quals
                };
                self.var_quals.insert(name_for_decl.name.clone(), var_q);
                if !self.in_function {
                    self.globals.insert(name_for_decl.name.clone(), ty.clone());
                }
                let pragma_weak = self.is_weak_symbol(&name_for_decl.name);
                Some(vec![Decl::Var(VarDecl {
                    ty,
                    name: name_for_decl,
                    init: None,
                    span: sp,
                    is_extern: storage.is_extern,
                    is_static: storage.is_static,
                    section: attrs.section,
                    alignment: attrs.alignment,
                    is_weak: attrs.is_weak || pragma_weak,
                    visibility: attrs.visibility,
                    is_unused: attrs.is_unused,
                    register_name: attrs.register_name,
                    cleanup_fn: attrs.cleanup_fn,
                    is_thread_local: storage.is_thread_local,
                    no_sanitize: attrs.no_sanitize.clone(),
                    alias_name: attrs.alias_name.clone(),
                    copy_fn: attrs.copy_fn.clone(),
                    is_retain: attrs.is_retain,
                    is_used_attr: attrs.is_used,
                    quals: self.spec_quals.iso_c(),
                    addr_space: self.spec_addr_space,
                    tls_model: attrs.tls_model.clone(),
                    asm_label: attrs.asm_label.clone(),
                })])
            } else {
                self.err_here(
                    "E0002",
                    format!("expected ';', '=', '(', got {:?}", self.cur.kind),
                    Some("declaration must end with ';' or have an initializer"),
                );
                None
            }
        }
    }

    /// 解析函数指针内层： caller 已吃掉 `(*`、当前 token 为名字
    /// 类型构造与 parse_var_or_func 原始函数指针分支一致。
    fn parse_func_ptr_inner(
        &mut self,
        ty: Type,
        leading_attrs: ParsedAttrs,
        storage: StorageClass,
    ) -> Option<Vec<Decl>> {
        // 可选的 const/volatile/restrict 限定符与段寄存器地址空间限定符
        // (如 __seg_gs): 与 parse_ptr_suffix 中 * 与标识符之间的处理一致,
        // 用 skip_ptr_qualifiers 统一跳过, 避免 E0001.
        self.skip_ptr_qualifiers();
        // 处理 * 和名字之间的属性: 如 void (*__attribute__((...)) fp)();
        let mut attrs = Self::merge_attrs(leading_attrs, self.parse_attributes());
        let name = self.parse_ident()?;
        // (*name(params)): 返回指针的函数
        // (*name(params1))(params2): 返回函数指针的函数
        // (*name)(params) / (*name[N])(params): 函数指针（数组）
        if self.cur.kind == TokKind::LParen {
            self.advance();
            let inner_params = self.parse_params()?;
            if !self.expect(TokKind::RParen, "')'") {
                return None;
            }
            // 关闭 (*...) 分组
            if !self.expect(TokKind::RParen, "')'") {
                return None;
            }
            // 外层函数参数列表 (params2)
            if self.cur.kind == TokKind::LParen {
                self.advance();
                let outer_params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                let inner_func = Type::Func(
                    Box::new(ty.clone().into()),
                    inner_params.iter().map(|p| p.ty.clone().into()).collect(),
                );
                let ptr_ty = Type::Ptr(Box::new(inner_func.into()));
                let outer_func = Type::Func(
                    Box::new(ptr_ty.into()),
                    outer_params.iter().map(|p| p.ty.clone().into()).collect(),
                );
                attrs.merge(self.parse_attributes());
                self.skip_asm_label();
                self.locals.load_globals(&self.globals);
                self.in_function = true;
                let saved_func = self.current_function.replace(name.name.clone());
                for p in &outer_params {
                    if let Some(nm) = &p.name {
                        self.locals.insert(nm.name.clone(), p.ty.clone());
                    }
                }
                let body = if self.cur.kind == TokKind::LBrace {
                    self.advance();
                    let b = self.parse_block();
                    if !self.expect(TokKind::RBrace, "'}'") {
                        return None;
                    }
                    Some(b)
                } else {
                    if !self.expect(TokKind::Semi, "';' or '{'") {
                        return None;
                    }
                    None
                };
                attrs.merge(self.parse_attributes());
                self.locals.load_globals(&self.globals);
                self.in_function = false;
                self.current_function = saved_func;
                let sp = name.span.clone();
                return Some(vec![Decl::Func(FuncDecl {
                    ret_ty: outer_func,
                    name,
                    params: outer_params,
                    body,
                    span: sp,
                    is_extern: storage.is_extern,
                    is_static: storage.is_static,
                    section: attrs.section,
                    alignment: attrs.alignment,
                    is_weak: attrs.is_weak,
                    visibility: attrs.visibility,
                    is_unused: attrs.is_unused,
                    is_cold: attrs.is_cold,
                    is_hot: attrs.is_hot,
                    is_noinline: attrs.is_noinline,
                    is_always_inline: attrs.is_always_inline,
                    is_inline: attrs.is_inline,
                    is_variadic: self.last_variadic,
                    is_constructor: attrs.is_constructor,
                    is_destructor: attrs.is_destructor,
                    alias_name: attrs.alias_name.clone().or(attrs.copy_fn.clone()),
                    is_nested: false,
                    upvars: vec![],
                    is_old_style: false,
                    format_kind: attrs.format_kind,
                    format_string_idx: attrs.format_string_idx,
                    format_first_arg: attrs.format_first_arg,
                    no_sanitize: attrs.no_sanitize.clone(),
                    target_clones: attrs.target_clones.clone(),
                    returns_nonnull: attrs.returns_nonnull,
                    ctor_priority: attrs.ctor_priority,
                    dtor_priority: attrs.dtor_priority,
                    is_retain: attrs.is_retain,
                    is_used_attr: attrs.is_used,
                    is_flatten: attrs.is_flatten,
                    error_msg: attrs.error_msg.clone(),
                    warning_msg: attrs.warning_msg.clone(),
                    deprecated_msg: attrs.deprecated_msg.clone(),
                    ifunc_resolver: attrs.ifunc_resolver.clone(),
                    asm_output_template: attrs.asm_output_template.clone(),
                    is_global: attrs.is_global,
                    is_device: attrs.is_device,
                    is_host: attrs.is_host,
                    is_naked: attrs.is_naked,
                    is_noreturn: attrs.is_noreturn,
                    is_pure: attrs.is_pure,
                    is_const_attr: attrs.is_const_attr,
                    returns_twice: attrs.returns_twice,
                    no_instrument: attrs.no_instrument,
                    is_malloc: attrs.is_malloc,
                    malloc_dealloc: attrs.malloc_dealloc.clone(),
                    malloc_dealloc_argno: attrs.malloc_dealloc_argno,
                    alloc_size: attrs.alloc_size,
                    assume_aligned: attrs.assume_aligned,
                    warn_unused_result: attrs.warn_unused_result,
                    is_sentinel: attrs.is_sentinel,
                    format_arg_idx: attrs.format_arg_idx,
                    is_nodiscard: attrs.is_nodiscard,
                    asm_label: attrs.asm_label.clone(),
                })])
            }
            // (*name(params)) — 返回指针的函数，无外层参数表
            let ptr_ty = Type::Ptr(Box::new(ty.into()));
            let func_ty = Type::Func(
                Box::new(ptr_ty.clone().into()),
                inner_params.iter().map(|p| p.ty.clone().into()).collect(),
            );
            attrs.merge(self.parse_attributes());
            self.skip_asm_label();
            if !self.in_function {
                self.globals.insert(name.name.clone(), func_ty.clone());
            }
            let body = if self.cur.kind == TokKind::LBrace {
                self.advance();
                self.locals.load_globals(&self.globals);
                self.in_function = true;
                let saved_func = self.current_function.replace(name.name.clone());
                for p in &inner_params {
                    if let Some(nm) = &p.name {
                        self.locals.insert(nm.name.clone(), p.ty.clone());
                    }
                }
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                self.locals.load_globals(&self.globals);
                self.in_function = false;
                self.current_function = saved_func;
                Some(b)
            } else {
                if !self.expect(TokKind::Semi, "';' or '{'") {
                    return None;
                }
                None
            };
            let sp = name.span.clone();
            return Some(vec![Decl::Func(FuncDecl {
                ret_ty: ptr_ty,
                name,
                params: inner_params,
                body,
                span: sp,
                is_extern: storage.is_extern,
                is_static: storage.is_static,
                section: attrs.section,
                alignment: attrs.alignment,
                is_weak: attrs.is_weak,
                visibility: attrs.visibility,
                is_unused: attrs.is_unused,
                is_cold: attrs.is_cold,
                is_hot: attrs.is_hot,
                is_noinline: attrs.is_noinline,
                is_always_inline: attrs.is_always_inline,
                is_inline: attrs.is_inline,
                is_variadic: self.last_variadic,
                is_constructor: attrs.is_constructor,
                is_destructor: attrs.is_destructor,
                alias_name: attrs.alias_name.clone().or(attrs.copy_fn.clone()),
                is_nested: false,
                upvars: vec![],
                is_old_style: false,
                format_kind: attrs.format_kind,
                format_string_idx: attrs.format_string_idx,
                format_first_arg: attrs.format_first_arg,
                no_sanitize: attrs.no_sanitize.clone(),
                target_clones: attrs.target_clones.clone(),
                returns_nonnull: attrs.returns_nonnull,
                ctor_priority: attrs.ctor_priority,
                dtor_priority: attrs.dtor_priority,
                is_retain: attrs.is_retain,
                is_used_attr: attrs.is_used,
                is_flatten: attrs.is_flatten,
                error_msg: attrs.error_msg.clone(),
                warning_msg: attrs.warning_msg.clone(),
                deprecated_msg: attrs.deprecated_msg.clone(),
                ifunc_resolver: attrs.ifunc_resolver.clone(),
                asm_output_template: attrs.asm_output_template.clone(),
                is_global: attrs.is_global,
                is_device: attrs.is_device,
                is_host: attrs.is_host,
                is_naked: attrs.is_naked,
                is_noreturn: attrs.is_noreturn,
                is_pure: attrs.is_pure,
                is_const_attr: attrs.is_const_attr,
                returns_twice: attrs.returns_twice,
                no_instrument: attrs.no_instrument,
                is_malloc: attrs.is_malloc,
                malloc_dealloc: attrs.malloc_dealloc.clone(),
                malloc_dealloc_argno: attrs.malloc_dealloc_argno,
                alloc_size: attrs.alloc_size,
                assume_aligned: attrs.assume_aligned,
                warn_unused_result: attrs.warn_unused_result,
                is_sentinel: attrs.is_sentinel,
                format_arg_idx: attrs.format_arg_idx,
                is_nodiscard: attrs.is_nodiscard,
                asm_label: attrs.asm_label.clone(),
            })])
        }
        // 标准函数指针变量: type (*name)(params) 或 type (*name[N])(params)
        let array_sizes = {
            let mut sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr()?;
                    // 整型常量表达式按标准折叠为编译期常量, 否则保留为 VLA
                    if let Ok(v) = crate::const_eval::eval(&e) {
                        ArraySize::Const(v.max(0) as usize)
                    } else {
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    }
                } else {
                    ArraySize::Const(0)
                };
                if !self.expect(TokKind::RBracket, "']'") {
                    return None;
                }
                sizes.push(arr_size);
            }
            sizes
        };
        if !self.expect(TokKind::RParen, "')'") {
            return None;
        }
        // 处理 `(*name)[N]` 形式: 指向数组的指针 (局部变量)
        // 例如: pmd_t (*early_pgts)[512] = ...;
        let outer_array_sizes = {
            let mut sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr()?;
                    if let Ok(v) = crate::const_eval::eval(&e) {
                        ArraySize::Const(v.max(0) as usize)
                    } else {
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    }
                } else {
                    ArraySize::Const(0)
                };
                if !self.expect(TokKind::RBracket, "']'") {
                    return None;
                }
                sizes.push(arr_size);
            }
            sizes
        };
        if self.cur.kind == TokKind::LParen {
            self.advance();
            let params = self.parse_params()?;
            if !self.expect(TokKind::RParen, "')'") {
                return None;
            }
            let func_ty = Type::Func(
                Box::new(ty.clone().into()),
                params.iter().map(|p| p.ty.clone().into()).collect(),
            );
            let mut ptr_ty = Type::Ptr(Box::new(func_ty.into()));
            for sz in array_sizes.into_iter().rev() {
                ptr_ty = Type::Array(Box::new(ptr_ty.into()), sz);
            }
            attrs.merge(self.parse_attributes());
            self.skip_asm_label();
            self.locals.insert(name.name.clone(), ptr_ty.clone());
            if !self.in_function {
                self.globals.insert(name.name.clone(), ptr_ty.clone());
            }
            let init = if self.cur.kind == TokKind::Assign {
                self.advance();
                Some(self.parse_expr_bp(2)?)
            } else {
                None
            };
            if !self.expect(TokKind::Semi, "';'") {
                return None;
            }
            let sp = name.span.clone();
            return Some(vec![Decl::Var(VarDecl {
                ty: ptr_ty,
                name,
                init,
                span: sp,
                is_extern: storage.is_extern,
                is_static: storage.is_static,
                section: attrs.section,
                alignment: attrs.alignment,
                is_weak: attrs.is_weak,
                visibility: attrs.visibility,
                is_unused: attrs.is_unused,
                register_name: attrs.register_name,
                cleanup_fn: attrs.cleanup_fn,
                is_thread_local: storage.is_thread_local,
                no_sanitize: attrs.no_sanitize.clone(),
                alias_name: attrs.alias_name.clone(),
                copy_fn: attrs.copy_fn.clone(),
                is_retain: attrs.is_retain,
                is_used_attr: attrs.is_used,
                quals: self.spec_quals.iso_c(),
                addr_space: self.spec_addr_space,
                tls_model: attrs.tls_model.clone(),
                asm_label: attrs.asm_label.clone(),
            })]);
        }
        // 不是函数指针, 只是带括号的指针变量 (罕见)
        // 处理 `(*name)[N]` 形式: 指向数组的指针
        attrs.merge(self.parse_attributes());
        self.skip_asm_label();
        let ptr_ty = if !outer_array_sizes.is_empty() {
            // 先构造内层数组类型: Array(ty, N1, N2, ...)
            let mut inner_ty = ty;
            for sz in outer_array_sizes.into_iter().rev() {
                inner_ty = Type::Array(Box::new(inner_ty.into()), sz);
            }
            // 然后构造指针类型: Ptr(inner_array)
            Type::Ptr(Box::new(inner_ty.into()))
        } else {
            Type::Ptr(Box::new(ty.into()))
        };
        let init = if self.cur.kind == TokKind::Assign {
            self.advance();
            Some(self.parse_expr_bp(2)?)
        } else {
            None
        };
        if !self.expect(TokKind::Semi, "';'") {
            return None;
        }
        let sp = name.span.clone();
        Some(vec![Decl::Var(VarDecl {
            ty: ptr_ty,
            name,
            init,
            span: sp,
            is_extern: storage.is_extern,
            is_static: storage.is_static,
            section: attrs.section,
            alignment: attrs.alignment,
            is_weak: attrs.is_weak,
            visibility: attrs.visibility,
            is_unused: attrs.is_unused,
            register_name: attrs.register_name,
            cleanup_fn: attrs.cleanup_fn,
            is_thread_local: storage.is_thread_local,
            no_sanitize: attrs.no_sanitize.clone(),
            alias_name: attrs.alias_name.clone(),
            copy_fn: attrs.copy_fn.clone(),
            is_retain: attrs.is_retain,
            is_used_attr: attrs.is_used,
            quals: self.spec_quals.iso_c(),
            addr_space: self.spec_addr_space,
            tls_model: attrs.tls_model.clone(),
            asm_label: attrs.asm_label.clone(),
        })])
    }

    /// 判断 peek 是否像声明符起始（指针/标识符），但排除 __attribute__ 等后缀属性。
    fn peek_declarator_start(&self) -> bool {
        match self.peek.kind {
            TokKind::Star => true,
            TokKind::Ident => !matches!(
                self.peek.text.as_str(),
                "__attribute__" | "__asm__" | "__asm" | "asm" | "__volatile__" | "__volatile" | "__const__" | "__const"
            ),
            _ => false,
        }
    }

    fn parse_decl_specifiers(&mut self) -> Option<Type> {
        self.parse_type_mode(true)
    }

    fn parse_type(&mut self) -> Option<Type> {
        self.parse_type_mode(false)
    }

    fn finish_type(&mut self, ty: Type, specifiers_only: bool) -> Type {
        // 应用 parse_type_mode 中通过 parse_attributes 存储的 vector_size,
        // 处理 __attribute__((vector_size(N))) 出现在类型之前的场景
        let ty = if let Some(vsize) = self.decl_attrs.vector_size.take() {
            let elem_size = ty.size().max(1);
            let count = vsize / elem_size;
            Type::Vector(Box::new(ty.into()), count)
        } else {
            ty
        };
        let ty = self.try_parse_vector_attr(ty);
        if specifiers_only {
            return ty;
        }
        // 限定符可在基类型与 * 之间 (C 声明符语法); 也可出现地址空间限定符
        // `T const *p`: const 在 * 之前, 应属于 pointed-to 类型
        // `T * const p`: const 在 * 之后, 应属于指针变量
        // 先保存 decl_quals, 如果没有 * 则恢复
        let saved_decl_quals = self.decl_quals;
        self.skip_ptr_qualifiers();
        let has_ptr = self.cur.kind == TokKind::Star;
        if !has_ptr {
            // 没冇 *, 限定符属于变量而非 pointed-to 类型, 恢复
            self.decl_quals = saved_decl_quals;
        } else {
            // 有 *: 限定符属于 pointed-to 类型
            // 将 skip_ptr_qualifiers 收集的限定符应用到内部类型
            let inner_quals = self.decl_quals;
            self.decl_quals = QualifierFlags::NONE;
            let ty = self.parse_ptr_suffix_with_quals(ty, inner_quals);
            // 抽象函数指针声明符
            let ty = self.try_parse_abstract_func_ptr(ty);
            return self.parse_array_suffix(ty);
        }
        let ty = self.parse_ptr_suffix(ty);
        // 抽象函数指针声明符: `(*)(params)` 形式 (无变量名的函数指针类型)
        // 例如: void (*)(struct pt_regs *) 在 __builtin_types_compatible_p 中
        let ty = self.try_parse_abstract_func_ptr(ty);
        // 数组后缀: `int [N]`, `int []` (C99 柔性数组), `(int)[N]`
        self.parse_array_suffix(ty)
    }

    /// 尝试解析抽象函数指针声明符: `(*)(params)`
    /// 匹配模式: 当前 token 为 `(`, peek 为 `*`, 再后面为 `)` 或限定符后跟 `)`
    /// 仅匹配无名形式 (抽象声明符), 不匹配 `(*name)(params)` 形式
    fn try_parse_abstract_func_ptr(&mut self, mut ty: Type) -> Type {
        if self.cur.kind != TokKind::LParen || self.peek.kind != TokKind::Star {
            return ty;
        }
        // 保存解析器状态, 以便在不是抽象函数指针时恢复
        let saved_cur = self.cur.clone();
        let saved_peek = self.peek.clone();
        let saved_pos = self.lex.pos;
        let saved_line = self.lex.line;
        let saved_col = self.lex.col;

        self.advance(); // (
        self.advance(); // *
        // 跳过可能的 const/volatile/restrict 限定符与段寄存器限定符
        // (__seg_fs/__seg_gs 等): 与 parse_ptr_suffix / parse_func_ptr_inner
        // 同步, 避免抽象函数指针类型 (如 typeof 中的 `int (*)(...)`)
        // 报告意外 token.
        self.skip_ptr_qualifiers();
        // 跳过可能的地址空间属性 __attribute__((...))
        if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.skip_attributes();
        }
        // 如果有标识符, 说明是命名声明符如 (*func), 不是抽象函数指针类型
        // 恢复解析器状态并返回
        if self.cur.kind == TokKind::Ident {
            self.cur = saved_cur;
            self.peek = saved_peek;
            self.lex.pos = saved_pos;
            self.lex.line = saved_line;
            self.lex.col = saved_col;
            return ty;
        }
        if self.cur.kind != TokKind::RParen {
            // 不是抽象函数指针, 恢复状态
            self.cur = saved_cur;
            self.peek = saved_peek;
            self.lex.pos = saved_pos;
            self.lex.line = saved_line;
            self.lex.col = saved_col;
            return ty;
        }
        self.advance(); // )
        // 现在期望 `(params)` 形式的函数参数列表
        if self.cur.kind != TokKind::LParen {
            // 不是函数指针, 可能只是指针抽象 `(*)`
            ty = Type::Ptr(Box::new(ty.into()));
            return ty;
        }
        self.advance(); // (
        let params = self.parse_params().unwrap_or_default();
        if self.cur.kind == TokKind::RParen {
            self.advance();
        }
        let func_ty = Type::Func(
            Box::new(ty.into()),
            params.iter().map(|p| p.ty.clone().into()).collect(),
        );
        ty = Type::Ptr(Box::new(func_ty.into()));
        // 可能后面还有函数指针后缀
        self.try_parse_abstract_func_ptr(ty)
    }

    /// 解析类型后的数组后缀: `int`, `int [N]`, `int [N][M]`, `int []`.
    /// 出现在 cast / sizeof / 变量声明等多种上下文. 空括号 `[]` 用 ArraySize::Const(0)
    /// 表示柔性数组, 后续 codegen 阶段会处理为指针语义.
    fn parse_array_suffix(&mut self, mut ty: Type) -> Type {
        // 多维数组从内向外结合 (C11 6.7.6.2p4): `int [2][8]` 是
        // "2 个 int[8]", 收集维度后反转包裹, 与函数指针数组一致.
        let mut arr_sizes = Vec::new();
        while self.cur.kind == TokKind::LBracket {
            let la = self.cur.span.clone();
            self.advance();
            let arr_size = if self.cur.kind != TokKind::RBracket {
                // 跳过 const/static/restrict 限定符
                while matches!(
                    self.cur.kind,
                    TokKind::KwConst
                        | TokKind::KwVolatile
                        | TokKind::KwRestrict
                        | TokKind::KwStatic
                ) {
                    self.advance();
                }
                if self.cur.kind == TokKind::RBracket {
                    ArraySize::Const(0)
                } else {
                    let e = self.parse_expr().unwrap_or(Expr::Int {
                        v: 0,
                        span: la.clone(),
                        ty: Type::Int,
                    });
                    if let Ok(v) = crate::const_eval::eval(&e) {
                        ArraySize::Const(v.max(0) as usize)
                    } else {
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    }
                }
            } else {
                ArraySize::Const(0)
            };
            let _ = self.expect(TokKind::RBracket, "']'");
            arr_sizes.push(arr_size);
        }
        for sz in arr_sizes.into_iter().rev() {
            ty = Type::Array(Box::new(ty.into()), sz);
        }
        ty
    }

    fn parse_type_mode(&mut self, specifiers_only: bool) -> Option<Type> {
        let mut is_unsigned = false;
        let mut is_long = 0u8;
        let mut is_short = false;
        let mut base: Option<Type> = None;
        // unsigned/signed 在第一个 loop 中被跳过时标记,
        // 传递给第二个 loop 处理. 这样 `unsigned const char`
        // 和单独的 `unsigned` 都能正确解析.
        let mut pre_unsigned = false;
        // 重置当前声明说明符的限定符和地址空间 (每次新的类型解析开始时清空)
        self.spec_quals = QualifierFlags::NONE;
        self.decl_quals = QualifierFlags::NONE;
        self.spec_addr_space = AddrSpace::None;
        // 跳过存储类/const/volatile/_Atomic/__extension__/__attribute__
        loop {
            // C23 [[attributes]]: 在类型限定符之间出现，在 match 之前检查
            if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                self.parse_std_attributes();
                continue;
            }
            match self.cur.kind {
                TokKind::KwStatic => {
                    self.decl_storage.is_static = true;
                    self.advance();
                }
                TokKind::KwExtern => {
                    self.decl_storage.is_extern = true;
                    self.advance();
                }
                TokKind::KwRegister => {
                    self.decl_storage.is_register = true;
                    self.advance();
                }
                TokKind::KwConst => {
                    self.spec_quals |= QualifierFlags::CONST;
                    self.advance();
                }
                TokKind::KwVolatile => {
                    self.spec_quals |= QualifierFlags::VOLATILE;
                    self.advance();
                }
                TokKind::KwRestrict => {
                    self.spec_quals |= QualifierFlags::RESTRICT;
                    self.advance();
                }
                TokKind::KwInline => {
                    self.decl_attrs.is_inline = true;
                    self.advance();
                }
                // constexpr: C23 编译期常量声明.
                // 变量: 隐含 const, 必须以常量表达式初始化
                // 函数: 等价 inline + always_inline
                TokKind::Ident if self.cur.text == "constexpr" => {
                    self.decl_attrs.is_always_inline = true;
                    self.decl_attrs.is_constexpr = true;
                    self.spec_quals |= QualifierFlags::CONST;
                    self.advance();
                }
                // auto: C89 存储类 或 C23 类型推导
                TokKind::KwAuto => {
                    // 检查下一个 token 是否是类型说明符
                    // 如果是, auto 是 C89 存储类 (等价于跳过)
                    // 否则是 C23 类型推导 (auto x = expr;)
                    let next_is_type = matches!(
                        self.peek.kind,
                        TokKind::KwChar
                            | TokKind::KwInt
                            | TokKind::KwShort
                            | TokKind::KwLong
                            | TokKind::KwFloat
                            | TokKind::KwDouble
                            | TokKind::KwSigned
                            | TokKind::KwUnsigned
                            | TokKind::KwVoid
                            | TokKind::KwBool
                            | TokKind::KwStruct
                            | TokKind::KwUnion
                            | TokKind::KwEnum
                            | TokKind::KwConst
                            | TokKind::KwVolatile
                            | TokKind::KwStatic
                            | TokKind::KwExtern
                            | TokKind::KwRegister
                            | TokKind::KwComplex
                            | TokKind::KwAtomic
                            | TokKind::KwAuto
                    );
                    if next_is_type {
                        // C89 存储类: 跳过 (auto 是默认存储类)
                        self.advance();
                    } else {
                        // C23 类型推导
                        self.advance();
                        self.decl_auto_type = true;
                        return Some(self.finish_type(Type::Int, specifiers_only));
                    }
                }
                // C23 bool 关键字; 非 C23 模式下也识别为 bool,
                // 因为 C++  lowered 输出和 glibc 头文件中广泛使用.
                TokKind::Ident if self.cur.text == "bool" => {
                    base = Some(Type::Bool);
                    self.advance();
                }
                // C23 thread_local (仅 C23 模式; C11 拼写为 _Thread_local)
                TokKind::Ident if self.cur.text == "thread_local" && self.opts.is_c23 => {
                    self.decl_storage.is_thread_local = true;
                    self.advance();
                }
                // C23 alignas(N) / alignas(type); _Alignas 为 C11 拼写 (无条件)
                TokKind::Ident
                    if (self.cur.text == "alignas" && self.opts.is_c23)
                        || self.cur.text == "_Alignas" =>
                {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        if self.is_type_start() && self.cur.kind != TokKind::Number {
                            // alignas(type): 取该类型的对齐值
                            if let Some(t) = self.parse_type() {
                                let a = t.align();
                                let cur = self.decl_attrs.alignment.unwrap_or(0);
                                self.decl_attrs.alignment = Some(cur.max(a));
                            }
                        } else {
                            // alignas(常量表达式): 编译期求值
                            if let Some(e) = self.parse_expr_bp(2) {
                                if let Ok(v) = crate::const_eval::eval(&e) {
                                    if v > 0 {
                                        let cur = self.decl_attrs.alignment.unwrap_or(0);
                                        self.decl_attrs.alignment = Some(cur.max(v as usize));
                                    }
                                }
                            }
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance();
                        }
                    }
                }
                // _Noreturn: C11 函数限定符, 标记函数不返回
                // codegen 在函数末尾插入 trap 指令 (ud2 / brk)
                // 防止执行流落入后续代码
                TokKind::Ident if self.cur.text == "_Noreturn" => {
                    self.decl_attrs.is_noreturn = true;
                    self.advance();
                }
                // _Thread_local / __thread：线程局部存储
                TokKind::Ident
                    if self.cur.text == "_Thread_local" || self.cur.text == "__thread" =>
                {
                    self.decl_storage.is_thread_local = true;
                    self.advance();
                }
                TokKind::KwAtomic => {
                    self.advance();
                    // _Atomic(type) 形式: 解析内部类型并包装为 Atomic
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let inner = self.parse_type();
                        if self.cur.kind == TokKind::RParen {
                            self.advance();
                        }
                        let ty = Type::Atomic(Box::new(inner?.into()));
                        return Some(self.finish_type(ty, specifiers_only));
                    }
                    // _Atomic 作为类型限定符: 设置 is_atomic 标志
                    // 注意: 不做类型包装, 让后续解析决定具体的类型上下文
                    // 在 parse_ptr_suffix 中也会处理 _Atomic 限定符
                    // 当前简化: 作为 qualifier 跳过, 不生成 Type::Atomic 包装
                    // (详细的类型包装在 _Atomic(type) 语法中已完成)
                }
                // unsigned/signed: 在第一个 loop 中跳过并标记,
                // 不设 is_unsigned 以避免提前返回.
                // const/volatile 等可在其后出现 (如 `unsigned const char`).
                TokKind::KwUnsigned => {
                    pre_unsigned = true;
                    self.advance();
                }
                TokKind::KwSigned => {
                    self.advance();
                }
                // __extension__: GNU 扩展, 抑制警告, 直接跳过
                TokKind::Ident if self.cur.text == "__extension__" => {
                    self.advance();
                }
                // __attribute__: 在类型限定符之间出现, 跳过
                TokKind::Ident if self.cur.text == "__attribute__" => {
                    let attrs = self.parse_attributes();
                    self.decl_attrs.merge(attrs);
                }
                // __asm__("label"): 在类型限定符之间出现, 跳过
                TokKind::Ident if self.cur.text == "__asm__" || self.cur.text == "__asm" => {
                    self.skip_asm_label();
                }
                // __auto_type: GNU 扩展，类型由 = 后表达式推导
                TokKind::Ident if self.cur.text == "__auto_type" => {
                    self.advance();
                    self.decl_auto_type = true;
                    return Some(self.finish_type(Type::Int, specifiers_only));
                }
                // typeof(expr/type): GNU 扩展, 推导类型
                // typeof_unqual: C23 关键字 (仅 C23 模式); dcc 类型系统不保留
                // 限定符, 故与 typeof 等价
                TokKind::Ident
                    if self.cur.text == "typeof"
                        || self.cur.text == "__typeof__"
                        || self.cur.text == "__typeof"
                        || (self.cur.text == "typeof_unqual" && self.opts.is_c23) =>
                {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        if self.typeof_operand_is_type_form() {
                            self.allow_named_type = true;
                            let ty = self.parse_type()?;
                            self.allow_named_type = false;
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                            return Some(self.finish_type(ty, specifiers_only));
                        } else {
                            let e = self.parse_expr()?;
                            if self.cur.kind == TokKind::RParen {
                                self.advance();
                            }
                            return Some(self.finish_type(e.ty(), specifiers_only));
                        }
                    }
                }
                // __const__/__volatile__/__restrict__/__restrict: GNU 扩展拼写, 同 const/volatile/restrict
                TokKind::Ident
                    if self.cur.text == "__const__"
                        || self.cur.text == "__const"
                        || self.cur.text == "__volatile__"
                        || self.cur.text == "__volatile"
                        || self.cur.text == "__restrict__"
                        || self.cur.text == "__restrict" =>
                {
                    self.advance();
                }
                // __seg_fs / __seg_gs / __seg_cs / __seg_ds / __seg_es / __seg_ss:
                // x86 段寄存器地址空间限定符 (per-cpu 变量访问需要).
                // 捕获地址空间并存入 spec_addr_space, 最终写入 VarDecl.
                // 与 skip_ptr_qualifiers 中的同名关键字配合,
                // 确保在 decl-specifier / 类型与 * 之间 / * 与标识符之间
                // 等位置都被正确处理.
                TokKind::Ident
                    if matches!(
                        self.cur.text.as_str(),
                        "__seg_fs"
                            | "__seg_gs"
                            | "__seg_cs"
                            | "__seg_ds"
                            | "__seg_es"
                            | "__seg_ss"
                    ) =>
                {
                    self.spec_addr_space = AddrSpace::from_ident(&self.cur.text);
                    self.advance();
                }
                // CUDA/HIP 函数限定符: __global__ / __device__ / __host__
                // 仅 -fcuda / -fhip 时解析为限定符 (否则是普通标识符,
                // 由后续语义阶段报 undeclared, 位置精确到标识符)
                TokKind::Ident
                    if matches!(
                        self.cur.text.as_str(),
                        "__global__" | "__device__" | "__host__"
                    ) && (self.opts.cuda || self.opts.hip) =>
                {
                    match self.cur.text.as_str() {
                        "__global__" => self.decl_attrs.is_global = true,
                        "__device__" => self.decl_attrs.is_device = true,
                        _ => self.decl_attrs.is_host = true,
                    }
                    self.advance();
                }
                // MSVC 扩展 _ms_restrict: 视为 restrict 同义
                TokKind::Ident if self.cur.text == "_ms_restrict" => {
                    self.advance();
                }
                // MSVC 扩展 __declspec: 解析参数并转化为等价 GCC 属性
                // 形式: __declspec(align(8)) / __declspec(noreturn) / __declspec(thread)
                //       __declspec(dllimport) / __declspec(dllexport)
                TokKind::Ident if self.cur.text == "__declspec" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        // 解析 __declspec 的内容, 逐个 ident 应用等价属性
                        while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                            if self.cur.kind == TokKind::Ident {
                                let kw = self.cur.text.clone();
                                self.advance();
                                if kw == "thread" {
                                    // __declspec(thread) ≡ __thread (TLS 变量)
                                    self.decl_storage.is_thread_local = true;
                                } else if kw == "noreturn" || kw == "_Noreturn" {
                                    // __declspec(noreturn) 标记函数不返回:
                                    // 加 cold + noinline 提示, 同时设 is_noreturn
                                    // 让 codegen 在函数末尾插入 trap 指令
                                    // 同时接受 _Noreturn (预处理器展开 stdnoreturn.h 的 noreturn 宏后)
                                    self.decl_attrs.is_cold = true;
                                    self.decl_attrs.is_noinline = true;
                                    self.decl_attrs.is_noreturn = true;
                                } else if kw == "align" {
                                    // __declspec(align(N)): 设置对齐
                                    if self.cur.kind == TokKind::LParen {
                                        self.advance();
                                        if self.cur.kind == TokKind::Number {
                                            if let Ok(n) = self.cur.text.parse::<usize>() {
                                                // 已有 alignment 则取较大者
                                                let cur = self.decl_attrs.alignment.unwrap_or(0);
                                                self.decl_attrs.alignment = Some(cur.max(n));
                                            }
                                            self.advance();
                                        }
                                        if self.cur.kind == TokKind::RParen {
                                            self.advance();
                                        }
                                    }
                                }
                                // dllimport / dllexport / 其他: 接受但无 codegen 差异
                            } else if self.cur.kind == TokKind::Number {
                                self.advance();
                            } else if self.cur.kind == TokKind::LParen {
                                // 跳过一个带参数的子句 (例如 align(N))
                                self.advance();
                                let mut depth = 1i32;
                                while depth > 0 && self.cur.kind != TokKind::Eof {
                                    if self.cur.kind == TokKind::LParen {
                                        depth += 1;
                                    } else if self.cur.kind == TokKind::RParen {
                                        depth -= 1;
                                    }
                                    if depth > 0 {
                                        self.advance();
                                    }
                                }
                                if self.cur.kind == TokKind::RParen {
                                    self.advance();
                                }
                            } else {
                                self.advance();
                            }
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance();
                        }
                    }
                }
                // MSVC 扩展 __pragma: 跳过参数
                TokKind::Ident if self.cur.text == "__pragma" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let mut depth = 1i32;
                        while depth > 0 && self.cur.kind != TokKind::Eof {
                            if self.cur.kind == TokKind::LParen {
                                depth += 1;
                            } else if self.cur.kind == TokKind::RParen {
                                depth -= 1;
                            }
                            self.advance();
                        }
                    }
                }
                // _Pragma("..."): C99 操作符
                TokKind::Ident if self.cur.text == "_Pragma" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        // 提取字符串字面量; 实际应用在 preproc 阶段已经完成
                        // (preproc 把 _Pragma(...) 整体收集到 pending_pragmas 后批量执行).
                        // 此处仅做语法消费, 不再额外处理.
                        if self.cur.kind == TokKind::StringLit {
                            self.advance();
                        }
                        while self.cur.kind != TokKind::RParen && self.cur.kind != TokKind::Eof {
                            self.advance();
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance();
                        }
                    }
                }
                // SysV AMD64 va_list: gp_offset, fp_offset,
                // overflow_arg_area, reg_save_area.  C ABI 中它是单元素数组；
                // AST 内保留等价的 24 字节对象，builtin 操作直接取得其地址。
                TokKind::Ident if self.cur.text == "__builtin_va_list" => {
                    self.advance();
                    base = Some(Type::Struct(
                        "__dcc_sysv_va_list".into(),
                        Arc::new(vec![
                            Field {
                                name: "gp_offset".into(),
                                ty: Type::UInt,
                                offset: 0,
                                bit_width: None,
                                bit_offset: 0,
                                quals: QualifierFlags::NONE,
                                ptr_quals: vec![],
                                field_align: None,
                            },
                            Field {
                                name: "fp_offset".into(),
                                ty: Type::UInt,
                                offset: 4,
                                bit_width: None,
                                bit_offset: 0,
                                quals: QualifierFlags::NONE,
                                ptr_quals: vec![],
                                field_align: None,
                            },
                            Field {
                                name: "overflow_arg_area".into(),
                                ty: Type::Ptr(Box::new(Type::Void.into())),
                                offset: 8,
                                bit_width: None,
                                bit_offset: 0,
                                quals: QualifierFlags::NONE,
                                ptr_quals: vec![],
                                field_align: None,
                            },
                            Field {
                                name: "reg_save_area".into(),
                                ty: Type::Ptr(Box::new(Type::Void.into())),
                                offset: 16,
                                bit_width: None,
                                bit_offset: 0,
                                quals: QualifierFlags::NONE,
                                ptr_quals: vec![],
                                field_align: None,
                            },
                        ]),
                        8, // va_list align: max field = ptr (8)
                    ));
                }
                // constexpr: C23 编译期常量声明.
                // 变量: 隐含 const, 必须以常量表达式初始化
                // 函数: 等价 inline + always_inline
                TokKind::Ident if self.cur.text == "constexpr" => {
                    self.decl_attrs.is_always_inline = true;
                    self.decl_attrs.is_constexpr = true;
                    self.spec_quals |= QualifierFlags::CONST;
                    self.advance();
                }
                // ISO/IEC TS 18661 / GCC：_FloatN 二进制浮点类型
                TokKind::Ident
                    if matches!(
                        self.cur.text.as_str(),
                        "_Float32" | "_Float32x" | "_Float16"
                    ) =>
                {
                    base = Some(Type::Float);
                    self.advance();
                }
                TokKind::Ident if matches!(self.cur.text.as_str(), "_Float64" | "_Float64x") => {
                    base = Some(Type::Double);
                    self.advance();
                }
                TokKind::Ident if matches!(self.cur.text.as_str(), "_Float128" | "_Float128x") => {
                    base = Some(Type::LongDouble);
                    self.advance();
                }
                // _BitInt(N): C23 定宽整数类型
                // 用 base 类型 + 标志位, 让 resolve_base 统一处理 signed/unsigned
                TokKind::Ident if self.cur.text == "_BitInt" => {
                    self.advance();
                    let width = if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let w = if self.cur.kind == TokKind::Number {
                            self.cur.text.parse::<u32>().unwrap_or(32)
                        } else {
                            32
                        };
                        if matches!(self.cur.kind, TokKind::Number) {
                            self.advance();
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance();
                        }
                        w
                    } else {
                        32
                    };
                    // 映射到基础类型, signed/unsigned 由 is_unsigned 标志决定
                    match width {
                        0..=8 => {
                            base = Some(Type::Char);
                        }
                        9..=16 => {
                            is_short = true;
                        }
                        17..=32 => {
                            base = Some(Type::Int);
                        }
                        _ => {
                            is_long = 2;
                        }
                    }
                }
                // 内核属性宏 (__weak, __init 等): 仅在后面是标识符或指针时跳过
                TokKind::Ident
                    if self.cur.text.starts_with("__")
                        && !self.typedefs.contains_key(&self.cur.text)
                        && self.peek_declarator_start() =>
                {
                    self.advance();
                }
                _ => break,
            }
        }
        // 如果已从 _BitInt(typeof 等确定了 base 类型,立即解析避免后续 ident 误判
        // 注意: 必须放在 struct/union/enum/ident 检查之前
        // _Complex 不在此触发早退: 它可与 _Atomic/限定符任意顺序组合,
        // 且必须配 float/double/long double 基类型, 留到第二个 loop 统一处理.
        // pre_unsigned: 第一个 loop 中看到了 unsigned, 若后面不是类型关键字
        // (如 `unsigned x`), 则 unsigned 是完整类型, 立即返回.
        // 若后面是类型关键字 (如 `unsigned const char`), 不返回, 让第二个 loop 处理.
        if pre_unsigned && !self.is_peek_type_start_after_unsigned() {
            is_unsigned = true;
        }
        if base.is_some() || is_unsigned || is_long > 0 || is_short {
            let ty = self.resolve_base(base, is_unsigned, is_long, is_short);
            return Some(self.finish_type(ty, specifiers_only));
        }
        // struct/union/enum
        if self.cur.kind == TokKind::KwStruct || self.cur.kind == TokKind::KwUnion {
            let is_u = self.cur.kind == TokKind::KwUnion;
            // struct 关键字所在行, 用于查询 #pragma pack
            let struct_line = self.cur.span.line;
            self.advance();
            // struct __attribute__((packed/aligned/may_alias)) name
            let mut pack = false;
            let mut align_n: Option<usize> = None;
            let mut may_alias = false;
            while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                let pre_attrs = self.parse_attributes();
                if pre_attrs.is_packed { pack = true; }
                if pre_attrs.alignment.is_some() { align_n = pre_attrs.alignment; }
                if pre_attrs.may_alias { may_alias = true; }
                // 非类型属性存入 decl_attrs 供后续声明使用
                self.decl_attrs.merge(pre_attrs);
            }
            let nm = if self.cur.kind == TokKind::Ident {
                let n = self.cur.text.clone();
                self.advance();
                n
            } else {
                String::new()
            };
            // If the "name" is __attribute__, it's a post-body type attribute,
            // not a variable name.  Parse it to extract packed/aligned, then
            // continue looking for the struct body '{'.
            // e.g.: struct page { ... } __attribute__((__aligned__(N)));
            // Here the attribute is actually after '}', but since there is no
            // explicit struct name token, the parser encounters __attribute__
            // in the name position.
            let nm = if nm == "__attribute__" {
                // Parse post-body __attribute__((...)) blocks
                let post_attrs = self.parse_attributes();
                if post_attrs.is_packed { pack = true; }
                if post_attrs.alignment.is_some() { align_n = post_attrs.alignment; }
                if post_attrs.may_alias { may_alias = true; }
                self.decl_attrs.merge(post_attrs);
                // After attributes, check for actual struct name
                if self.cur.kind == TokKind::Ident
                    && self.cur.text != "__attribute__"
                {
                    let n = self.cur.text.clone();
                    self.advance();
                    n
                } else {
                    String::new()
                }
            } else {
                nm
            };
            if self.cur.kind == TokKind::LBrace {
                self.advance();
                let fields = self.parse_fields();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                let mut fields = fields;
                // C99 6.7.2.1: flexible array member (FAM) 必须是结构体末尾成员,
                // 且结构体必须含 ≥ 1 个具名字段. union 内不允许 FAM.
                // GCC 扩展允许所有这些模式, Linux 内核大量使用:
                //   - FAM 在 struct 中间: __cacheline_group, stack guards, LSM hooks
                //   - 仅含 FAM 的 struct: UAPI 头文件
                //   - union 中零长数组: 某些宏展开
                // GCC 默认不警告这些模式, dcc 也不应警告.
                if !is_u {
                    let last_idx = fields.len().saturating_sub(1);
                    for (idx, f) in fields.iter().enumerate() {
                        if is_zero_sized_array(&f.ty) {
                            if idx != last_idx {
                                // GCC 扩展: FAM 出现在 struct 中间, 静默接受
                            } else if count_named_fields(&fields) < 2 {
                                // GCC 扩展: 仅含 FAM 的 struct, 静默接受
                            }
                        }
                    }
                }
                // union 中零长数组: GCC 扩展, 静默接受
                // 检查 struct 后的 __attribute__((packed/aligned(N)/may_alias/...))
                if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                    let post_attrs = self.parse_attributes();
                    if post_attrs.is_packed { pack = true; }
                    if post_attrs.alignment.is_some() { align_n = post_attrs.alignment; }
                    if post_attrs.may_alias { may_alias = true; }
                    // 非类型属性存入 decl_attrs 供后续声明使用
                    self.decl_attrs.merge(post_attrs);
                }
                if pack {
                    Type::layout_struct_packed(&mut fields, is_u);
                    // Packed union: 添加零宽度哨兵字段, 使 aggregate_align_from_offsets
                    // 能检测到该 union 是 packed 的 (对齐为 1 而非自然对齐).
                    // 哨兵 bit_width=Some(0) 不影响 size() 计算 (union size = max field size).
                    if is_u {
                        fields.push(Field {
                            name: "\0__packed__".into(),
                            ty: Type::UChar,
                            offset: 0,
                            bit_width: Some(0),
                            bit_offset: 0,
                            quals: QualifierFlags::NONE,
                            ptr_quals: vec![],
                            field_align: None,
                        });
                    }
                } else {
                    // 优先级: __attribute__((packed)) > 当前 #pragma pack(n) > 自然对齐
                    let max_align = self.pack_at_line(struct_line);
                    Type::layout_struct_with_pack(&mut fields, is_u, max_align);
                }
                // __attribute__((aligned(n))) only raises the struct's
                // overall alignment requirement; it does NOT realign each
                // field. The size() method already rounds up the final size
                // to the struct alignment.
                // 计算并存储结构体对齐值
                let struct_align = if pack {
                    1
                } else {
                    let pragma_pack = self.pack_at_line(struct_line);
                    // 考虑字段级 aligned 属性: max(field_align 或 natural alignment)
                    let max_field = fields
                        .iter()
                        .map(|f| match f.field_align {
                            Some(a) => a,
                            None => f.ty.align(),
                        })
                        .max()
                        .unwrap_or(1)
                        .max(1);
                    match pragma_pack {
                        Some(n) if n < max_field => n,
                        _ => max_field,
                    }
                };
                // __attribute__((aligned(n))) 可提升对齐
                let struct_align = if let Some(a) = align_n {
                    struct_align.max(a)
                } else {
                    struct_align
                };
                let fields_rc = Arc::new(fields);
                self.struct_defs
                    .insert(nm.clone(), (Arc::clone(&fields_rc), is_u, struct_align));
                // 记录 may_alias 类型属性供后续使用
                if may_alias {
                    self.decl_attrs.may_alias = true;
                }
                let ty = if is_u {
                    Type::Union(nm, fields_rc, struct_align)
                } else {
                    Type::Struct(nm, fields_rc, struct_align)
                };
                return Some(self.finish_type(ty, specifiers_only));
            } else if !nm.is_empty() {
                if let Some((f, _, a)) = self.struct_defs.get(&nm) {
                    let ty = if is_u {
                        Type::Union(nm, Arc::clone(f), *a)
                    } else {
                        Type::Struct(nm, Arc::clone(f), *a)
                    };
                    return Some(self.finish_type(ty, specifiers_only));
                }
                // 不完整类型: 登记占位，待后续定义补全字段
                let empty = Arc::new(vec![]);
                self.struct_defs
                    .insert(nm.clone(), (Arc::clone(&empty), is_u, 0));
                let ty = if is_u {
                    Type::Union(nm, empty, 0)
                } else {
                    Type::Struct(nm, empty, 0)
                };
                return Some(self.finish_type(ty, specifiers_only));
            }
            self.err_here(
                "E0003",
                "expected struct name or '{'",
                Some("struct/union definition needs a name or body: struct Name { ... }"),
            );
            return None;
        }
        if self.cur.kind == TokKind::KwEnum {
            self.advance();
            // 检测 enum 名前的属性: `enum __attribute__((packed)) E`
            let pre_enum_attrs = self.parse_attributes();
            let mut enum_packed = pre_enum_attrs.is_packed;
            // 非类型属性 (section, used 等) 存入 decl_attrs 供后续声明使用
            self.decl_attrs.merge(pre_enum_attrs);
            let nm = if self.cur.kind == TokKind::Ident {
                let n = self.cur.text.clone();
                self.advance();
                n
            } else {
                String::new()
            };
            // C23 底层类型: enum E : unsigned char { ... }
            // 仅允许整数类型说明符; packed 属性后续编码为 Some(SChar)
            let mut under_ty: Option<Box<QualType>> = None;
            if self.cur.kind == TokKind::Colon {
                self.advance();
                let mut uns = false;
                let mut lng = 0u8;
                let mut shrt = false;
                let mut b: Option<Type> = None;
                loop {
                    match self.cur.kind {
                        TokKind::KwSigned => {
                            self.advance();
                        }
                        TokKind::KwUnsigned => {
                            uns = true;
                            self.advance();
                        }
                        TokKind::KwInt => {
                            b = Some(Type::Int);
                            self.advance();
                        }
                        TokKind::KwChar => {
                            b = Some(Type::Char);
                            self.advance();
                        }
                        TokKind::KwShort => {
                            shrt = true;
                            self.advance();
                        }
                        TokKind::KwLong => {
                            lng += 1;
                            self.advance();
                        }
                        TokKind::KwBool => {
                            b = Some(Type::Bool);
                            self.advance();
                        }
                        // bool 关键字: 始终识别 (C23 关键字, 但 gnu11 下也常用)
                        TokKind::Ident if self.cur.text == "bool" => {
                            b = Some(Type::Bool);
                            self.advance();
                        }
                        TokKind::Ident if self.cur.text == "_BitInt" => {
                            self.advance();
                            let width = if self.cur.kind == TokKind::LParen {
                                self.advance();
                                let w = if self.cur.kind == TokKind::Number {
                                    self.cur.text.parse::<u32>().unwrap_or(32)
                                } else {
                                    32
                                };
                                if matches!(self.cur.kind, TokKind::Number) {
                                    self.advance();
                                }
                                if self.cur.kind == TokKind::RParen {
                                    self.advance();
                                }
                                w
                            } else {
                                32
                            };
                            match width {
                                0..=8 => {
                                    b = Some(Type::Char);
                                }
                                9..=16 => {
                                    shrt = true;
                                }
                                17..=32 => {
                                    b = Some(Type::Int);
                                }
                                _ => {
                                    lng = 2;
                                }
                            }
                        }
                        _ => break,
                    }
                }
                if b.is_some() || uns || lng > 0 || shrt {
                    under_ty = Some(Box::new(self.resolve_base(b, uns, lng, shrt).into()));
                }
            }
            if self.cur.kind == TokKind::LBrace {
                self.advance();
                let vs = self.parse_enum_body();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                let vals: Vec<(String, i64)> = vs
                    .iter()
                    .map(|(id, v)| (id.name.clone(), v.unwrap_or(0)))
                    .collect();
                let vals_rc = Arc::new(vals);
                // 检查 enum 体后的 __attribute__((packed/used/section/...))
                let post_enum_attrs = self.parse_attributes();
                enum_packed |= post_enum_attrs.is_packed;
                // 非类型属性存入 decl_attrs 供后续变量声明使用
                self.decl_attrs.merge(post_enum_attrs);
                // packed 属性优先: 编码为 1 字节有符号 (与旧行为一致)
                let under = if enum_packed {
                    Some(Box::new(Type::SChar.into()))
                } else {
                    under_ty.clone()
                };
                self.enum_defs
                    .insert(nm.clone(), (Arc::clone(&vals_rc), under.clone()));
                // C23: 枚举常量类型 = 枚举底层类型 (packed 为 GNU 扩展, 保持 int)
                let mut nv = 0i64;
                for (id, v) in &vs {
                    let val = v.unwrap_or(nv);
                    let cty = if enum_packed {
                        Type::Int
                    } else {
                        under_ty.as_deref().map(|q| q.base.clone()).unwrap_or(Type::Int)
                    };
                    self.enum_consts.insert(id.name.clone(), (val, cty));
                    nv = val + 1;
                }
                return Some(self.finish_type(Type::Enum(nm, vals_rc, under), specifiers_only));
            } else if !nm.is_empty() {
                if let Some((v, under)) = self.enum_defs.get(&nm) {
                    return Some(self.finish_type(
                        Type::Enum(nm, Arc::clone(v), under.clone()),
                        specifiers_only,
                    ));
                }
                // 前向声明带底层类型: 登记占位 (C23: enum E : unsigned char;)
                if let Some(u) = under_ty {
                    self.enum_defs
                        .insert(nm.clone(), (Arc::new(vec![]), Some(u.clone())));
                    return Some(
                        self.finish_type(
                            Type::Enum(nm, Arc::new(vec![]), Some(u)),
                            specifiers_only,
                        ),
                    );
                }
                return Some(
                    self.finish_type(Type::Enum(nm, Arc::new(vec![]), None), specifiers_only),
                );
            }
        }
        // typedef 引用
        if self.cur.kind == TokKind::Ident {
            if let Some(ty) = self.typedefs.get(&self.cur.text).cloned() {
                self.advance();
                // typedef 名后的 __attribute__: 如 asmlinkage = __attribute__((aligned(4)))
                // 解析并存入 decl_attrs 供后续声明使用
                let td_attrs = self.parse_attributes();
                self.decl_attrs.merge(td_attrs);
                return Some(self.finish_type(ty, specifiers_only));
            }
            if self.allow_named_type {
                let name = self.cur.text.clone();
                self.advance();
                return Some(self.finish_type(Type::Named(name), specifiers_only));
            }
            if specifiers_only && self.has_storage_without_base(&base) && self.peek_decl_context() {
                self.warn_here(
                    "W0001",
                    "type defaults to 'int' in declaration without a type specifier",
                    Some("add an explicit type such as 'int'"),
                );
                base = Some(Type::Int);
            } else {
                let name = self.cur.text.clone();
                if self.opts.permissive {
                    self.warn_here(
                        "W0002",
                        format!("unknown type '{}' treated as 'int' (-fpermissive)", name),
                        Some("declare the typedef or remove -fpermissive for strict checking"),
                    );
                    self.advance();
                    return Some(self.finish_type(Type::Int, specifiers_only));
                }
                self.diags.push(
                    Diagnostic::err(
                        "E0009",
                        format!("unknown type or typedef '{}'", name),
                        self.cur.span.clone(),
                    )
                    .with_hint("declare the typedef before using it"),
                );
                self.advance();
                return None;
            }
        }
        // 基本类型
        let mut is_complex = false;
        let mut is_imaginary = false;
        // 携带第一个 loop 中看到的 unsigned 标志
        if pre_unsigned {
            is_unsigned = true;
        }
        loop {
            // 注意: 浮点类 (float / double / long double / decimal) 与 _Complex
            // 复合 (例 `double _Complex z`), 故其 arm 不带 `break`, 让第二 loop
            // 继续消费后续修饰符. 整数类 (int / char / short / long / int128)
            // 与 _Bool / void / _Atomic 不与 _Complex 组合, 加 `break` 防止吞掉
            // 后续 declarator ident (例 `typedef _Bool _Bool;`).
            match self.cur.kind {
                TokKind::KwComplex => {
                    is_complex = true;
                    self.advance();
                }
                TokKind::KwImaginary => {
                    is_imaginary = true;
                    self.advance();
                }
                TokKind::KwInt => {
                    base = Some(Type::Int);
                    self.advance();
                }
                TokKind::KwChar => {
                    base = Some(Type::Char);
                    self.advance();
                }
                TokKind::KwShort => {
                    is_short = true;
                    self.advance();
                }
                TokKind::KwLong => {
                    is_long += 1;
                    self.advance();
                }
                TokKind::KwVoid => {
                    base = Some(Type::Void);
                    self.advance();
                    break;
                }
                TokKind::KwBool => {
                    base = Some(Type::Bool);
                    self.advance();
                    break;
                }
                TokKind::KwChar8 => {
                    base = Some(Type::Char8);
                    self.advance();
                    break;
                }
                TokKind::KwChar16 => {
                    base = Some(Type::Char16);
                    self.advance();
                    break;
                }
                TokKind::KwChar32 => {
                    base = Some(Type::Char32);
                    self.advance();
                    break;
                }
                TokKind::KwFloat => {
                    base = Some(Type::Float);
                    self.advance();
                }
                TokKind::KwDouble => {
                    base = Some(Type::Double);
                    self.advance();
                }
                TokKind::KwDecimal32 => {
                    base = Some(Type::Float);
                    self.advance();
                }
                TokKind::KwDecimal64 => {
                    base = Some(Type::Double);
                    self.advance();
                }
                TokKind::KwDecimal128 => {
                    base = Some(Type::LongDouble);
                    self.advance();
                }
                TokKind::KwInt128 => {
                    base = Some(Type::Int128);
                    self.advance();
                    break;
                }
                TokKind::KwUInt128 => {
                    base = Some(Type::UInt128);
                    self.advance();
                    break;
                }
                TokKind::KwFract => {
                    base = Some(Type::Fract);
                    self.advance();
                    break;
                }
                TokKind::KwAccum => {
                    base = Some(Type::Accum);
                    self.advance();
                    break;
                }
                TokKind::KwSigned => {
                    self.advance();
                }
                TokKind::KwUnsigned => {
                    is_unsigned = true;
                    self.advance();
                }
                // const/volatile/restrict 可在类型关键字之后出现
                // (如 `unsigned const char`), 在此消费
                TokKind::KwConst => {
                    self.spec_quals |= QualifierFlags::CONST;
                    self.advance();
                }
                TokKind::KwVolatile => {
                    self.spec_quals |= QualifierFlags::VOLATILE;
                    self.advance();
                }
                TokKind::KwRestrict => {
                    self.spec_quals |= QualifierFlags::RESTRICT;
                    self.advance();
                }
                // _BitInt(N): C23 定宽整数类型 (也在第一个 loop 中处理,但 unsigned 在第二个 loop)
                TokKind::Ident if self.cur.text == "_BitInt" => {
                    self.advance();
                    let width = if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let w = if self.cur.kind == TokKind::Number {
                            self.cur.text.parse::<u32>().unwrap_or(32)
                        } else {
                            32
                        };
                        if matches!(self.cur.kind, TokKind::Number) {
                            self.advance();
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance();
                        }
                        w
                    } else {
                        32
                    };
                    match width {
                        0..=8 => {
                            base = Some(Type::Char);
                        }
                        9..=16 => {
                            is_short = true;
                        }
                        17..=32 => {
                            base = Some(Type::Int);
                        }
                        _ => {
                            is_long = 2;
                        }
                    }
                }
                // constexpr: C23 编译期常量声明.
                // 变量: 隐含 const, 必须以常量表达式初始化
                // 函数: 等价 inline + always_inline
                TokKind::Ident if self.cur.text == "constexpr" => {
                    self.decl_attrs.is_always_inline = true;
                    self.decl_attrs.is_constexpr = true;
                    self.spec_quals |= QualifierFlags::CONST;
                    self.advance();
                }
                _ => break,
            }
        }
        // 消耗类型后的内核属性宏 (仅在后面是标识符或指针时, 避免误吞函数名)
        while self.cur.kind == TokKind::Ident
            && self.cur.text.starts_with("__")
            && !self.typedefs.contains_key(&self.cur.text)
            && self.cur.text != "__attribute__"
            && self.cur.text != "__asm__"
            && self.cur.text != "__asm"
            && self.peek_declarator_start()
        {
            self.advance();
        }
        if base.is_some() || is_unsigned || is_long > 0 || is_short || is_complex || is_imaginary {
            let ty = self.resolve_base(base, is_unsigned, is_long, is_short);
            let ty = if is_complex {
                Type::Complex(Box::new(ty.into()))
            } else if is_imaginary {
                // C99 6.2.5p11: _Imaginary 仅可与浮点类型组合
                if !matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                    self.err_here(
                        "E0003",
                        "_Imaginary requires a floating-point type".to_string(),
                        Some("use _Imaginary float, _Imaginary double, or _Imaginary long double"),
                    );
                }
                Type::Imaginary(Box::new(ty.into()))
            } else {
                ty
            };
            Some(self.finish_type(ty, specifiers_only))
        } else {
            self.err_here(
                "E0003",
                format!("expected type, got {:?}", self.cur.kind),
                Some("use a known type like int, void, or a typedef/struct/enum"),
            );
            None
        }
    }

    fn resolve_base(&self, base: Option<Type>, uns: bool, long: u8, short: bool) -> Type {
        if let Some(
            ty @ (Type::Struct(_, _, _) | Type::Union(_, _, _) | Type::Enum(_, _, _) | Type::Atomic(_)),
        ) = base
        {
            return ty;
        }
        match (base, uns, long, short) {
            (Some(Type::Void), ..) => Type::Void,
            (Some(Type::Bool), ..) => Type::Bool,
            (Some(Type::Char16), ..) => Type::Char16,
            (Some(Type::Char32), ..) => Type::Char32,
            (Some(Type::Int128), true, ..) => Type::UInt128,
            (Some(Type::Int128), false, ..) => Type::Int128,
            (Some(Type::UInt128), ..) => Type::UInt128,
            (Some(Type::Float), ..) => Type::Float,
            (Some(Type::Double), _, 0, _) => Type::Double,
            (Some(Type::Double), _, _, _) => Type::LongDouble,
            (Some(Type::LongDouble), ..) => Type::LongDouble, // _Decimal128 直接映射
            (Some(Type::Fract), ..) => Type::Fract,
            (Some(Type::Accum), ..) => Type::Accum,
            (Some(Type::Char), false, ..) => Type::SChar,
            (Some(Type::Char), true, ..) => Type::UChar,
            (_, _, _, true) if uns => Type::UShort,
            (_, _, _, true) => Type::Short,
            (_, true, 0, _) => Type::UInt,
            (_, true, 1, _) => Type::ULong,
            (_, true, _, _) => Type::ULongLong,
            (_, false, 0, false) => Type::Int,
            (_, false, 1, _) => Type::Long,
            (_, false, _, _) => Type::LongLong,
            // 仅有修饰符无基类型时, 默认基类型为 int (C 标准隐式 int 规则)
            _ => Type::Int,
        }
    }

    fn skip_ptr_qualifiers(&mut self) {
        while matches!(
            self.cur.kind,
            TokKind::KwConst | TokKind::KwVolatile | TokKind::KwAtomic | TokKind::KwRestrict
        ) || (self.cur.kind == TokKind::Ident
            && matches!(
                self.cur.text.as_str(),
                "__restrict__"
                    | "__restrict"
                    | "__const__"
                    | "__const"
                    | "__volatile__"
                    | "__volatile"
                    | "__attribute__"
                    | "_ms_restrict"
                    | "_restrict"
                    | "__seg_gs"
                    | "__seg_fs"
                    | "__seg_ds"
                    | "__seg_es"
                    | "__seg_cs"
                    | "__seg_ss"
            ))
        {
            if self.cur.text == "__attribute__" {
                self.skip_attributes();
            } else {
                // Capture const/volatile/atomic/restrict qualifiers
                // that appear after * in the declarator
                match self.cur.kind {
                    TokKind::KwConst => self.decl_quals |= QualifierFlags::CONST,
                    TokKind::KwVolatile => self.decl_quals |= QualifierFlags::VOLATILE,
                    TokKind::KwAtomic => self.decl_quals |= QualifierFlags::ATOMIC,
                    TokKind::KwRestrict => self.decl_quals |= QualifierFlags::RESTRICT,
                    TokKind::Ident if self.cur.text == "__const__" || self.cur.text == "__const" => {
                        self.decl_quals |= QualifierFlags::CONST
                    }
                    TokKind::Ident if self.cur.text == "__volatile__" || self.cur.text == "__volatile" => {
                        self.decl_quals |= QualifierFlags::VOLATILE
                    }
                    TokKind::Ident if self.cur.text == "__restrict__"
                        || self.cur.text == "__restrict"
                        || self.cur.text == "_ms_restrict"
                        || self.cur.text == "_restrict" =>
                    {
                        self.decl_quals |= QualifierFlags::RESTRICT
                    }
                    _ => {
                        // Address space qualifiers (__seg_gs etc.) are
                        // handled by spec_addr_space elsewhere; only warn
                        // for truly unknown identifier tokens.
                        if self.cur.kind == TokKind::Ident
                            && !matches!(
                                self.cur.text.as_str(),
                                "__seg_gs" | "__seg_fs" | "__seg_ds"
                                    | "__seg_es" | "__seg_cs" | "__seg_ss"
                            )
                        {
                            self.warn_here(
                                "W1014",
                                format!(
                                    "unknown qualifier token '{}' in pointer declarator",
                                    self.cur.text
                                ),
                                Some("expected const, volatile, atomic, or restrict"),
                            );
                        }
                    }
                }
                self.advance();
            }
        }
    }

    fn parse_ptr_suffix(&mut self, mut ty: Type) -> Type {
        loop {
            // 在 * 之前可能出现的地址空间限定符 (如 __seg_gs)
            self.skip_ptr_qualifiers();
            if self.cur.kind != TokKind::Star {
                break;
            }
            self.advance();
            self.skip_ptr_qualifiers();
            // 将 __seg_gs/__seg_fs 地址空间传播到指针类型的 QualType.
            // 例如 `int __seg_gs *p` 或 cast `(__seg_gs int *)` 中,
            // spec_addr_space 在类型说明符循环中被设置, 此处捕获并
            // 写入指针的 QualType.addr_space, 使代码生成阶段能在
            // 解引用时发射正确的段前缀 (%gs:/%fs:).
            if matches!(self.spec_addr_space, AddrSpace::GS | AddrSpace::FS) {
                let addr_space = self.spec_addr_space;
                self.spec_addr_space = AddrSpace::None;
                ty = Type::Ptr(Box::new(QualType::with_addr_space(ty, addr_space)));
            } else {
                ty = Type::Ptr(Box::new(ty.into()));
            }
        }
        ty
    }

    /// 类似 parse_ptr_suffix, 但在创建指针时将限定符应用到内部类型.
    /// 用于处理 `T const *p` 中 const 属于 pointed-to 类型的情况.
    fn parse_ptr_suffix_with_quals(&mut self, mut ty: Type, inner_quals: QualifierFlags) -> Type {
        loop {
            if self.cur.kind != TokKind::Star {
                break;
            }
            self.advance();
            // 将内部限定符应用到第一次指针创建时的内部类型
            let inner = QualType { base: ty.clone(), quals: inner_quals, addr_space: AddrSpace::None };
            ty = Type::Ptr(Box::new(inner));
            // 后续的限定符 (如 `T const * const *p`) 属于外层指针
            self.skip_ptr_qualifiers();
        }
        ty
    }

    /// 解析 __attribute__((vector_size(N))) 等属性, 返回可能包装为 Vector 的类型
    /// 仅在确认是 vector_size/vector_size 属性时才消费 token,
    /// 其他 __attribute__ 保留给后续解析 (如 __attribute__((used))).
    fn try_parse_vector_attr(&mut self, ty: Type) -> Type {
        if self.cur.kind != TokKind::Ident || self.cur.text != "__attribute__" {
            return ty;
        }
        // 保存状态: 仅在确认是 vector_size 时才消费
        let saved_cur = self.cur.clone();
        let saved_peek = self.peek.clone();
        let saved_pos = self.lex.pos;
        let saved_line = self.lex.line;
        let saved_col = self.lex.col;
        self.advance(); // __attribute__
        if self.cur.kind != TokKind::LParen {
            self.cur = saved_cur; self.peek = saved_peek;
            self.lex.pos = saved_pos; self.lex.line = saved_line; self.lex.col = saved_col;
            return ty;
        }
        self.advance(); // (
        if self.cur.kind != TokKind::LParen {
            self.cur = saved_cur; self.peek = saved_peek;
            self.lex.pos = saved_pos; self.lex.line = saved_line; self.lex.col = saved_col;
            return ty;
        }
        self.advance(); // (
        // 解析 vector_size(N) (含 __vector_size__ 别名)
        if self.cur.kind == TokKind::Ident
            && (self.cur.text == "vector_size" || self.cur.text == "__vector_size__")
        {
            self.advance(); // vector_size
            if self.cur.kind == TokKind::LParen {
                self.advance(); // (
                let size = if self.cur.kind == TokKind::Number {
                    let v: usize = self.cur.text.parse().unwrap_or(16);
                    self.advance();
                    v
                } else {
                    16
                };
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                } // )
                let elem_size = ty.size().max(1);
                let count = size / elem_size;
                // 消耗 ))
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                return Type::Vector(Box::new(ty.into()), count);
            }
        }
        // 非 vector_size 属性: 恢复状态, 保留给后续解析
        self.cur = saved_cur; self.peek = saved_peek;
        self.lex.pos = saved_pos; self.lex.line = saved_line; self.lex.col = saved_col;
        ty
    }

    fn parse_field_declarator(&mut self, spec: Type) -> (Type, String) {
        // 函数指针字段: type (*name)(params) / type (*name[N])(params)
        // 也处理: type (__attribute__((...)) *name)(params)
        let mut is_func_ptr = false;
        if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
            self.advance(); // (
            self.advance(); // *
            is_func_ptr = true;
        } else if self.cur.kind == TokKind::LParen
            && self.peek.kind == TokKind::Ident
            && self.peek.text == "__attribute__"
        {
            self.advance(); // (
            self.skip_attributes();
            if self.cur.kind == TokKind::Star {
                self.advance(); // *
                is_func_ptr = true;
            }
        }
        if is_func_ptr {
            // 跳过可能的 const/volatile/restrict 限定符与段寄存器限定符
            // (__seg_fs/__seg_gs 等): 与 parse_ptr_suffix / parse_func_ptr_inner
            // 同步, 避免函数指针字段声明符报 E0001.
            self.skip_ptr_qualifiers();
            let nm = if self.cur.kind == TokKind::Ident {
                let n = self.cur.text.clone();
                self.advance();
                n
            } else {
                String::new()
            };
            // 可选数组维: (*fns[32])
            let mut array_sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr().unwrap_or(Expr::Int {
                        v: 0,
                        span: Span::point("", 0, 0),
                        ty: Type::Int,
                    });
                    // 整型常量表达式按标准折叠为编译期常量, 否则保留为 VLA
                    if let Ok(v) = crate::const_eval::eval(&e) {
                        ArraySize::Const(v.max(0) as usize)
                    } else {
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    }
                } else {
                    ArraySize::Const(0)
                };
                let _ = self.expect(TokKind::RBracket, "']'");
                array_sizes.push(arr_size);
            }
            if self.cur.kind == TokKind::RParen {
                self.advance();
            }
            // 处理 `(*name)[N]` 形式: 指向数组的指针
            // 例如: pmd_t (*early_pgts)[512] = ...;
            let mut outer_array_sizes = Vec::new();
            while self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr().unwrap_or(Expr::Int {
                        v: 0,
                        span: Span::point("", 0, 0),
                        ty: Type::Int,
                    });
                    if let Ok(v) = crate::const_eval::eval(&e) {
                        ArraySize::Const(v.max(0) as usize)
                    } else {
                        match &e {
                            Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                            _ => ArraySize::Var(Box::new(e)),
                        }
                    }
                } else {
                    ArraySize::Const(0)
                };
                let _ = self.expect(TokKind::RBracket, "']'");
                outer_array_sizes.push(arr_size);
            }
            let ty = if self.cur.kind == TokKind::LParen {
                self.advance();
                let params = self.parse_params().unwrap_or_default();
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                let func_ty = Type::Func(
                    Box::new(spec.into()),
                    params.iter().map(|p| p.ty.clone().into()).collect(),
                );
                let mut ty = Type::Ptr(Box::new(func_ty.into()));
                for sz in array_sizes.into_iter().rev() {
                    ty = Type::Array(Box::new(ty.into()), sz);
                }
                ty
            } else {
                // 处理 `(*name)[N]` 形式: 指向数组的指针
                // 类型构造: Ptr(Array(spec, N)) 而非 Array(Ptr(spec), N)
                if !outer_array_sizes.is_empty() {
                    // 先构造内层数组类型: Array(spec, N1, N2, ...)
                    let mut inner_ty = spec;
                    for sz in outer_array_sizes.into_iter().rev() {
                        inner_ty = Type::Array(Box::new(inner_ty.into()), sz);
                    }
                    // 然后构造指针类型: Ptr(inner_array)
                    let mut ty = Type::Ptr(Box::new(inner_ty.into()));
                    // 最后处理 (*name[N]) 形式的内层数组
                    for sz in array_sizes.into_iter().rev() {
                        ty = Type::Array(Box::new(ty.into()), sz);
                    }
                    ty
                } else {
                    let mut ty = Type::Ptr(Box::new(spec.into()));
                    for sz in array_sizes.into_iter().rev() {
                        ty = Type::Array(Box::new(ty.into()), sz);
                    }
                    ty
                }
            };
            (ty, nm)
        } else {
            let mut ty = self.parse_ptr_suffix(spec);
            // `void *(*name)(params)`：指针后再跟函数指针声明符
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                self.advance(); // (
                self.advance(); // *
                // 跳过可能的 const/volatile/restrict 限定符与段寄存器限定符
                // (__seg_fs/__seg_gs 等): 与 parse_ptr_suffix / parse_func_ptr_inner
                // 同步, 避免函数指针字段声明符报 E0001.
                self.skip_ptr_qualifiers();
                let nm = if self.cur.kind == TokKind::Ident {
                    let n = self.cur.text.clone();
                    self.advance();
                    n
                } else {
                    String::new()
                };
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params().unwrap_or_default();
                    if self.cur.kind == TokKind::RParen {
                        self.advance();
                    }
                    let func_ty =
                        Type::Func(Box::new(ty.into()), params.iter().map(|p| p.ty.clone().into()).collect());
                    ty = Type::Ptr(Box::new(func_ty.into()));
                } else {
                    ty = Type::Ptr(Box::new(ty.into()));
                }
                return (ty, nm);
            }
            self.skip_attributes();
            // 匿名 struct/union 字段后直接接另一个字段的场景：
            //   union { struct {...} ATTRS; struct {...} ATTRS NAME; }  NAME_or_next_field;
            // 如果 spec 是 struct/union 且当前 ident 看起来是类型说明符 (typedef
            // 或关键字), 则它其实是下一个字段的类型, 匿名 struct/union 名字为空。
            // 这样可避免下个字段的类型名被当作当前匿名 struct/union 的字段名
            // (内核 struct_group(..., MEMBERS...) 展开丢失尾 `;` 后常出
            // 现这种状态)。
            // 深层嵌套 (3+ 层) 场景: 匿名 struct/union 内嵌匿名 struct/union,
            // 后续字段可能以 __attribute__ 开头 (GCC/内核宏常见模式).
            // __attribute__ 是 Ident 但不是字段名, 必须排除.
            let nm = if self.cur.kind == TokKind::Ident
                && self.cur.text != "__attribute__"
                && !(matches!(ty, Type::Struct(_, _, _) | Type::Union(_, _, _))
                    && (Self::is_type_specifier_ident(
                        &self.cur.text,
                        &self.typedefs,
                        self.opts.is_c23,
                    ) || self.cur.text == "__attribute__"))
            {
                let n = self.cur.text.clone();
                self.advance();
                n
            } else {
                String::new()
            };
            (ty, nm)
        }
    }

    fn parse_fields(&mut self) -> Vec<Field> {
        let mut fs = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            // GCC/内核宏偶发空字段: struct { int a; ; }
            if self.cur.kind == TokKind::Semi {
                self.advance();
                continue;
            }
            // C11: _Static_assert(expr, msg) 可以作为 struct/union 成员
            // (用于内核的 compile-time 断言). 解析后丢弃, 不影响布局.
            if self.cur.kind == TokKind::Ident && self.cur.text == "_Static_assert" {
                let _ = self.parse_static_assert();
                continue;
            }
            // C23 static_assert 同义
            if self.cur.kind == TokKind::Ident && self.cur.text == "static_assert" {
                let _ = self.parse_static_assert();
                continue;
            }
            let spec = match self.parse_decl_specifiers() {
                Some(t) => t,
                None => {
                    self.advance();
                    continue;
                }
            };
            // 同一声明说明符后可用逗号分隔多个声明符: *next, *prev / a, b
            loop {
                let (mut ty, nm) = self.parse_field_declarator(spec.clone());
                // 字段级属性: 提取 aligned 值
                let field_aligned = self.skip_attributes_extract_aligned();
                // C23 alignas(N) 在 parse_decl_specifiers 中已解析并
                // 存入 self.decl_attrs.alignment. 将其合并到字段对齐值,
                // 取 alignas 与 __attribute__((aligned)) 的较大值.
                let field_aligned = match (field_aligned, self.decl_attrs.alignment.take()) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
                let bw = if self.cur.kind == TokKind::Colon {
                    self.advance();
                    // 位域宽度必须是单个表达式，不能含逗号（同类型多个位域: a:8, b:4）
                    // 使用 const_eval::eval 求值常量表达式 (如 `1 << 3`,
                    // `sizeof(int)`, `(8 + 4)` 等), 而非仅匹配字面量整数.
                    if let Some(e) = self.parse_expr_bp(2) {
                        match crate::const_eval::eval(&e) {
                            Ok(v) if v >= 0 => Some(v as u32),
                            Ok(_) => {
                                self.err_here(
                                    "E0012",
                                    "negative bit-field width",
                                    Some("bit-field width must be non-negative"),
                                );
                                None
                            }
                            Err(_) => {
                                // 非整型常量: 尝试浮点截断 (GCC 兼容)
                                match crate::const_eval::eval_full(&e) {
                                    Ok(crate::const_eval::ConstValue::Float(f)) if f >= 0.0 => {
                                        Some(f as u32)
                                    }
                                    _ => None,
                                }
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                ty = {
                    let mut arr_ty = ty;
                    while self.cur.kind == TokKind::LBracket {
                        self.advance();
                        let arr_size = if self.cur.kind != TokKind::RBracket {
                            let e = self.parse_expr().unwrap_or(Expr::Int {
                                v: 0,
                                span: Span::point("", 0, 0),
                                ty: Type::Int,
                            });
                            if let Ok(v) = crate::const_eval::eval(&e) {
                                ArraySize::Const(v.max(0) as usize)
                            } else {
                                match &e {
                                    Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                                    _ => ArraySize::Var(Box::new(e)),
                                }
                            }
                        } else {
                            ArraySize::Const(0)
                        };
                        let _ = self.expect(TokKind::RBracket, "']'");
                        arr_ty = Type::Array(Box::new(arr_ty.into()), arr_size);
                    }
                    arr_ty
                };
                // 数组/名字后的 __attribute__((aligned(...))) 等
                // 合并之前提取的 aligned 值
                let field_aligned = match self.skip_attributes_extract_aligned() {
                    Some(a) => Some(a),
                    None => field_aligned,
                };
                // C11 匿名 struct/union 成员: 无字段名时作为匿名成员,
                // 贡献大小并展平字段到父结构。包括:
                //   struct { ... };     — 匿名内联定义 (标签名为空)
                //   struct tag { ... }; — 匿名引用已有标签 (标签名非空)
                // GCC/Clang 均支持两种形式。find_field() 已实现对
                // name 为空的 struct/union 字段做递归穿透查找。
                //
                // 区分:
                //   - 标签名为空 (匿名内联定义): 总是添加
                //   - 字段名非空: 普通命名字段, 添加
                //   - 标签名非空 + 字段名为空 + 类型有字段: 匿名引用已有
                //     完整类型 (如 `struct __filename_head;`), 添加
                //   - 标签名非空 + 字段名为空 + 类型无字段: 纯前向声明
                //     (如 `struct incomplete;`), 不贡献大小, 跳过
                let should_add = match &ty {
                    Type::Struct(tag, fields, _) | Type::Union(tag, fields, _) => {
                        tag.is_empty() || !nm.is_empty() || !fields.is_empty()
                    }
                    _ => true, // 非 struct/union 类型总是添加
                };
                if should_add {
                    fs.push(Field {
                        name: nm,
                        ty,
                        offset: 0,
                        bit_width: bw,
                        bit_offset: 0,
                        quals: self.spec_quals.iso_c(),
                        ptr_quals: vec![],
                        field_align: field_aligned,
                    });
                }
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                    continue;
                }
                if self.cur.kind == TokKind::Semi {
                    self.advance();
                }
                break;
            }
        }
        fs
    }

    fn parse_enum_body(&mut self) -> Vec<(Ident, Option<i64>)> {
        let mut vs = vec![];
        let mut seen = std::collections::HashSet::new();
        let mut nv: i64 = 0; // 隐式递增游标 (enum { A, B } -> A=0, B=1)
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::Comma {
                self.advance();
                continue;
            }
            let nm = match self.parse_ident() {
                Some(id) => id,
                None => {
                    self.advance();
                    continue;
                }
            };
            if !seen.insert(nm.name.clone()) {
                self.err_here(
                    "E0005",
                    format!("duplicate enum member '{}'", nm.name),
                    Some("each enumerator name must be unique"),
                );
            }
            let val = if self.cur.kind == TokKind::Assign {
                self.advance();
                // 枚举值必须是整型常量表达式，不能含逗号运算符 (A = 0, B 会误吞后续成员)。
                // 引用前面已定义枚举常量/字面量/算术/移位等表达式: Ident 在
                // parse_primary 已替换为 Expr::Int (见 enum_consts 查找), 因此
                // 只需对完整表达式做常量折叠。非整型常量表达式 (如变量引用)
                // 返回 None, 与旧行为一致: 按隐式递增赋值。
                if let Some(e) = self.parse_expr_bp(2) {
                    const_eval::eval(&e).ok().map(|v| v as i64)
                } else {
                    None
                }
            } else {
                None
            };
            // 立即登记: 同一 enum 内后续成员可引用本成员的值
            // (enum { A = 1, B = A + 1 }; 的 B 表达式解析时 A 必须已可查)。
            // 类型先用 Int 占位, 解析完整个 enum 后由调用方统一覆盖为
            // C23 底层类型 (值不受影响)。
            let v = val.unwrap_or(nv);
            self.enum_consts.insert(nm.name.clone(), (v, Type::Int));
            nv = v + 1;
            vs.push((nm, val));
            if self.cur.kind == TokKind::Comma {
                self.advance();
            }
        }
        vs
    }

    fn parse_typedef_name(&mut self) -> Option<Ident> {
        match self.cur.kind {
            TokKind::Ident | TokKind::KwBool => {
                let id = Ident {
                    name: self.cur.text.clone(),
                    span: self.cur.span.clone(),
                };
                self.advance();
                Some(id)
            }
            _ => {
                self.err_here(
                    "E0004",
                    format!("expected identifier, got {:?}", self.cur.kind),
                    Some("identifiers must start with a letter or '_'"),
                );
                None
            }
        }
    }

    fn parse_ident(&mut self) -> Option<Ident> {
        if self.cur.kind != TokKind::Ident {
            self.err_here(
                "E0004",
                format!("expected identifier, got {:?}", self.cur.kind),
                Some("identifiers must start with a letter or '_'"),
            );
            return None;
        }
        let id = Ident {
            name: self.cur.text.clone(),
            span: self.cur.span.clone(),
        };
        self.advance();
        Some(id)
    }

    /// 解析一个或多个链式 designator: .a 或 .a.b.c 或 [0] 或 [0].a.b
    /// 返回单元素 (Field/Index) 或 Chain (多个).
    /// ISO C99 6.7.8/19: chain 顺序应用于子对象定位, 例如
    ///   struct { union { int x; int y; } u; } s = { .u.x = 1 };
    fn parse_designator(&mut self) -> Option<Designator> {
        let mut chain: Vec<Designator> = Vec::new();
        loop {
            if self.cur.kind == TokKind::Dot {
                self.advance();
                let field = self.parse_ident()?;
                chain.push(Designator::Field(field));
            } else if self.cur.kind == TokKind::LBracket {
                self.advance();
                let first = self.parse_expr()?;
                if self.cur.kind == TokKind::Ellipsis {
                    // GNU 数组范围设计符: [first ... last] = value (GCC 扩展)
                    // 范围内所有元素赋同一值。端点必须是常量表达式。
                    self.advance();
                    let last = self.parse_expr()?;
                    if !self.expect(TokKind::RBracket, "']'") {
                        return None;
                    }
                    chain.push(Designator::Range(first, last));
                } else {
                    if !self.expect(TokKind::RBracket, "']'") {
                        return None;
                    }
                    chain.push(Designator::Index(first));
                }
            } else {
                break;
            }
        }
        if chain.is_empty() {
            self.err_here("E0004", "expected designator ('.' or '[')", None);
            return None;
        }
        if chain.len() == 1 {
            Some(chain.into_iter().next().unwrap())
        } else {
            Some(Designator::Chain(chain))
        }
    }

    fn parse_params(&mut self) -> Option<Vec<Param>> {
        let mut ps = vec![];
        self.last_variadic = false;
        if self.cur.kind == TokKind::KwVoid && self.peek.kind == TokKind::RParen {
            self.advance();
            return Some(ps);
        }
        if self.cur.kind == TokKind::RParen {
            return Some(ps);
        }
        if self.cur.kind == TokKind::Ellipsis {
            self.advance();
            self.last_variadic = true;
            return Some(ps);
        }
        // K&R 风格函数: 参数只有标识符, 没有类型 (如 func(a, b, c))
        // 检测: 当前是 Ident 且不是类型起始 (不是 typedef/typeof 等)
        // 且不是已知的 typedef 名 (避免参数名与 typedef 同名时误判)
        if self.cur.kind == TokKind::Ident
            && !self.is_type_start()
            && !self.typedefs.contains_key(&self.cur.text)
        {
            let mut kr_names = vec![];
            while self.cur.kind == TokKind::Ident {
                kr_names.push(self.cur.text.clone());
                self.advance();
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                }
            }
            // 返回特殊标记: 参数名列表, 类型默认为 int
            for name in &kr_names {
                ps.push(Param {
                    ty: Type::Int,
                    name: Some(Ident {
                        name: name.clone(),
                        span: Span::point("", 0, 0),
                    }),
                    span: Span::point("", 0, 0),
                    quals: QualifierFlags::NONE,
                });
            }
            // 设置 is_kr 标志 (通过返回特殊 Param, 后续在 parse_var_or_func 中处理)
            return Some(ps);
        }
        loop {
            // 函数指针参数: ret_type (*name)(params)
            // 由下面 parse_type() 之后的 LParen-Star 检查处理, 不再预测
            // 调用 parse_type (避免词法器位置不可回滚, 导致重复 parse_type
            // 拿到不一样的 token)
            let param_span = self.cur.span.clone();
            let mut ty = self.parse_type()?;
            // 普通路径可能漏掉指针: parse_type 已含 *, 这里再吞一层 qual
            self.skip_ptr_qualifiers();
            // 嵌套函数指针参数: char *(*pfn)(const char *)
            // 也处理: ret (__attribute__((...)) *name)(params)
            let mut is_nested_fptr = false;
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                is_nested_fptr = true;
            } else if self.cur.kind == TokKind::LParen
                && self.peek.kind == TokKind::Ident
                && self.peek.text == "__attribute__"
            {
                // 探测: 跳过属性后是否是 *
                is_nested_fptr = true; // 先标记, 下面统一处理
            }
            if is_nested_fptr {
                self.advance(); // (
                self.skip_attributes(); // 跳过 __attribute__((...))
                // 支持多级指针: (*name), (**name), (***name) 等
                let mut ptr_depth = 0;
                while self.cur.kind == TokKind::Star {
                    self.advance(); // *
                    ptr_depth += 1;
                    while matches!(
                        self.cur.kind,
                        TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict
                    ) {
                        self.advance();
                    }
                }
                if ptr_depth == 0 {
                    ptr_depth = 1; // 至少一级指针
                }
                let nm = if self.cur.kind == TokKind::Ident {
                    let id = Ident {
                        name: self.cur.text.clone(),
                        span: self.cur.span.clone(),
                    };
                    self.advance();
                    Some(id)
                } else {
                    None
                };
                self.skip_attributes();
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let inner_params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    let func_ty = Type::Func(
                        Box::new(ty.into()),
                        inner_params.iter().map(|p| p.ty.clone().into()).collect(),
                    );
                    // 按指针深度包裹: (**name)(params) -> Ptr(Ptr(Func(...)))
                    let mut wrapped: Type = func_ty;
                    for _ in 0..ptr_depth {
                        wrapped = Type::Ptr(Box::new(wrapped.into()));
                    }
                    ty = wrapped;
                } else {
                    // 无后续 (params): 只是多级指针, 不是函数指针
                    let mut wrapped: Type = ty;
                    for _ in 0..ptr_depth {
                        wrapped = Type::Ptr(Box::new(wrapped.into()));
                    }
                    ty = wrapped;
                }
                // 函数指针参数后的属性: 如 `int (*cb)(params) ATTRIBUTE_UNUSED`
                self.skip_attributes();
                ps.push(Param {
                    ty,
                    name: nm,
                    span: param_span,
                    quals: QualifierFlags::NONE,
                });
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                    if self.cur.kind == TokKind::Ellipsis {
                        self.advance();
                        self.last_variadic = true;
                        break;
                    }
                    continue;
                }
                if self.cur.kind == TokKind::Ellipsis {
                    self.advance();
                    self.last_variadic = true;
                }
                break;
            }
            // 内核宏 DECL_BUCKET_PARAMS 等使用 `size_t (name)` 在类型与参数名之间
            // 加括号, 避免被预处理时替换为多 token 表达式. dcc 在 parse_type 之前
            // 已经吞掉外层 __attribute__ 等, 但参数声明处的括号需要单独处理.
            // 另一种情形: GNU 允许 `int (name)` 作为 `int name` 的语法变体
            // (`(name)` 是 GNU/C11 允许的 declarator 形式).
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Ident {
                self.advance(); // (
                let id = Ident {
                    name: self.cur.text.clone(),
                    span: self.cur.span.clone(),
                };
                self.advance(); // ident
                if self.cur.kind == TokKind::RParen {
                    self.advance();
                }
                // `void (cb)(struct inode *)`: 括号参数名后跟参数列表
                // 等价于函数指针 `void (*cb)(struct inode *)`
                if self.cur.kind == TokKind::LParen {
                    self.advance(); // (
                    let inner_params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    let func_ty = Type::Func(
                        Box::new(ty.into()),
                        inner_params.iter().map(|p| p.ty.clone().into()).collect(),
                    );
                    ty = Type::Ptr(Box::new(func_ty.into()));
                }
                ps.push(Param {
                    ty,
                    name: Some(id),
                    span: param_span,
                    quals: QualifierFlags::NONE,
                });
                if self.cur.kind == TokKind::Comma {
                    self.advance();
                    if self.cur.kind == TokKind::Ellipsis {
                        self.advance();
                        self.last_variadic = true;
                        break;
                    }
                    continue;
                }
                if self.cur.kind == TokKind::Ellipsis {
                    self.advance();
                    self.last_variadic = true;
                }
                break;
            }
            let nm = if self.cur.kind == TokKind::Ident {
                let id = Ident {
                    name: self.cur.text.clone(),
                    span: self.cur.span.clone(),
                };
                self.advance();
                Some(id)
            } else {
                None
            };
            // C89 函数类型参数: `int getfrag(void *from, char *to, ...)`
            // 声明为函数类型的参数按 C11 6.7.6.3p8 调整为指向函数的指针.
            if nm.is_some() && self.cur.kind == TokKind::LParen {
                self.advance();
                let inner_params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                ty = Type::Ptr(Box::new(Type::Func(
                    Box::new(ty.into()),
                    inner_params.iter().map(|p| p.ty.clone().into()).collect(),
                ).into()));
            }
            // 参数名后的 __attribute__((unused)) 等
            self.skip_attributes();
            if self.cur.kind == TokKind::LBracket {
                // 多维数组参数按 C11 6.7.6.2p4 从内向外结合: `int a[][3]`
                // 是"未知行数的 int[3]", 收集维度后反转包裹. 参数退化时只
                // 剥最外层 (int a[][3] → int (*a)[3]), 内层数组结构必须
                // 保留, 因此维度顺序与声明处保持一致.
                let mut arr_sizes = Vec::new();
                while self.cur.kind == TokKind::LBracket {
                    self.advance();
                    // 跳过 C99 数组参数限定符: [restrict] / [static N] / [const]
                    while matches!(
                        self.cur.kind,
                        TokKind::KwConst
                            | TokKind::KwVolatile
                            | TokKind::KwRestrict
                            | TokKind::KwStatic
                    ) || (self.cur.kind == TokKind::Ident
                        && matches!(
                            self.cur.text.as_str(),
                            "__restrict__" | "__restrict" | "__const__" | "__volatile__"
                        ))
                    {
                        self.advance();
                    }
                    let arr_size = if self.cur.kind != TokKind::RBracket {
                        let e = self.parse_expr()?;
                        // 整型常量表达式按标准折叠为编译期常量, 否则保留为 VLA
                        if let Ok(v) = crate::const_eval::eval(&e) {
                            ArraySize::Const(v.max(0) as usize)
                        } else {
                            match &e {
                                Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                                _ => ArraySize::Var(Box::new(e)),
                            }
                        }
                    } else {
                        ArraySize::Const(0)
                    };
                    if !self.expect(TokKind::RBracket, "']'") {
                        return None;
                    }
                    arr_sizes.push(arr_size);
                }
                for sz in arr_sizes.into_iter().rev() {
                    ty = Type::Array(Box::new(ty.into()), sz);
                }
            }
            let ty = match ty {
                Type::Array(e, _) => Type::Ptr(e),
                o => o,
            };
            ps.push(Param {
                ty,
                name: nm,
                span: self.cur.span.clone(),
                quals: QualifierFlags::NONE,
            });
            if self.cur.kind == TokKind::Comma {
                self.advance();
                if self.cur.kind == TokKind::Ellipsis {
                    self.advance();
                    self.last_variadic = true;
                    break;
                }
                continue;
            }
            if self.cur.kind == TokKind::Ellipsis {
                self.advance();
                self.last_variadic = true;
            }
            break;
        }
        Some(ps)
    }

    fn parse_block(&mut self) -> Block {
        let mut items = vec![];
        // 当前块的遮蔽重命名映射: 原名 -> 新名
        // 仅在本块有效，不进入嵌套块 (嵌套块自行处理)
        let mut shadow_map: Vec<(String, String)> = vec![];
        loop {
            if self.cur.kind == TokKind::RBrace || self.cur.kind == TokKind::Eof {
                break;
            }
            // C23 [[attributes]]: 在块级作用域中出现，静默跳过
            if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                self.parse_std_attributes();
            }
            if self.cur.kind == TokKind::Ident && self.cur.text == "_Static_assert" {
                let _ = self.parse_static_assert();
            // MSVC 扩展 __pragma: 块级作用域内也接受 (无操作)
            } else if self.cur.kind == TokKind::Ident && self.cur.text == "__pragma" {
                self.advance();
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let mut depth = 1i32;
                    while depth > 0 && self.cur.kind != TokKind::Eof {
                        if self.cur.kind == TokKind::LParen {
                            depth += 1;
                        } else if self.cur.kind == TokKind::RParen {
                            depth -= 1;
                        }
                        self.advance();
                    }
                }
            } else if self.cur.kind == TokKind::Ident
                && self.cur.text == "__attribute__"
                && self.is_attribute_stmt()
            {
                // __attribute__((...)) 作为独立语句 (如 ATTRIBUTE_FALLTHROUGH)
                // 必须在 is_decl_start() 之前检查
                match self.parse_stmt() {
                    Some(s) => items.push(BlockItem::Stmt(s)),
                    None => { self.advance(); }
                }
            } else if self.is_decl_start() {
                match self.parse_decl() {
                    Some(ds) => {
                        for d in ds {
                            // 嵌套块遮蔽: 如果声明的变量名在外层作用域已存在,
                            // 重命名为内部唯一名以避免 codegen var_map 冲突。
                            let (d, new_mapping) = if self.in_function {
                                self.rename_shadow_var(d)
                            } else {
                                (d, None)
                            };
                            // 应用已有的遮蔽映射到当前声明 (如初始化表达式引用了被遮蔽的变量)
                            let mut d = d;
                            if !shadow_map.is_empty() {
                                Self::apply_shadow_map_decl(&mut d, &shadow_map);
                            }
                            if let Some((old, new)) = new_mapping {
                                shadow_map.push((old, new));
                            }
                            items.push(BlockItem::Decl(d));
                        }
                    }
                    None => {
                        self.advance();
                    }
                }
            } else {
                match self.parse_stmt() {
                    Some(mut s) => {
                        // 应用遮蔽映射到语句中的变量引用
                        if !shadow_map.is_empty() {
                            Self::apply_shadow_map_stmt(&mut s, &shadow_map);
                        }
                        items.push(BlockItem::Stmt(s));
                    }
                    None => {
                        self.advance();
                    }
                }
            }
        }
        Block {
            items,
            span: self.cur.span.clone(),
        }
    }

    /// 嵌套块遮蔽重命名: 如果变量名在外层作用域已存在,
    /// 生成唯一内部名。返回 (可能重命名后的声明, 新增映射)。
    fn rename_shadow_var(
        &mut self,
        decl: Decl,
    ) -> (Decl, Option<(String, String)>) {
        if let Decl::Var(ref v) = decl {
            let name = &v.name.name;
            // 检查名字是否在外层作用域中存在 (即被遮蔽)
            if self.locals.exists_in_outer_scopes(name) {
                // 生成唯一名
                let new_name = format!("{}__dcc_shadow_{}", name, self.shadow_counter);
                self.shadow_counter += 1;
                let old_name = name.clone();
                // 更新当前声明的名称
                let mut v = v.clone();
                v.name.name = new_name.clone();
                // 添加新名到 self.locals (保留旧名, 以便 lookup 不报警告)
                // shadow_map 会负责重命名后续引用
                self.locals.insert(new_name.clone(), v.ty.clone());
                return (Decl::Var(v), Some((old_name, new_name)));
            }
        }
        (decl, None)
    }

    /// 应用遮蔽映射到声明 (重命名初始化表达式中的引用)
    fn apply_shadow_map_decl(decl: &mut Decl, map: &[(String, String)]) {
        if let Decl::Var(v) = decl {
            if let Some(init) = &mut v.init {
                Self::apply_shadow_map_expr(init, map);
            }
        }
    }

    /// 应用遮蔽映射到语句
    fn apply_shadow_map_stmt(stmt: &mut Stmt, map: &[(String, String)]) {
        match stmt {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::apply_shadow_map_expr(e, map);
            }
            Stmt::Block { .. } => {
                // 不进入嵌套块，它们自行处理遮蔽
            }
            Stmt::If { cond, .. } => {
                Self::apply_shadow_map_expr(cond, map);
                // 不进入 then_/else_ 块
            }
            Stmt::While { cond, .. } | Stmt::Do { cond, .. } => {
                Self::apply_shadow_map_expr(cond, map);
            }
            Stmt::For {
                init, cond, step, ..
            } => {
                if let Some(s) = init {
                    Self::apply_shadow_map_stmt(s, map);
                }
                if let Some(e) = cond {
                    Self::apply_shadow_map_expr(e, map);
                }
                if let Some(e) = step {
                    Self::apply_shadow_map_expr(e, map);
                }
            }
            Stmt::Switch { expr, .. } => {
                Self::apply_shadow_map_expr(expr, map);
            }
            _ => {}
        }
    }

    /// 应用遮蔽映射到表达式 (重命名 Expr::Ident 引用)
    fn apply_shadow_map_expr(expr: &mut Expr, map: &[(String, String)]) {
        match expr {
            Expr::Ident { name, .. } => {
                for (old, new) in map {
                    if name.name == *old {
                        name.name = new.clone();
                        break;
                    }
                }
            }
            Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
                Self::apply_shadow_map_expr(l, map);
                Self::apply_shadow_map_expr(r, map);
            }
            Expr::Call { callee, args, .. } => {
                Self::apply_shadow_map_expr(callee, map);
                for arg in args {
                    Self::apply_shadow_map_expr(arg, map);
                }
            }
            Expr::Unary { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. } => {
                Self::apply_shadow_map_expr(e, map);
            }
            Expr::Cast { e, .. } | Expr::SizeofExpr { e, .. } | Expr::AlignofExpr { e, .. } => {
                Self::apply_shadow_map_expr(e, map);
            }
            Expr::Deref { e, .. } | Expr::AddrOf { e, .. } => {
                Self::apply_shadow_map_expr(e, map);
            }
            Expr::Index { arr, idx, .. } => {
                Self::apply_shadow_map_expr(arr, map);
                Self::apply_shadow_map_expr(idx, map);
            }
            Expr::Member { expr, .. } => {
                Self::apply_shadow_map_expr(expr, map);
            }
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                Self::apply_shadow_map_expr(cond, map);
                Self::apply_shadow_map_expr(then_, map);
                Self::apply_shadow_map_expr(else_, map);
            }
            Expr::InitList { inits, .. } => {
                for init in inits {
                    match init {
                        InitItem::Expr(e) => Self::apply_shadow_map_expr(e, map),
                        InitItem::Designated { value, designator, .. } => {
                            Self::apply_shadow_map_expr(value, map);
                            // Also handle index expressions in designators
                            Self::apply_shadow_map_designator(designator, map);
                        }
                    }
                }
            }
            Expr::StmtExpr { block, .. } => {
                for item in &mut block.items {
                    match item {
                        BlockItem::Stmt(s) => Self::apply_shadow_map_stmt(s, map),
                        BlockItem::Decl(d) => Self::apply_shadow_map_decl(d, map),
                    }
                }
            }
            Expr::VaArg { ap, .. } => {
                Self::apply_shadow_map_expr(ap, map);
            }
            Expr::AsmExpr {
                outputs, inputs, ..
            } => {
                for o in outputs {
                    Self::apply_shadow_map_expr(o, map);
                }
                for i in inputs {
                    Self::apply_shadow_map_expr(i, map);
                }
            }
            Expr::Real { e, .. } | Expr::Imag { e, .. } => {
                Self::apply_shadow_map_expr(e, map);
            }
            Expr::CompoundLit { inits, .. } => {
                for init in inits {
                    match init {
                        InitItem::Expr(e) => Self::apply_shadow_map_expr(e, map),
                        InitItem::Designated { value, .. } => {
                            Self::apply_shadow_map_expr(value, map);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// 应用遮蔽映射到设计符中的索引表达式
    fn apply_shadow_map_designator(designator: &mut Designator, map: &[(String, String)]) {
        match designator {
            Designator::Index(e) => Self::apply_shadow_map_expr(e, map),
            Designator::Range(l, r) => {
                Self::apply_shadow_map_expr(l, map);
                Self::apply_shadow_map_expr(r, map);
            }
            Designator::Chain(items) => {
                for item in items {
                    Self::apply_shadow_map_designator(item, map);
                }
            }
            Designator::Field(_) => {}
        }
    }

    fn is_type_start(&self) -> bool {
        matches!(
            self.cur.kind,
            TokKind::KwInt
                | TokKind::KwChar
                | TokKind::KwLong
                | TokKind::KwVoid
                | TokKind::KwShort
                | TokKind::KwUnsigned
                | TokKind::KwSigned
                | TokKind::KwFloat
                | TokKind::KwDouble
                | TokKind::KwStruct
                | TokKind::KwUnion
                | TokKind::KwEnum
                | TokKind::KwConst
                | TokKind::KwVolatile
                | TokKind::KwStatic
                | TokKind::KwExtern
                | TokKind::KwInline
                | TokKind::KwRegister
                | TokKind::KwAuto
                | TokKind::KwTypedef
                | TokKind::KwAtomic
                | TokKind::KwFract
                | TokKind::KwAccum
                | TokKind::KwComplex
                | TokKind::KwImaginary
                | TokKind::KwDecimal32
                | TokKind::KwDecimal64
                | TokKind::KwDecimal128
                | TokKind::KwBool
                | TokKind::KwChar8
                | TokKind::KwChar16
                | TokKind::KwChar32
                | TokKind::KwInt128
                | TokKind::KwUInt128
        ) || (self.cur.kind == TokKind::Ident
            // 局部变量遮蔽 typedef: 若同名局部变量存在，不应视为类型起始
            && !self.locals.contains_key(&self.cur.text)
            // __seg_fs / __seg_gs / __seg_cs / __seg_ds / __seg_es / __seg_ss
            // 出现在类型最前 (如 `sizeof(__seg_gs int)`), 是类型起始.
            && (matches!(
                self.cur.text.as_str(),
                "__seg_fs"
                    | "__seg_gs"
                    | "__seg_cs"
                    | "__seg_ds"
                    | "__seg_es"
                    | "__seg_ss"
            ) || Self::is_type_specifier_ident(&self.cur.text, &self.typedefs, self.opts.is_c23)))
    }

    /// 检查在 `unsigned` 被消费后, 当前 token 是否是可扩展类型的关键字.
    /// 用于判断 `unsigned` 是完整类型 (`unsigned x`) 还是类型前缀 (`unsigned char`).
    fn is_peek_type_start_after_unsigned(&self) -> bool {
        matches!(
            self.cur.kind,
            TokKind::KwChar | TokKind::KwInt | TokKind::KwShort | TokKind::KwLong
            | TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict
            | TokKind::KwSigned | TokKind::KwUnsigned
            | TokKind::KwComplex | TokKind::KwImaginary
            | TokKind::KwInt128 | TokKind::KwUInt128
        )
    }

    /// 检查当前 `__attribute__` 是独立语句还是声明前缀.
    /// 通过前瞻: 跳过 __attribute__((...)) 后检查后续 token.
    /// 若后续是 `;` 或语句上下文, 返回 true (独立语句).
    /// 若后续是 extern/static/typedef/类型关键字等, 返回 false (声明).
    fn is_attribute_stmt(&self) -> bool {
        // 保存状态
        let saved_pos = self.lex.pos;
        let _saved_line = self.lex.line;
        let _saved_col = self.lex.col;
        let _saved_cur = self.cur.clone();
        let _saved_peek = self.peek.clone();
        // 需要可变副本来前瞻; 使用 unsafe 不可取,
        // 改为简单启发式: 检查 __attribute__ 后面的 token
        // 如果是 __attribute__((...)); 则 RParen 深度归零后下一个是 `;`
        // 简单方法: 扫描 token 流找到属性结束位置
        let mut p = saved_pos;
        let src = &self.lex.src;
        // 跳过 __attribute__
        while p < src.len() && (src[p] as char).is_ascii_alphanumeric() || (p < src.len() && src[p] == b'_') {
            p += 1;
        }
        // 跳过空白
        while p < src.len() && (src[p] as char).is_ascii_whitespace() {
            p += 1;
        }
        // 应该看到 ((
        if p + 1 < src.len() && src[p] == b'(' && src[p + 1] == b'(' {
            p += 2;
            let mut depth = 2i32;
            while p < src.len() && depth > 0 {
                if src[p] == b'(' { depth += 1; }
                else if src[p] == b')' { depth -= 1; }
                p += 1;
            }
            // 跳过空白
            while p < src.len() && (src[p] as char).is_ascii_whitespace() {
                p += 1;
            }
            // 检查后续字符
            if p < src.len() {
                let ch = src[p] as char;
                // `;` 表示独立语句
                if ch == ';' { return true; }
                // `{` 表示独立语句 (do-while 宏等)
                if ch == '{' { return true; }
                // 字母/下划线: 检查是否是声明关键字
                if ch.is_ascii_alphabetic() || ch == '_' {
                    let start = p;
                    while p < src.len() && (src[p] as char).is_ascii_alphanumeric() || (p < src.len() && src[p] == b'_') {
                        p += 1;
                    }
                    let word = std::str::from_utf8(&src[start..p]).unwrap_or("");
                    // 声明关键字 => 不是独立语句
                    return !matches!(
                        word,
                        "extern" | "static" | "typedef" | "register"
                        | "inline" | "void" | "int" | "char" | "float"
                        | "double" | "struct" | "union" | "enum" | "const"
                        | "volatile" | "unsigned" | "signed" | "long"
                        | "short" | "_Bool" | "bool" | "__extension__"
                    );
                }
                // 其他字符 (如 `*`, `(` 等) => 可能是声明
                return false;
            }
        }
        // 默认: 作为语句处理
        true
    }

    /// 声明上下文允许把 `unknown_type value` 交给 parse_type 产生精确诊断，
    /// 但表达式中的 `(value * other)` 不能因此被误判为类型名。
    fn is_decl_start(&self) -> bool {
        // __attribute__: 在语句起始位置永远是声明 (gcc 扩展). 例如
        // 内核 BUILD_BUG_ON 宏展开: do { __attribute__((__noreturn__))
        // extern void f(void) __attribute__((__error__("..."))); ... } while (0)
        // 若不识别 __attribute__ 为声明起始, 解析器会落入 expression 分支
        // 而报 "expected ';', got KwExtern".
        if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            return true;
        }
        // C11/C23 static_assert: 声明上下文 (parse_decl 中求值并丢弃)
        if self.cur.kind == TokKind::Ident
            && (self.cur.text == "_Static_assert" || self.cur.text == "static_assert")
        {
            return true;
        }
        self.is_type_start()
            || (self.cur.kind == TokKind::Ident
                && self.cur.text != "_Generic"
                && self.cur.text != "nullptr"
                // 局部变量遮蔽 typedef 时不应视为声明起始
                && !self.locals.contains_key(&self.cur.text)
                && Self::is_type_specifier_ident(&self.cur.text, &self.typedefs, self.opts.is_c23)
                && matches!(self.peek.kind, TokKind::Ident | TokKind::Star))
    }

    fn is_peek_type_start(&self) -> bool {
        matches!(
            self.peek.kind,
            TokKind::KwInt
                | TokKind::KwChar
                | TokKind::KwLong
                | TokKind::KwVoid
                | TokKind::KwShort
                | TokKind::KwUnsigned
                | TokKind::KwSigned
                | TokKind::KwFloat
                | TokKind::KwDouble
                | TokKind::KwStruct
                | TokKind::KwUnion
                | TokKind::KwEnum
                | TokKind::KwConst
                | TokKind::KwVolatile
                | TokKind::KwStatic
                | TokKind::KwExtern
                | TokKind::KwInline
                | TokKind::KwRegister
                | TokKind::KwAuto
                | TokKind::KwTypedef
                | TokKind::KwAtomic
                | TokKind::KwFract
                | TokKind::KwAccum
                | TokKind::KwComplex
                | TokKind::KwImaginary
                | TokKind::KwDecimal32
                | TokKind::KwDecimal64
                | TokKind::KwDecimal128
                | TokKind::KwBool
                | TokKind::KwChar8
                | TokKind::KwChar16
                | TokKind::KwChar32
                | TokKind::KwInt128
                | TokKind::KwUInt128
        ) || (self.peek.kind == TokKind::Ident
            // __seg_fs / __seg_gs / __seg_cs / __seg_ds / __seg_es / __seg_ss
            // 在类型最前 (sizeof/Alignof 等), 应识别为 peek 类型起始.
            && (matches!(
                self.peek.text.as_str(),
                "__seg_fs"
                    | "__seg_gs"
                    | "__seg_cs"
                    | "__seg_ds"
                    | "__seg_es"
                    | "__seg_ss"
            ) || Self::is_type_specifier_ident(&self.peek.text, &self.typedefs, self.opts.is_c23)))
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::KwReturn => {
                self.advance();
                let val = if self.cur.kind != TokKind::Semi {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                Some(Stmt::Return { val, span: sp })
            }
            TokKind::Semi => {
                self.advance();
                Some(Stmt::Expr {
                    e: Expr::Int {
                        v: 0,
                        span: sp.clone(),
                        ty: int_t(),
                    },
                    span: sp,
                })
            }
            TokKind::LBrace => {
                self.advance();
                // C11 6.2.1: 嵌套块引入新作用域
                // 使用 push/pop 代替 clone/restore, 消除 O(vars) 克隆
                self.locals.push_scope();
                let b = self.parse_block();
                self.locals.pop_scope();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                Some(Stmt::Block { b, span: sp })
            }
            TokKind::KwIf => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") {
                    return None;
                }
                let cond = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                let then_ = Box::new(self.parse_stmt()?);
                let else_ = if self.cur.kind == TokKind::KwElse {
                    self.advance();
                    Some(Box::new(self.parse_stmt()?))
                } else {
                    None
                };
                Some(Stmt::If {
                    cond,
                    then_,
                    else_,
                    span: sp,
                })
            }
            TokKind::KwWhile => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") {
                    return None;
                }
                let cond = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                Some(Stmt::While {
                    cond,
                    body: Box::new(self.parse_stmt()?),
                    span: sp,
                })
            }
            TokKind::KwDo => {
                self.advance();
                let body = Box::new(self.parse_stmt()?);
                if !self.expect(TokKind::KwWhile, "'while'") {
                    return None;
                }
                if !self.expect(TokKind::LParen, "'('") {
                    return None;
                }
                let cond = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                Some(Stmt::Do {
                    body,
                    cond,
                    span: sp,
                })
            }
            TokKind::KwFor => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") {
                    return None;
                }
                let init = if self.cur.kind == TokKind::Semi {
                    self.advance();
                    None
                } else {
                    Some(Box::new(self.parse_stmt()?))
                };
                let cond = if self.cur.kind == TokKind::Semi {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                let step = if self.cur.kind == TokKind::RParen {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                Some(Stmt::For {
                    init,
                    cond,
                    step,
                    body: Box::new(self.parse_stmt()?),
                    span: sp,
                })
            }
            TokKind::KwBreak => {
                self.advance();
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                Some(Stmt::Break { span: sp })
            }
            TokKind::KwContinue => {
                self.advance();
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                Some(Stmt::Continue { span: sp })
            }
            TokKind::KwSwitch => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") {
                    return None;
                }
                let expr = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                if !self.expect(TokKind::LBrace, "'{'") {
                    return None;
                }
                let cases = self.parse_cases();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                Some(Stmt::Switch {
                    expr,
                    cases,
                    span: sp,
                })
            }
            TokKind::KwGoto => {
                self.advance();
                // goto *expr: 间接跳转 (GCC 扩展)
                if self.cur.kind == TokKind::Star {
                    self.advance();
                    let expr = self.parse_expr()?;
                    if !self.expect(TokKind::Semi, "';'") {
                        return None;
                    }
                    Some(Stmt::GotoIndirect {
                        expr: Box::new(expr),
                        span: sp,
                    })
                } else {
                    let t = self.parse_ident()?;
                    if !self.expect(TokKind::Semi, "';'") {
                        return None;
                    }
                    Some(Stmt::Goto {
                        target: t,
                        span: sp,
                    })
                }
            }
            // __attribute__((...)) 作为独立语句: 跳过属性和后续分号.
            // GCC 代码中常用 __attribute__((fallthrough)); 标记 switch case 贯穿.
            TokKind::Ident if self.cur.text == "__attribute__" => {
                self.skip_attributes();
                if self.cur.kind == TokKind::Semi {
                    self.advance();
                }
                Some(Stmt::Expr {
                    e: Expr::Int {
                        v: 0,
                        span: sp.clone(),
                        ty: int_t(),
                    },
                    span: sp,
                })
            }
            // asm goto 语句；普通 asm 在同 arm 内走 parse_expr
            TokKind::Ident if matches!(self.cur.text.as_str(), "__asm__" | "__asm" | "asm") => {
                if self.probe_asm_goto() {
                    self.advance(); // asm
                    self.skip_asm_qualifiers();
                    self.advance(); // goto
                    self.skip_asm_qualifiers();
                    // asm goto is always volatile per GCC semantics
                    if self.cur.kind != TokKind::LParen {
                        self.err_here("E0001", "expected '(' after asm goto", None);
                        return None;
                    }
                    self.advance(); // (
                                    // 解析汇编字符串
                    let mut asm_str = String::new();
                    while self.cur.kind == TokKind::StringLit {
                        asm_str.push_str(&Self::decode_string_literal(&self.cur.text));
                        self.advance();
                    }
                    // 解析 : outputs : inputs : clobbers : goto_labels
                    let mut outputs = vec![];
                    let mut output_names = vec![];
                    let mut output_constraints = vec![];
                    let mut inputs = vec![];
                    let mut input_names = vec![];
                    let mut input_constraints = vec![];
                    let mut clobbers = vec![];
                    let mut goto_labels = vec![];
                    if self.cur.kind == TokKind::Colon {
                        self.advance();
                        let (o, n, c) = self.parse_asm_operand_list();
                        outputs = o;
                        output_names = n;
                        output_constraints = c;
                    }
                    if self.cur.kind == TokKind::Colon {
                        self.advance();
                        let (o, n, c) = self.parse_asm_operand_list();
                        inputs = o;
                        input_names = n;
                        input_constraints = c;
                    }
                    if self.cur.kind == TokKind::Colon {
                        self.advance();
                        clobbers = self.parse_asm_clobber_list();
                    }
                    if self.cur.kind == TokKind::Colon {
                        self.advance();
                        goto_labels = self.parse_asm_label_list();
                    }
                    // 诊断: 检查 asm 模板中引用的标签是否都在 goto label 列表中。
                    // 越界/缺失保持 W0004 warning，不阻断后续解析。
                    self.validate_asm_goto_labels(
                        &asm_str,
                        &goto_labels,
                        outputs.len() + inputs.len(),
                    );
                    // 语义校验: 约束前缀/重名/类型匹配/占位符数量
                    self.validate_asm_semantics(
                        &asm_str,
                        &outputs,
                        &output_names,
                        &output_constraints,
                        &inputs,
                        &input_names,
                        &input_constraints,
                        &sp,
                    );
                    if self.cur.kind == TokKind::RParen {
                        self.advance();
                    }
                    if self.cur.kind == TokKind::Semi {
                        self.advance();
                    }
                    Some(Stmt::AsmGoto {
                        asm_str,
                        outputs,
                        output_names,
                        output_constraints,
                        inputs,
                        input_names,
                        input_constraints,
                        clobbers,
                        goto_labels,
                        volatile: true, // asm goto is always volatile
                        span: sp,
                    })
                } else {
                    let e = self.parse_expr()?;
                    self.check_nodiscard_call(&e);
                    if !self.expect(TokKind::Semi, "';'") {
                        return None;
                    }
                    Some(Stmt::Expr { e, span: sp })
                }
            }
            // GNU 扩展: `__label__ name;` 块内局部 label 声明 (可多个, 逗号分隔)。
            // 声明本身无运行时代码; 后面的 goto 由 Stmt::Goto 处理。
            TokKind::Ident if self.cur.text == "__label__" => {
                self.advance();
                loop {
                    self.parse_ident()?;
                    if self.cur.kind == TokKind::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                Some(Stmt::Expr {
                    e: Expr::Int {
                        v: 0,
                        span: sp.clone(),
                        ty: int_t(),
                    },
                    span: sp,
                })
            }
            TokKind::Ident if self.peek.kind == TokKind::Colon => {
                let nm = self.parse_ident()?;
                self.advance();
                // switch 内 `label: case X:`：标签后直接跟 case/default
                if matches!(
                    self.cur.kind,
                    TokKind::KwCase | TokKind::KwDefault | TokKind::RBrace
                ) {
                    Some(Stmt::Label {
                        name: nm,
                        stmt: Box::new(Stmt::Block {
                            b: Block {
                                items: vec![],
                                span: sp.clone(),
                            },
                            span: sp.clone(),
                        }),
                        span: sp,
                    })
                } else {
                    Some(Stmt::Label {
                        name: nm,
                        stmt: Box::new(self.parse_stmt()?),
                        span: sp,
                    })
                }
            }
            _ if self.is_decl_start() => {
                let ds = self.parse_decl()?;
                let items = ds.into_iter().map(BlockItem::Decl).collect();
                Some(Stmt::Block {
                    b: Block {
                        items,
                        span: sp.clone(),
                    },
                    span: sp,
                })
            }
            _ => {
                let e = self.parse_expr()?;
                // C23 [[nodiscard]]: 调用结果被丢弃时发射警告
                self.check_nodiscard_call(&e);
                if !self.expect(TokKind::Semi, "';'") {
                    return None;
                }
                Some(Stmt::Expr { e, span: sp })
            }
        }
    }

    fn parse_cases(&mut self) -> Vec<Case> {
        let mut cases = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            let sp = self.cur.span.clone();
            let (val, range_end) = if self.cur.kind == TokKind::KwCase {
                self.advance();
                let low = self.parse_expr();
                let mut hi = None;
                if self.cur.kind == TokKind::Ellipsis {
                    self.advance();
                    hi = self.parse_expr();
                }
                let _ = self.expect(TokKind::Colon, "':'");
                (low, hi)
            } else if self.cur.kind == TokKind::KwDefault {
                self.advance();
                let _ = self.expect(TokKind::Colon, "':'");
                (None, None)
            } else {
                self.advance();
                continue;
            };
            let mut items = vec![];
            while !matches!(
                self.cur.kind,
                TokKind::KwCase | TokKind::KwDefault | TokKind::RBrace | TokKind::Eof
            ) {
                // C23 [[attributes]] in case body (e.g. [[fallthrough]])
                if self.cur.kind == TokKind::LBracket && self.peek.kind == TokKind::LBracket {
                    self.parse_std_attributes();
                    continue;
                }
                // __attribute__((...)) 作为独立语句 (如 ATTRIBUTE_FALLTHROUGH)
                // 必须在 is_decl_start() 之前检查, 否则 __attribute__
                // 会被 is_decl_start() 误判为声明起始而进入 parse_decl.
                if self.cur.kind == TokKind::Ident
                    && self.cur.text == "__attribute__"
                {
                    // 检查后面是否有 extern/static/typedef 等声明关键字
                    // 如果只是 __attribute__((fallthrough)); 则作为语句处理
                    if self.is_attribute_stmt() {
                        if let Some(s) = self.parse_stmt() {
                            items.push(BlockItem::Stmt(s));
                            continue;
                        }
                    }
                }
                if self.is_decl_start() {
                    match self.parse_decl() {
                        Some(ds) => {
                            for d in ds {
                                items.push(BlockItem::Decl(d));
                            }
                        }
                        None => {
                            self.advance();
                        }
                    }
                } else {
                    match self.parse_stmt() {
                        Some(s) => items.push(BlockItem::Stmt(s)),
                        None => {
                            self.advance();
                        }
                    }
                }
            }
            cases.push(Case {
                val,
                range_end,
                body: Block {
                    items,
                    span: sp.clone(),
                },
                span: sp,
            });
        }
        cases
    }

    // ===== Pratt 表达式 =====
    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            // 逗号运算符: bp=1, 最低优先级
            if self.cur.kind == TokKind::Comma && min_bp <= 1 {
                let sp = self.cur.span.clone();
                self.advance();
                let r = self.parse_expr_bp(1)?;
                let ty = r.ty();
                lhs = Expr::Comma {
                    l: Box::new(lhs),
                    r: Box::new(r),
                    span: sp,
                    ty,
                };
                continue;
            }
            if self.cur.kind == TokKind::Assign && min_bp <= 2 {
                let assign_sp = self.cur.span.clone();
                self.advance();
                let r = self.parse_expr_bp(2)?;
                let ty = lhs.ty();
                // C89 6.3.16: check const-qualified assignment target
                if let Some(name) = Self::assign_target_name(&lhs) {
                    if let Some(q) = self.var_quals.get(&name) {
                        if q.is_const() {
                            let msg = format!(
                                "assignment of read-only variable '{}'",
                                name
                            );
                            let hint = "variable was declared const";
                            if self.opts.permissive {
                                // GCC treats const-assignment as warning;
                                // -fpermissive matches that behaviour for
                                // kernel code that relies on this pattern.
                                self.diags.push(
                                    Diagnostic::warn(
                                        "W0030",
                                        msg,
                                        assign_sp.clone(),
                                    )
                                    .with_hint(hint),
                                );
                            } else {
                                self.diags.push(
                                    Diagnostic::err(
                                        "E0030",
                                        msg,
                                        assign_sp.clone(),
                                    )
                                    .with_hint(hint),
                                );
                            }
                        }
                    }
                }
                lhs = Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(lhs),
                    r: Box::new(r),
                    span: assign_sp,
                    ty,
                };
                continue;
            }
            if let Some(op) = self.compound_op() {
                if min_bp <= 2 {
                    let compound_sp = self.cur.span.clone();
                    self.advance();
                    let r = self.parse_expr_bp(2)?;
                    // 展开 lhs += e → target = lhs + e
                    // 提取赋值目标 (如果 lhs 已是 Assign, 提取其目标)
                    let target = Self::assign_target(&lhs);
                    // C89 6.3.16: check const-qualified compound assignment target
                    if let Some(name) = Self::assign_target_name(&target) {
                        if let Some(q) = self.var_quals.get(&name) {
                            if q.is_const() {
                                let msg = format!(
                                    "assignment of read-only variable '{}'",
                                    name
                                );
                                let hint = "variable was declared const";
                                if self.opts.permissive {
                                    self.diags.push(
                                        Diagnostic::warn(
                                            "W0030",
                                            msg,
                                            compound_sp.clone(),
                                        )
                                        .with_hint(hint),
                                    );
                                } else {
                                    self.diags.push(
                                        Diagnostic::err(
                                            "E0030",
                                            msg,
                                            compound_sp.clone(),
                                        )
                                        .with_hint(hint),
                                    );
                                }
                            }
                        }
                    }
                    let read = Box::new(lhs.clone()); // 读取当前值
                    let target_ty = target.ty();
                    let binop_ty = match target_ty {
                        Type::Ptr(_) if matches!(op, BinOp::Add | BinOp::Sub) => target_ty.clone(),
                        Type::Float | Type::Double | Type::LongDouble => target_ty.clone(),
                        Type::Vector(_, _) => target_ty.clone(),
                        _ => int_t(),
                    };
                    let expanded = Expr::Binop {
                        op,
                        l: read,
                        r: Box::new(r),
                        span: self.cur.span.clone(),
                        ty: binop_ty,
                    };
                    let assign_ty = target.ty();
                    lhs = Expr::Assign {
                        op: BinOp::Add,
                        l: Box::new(target),
                        r: Box::new(expanded),
                        span: self.cur.span.clone(),
                        ty: assign_ty,
                    };
                    continue;
                }
            }
            if self.cur.kind == TokKind::Question {
                // 三元优先级高于逗号(bp=1)。else 分支是 conditional-expression，
                // 不得用 bp=0，否则会吞掉 `_Generic(a?b:c, type: x)` 中的逗号。
                if 2 < min_bp {
                    break;
                }
                let cond = lhs;
                self.advance();
                // GNU 扩展: x ?: y 等价于 x ? x : y
                let then_ = if self.cur.kind == TokKind::Colon {
                    cond.clone()
                } else {
                    self.parse_expr()?
                };
                if !self.expect(TokKind::Colon, "':'") {
                    return None;
                }
                let else_ = self.parse_expr_bp(2)?;
                let then_ty = then_.ty();
                let else_ty = else_.ty();
                let cond_ty = if matches!(then_ty, Type::Ptr(_)) {
                    then_ty
                } else if matches!(else_ty, Type::Ptr(_)) {
                    else_ty
                } else if matches!(then_ty, Type::Vector(_, _))
                    || matches!(else_ty, Type::Vector(_, _))
                {
                    // Vector ternary: if either branch is a vector,
                    // use the then-branch type.
                    then_ty
                } else if then_ty == else_ty {
                    then_ty
                } else {
                    int_t()
                };
                lhs = Expr::Cond {
                    cond: Box::new(cond),
                    then_: Box::new(then_),
                    else_: Box::new(else_),
                    span: self.cur.span.clone(),
                    ty: cond_ty,
                };
                continue;
            }
            let (op, lbp, rbp) = match self.cur.kind {
                TokKind::OrOr => (BinOp::OrOr, 4u8, 5),
                TokKind::AndAnd => (BinOp::AndAnd, 5, 6),
                TokKind::Pipe => (BinOp::Or, 6, 7),
                TokKind::Caret => (BinOp::Xor, 7, 8),
                TokKind::Amp => (BinOp::And, 8, 9),
                TokKind::EqEq => (BinOp::EqEq, 9, 10),
                TokKind::Ne => (BinOp::Ne, 9, 10),
                TokKind::Lt => (BinOp::Lt, 10, 11),
                TokKind::Gt => (BinOp::Gt, 10, 11),
                TokKind::Le => (BinOp::Le, 10, 11),
                TokKind::Ge => (BinOp::Ge, 10, 11),
                TokKind::Shl => (BinOp::Shl, 11, 12),
                TokKind::Shr => (BinOp::Shr, 11, 12),
                TokKind::Plus => (BinOp::Add, 12u8, 13),
                TokKind::Minus => (BinOp::Sub, 12, 13),
                TokKind::Star => (BinOp::Mul, 13, 14),
                TokKind::Slash => (BinOp::Div, 13, 14),
                TokKind::Percent => (BinOp::Mod, 13, 14),
                _ => break,
            };
            if lbp < min_bp {
                break;
            }
            self.advance();
            let r = self.parse_expr_bp(rbp)?;
            // 比较与逻辑运算结果恒为 int，即使操作数是浮点
            let binop_ty = {
                let lt = lhs.ty();
                let rt = r.ty();
                // 数组在表达式中自动衰减为指向首元素的指针
                // (C99 6.3.2.1.3): 除 sizeof/&/字符串字面量初始化外, 数组名
                // 在表达式上下文一律退化为 T*.  此处实现 arr+int 形式,
                // 让 libdecnumber 等库能写 `*(bcdar+1)`.
                let lt = if let Type::Array(inner, _) = &lt {
                    Type::Ptr(inner.clone())
                } else {
                    lt
                };
                let rt = if let Type::Array(inner, _) = &rt {
                    Type::Ptr(inner.clone())
                } else {
                    rt
                };
                if matches!(
                    op,
                    BinOp::EqEq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::AndAnd
                        | BinOp::OrOr
                ) {
                    int_t()
                } else {
                    let is_ca = matches!(&lt, Type::Complex(_));
                    let is_cb = matches!(&rt, Type::Complex(_));
                    let is_ia = matches!(&lt, Type::Imaginary(_));
                    let is_ib = matches!(&rt, Type::Imaginary(_));
                    let is_va = matches!(&lt, Type::Vector(_, _));
                    let is_vb = matches!(&rt, Type::Vector(_, _));
                    let is_fa = matches!(&lt, Type::Float | Type::Double | Type::LongDouble);
                    let is_fb = matches!(&rt, Type::Float | Type::Double | Type::LongDouble);
                    if is_va || is_vb {
                        if is_va {
                            lt
                        } else {
                            rt
                        }
                    } else if is_ca || is_cb {
                        // 复数算术结果仍为复数（供 Cast 等类型推导）
                        if is_ca {
                            lt
                        } else {
                            rt
                        }
                    } else if is_ia || is_ib {
                        // _Imaginary 算术类型推导
                        let inner_a = if let Type::Imaginary(ref i) = lt { i.base.clone() } else { lt.clone().into() };
                        let inner_b = if let Type::Imaginary(ref i) = rt { i.base.clone() } else { rt.clone().into() };
                        // 取较宽的内型
                        let wider = if inner_a.size() >= inner_b.size() { inner_a } else { inner_b };
                        match op {
                            BinOp::Add | BinOp::Sub if is_ia && is_ib => {
                                // imag ± imag → imag
                                Type::Imaginary(Box::new(wider.into()))
                            }
                            BinOp::Mul | BinOp::Div if is_ia && is_ib => {
                                // imag * imag → real (负), imag / imag → real
                                wider
                            }
                            BinOp::Add | BinOp::Sub => {
                                // real ± imag → complex
                                Type::Complex(Box::new(wider.into()))
                            }
                            BinOp::Mul | BinOp::Div => {
                                // real * imag → imag, real / imag → imag
                                Type::Imaginary(Box::new(wider.into()))
                            }
                            _ => wider,
                        }
                    } else if is_fa || is_fb {
                        if matches!(&lt, Type::LongDouble) || matches!(&rt, Type::LongDouble) {
                            Type::LongDouble
                        } else if matches!(&lt, Type::Double) || matches!(&rt, Type::Double) {
                            Type::Double
                        } else {
                            Type::Float
                        }
                    } else if matches!(op, BinOp::Add | BinOp::Sub) {
                        if matches!(&lt, Type::Ptr(_)) {
                            lt
                        } else if matches!(op, BinOp::Add) && matches!(&rt, Type::Ptr(_)) {
                            rt
                        } else if matches!(&lt, Type::Int128 | Type::UInt128)
                            || matches!(&rt, Type::Int128 | Type::UInt128)
                        {
                            if matches!(&lt, Type::UInt128) || matches!(&rt, Type::UInt128) {
                                Type::UInt128
                            } else {
                                Type::Int128
                            }
                        } else {
                            Self::usual_arith_type(&lt, &rt)
                        }
                    } else if matches!(&lt, Type::Int128 | Type::UInt128)
                        || matches!(&rt, Type::Int128 | Type::UInt128)
                    {
                        if matches!(&lt, Type::UInt128) || matches!(&rt, Type::UInt128) {
                            Type::UInt128
                        } else {
                            Type::Int128
                        }
                    } else {
                        Self::usual_arith_type(&lt, &rt)
                    }
                }
            };
            lhs = Expr::Binop {
                op,
                l: Box::new(lhs),
                r: Box::new(r),
                span: self.cur.span.clone(),
                ty: binop_ty,
            };
        }
        Some(lhs)
    }

    fn compound_op(&self) -> Option<BinOp> {
        match self.cur.kind {
            TokKind::PlusEq => Some(BinOp::Add),
            TokKind::MinusEq => Some(BinOp::Sub),
            TokKind::StarEq => Some(BinOp::Mul),
            TokKind::SlashEq => Some(BinOp::Div),
            TokKind::PercentEq => Some(BinOp::Mod),
            TokKind::AmpEq => Some(BinOp::And),
            TokKind::PipeEq => Some(BinOp::Or),
            TokKind::CaretEq => Some(BinOp::Xor),
            TokKind::ShlEq => Some(BinOp::Shl),
            TokKind::ShrEq => Some(BinOp::Shr),
            _ => None,
        }
    }

    // 提取赋值目标: 如果 expr 是 Assign, 递归提取其目标; 否则克隆 expr
    fn assign_target(expr: &Expr) -> Expr {
        match expr {
            Expr::Assign { l, .. } => Self::assign_target(l),
            _ => expr.clone(),
        }
    }

    /// Extract the variable name from an assignment target expression.
    /// Returns Some(name) if the target is a simple identifier (possibly
    /// through deref/member chains), None otherwise.
    fn assign_target_name(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident { name, .. } => Some(name.name.clone()),
            // Penetrate through deref, member, and index chains
            // to find the base variable name for const-ness checks
            Expr::Deref { e, .. } => Self::assign_target_name(e),
            Expr::Member { expr, .. } => Self::assign_target_name(expr),
            Expr::Index { arr, .. } => Self::assign_target_name(arr),
            _ => None,
        }
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        // __extension__ 前缀: GNU 扩展抑制标记 (complex.h 中 _Complex_I
        // 宏展开为 (__extension__ 1.0iF)), 无运行时语义, 透传解析内层表达式
        if self.cur.kind == TokKind::Ident && self.cur.text == "__extension__" {
            self.advance();
            return self.parse_unary();
        }
        match self.cur.kind {
            TokKind::Plus => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = e.ty();
                Some(Expr::Unary {
                    op: UnOp::Plus,
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            TokKind::Minus => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = e.ty();
                Some(Expr::Unary {
                    op: UnOp::Neg,
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            TokKind::Not => {
                self.advance();
                let e = self.parse_unary()?;
                Some(Expr::Unary {
                    op: UnOp::Not,
                    e: Box::new(e),
                    span: sp,
                    ty: int_t(),
                })
            }
            TokKind::Tilde => {
                self.advance();
                let e = self.parse_unary()?;
                Some(Expr::Unary {
                    op: UnOp::BitNot,
                    e: Box::new(e),
                    span: sp,
                    ty: int_t(),
                })
            }
            TokKind::Star => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = if let Type::Ptr(inner) = &e.ty() {
                    inner.base.clone()
                } else {
                    int_t()
                };
                Some(Expr::Deref {
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            TokKind::Amp => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = Type::Ptr(Box::new(e.ty().into()));
                Some(Expr::AddrOf {
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            // &&label: GCC 扩展 label 地址 (词法分析器把 && 识别为 AndAnd)
            TokKind::AndAnd => {
                self.advance(); // &&
                if self.cur.kind == TokKind::Ident {
                    let label = self.parse_ident()?;
                    let ptr_ty = Type::Ptr(Box::new(Type::Void.into()));
                    Some(Expr::LabelAddr {
                        label,
                        span: sp,
                        ty: ptr_ty,
                    })
                } else {
                    self.err_here(
                        "E0001",
                        "expected label name after &&",
                        Some("use &&label for GNU label address extension"),
                    );
                    None
                }
            }
            TokKind::PlusPlus => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = e.ty();
                Some(Expr::PreInc {
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            TokKind::MinusMinus => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = e.ty();
                Some(Expr::PreDec {
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            // __real__ expr: 取复数实部 (GCC 扩展; 对标量返回自身)
            TokKind::KwReal => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = match &e.ty() {
                    Type::Complex(inner) => inner.base.clone(),
                    Type::Imaginary(inner) => inner.base.clone(),
                    t => t.clone().into(),
                };
                Some(Expr::Real {
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            // __imag__ expr: 取复数虚部 (对标量返回 0)
            TokKind::KwImag => {
                self.advance();
                let e = self.parse_unary()?;
                let ty = match &e.ty() {
                    Type::Complex(inner) => inner.base.clone(),
                    Type::Imaginary(inner) => inner.base.clone(),
                    t => t.clone().into(),
                };
                Some(Expr::Imag {
                    e: Box::new(e),
                    span: sp,
                    ty,
                })
            }
            TokKind::KwSizeof => {
                self.advance();
                if self.cur.kind == TokKind::LParen && self.is_peek_type_start() {
                    self.advance();
                    let ty = self.parse_type()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    Some(Expr::SizeofTy { ty, span: sp })
                } else {
                    let e = self.parse_unary()?;
                    Some(Expr::SizeofExpr {
                        e: Box::new(e),
                        span: sp,
                    })
                }
            }
            // _Alignof(type) 或 _Alignof(expr)
            TokKind::KwAlignof => {
                self.advance();
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    if self.is_type_start() {
                        let ty = self.parse_type()?;
                        if !self.expect(TokKind::RParen, "')'") {
                            return None;
                        }
                        Some(Expr::AlignofTy { ty, span: sp })
                    } else {
                        let e = self.parse_expr()?;
                        if !self.expect(TokKind::RParen, "')'") {
                            return None;
                        }
                        Some(Expr::AlignofExpr {
                            e: Box::new(e),
                            span: sp,
                        })
                    }
                } else {
                    self.err_here(
                        "E0001",
                        "expected '(' after _Alignof",
                        Some("_Alignof requires parentheses: _Alignof(type)"),
                    );
                    None
                }
            }
            // __extension__: 跳过, 继续解析下一个一元表达式
            TokKind::Ident if self.cur.text == "__extension__" => {
                self.advance();
                self.parse_unary()
            }
            // _Generic(controlling_expr, type: expr, ..., default: expr): C11 泛型选择
            TokKind::Ident if self.cur.text == "_Generic" => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") {
                    return None;
                }
                // 控制表达式: 不能用 parse_expr() (会消费逗号为逗号运算符)
                // 用 bp=2 跳过逗号运算符 (bp=1), 让逗号留给 association list
                let controlling = self.parse_expr_bp(2)?;
                if !self.expect(TokKind::Comma, "','") {
                    return None;
                }
                // C11 6.5.1.1p2: 控制表达式经历 lvalue conversions (数组→指针、
                // 函数→指针) 后才与 association 类型比较。
                let ctrl_ty = match controlling.ty() {
                    Type::Array(elem, _) => Type::Ptr(elem),
                    Type::Func(ret, params) => Type::Ptr(Box::new(Type::Func(ret, params).into())),
                    t => t,
                };
                let mut result: Option<Expr> = None;
                let mut default_result: Option<Expr> = None;
                // 解析 association-list: type-name: expr 或 default: expr
                loop {
                    if self.cur.kind == TokKind::RParen {
                        break;
                    }
                    // default: expr
                    if (self.cur.kind == TokKind::Ident && self.cur.text == "default")
                        || self.cur.kind == TokKind::KwDefault
                    {
                        self.advance();
                        if !self.expect(TokKind::Colon, "':'") {
                            return None;
                        }
                        default_result = Some(self.parse_expr_bp(2)?);
                    } else {
                        // type-name: expr
                        let assoc_ty = self.parse_type()?;
                        if !self.expect(TokKind::Colon, "':'") {
                            return None;
                        }
                        let assoc_expr = self.parse_expr_bp(2)?;
                        // 类型匹配: 简化比较 (忽略 const/volatile 限定符)
                        if result.is_none() && Self::types_compatible(&ctrl_ty, &assoc_ty) {
                            result = Some(assoc_expr);
                        }
                    }
                    if self.cur.kind == TokKind::Comma {
                        self.advance();
                    }
                }
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                // 优先使用精确匹配, 否则使用 default
                let chosen = result.or(default_result);
                let sp2 = self.cur.span.clone();
                match chosen {
                    Some(e) => self.parse_postfix_suffix(e, sp2),
                    None => {
                        self.err_here(
                            "E0001",
                            "no matching generic selection",
                            Some("add a 'default' association to _Generic"),
                        );
                        None
                    }
                }
            }
            // nullptr: C23 空指针常量, 等价于 (void*)0
            TokKind::Ident if self.cur.text == "nullptr" => {
                self.advance();
                let ptr_ty = Type::Ptr(Box::new(Type::Void.into()));
                Some(Expr::Int {
                    v: 0,
                    span: sp,
                    ty: ptr_ty,
                })
            }
            // C23 true/false 关键字常量 (仅 C23 模式;
            // 非 C23 模式由 stdbool.h 宏或枚举提供)
            TokKind::Ident
                if (self.cur.text == "true" || self.cur.text == "false") && self.opts.is_c23 =>
            {
                let v = if self.cur.text == "true" { 1 } else { 0 };
                self.advance();
                Some(Expr::Int {
                    v,
                    span: sp,
                    ty: Type::Bool,
                })
            }
            // __asm__("..."): 内联汇编表达式
            TokKind::Ident
                if self.cur.text == "__asm__"
                    || self.cur.text == "__asm"
                    || self.cur.text == "asm" =>
            {
                self.advance();
                let is_volatile = self.skip_asm_qualifiers();
                self.parse_asm_expr(sp, is_volatile)
            }
            // 语句表达式: ({...})
            TokKind::LParen if self.peek.kind == TokKind::LBrace => {
                self.advance(); // (
                self.advance(); // {
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                // 类型取最后一个表达式语句的类型
                let ty = b.items.last().map_or(Type::Void, |item| {
                    if let BlockItem::Stmt(Stmt::Expr { e, .. }) = item {
                        e.ty()
                    } else if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = item {
                        e.ty()
                    } else {
                        Type::Void
                    }
                });
                // 语句表达式后可跟后缀运算符，如 ({ ... })()
                let e = Expr::StmtExpr {
                    block: Box::new(b),
                    span: sp.clone(),
                    ty,
                };
                self.parse_postfix_suffix(e, sp)
            }
            TokKind::LParen if self.is_cast() => {
                self.advance();
                // __extension__ 作表达式前缀 (complex.h: (__extension__ 1.0iF)):
                // 后随非类型 token 时不是 cast, 按括号表达式解析
                if self.cur.kind == TokKind::Ident
                    && self.cur.text == "__extension__"
                    && !self.is_peek_type_start()
                {
                    self.advance();
                    let e = self.parse_expr()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    return Some(e);
                }
                // 在 cast 中可能出现位于 type 之前的 __attribute__((...)) 修饰符,
                // 例如 (__attribute__((__vector_size__ (16))) int)x.
                // parse_type_mode 已在类型说明符中处理 __attribute__,
                // 会通过 parse_attributes 安全地消耗并跳过未知属性.
                let mut ty = self.parse_type()?;
                // 解析完类型后, 类型与 * 之间可能出现 __attribute__((...)) 修饰符与
                // 多个 * 指针, 例如内核 percpu 的
                //   (typeof(*(p)) __attribute__((force))
                //    __attribute__((address_space(0))) *)p.
                // 循环处理: 每轮先剥除 attribute 和地址空间限定符 (如 __seg_gs),
                // 然后吃掉一个 *, 直到遇到 ( 或 ).
                loop {
                    while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                        self.skip_attributes();
                    }
                    // 跳过地址空间限定符 (如 __seg_gs), 它们出现在类型和 * 之间
                    self.skip_ptr_qualifiers();
                    if self.cur.kind == TokKind::Star {
                        self.advance();
                        self.skip_ptr_qualifiers();
                        ty = Type::Ptr(Box::new(ty.into()));
                        continue;
                    }
                    break;
                }
                // 函数指针强制转换: (type (*)(params))expr
                // 解析完类型后, 检查是否是 (* 模式
                if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                    self.advance(); // (
                    self.advance(); // *
                    while matches!(
                        self.cur.kind,
                        TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict
                    ) {
                        self.advance();
                    }
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let _params = self.parse_params()?;
                        if !self.expect(TokKind::RParen, "')'") {
                            return None;
                        }
                    }
                    let ty = Type::Ptr(Box::new(ty.into()));
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    let e = self.parse_unary()?;
                    return Some(Expr::Cast {
                        ty,
                        e: Box::new(e),
                        span: sp,
                    });
                }
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                // (type){...} 被 is_cast() 拦截, 但实际是复合字面量而非 cast
                if self.cur.kind == TokKind::LBrace {
                    self.advance(); // 跳过 {
                    let mut inits = Vec::new();
                    while self.cur.kind != TokKind::RBrace {
                        let item = if self.cur.kind == TokKind::Dot
                            || self.cur.kind == TokKind::LBracket
                        {
                            let designator = self.parse_designator()?;
                            if !self.expect(TokKind::Assign, "'='") {
                                return None;
                            }
                            let value = self.parse_expr_bp(2)?;
                            InitItem::Designated { designator, value }
                        } else {
                            let e = self.parse_expr_bp(2)?;
                            InitItem::Expr(e)
                        };
                        inits.push(item);
                        if self.cur.kind == TokKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    if !self.expect(TokKind::RBrace, "'}'") {
                        return None;
                    }
                    return Some(Expr::CompoundLit {
                        ty,
                        inits,
                        span: sp,
                    });
                }
                let e = self.parse_unary()?;
                // 严格类型检查: 整数截断 / 整数↔指针转换
                let src_ty = e.ty();
                if let Some(diag) = self.check_strict_conversion(&src_ty, &ty, sp.clone()) {
                    self.diags.push(diag);
                }
                Some(Expr::Cast {
                    ty,
                    e: Box::new(e),
                    span: sp,
                })
            }
            // 函数指针强制转换: 当 is_cast() 未检测到时, 尝试解析 (type (*...))expr
            TokKind::LParen
                if matches!(
                    self.peek.kind,
                    TokKind::KwInt
                        | TokKind::KwChar
                        | TokKind::KwLong
                        | TokKind::KwVoid
                        | TokKind::KwShort
                        | TokKind::KwUnsigned
                        | TokKind::KwSigned
                        | TokKind::KwFloat
                        | TokKind::KwDouble
                        | TokKind::KwStruct
                        | TokKind::KwUnion
                        | TokKind::KwEnum
                        | TokKind::KwInt128
                        | TokKind::KwUInt128
                ) =>
            {
                // 推测性解析: 保存状态, 尝试解析为函数指针cast
                let saved_cur = self.cur.clone();
                let saved_peek = self.peek.clone();
                self.advance(); // (
                if let Some(ty) = self.parse_type() {
                    if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                        self.advance(); // (
                        self.advance(); // *
                        while matches!(
                            self.cur.kind,
                            TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict
                        ) || (self.cur.kind == TokKind::Ident
                            && self.cur.text == "_ms_restrict")
                        {
                            self.advance();
                        }
                        if self.cur.kind == TokKind::RParen {
                            self.advance(); // )
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                let _params = self.parse_params();
                                if self.cur.kind == TokKind::RParen {
                                    self.advance();
                                    if self.cur.kind == TokKind::RParen {
                                        self.advance();
                                        let ty = Type::Ptr(Box::new(ty.into()));
                                        let e = self.parse_unary()?;
                                        return Some(Expr::Cast {
                                            ty,
                                            e: Box::new(e),
                                            span: sp,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                // 不是函数指针cast, 恢复并作为普通括号表达式
                self.cur = saved_cur;
                self.peek = saved_peek;
                let e = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                Some(e)
            }
            _ => self.parse_postfix(),
        }
    }

    /// GNU/标准类型说明符起始（用于 is_cast / typeof 等）
    fn is_type_specifier_ident(
        text: &str,
        typedefs: &std::collections::HashMap<String, Type>,
        is_c23: bool,
    ) -> bool {
        typedefs.contains_key(text)
            // bool: 始终识别为类型说明符 (C23 关键字, 但 gnu11 下也广泛使用)
            || matches!(text, "bool")
            // C23 关键字 (仅 C23 模式)
            || (is_c23 && matches!(text, "typeof_unqual" | "alignas" | "thread_local"))
            || matches!(
                text,
                "typeof" | "__typeof__" | "__typeof" | "__auto_type" | "_Atomic" | "__extension__"
                    | "constexpr" | "_BitInt" | "_Noreturn" | "_Thread_local" | "__thread"
                    | "__builtin_va_list"
                    | "_Float16" | "_Float32" | "_Float32x" | "_Float64" | "_Float64x"
                    | "_Float128" | "_Float128x"
                    | "_ms_restrict" | "__declspec" | "__pragma" | "_Pragma"
            )
    }

    fn is_cast(&self) -> bool {
        if self.cur.kind != TokKind::LParen {
            return false;
        }
        matches!(
            self.peek.kind,
            TokKind::KwInt
                | TokKind::KwChar
                | TokKind::KwLong
                | TokKind::KwVoid
                | TokKind::KwShort
                | TokKind::KwUnsigned
                | TokKind::KwSigned
                | TokKind::KwFloat
                | TokKind::KwDouble
                | TokKind::KwStruct
                | TokKind::KwUnion
                | TokKind::KwEnum
                | TokKind::KwConst
                | TokKind::KwVolatile
                | TokKind::KwAtomic
                | TokKind::KwFract
                | TokKind::KwAccum
                | TokKind::KwBool
                | TokKind::KwChar8
                | TokKind::KwChar16
                | TokKind::KwChar32
                | TokKind::KwInt128
                | TokKind::KwUInt128
        ) || (self.peek.kind == TokKind::Ident
            // 局部变量遮蔽 typedef 时不应视为 cast
            // (如 `int macro = ...; (macro ? ... : 0)` 中 macro 是变量非类型)
            && !self.locals.contains_key(&self.peek.text)
            && (Self::is_type_specifier_ident(&self.peek.text, &self.typedefs, self.opts.is_c23)
                || self.peek.text == "__attribute__"
                || self.peek.text == "__vector_size__"))
    }

    fn parse_postfix(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        let e = self.parse_primary()?;
        self.parse_postfix_suffix(e, sp)
    }

    /// 解析已得表达式上的后缀运算符（调用、下标、成员访问等）
    fn parse_postfix_suffix(&mut self, mut e: Expr, sp: Span) -> Option<Expr> {
        loop {
            match self.cur.kind {
                TokKind::LParen => {
                    // 函数调用: 支持标识符和函数指针表达式
                    let callee_expr = e.clone();
                    self.advance();
                    // __builtin_va_arg(ap, type): 第二个参数是类型名
                    if let Expr::Ident { name, .. } = &callee_expr {
                        if name.name == "__builtin_va_arg" {
                            let va_arg_span = name.span.clone();
                            let ap = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") {
                                return None;
                            }
                            let arg_ty = self.parse_type()?;
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            e = Expr::VaArg {
                                ap: Box::new(ap),
                                ty: arg_ty,
                                span: va_arg_span,
                            };
                            continue;
                        }
                        // __builtin_types_compatible_p(type1, type2): 编译期类型比较
                        if name.name == "__builtin_types_compatible_p" {
                            let ty1 = self.parse_type()?;
                            if !self.expect(TokKind::Comma, "','") {
                                return None;
                            }
                            let ty2 = self.parse_type()?;
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            // 解析 typedef 别名以比较真实类型
                            let ty1 = if let Type::Named(ref n) = ty1 {
                                self.typedefs.get(n).cloned().unwrap_or(ty1)
                            } else {
                                ty1
                            };
                            let ty2 = if let Type::Named(ref n) = ty2 {
                                self.typedefs.get(n).cloned().unwrap_or(ty2)
                            } else {
                                ty2
                            };
                            let v = if Self::types_compatible(&ty1, &ty2) { 1 } else { 0 };
                            e = Expr::Int {
                                v: v as i128,
                                span: sp.clone(),
                                ty: int_t(),
                            };
                            continue;
                        }
                        // __builtin_choose_expr(cond, expr1, expr2): 编译期条件选择
                        if name.name == "__builtin_choose_expr" {
                            let cond = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") {
                                return None;
                            }
                            let expr1 = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") {
                                return None;
                            }
                            let expr2 = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            let chosen = match const_eval::eval(&cond) {
                                Ok(v) if v != 0 => expr1,
                                Ok(_) => expr2,
                                Err(_) => {
                                    self.diags.push(Diagnostic::err(
                                        "E0013",
                                        "__builtin_choose_expr condition is not constant",
                                        sp.clone(),
                                    ));
                                    expr2
                                }
                            };
                            e = chosen;
                            continue;
                        }
                        // __builtin_constant_p(expr): 只判断，不执行参数。
                        if name.name == "__builtin_constant_p" {
                            let arg = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            e = Expr::Int {
                                v: const_eval::is_constant(&arg) as i128,
                                span: sp.clone(),
                                ty: int_t(),
                            };
                            continue;
                        }
                        // __builtin_object_size(ptr, type): 编译期对象大小
                        if name.name == "__builtin_object_size" {
                            let ptr = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::Comma, "','") {
                                return None;
                            }
                            let type_arg = self.parse_expr_bp(2)?;
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            let mode = const_eval::eval(&type_arg).unwrap_or(0);
                            let size = const_eval::object_size(&ptr, mode).unwrap_or(-1);
                            e = Expr::Int {
                                v: size,
                                span: sp.clone(),
                                ty: int_t(),
                            };
                            continue;
                        }
                        // __builtin_FILE / __builtin_LINE / __builtin_FUNCTION /
                        // __builtin_FUNCSIG: 编译期折叠为常量 (按 GCC 行为)。
                        if name.name == "__builtin_FILE" {
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            let file_str = &*self.cur.span.file;
                            let mut data = file_str.as_bytes().to_vec();
                            data.push(0);
                            e = Expr::String {
                                data,
                                span: sp.clone(),
                                ty: Type::Ptr(Box::new(Type::Char.into())),
                            };
                            continue;
                        }
                        if name.name == "__builtin_LINE" {
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            e = Expr::Int {
                                v: self.cur.span.line.max(1) as i128,
                                span: sp.clone(),
                                ty: int_t(),
                            };
                            continue;
                        }
                        if name.name == "__builtin_FUNCTION"
                            || name.name == "__builtin_FUNCSIG"
                            || name.name == "__builtin_PRETTY_FUNCTION"
                        {
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            // 函数外: GCC 返回空串; 函数内: 返回函数名
                            let name_str = self.current_function.clone().unwrap_or_default();
                            let mut data = name_str.into_bytes();
                            data.push(0);
                            e = Expr::String {
                                data,
                                span: sp.clone(),
                                ty: Type::Ptr(Box::new(Type::Char.into())),
                            };
                            continue;
                        }
                        // __builtin_classify_type(type): GCC type classification.
                        // 1=void, 2=integer, 3=pointer, 4=real_float,
                        // 5=complex, 6=struct_union, 7=array, 8=function,
                        // 9=vector, 10=enum, -1=lang_mask (unknown).
                        if name.name == "__builtin_classify_type" {
                            let ty = self.parse_type()?;
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            // 若仍为 Type::Named, 查 typedef 映射以获得真实类型
                            let ty = if let Type::Named(ref n) = ty {
                                self.typedefs.get(n).cloned().unwrap_or(ty)
                            } else {
                                ty
                            };
                            let class = match ty {
                                Type::Void => 1,
                                Type::Bool
                                | Type::Char
                                | Type::SChar
                                | Type::UChar
                                | Type::Char8
                                | Type::Char16
                                | Type::Char32
                                | Type::Short
                                | Type::UShort
                                | Type::Int
                                | Type::UInt
                                | Type::Long
                                | Type::ULong
                                | Type::LongLong
                                | Type::ULongLong
                                | Type::Int128
                                | Type::UInt128
                                | Type::Fract
                                | Type::Accum => 2,
                                Type::Ptr(_) | Type::Func(_, _) => 3,
                                Type::Float | Type::Double | Type::LongDouble => 4,
                                Type::Complex(_) | Type::Imaginary(_) => 5,
                                Type::Struct(_, _, _) | Type::Union(_, _, _) => 6,
                                Type::Array(_, _) => 7,
                                Type::Vector(_, _) => 9,
                                Type::Enum(_, _, _) => 10,
                                Type::Atomic(_) | Type::Named(_) => 1,
                            };
                            e = Expr::Int {
                                v: class,
                                span: sp.clone(),
                                ty: int_t(),
                            };
                            continue;
                        }
                        // __builtin_offsetof(type, member): 结构体成员偏移
                        if name.name == "__builtin_offsetof" {
                            let base_ty = self.parse_type()?;
                            if !self.expect(TokKind::Comma, "','") {
                                return None;
                            }
                            // 解析成员设计器: field, field.sub, field[idx].sub 等
                            let mut member = String::new();
                            if self.cur.kind == TokKind::Ident {
                                member = self.cur.text.clone();
                                self.advance();
                            }
                            // 支持 .field 和 [expr] 后缀
                            loop {
                                if self.cur.kind == TokKind::Dot {
                                    self.advance();
                                    if self.cur.kind == TokKind::Ident {
                                        member.push('.');
                                        member.push_str(&self.cur.text);
                                        self.advance();
                                    }
                                } else if self.cur.kind == TokKind::LBracket {
                                    self.advance();
                                    let _idx = self.parse_expr_bp(2);
                                    if self.cur.kind == TokKind::RBracket {
                                        self.advance();
                                    }
                                    member.push_str("[]");
                                } else {
                                    break;
                                }
                            }
                            if !self.expect(TokKind::RParen, "')'") {
                                return None;
                            }
                            e = Expr::Offsetof {
                                ty: base_ty,
                                member,
                                span: sp.clone(),
                                result_ty: int_t(),
                            };
                            continue;
                        }
                        // __builtin_strlen(s): 字符串字面量编译期求值
                        // 只对 String 字面量做折叠; 其它情况正常 Call
                        if name.name == "__builtin_strlen" {
                            if let TokKind::StringLit = self.cur.kind {
                                let mut data = parse_str(&self.cur.text);
                                self.advance();
                                while self.cur.kind == TokKind::StringLit {
                                    let more = parse_str(&self.cur.text);
                                    data.extend(more);
                                    self.advance();
                                }
                                if self.expect(TokKind::RParen, "')'") {
                                    // 末尾 NUL 不计入长度
                                    let len = data.len();
                                    e = Expr::Int {
                                        v: len as i128,
                                        span: sp.clone(),
                                        ty: int_t(),
                                    };
                                    continue;
                                }
                            }
                        }
                        // __builtin_abs/__builtin_labs/__builtin_llabs:
                        // 整型字面量编译期求值
                        if matches!(
                            name.name.as_str(),
                            "__builtin_abs" | "__builtin_labs" | "__builtin_llabs"
                        ) {
                            if let TokKind::Number = self.cur.kind {
                                let lit = self.cur.text.clone();
                                self.advance();
                                if self.expect(TokKind::RParen, "')'") {
                                    if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                        let r = v.wrapping_abs();
                                        e = Expr::Int {
                                            v: r,
                                            span: sp.clone(),
                                            ty: int_t(),
                                        };
                                        continue;
                                    }
                                }
                            }
                        }
                        // __builtin_bswap16/32/64: 字节序反转 (编译期)
                        if matches!(
                            name.name.as_str(),
                            "__builtin_bswap16" | "__builtin_bswap32" | "__builtin_bswap64"
                        ) {
                            if let TokKind::Number = self.cur.kind {
                                let lit = self.cur.text.clone();
                                self.advance();
                                if self.expect(TokKind::RParen, "')'") {
                                    if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                        let bits: u64 = v as u64;
                                        let result = match name.name.as_str() {
                                            "__builtin_bswap16" => {
                                                (bits & 0xffff).swap_bytes() as i128
                                            }
                                            "__builtin_bswap32" => {
                                                (bits as u32).swap_bytes() as i128
                                            }
                                            _ => bits.swap_bytes() as i128,
                                        };
                                        e = Expr::Int {
                                            v: result,
                                            span: sp.clone(),
                                            ty: int_t(),
                                        };
                                        continue;
                                    }
                                }
                            }
                        }
                        // __builtin_expect(cond, expect): 提示分支预测
                        // __builtin_expect_with_probability(cond, expect, prob):
                        //   同样为提示, 编译期折叠为 cond 的值 (与 GCC 行为一致)
                        if name.name == "__builtin_expect"
                            || name.name == "__builtin_expect_with_probability"
                        {
                            if let TokKind::Number = self.cur.kind {
                                let lit = self.cur.text.clone();
                                self.advance();
                                if self.cur.kind == TokKind::Comma {
                                    self.advance();
                                }
                                // 跳过 expect 表达式 (简单形式: 标识符或数字)
                                while self.cur.kind != TokKind::RParen
                                    && self.cur.kind != TokKind::Eof
                                {
                                    self.advance();
                                }
                                // with_probability 多一个 probability 实参
                                if name.name == "__builtin_expect_with_probability" {
                                    if self.cur.kind == TokKind::Comma {
                                        self.advance();
                                    }
                                    while self.cur.kind != TokKind::RParen
                                        && self.cur.kind != TokKind::Eof
                                    {
                                        self.advance();
                                    }
                                }
                                if self.expect(TokKind::RParen, "')'") {
                                    if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                        e = Expr::Int {
                                            v,
                                            span: sp.clone(),
                                            ty: int_t(),
                                        };
                                        continue;
                                    }
                                }
                            }
                        }
                        // __builtin_popcount / __builtin_popcountl / __builtin_popcountll
                        if matches!(
                            name.name.as_str(),
                            "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll"
                        ) {
                            if let TokKind::Number = self.cur.kind {
                                let lit = self.cur.text.clone();
                                self.advance();
                                if self.expect(TokKind::RParen, "')'") {
                                    if let Some(v) = crate::const_eval::eval_int_lit(&lit) {
                                        let n = if v >= 0 { v as u64 } else { !v as u64 };
                                        e = Expr::Int {
                                            v: n.count_ones() as i128,
                                            span: sp.clone(),
                                            ty: int_t(),
                                        };
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    let mut args = vec![];
                    if self.cur.kind != TokKind::RParen {
                        loop {
                            args.push(self.parse_expr_bp(2)?);
                            if self.cur.kind == TokKind::Comma {
                                self.advance();
                                continue;
                            }
                            break;
                        }
                    }
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    if let Expr::Ident { name, .. } = &callee_expr {
                        self.note_implicit_function_call(name);
                    }
                    let call_ty = self.call_result_type(&callee_expr);
                    // Warn for unknown __builtin_* return types
                    if let Expr::Ident { name, .. } = &callee_expr {
                        if name.name.starts_with("__builtin_")
                            && !matches!(
                                callee_expr.ty(),
                                Type::Func(_, _) | Type::Ptr(_)
                            )
                            && !matches!(
                                name.name.as_str(),
                                "__builtin_fma"
                                    | "__builtin_fmaf"
                                    | "__builtin_fmal"
                            )
                        {
                            self.warn_here(
                                "W1015",
                                format!(
                                    "return type of '{}' is not known; assuming int",
                                    name.name
                                ),
                                Some(
                                    "declare the builtin before use or check the name spelling",
                                ),
                            );
                        }
                    }
                    e = Expr::Call {
                        callee: Box::new(callee_expr),
                        args,
                        span: sp.clone(),
                        ty: call_ty,
                    };
                }
                TokKind::LBracket => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    if !self.expect(TokKind::RBracket, "']'") {
                        return None;
                    }
                    let ty = Self::index_result_type(&e);
                    e = Expr::Index {
                        arr: Box::new(e),
                        idx: Box::new(idx),
                        span: sp.clone(),
                        ty,
                    };
                }
                TokKind::Dot => {
                    self.advance();
                    let f = self.parse_ident()?;
                    let ty = self.type_of_member(&e, &f, false);
                    e = Expr::Member {
                        expr: Box::new(e),
                        field: f,
                        span: sp.clone(),
                        ty,
                        is_arrow: false,
                    };
                }
                TokKind::Arrow => {
                    self.advance();
                    let f = self.parse_ident()?;
                    let ty = self.type_of_member(&e, &f, true);
                    e = Expr::Member {
                        expr: Box::new(e),
                        field: f,
                        span: sp.clone(),
                        ty,
                        is_arrow: true,
                    };
                }
                TokKind::PlusPlus => {
                    self.advance();
                    let ty = e.ty();
                    e = Expr::PostInc {
                        e: Box::new(e),
                        span: sp.clone(),
                        ty,
                    };
                }
                TokKind::MinusMinus => {
                    self.advance();
                    let ty = e.ty();
                    e = Expr::PostDec {
                        e: Box::new(e),
                        span: sp.clone(),
                        ty,
                    };
                }
                _ => break,
            }
        }
        Some(e)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Number => {
                let s = &self.cur.text;
                let v = parse_int_lit(s);
                let suffix = s.trim_start_matches(|c: char| {
                    c.is_ascii_hexdigit() || c == 'x' || c == 'X' || c == 'b' || c == 'B'
                });
                let has_unsigned = suffix.chars().any(|c| c == 'u' || c == 'U');
                let long_count = suffix.chars().filter(|c| *c == 'l' || *c == 'L').count();
                let ty = match (has_unsigned, long_count) {
                    (true, 2..) => Type::ULongLong,
                    (true, 1) => Type::ULong,
                    (true, 0) => Type::UInt,
                    (false, 2..) => Type::LongLong,
                    (false, 1) => Type::Long,
                    (false, 0) => Type::Int,
                };
                // C99 虚数后缀: 3i / 3j → _Complex int 虚数字面量 (实部 0)
                let is_imag = suffix
                    .chars()
                    .any(|c| c == 'i' || c == 'I' || c == 'j' || c == 'J');
                self.advance();
                if is_imag {
                    Some(Expr::ComplexLit {
                        real: 0.0,
                        imag: v as f64,
                        span: sp,
                        ty: Type::Complex(Box::new(ty.into())),
                    })
                } else {
                    Some(Expr::Int { v, span: sp, ty })
                }
            }
            TokKind::FloatLit => {
                let s = &self.cur.text;
                // 虚数后缀 i/I/j/J 在类型后缀 F/L 之后 (1.0if / 1.0iF / 4.0i)
                let s_clean = s.trim_end_matches(['f', 'F', 'l', 'L']);
                let is_imag = s_clean.ends_with('i')
                    || s_clean.ends_with('I')
                    || s_clean.ends_with('j')
                    || s_clean.ends_with('J');
                let v_s = if is_imag {
                    &s_clean[..s_clean.len() - 1]
                } else {
                    s_clean
                };
                let v: f64 = if v_s.starts_with("0x") || v_s.starts_with("0X") {
                    parse_hex_float(v_s)
                } else {
                    v_s.parse().unwrap_or(0.0)
                };
                let ty = if s.ends_with('f') || s.ends_with('F') {
                    Type::Float
                } else if s.ends_with('l') || s.ends_with('L') {
                    Type::LongDouble
                } else {
                    Type::Double
                };
                self.advance();
                if is_imag {
                    // 虚数字面量: 4.0i → _Complex double (实部 0, 虚部 v)
                    Some(Expr::ComplexLit {
                        real: 0.0,
                        imag: v,
                        span: sp,
                        ty: Type::Complex(Box::new(ty.into())),
                    })
                } else {
                    Some(Expr::Float { v, span: sp, ty })
                }
            }
            TokKind::StringLit => {
                let mut data = parse_str(&self.cur.text);
                self.advance();
                while self.cur.kind == TokKind::StringLit {
                    data.extend(parse_str(&self.cur.text));
                    self.advance();
                }
                data.push(0); // NUL 终止符
                Some(Expr::String {
                    data,
                    span: sp,
                    ty: Type::Ptr(Box::new(Type::Char.into())),
                })
            }
            TokKind::WideStringLit => {
                let short_wchar = self.opts.short_wchar;
                let mut data = parse_wide_str(&self.cur.text, short_wchar);
                self.advance();
                while self.cur.kind == TokKind::WideStringLit {
                    data.extend(parse_wide_str(&self.cur.text, short_wchar));
                    self.advance();
                }
                // wchar_t = int (4 bytes) on Linux, or short (2 bytes) with -fshort-wchar
                let wchar_ty = if short_wchar { Type::Short } else { Type::Int };
                Some(Expr::String {
                    data,
                    span: sp,
                    ty: Type::Ptr(Box::new(wchar_ty.into())),
                })
            }
            // u"..." char16_t string literal
            TokKind::Char16StringLit => {
                let mut data = parse_char16_str(&self.cur.text);
                self.advance();
                while self.cur.kind == TokKind::Char16StringLit {
                    data.extend(parse_char16_str(&self.cur.text));
                    self.advance();
                }
                // pointer to char16_t
                Some(Expr::String {
                    data,
                    span: sp,
                    ty: Type::Ptr(Box::new(Type::Char16.into())),
                })
            }
            // U"..." char32_t string literal
            TokKind::Char32StringLit => {
                let mut data = parse_char32_str(&self.cur.text);
                self.advance();
                while self.cur.kind == TokKind::Char32StringLit {
                    data.extend(parse_char32_str(&self.cur.text));
                    self.advance();
                }
                // pointer to char32_t
                Some(Expr::String {
                    data,
                    span: sp,
                    ty: Type::Ptr(Box::new(Type::Char32.into())),
                })
            }
            TokKind::CharLit => {
                let v = parse_chr(&self.cur.text);
                self.advance();
                Some(Expr::Char {
                    v,
                    span: sp,
                    ty: Type::Int,
                })
            }
            // L'x' / L'\u00e9' 宽字符字面量: 编码为 UTF-32LE (wchar_t 在 Linux
            // 为 4 字节 int) 或 UTF-16LE (-fshort-wchar 时为 2 字节).
            // Expr::Char.v 为 i128, 可完整容纳 wchar_t 值.
            TokKind::WideCharLit => {
                let short_wchar = self.opts.short_wchar;
                let wide_text = &self.cur.text;
                let bytes = parse_wide_str(wide_text, short_wchar);
                let v = if short_wchar {
                    // UTF-16LE: 2 bytes per char
                    if bytes.len() >= 2 {
                        u16::from_le_bytes([bytes[0], bytes[1]]) as i128
                    } else {
                        0
                    }
                } else {
                    // UTF-32LE: 4 bytes per char
                    if bytes.len() >= 4 {
                        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i128
                    } else {
                        0
                    }
                };
                self.advance();
                let wchar_ty = if short_wchar { Type::Short } else { Type::Int };
                Some(Expr::Char {
                    v,
                    span: sp,
                    ty: wchar_ty,
                })
            }
            // u'x' char16_t character literal
            TokKind::Char16Lit => {
                let text = &self.cur.text;
                let bytes = parse_char16_str(text);
                let v = if bytes.len() >= 2 {
                    u16::from_le_bytes([bytes[0], bytes[1]]) as i128
                } else {
                    0
                };
                self.advance();
                Some(Expr::Char {
                    v,
                    span: sp,
                    ty: Type::Char16,
                })
            }
            // U'x' char32_t character literal
            TokKind::Char32Lit => {
                let text = &self.cur.text;
                let bytes = parse_char32_str(text);
                let v = if bytes.len() >= 4 {
                    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i128
                } else {
                    0
                };
                self.advance();
                Some(Expr::Char {
                    v,
                    span: sp,
                    ty: Type::Char32,
                })
            }
            TokKind::Ident => {
                let t = self.cur.text.clone();
                // C99 __func__ / GCC __FUNCTION__ / __PRETTY_FUNCTION__:
                // 编译期折叠为当前函数名字符串字面量.
                if t == "__func__" || t == "__FUNCTION__" || t == "__PRETTY_FUNCTION__" {
                    self.advance();
                    let name_str = self.current_function.clone().unwrap_or_default();
                    let mut data = name_str.into_bytes();
                    data.push(0);
                    return Some(Expr::String {
                        data,
                        span: sp,
                        ty: Type::Ptr(Box::new(Type::Char.into())),
                    });
                }
                if let Some((val, cty)) = self.enum_consts.get(&t).cloned() {
                    self.advance();
                    return Some(Expr::Int {
                        v: val as i128,
                        span: sp,
                        ty: cty,
                    });
                }
                let id = Ident {
                    name: t,
                    span: sp.clone(),
                };
                self.advance();
                let id_ty = self.lookup_ident_type(&id.name);
                Some(Expr::Ident {
                    name: id,
                    span: sp,
                    ty: id_ty,
                })
            }
            TokKind::LParen => {
                self.advance();
                // (__extension__ expr): complex.h 中 _Complex_I 宏展开为
                // (__extension__ 1.0iF), 此处 __extension__ 是表达式前缀
                // 而非类型说明符 (is_type_specifier_ident 也列出它, 需先
                // 排除后随表达式的情况, 否则 parse_type 会误报 E0003)
                if self.cur.kind == TokKind::Ident
                    && self.cur.text == "__extension__"
                    && !self.is_peek_type_start()
                {
                    self.advance(); // __extension__
                    let e = self.parse_expr()?;
                    if !self.expect(TokKind::RParen, "')'") {
                        return None;
                    }
                    return Some(e);
                }
                // 检查是否是复合字面量: (type){...}
                if self.is_type_start() {
                    // 尝试解析类型
                    let saved_pos = self.cur.clone();
                    let saved_peek = self.peek.clone();
                    if let Some(ty) = self.parse_type() {
                        if self.cur.kind == TokKind::RParen && self.peek.kind == TokKind::LBrace {
                            // 复合字面量: (type){...}
                            self.advance(); // 跳过 )
                            self.advance(); // 跳过 {
                            let mut inits = Vec::new();
                            while self.cur.kind != TokKind::RBrace {
                                let item = if self.cur.kind == TokKind::Dot
                                    || self.cur.kind == TokKind::LBracket
                                {
                                    // 指定初始化器 (支持链式: .a.b.c / [0].field)
                                    let designator = self.parse_designator()?;
                                    if !self.expect(TokKind::Assign, "'='") {
                                        return None;
                                    }
                                    let value = self.parse_expr_bp(2)?;
                                    InitItem::Designated { designator, value }
                                } else {
                                    let e = self.parse_expr_bp(2)?;
                                    InitItem::Expr(e)
                                };
                                inits.push(item);
                                if self.cur.kind == TokKind::Comma {
                                    self.advance();
                                } else {
                                    break;
                                }
                            }
                            if !self.expect(TokKind::RBrace, "'}'") {
                                return None;
                            }
                            return Some(Expr::CompoundLit {
                                ty,
                                inits,
                                span: sp,
                            });
                        }
                    }
                    // 不是复合字面量，恢复状态
                    self.cur = saved_pos;
                    self.peek = saved_peek;
                }
                let e = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") {
                    return None;
                }
                Some(e)
            }
            TokKind::LBrace => {
                // 初始化列表: {1, 2, 3} 或 {.x = 1, .y = 2} 或 {.a.b.c = 1, [0].x = 2}
                self.advance();
                let mut inits = Vec::new();
                while self.cur.kind != TokKind::RBrace {
                    let item =
                        if self.cur.kind == TokKind::Dot || self.cur.kind == TokKind::LBracket {
                            // 指定初始化器 (支持链式: .a.b.c / [0].field)
                            let designator = self.parse_designator()?;
                            if !self.expect(TokKind::Assign, "'='") {
                                return None;
                            }
                            let value = self.parse_expr_bp(2)?;
                            InitItem::Designated { designator, value }
                        } else {
                            // 普通初始化值
                            let e = self.parse_expr_bp(2)?;
                            InitItem::Expr(e)
                        };
                    inits.push(item);
                    if self.cur.kind == TokKind::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                if !self.expect(TokKind::RBrace, "'}'") {
                    return None;
                }
                Some(Expr::InitList {
                    inits,
                    span: sp,
                    ty: Type::Int,
                }) // 类型稍后由上下文确定
            }
            _ => {
                self.err_here(
                    "E0006",
                    format!("expected expression, got {:?}", self.cur.kind),
                    Some("expected a value, variable, or expression here"),
                );
                None
            }
        }
    }

    /// 收集嵌套函数捕获的外层变量
    fn collect_upvars(
        items: &[BlockItem],
        local_vars: &HashMap<String, Type>,
        parent_locals: &HashMap<String, Type>,
        out: &mut Vec<(String, i32)>,
    ) {
        for item in items {
            match item {
                BlockItem::Stmt(s) => Self::collect_upvars_stmt(s, local_vars, parent_locals, out),
                BlockItem::Decl(Decl::Var(v)) => {
                    // 检查初始化表达式中的外层变量引用
                    if let Some(init) = &v.init {
                        Self::collect_upvars_expr(init, local_vars, parent_locals, out);
                    }
                    // 新声明的局部变量加入 local_vars (不修改原始 map, 只在递归时传递)
                }
                BlockItem::Decl(Decl::Func(f)) => {
                    // 递归检查嵌套函数体
                    if let Some(body) = &f.body {
                        Self::collect_upvars(&body.items, local_vars, parent_locals, out);
                    }
                }
                _ => {}
            }
        }
    }
    fn collect_upvars_stmt(
        s: &Stmt,
        local_vars: &HashMap<String, Type>,
        parent_locals: &HashMap<String, Type>,
        out: &mut Vec<(String, i32)>,
    ) {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::collect_upvars_expr(e, local_vars, parent_locals, out)
            }
            Stmt::Return { val: None, .. } => {}
            Stmt::Block { b, .. } => Self::collect_upvars(&b.items, local_vars, parent_locals, out),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::collect_upvars_expr(cond, local_vars, parent_locals, out);
                Self::collect_upvars_stmt(then_, local_vars, parent_locals, out);
                if let Some(e) = else_ {
                    Self::collect_upvars_stmt(e, local_vars, parent_locals, out);
                }
            }
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                Self::collect_upvars_expr(cond, local_vars, parent_locals, out);
                Self::collect_upvars_stmt(body, local_vars, parent_locals, out);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::collect_upvars_stmt(i, local_vars, parent_locals, out);
                }
                if let Some(c) = cond {
                    Self::collect_upvars_expr(c, local_vars, parent_locals, out);
                }
                if let Some(s) = step {
                    Self::collect_upvars_expr(s, local_vars, parent_locals, out);
                }
                Self::collect_upvars_stmt(body, local_vars, parent_locals, out);
            }
            Stmt::Switch { expr, cases, .. } => {
                Self::collect_upvars_expr(expr, local_vars, parent_locals, out);
                for c in cases {
                    Self::collect_upvars(&c.body.items, local_vars, parent_locals, out);
                }
            }
            Stmt::GotoIndirect { expr, .. } => {
                Self::collect_upvars_expr(expr, local_vars, parent_locals, out);
            }
            Stmt::Label { stmt, .. } => {
                Self::collect_upvars_stmt(stmt, local_vars, parent_locals, out);
            }
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for o in outputs {
                    Self::collect_upvars_expr(o, local_vars, parent_locals, out);
                }
                for i in inputs {
                    Self::collect_upvars_expr(i, local_vars, parent_locals, out);
                }
            }
            _ => {}
        }
    }
    fn collect_upvars_expr(
        e: &Expr,
        local_vars: &HashMap<String, Type>,
        parent_locals: &HashMap<String, Type>,
        out: &mut Vec<(String, i32)>,
    ) {
        match e {
            Expr::Ident { name, .. } => {
                let n = &name.name;
                // 如果变量不在当前局部变量表中, 但在父级局部变量表中, 则是捕获的 upvar
                if !local_vars.contains_key(n)
                    && parent_locals.contains_key(n)
                    && !out.iter().any(|(name, _)| name == n)
                {
                    // 偏移占位符 0, 实际偏移在 codegen 中计算
                    out.push((n.clone(), 0));
                }
            }
            Expr::Binop { l, r, .. } => {
                Self::collect_upvars_expr(l, local_vars, parent_locals, out);
                Self::collect_upvars_expr(r, local_vars, parent_locals, out);
            }
            Expr::Unary { e, .. } => Self::collect_upvars_expr(e, local_vars, parent_locals, out),
            Expr::Assign { l, r, .. } => {
                Self::collect_upvars_expr(l, local_vars, parent_locals, out);
                Self::collect_upvars_expr(r, local_vars, parent_locals, out);
            }
            Expr::Call { callee, args, .. } => {
                Self::collect_upvars_expr(callee, local_vars, parent_locals, out);
                for a in args {
                    Self::collect_upvars_expr(a, local_vars, parent_locals, out);
                }
            }
            Expr::Member { expr, .. } => {
                Self::collect_upvars_expr(expr, local_vars, parent_locals, out)
            }
            Expr::Index { arr, idx, .. } => {
                Self::collect_upvars_expr(arr, local_vars, parent_locals, out);
                Self::collect_upvars_expr(idx, local_vars, parent_locals, out);
            }
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                Self::collect_upvars_expr(cond, local_vars, parent_locals, out);
                Self::collect_upvars_expr(then_, local_vars, parent_locals, out);
                Self::collect_upvars_expr(else_, local_vars, parent_locals, out);
            }
            Expr::Cast { e, .. } | Expr::AddrOf { e, .. } | Expr::Deref { e, .. } => {
                Self::collect_upvars_expr(e, local_vars, parent_locals, out);
            }
            Expr::StmtExpr { block, .. } => {
                Self::collect_upvars(&block.items, local_vars, parent_locals, out)
            }
            Expr::VaArg { ap, .. } => {
                Self::collect_upvars_expr(ap, local_vars, parent_locals, out);
            }
            Expr::AsmExpr {
                outputs, inputs, ..
            } => {
                for o in outputs {
                    Self::collect_upvars_expr(o, local_vars, parent_locals, out);
                }
                for i in inputs {
                    Self::collect_upvars_expr(i, local_vars, parent_locals, out);
                }
            }
            Expr::Real { e, .. } | Expr::Imag { e, .. } => {
                Self::collect_upvars_expr(e, local_vars, parent_locals, out);
            }
            Expr::Comma { l, r, .. } => {
                Self::collect_upvars_expr(l, local_vars, parent_locals, out);
                Self::collect_upvars_expr(r, local_vars, parent_locals, out);
            }
            Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
                for init in inits {
                    match init {
                        InitItem::Expr(e) => {
                            Self::collect_upvars_expr(e, local_vars, parent_locals, out);
                        }
                        InitItem::Designated { value, .. } => {
                            Self::collect_upvars_expr(value, local_vars, parent_locals, out);
                        }
                    }
                }
            }
            Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => {
                Self::collect_upvars_expr(e, local_vars, parent_locals, out);
            }
            _ => {}
        }
    }

    /// 宿主头文件中常见的整型 typedef 重定义（如 dev_t / uint64_t）
    fn is_integer_typedef(ty: &Type) -> bool {
        match ty {
            Type::Atomic(inner) => Self::is_integer_typedef(&inner.base),
            Type::Bool
            | Type::Char
            | Type::SChar
            | Type::UChar
            | Type::Short
            | Type::UShort
            | Type::Int
            | Type::UInt
            | Type::Long
            | Type::ULong
            | Type::LongLong
            | Type::ULongLong
            | Type::Int128
            | Type::UInt128
            | Type::Enum(_, _, _)
            | Type::Named(_) => true,
            _ => false,
        }
    }

    /// glibc/linux 头文件对同一名字的重复 typedef（如 timer_t void* vs int）
    fn typedef_redef_allowed(old: &Type, new: &Type) -> bool {
        if matches!(old, Type::Named(_)) || matches!(new, Type::Named(_)) {
            return true;
        }
        matches!(old, Type::Ptr(_)) || matches!(new, Type::Ptr(_))
    }

    /// _Generic 类型兼容性检查 (C11 6.2.7): 指针按 pointee 匹配，void* 与任意对象指针兼容
    fn types_compatible(a: &Type, b: &Type) -> bool {
        let a = match a {
            Type::Atomic(inner) => &inner.base,
            other => other,
        };
        let b = match b {
            Type::Atomic(inner) => &inner.base,
            other => other,
        };
        match (a, b) {
            (Type::Bool, Type::Bool) => true,
            (Type::Char | Type::SChar | Type::UChar, Type::Char | Type::SChar | Type::UChar) => {
                true
            }
            (Type::Short | Type::UShort, Type::Short | Type::UShort) => true,
            (Type::Int | Type::UInt, Type::Int | Type::UInt) => true,
            (Type::Long | Type::ULong, Type::Long | Type::ULong) => true,
            (Type::LongLong | Type::ULongLong, Type::LongLong | Type::ULongLong) => true,
            (Type::Float, Type::Float) => true,
            (Type::Double, Type::Double) => true,
            (Type::LongDouble, Type::LongDouble) => true,
            (Type::Ptr(pa), Type::Ptr(pb)) => {
                if matches!(&pa.base, Type::Void) || matches!(&pb.base, Type::Void) {
                    return true;
                }
                Self::types_compatible(&pa.base, &pb.base)
            }
            (Type::Void, Type::Void) => true,
            (Type::Fract, Type::Fract) => true,
            (Type::Accum, Type::Accum) => true,
            (Type::Struct(na, _, _), Type::Struct(nb, _, _)) => na == nb,
            (Type::Union(na, _, _), Type::Union(nb, _, _)) => na == nb,
            (Type::Enum(na, _, _), Type::Enum(nb, _, _)) => na == nb,
            _ => a == b,
        }
    }
}

fn parse_int_lit(s: &str) -> i128 {
    let s = s.trim_end_matches(|c: char| "lLuUiIjJ".contains(c));
    if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(r, 16).unwrap_or(0)
    } else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        i128::from_str_radix(r, 2).unwrap_or(0)
    } else if let Some(r) = s
        .strip_prefix("0")
        .filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
    {
        i128::from_str_radix(r, 8).unwrap_or(0)
    } else {
        s.parse::<i128>().unwrap_or(0)
    }
}

/// 解析字符串字面量内容（不含末尾 NUL 终止符）。
/// 相邻字符串拼接时由调用方负责在最终数据后补 NUL，
/// 避免中间字符串的 NUL 嵌入结果（如 "a" "b" → "a\0b"）。
fn parse_str(raw: &str) -> Vec<u8> {
    let inner = &raw[1..raw.len() - 1];
    let mut d = vec![];
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => d.push(b'\n'),
                Some('t') => d.push(b'\t'),
                Some('r') => d.push(b'\r'),
                Some('0') => d.push(0),
                Some('\\') => d.push(b'\\'),
                Some('"') => d.push(b'"'),
                Some('\'') => d.push(b'\''),
                Some(c) => {
                    d.push(b'\\');
                    d.push(c as u8);
                }
                None => d.push(b'\\'),
            }
        } else {
            d.extend_from_slice(c.to_string().as_bytes());
        }
    }
    d
}

/// Parse wide string literal L"..." to UTF-32LE bytes (4 bytes per wchar_t)
/// or UTF-16LE bytes (2 bytes per wchar_t) when short_wchar is true.
fn parse_wide_str(raw: &str, short_wchar: bool) -> Vec<u8> {
    // Strip L prefix and quotes: L"..." -> ...
    let inner = if raw.starts_with("L\"") || raw.starts_with("L'") {
        &raw[2..raw.len() - 1]
    } else {
        &raw[1..raw.len() - 1]
    };
    let mut d = vec![];
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        let cp = if c == '\\' {
            match it.next() {
                Some('n') => '\n' as u32,
                Some('t') => '\t' as u32,
                Some('r') => '\r' as u32,
                Some('0') => 0,
                Some('\\') => '\\' as u32,
                Some('"') => '"' as u32,
                Some('\'') => '\'' as u32,
                Some('x') => {
                    // \xHH 十六进制转义, 1-2 位
                    let mut val = 0u32;
                    for _ in 0..2 {
                        if let Some(h) = it.clone().next() {
                            if let Some(d) = h.to_digit(16) {
                                val = val * 16 + d;
                                it.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    val
                }
                Some('u') => {
                    // \u{XXXX} 通用字符名 (C99), 1-6 位十六进制
                    let mut val = 0u32;
                    if it.next() == Some('{') {
                        for _ in 0..6 {
                            if let Some(h) = it.clone().next() {
                                if h == '}' {
                                    it.next();
                                    break;
                                }
                                if let Some(d) = h.to_digit(16) {
                                    val = val * 16 + d;
                                    it.next();
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    val
                }
                Some('U') => {
                    // \UXXXXXXXX 8 位十六进制
                    let mut val = 0u32;
                    for _ in 0..8 {
                        if let Some(h) = it.clone().next() {
                            if let Some(d) = h.to_digit(16) {
                                val = val * 16 + d;
                                it.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    val
                }
                Some(c) => c as u32,
                None => '\\' as u32,
            }
        } else {
            c as u32
        };
        if short_wchar {
            // UTF-16LE: 2 bytes per code unit
            // For BMP (U+0000 to U+FFFF), encode directly
            // For supplementary characters, use surrogate pairs
            if cp <= 0xFFFF {
                d.extend_from_slice(&(cp as u16).to_le_bytes());
            } else {
                // Surrogate pair
                let cp = cp - 0x10000;
                let high = 0xD800 + (cp >> 10);
                let low = 0xDC00 + (cp & 0x3FF);
                d.extend_from_slice(&(high as u16).to_le_bytes());
                d.extend_from_slice(&(low as u16).to_le_bytes());
            }
        } else {
            // UTF-32LE: 4 bytes per code point
            d.extend_from_slice(&cp.to_le_bytes());
        }
    }
    // null terminator: 2 bytes for short_wchar, 4 bytes otherwise
    if short_wchar {
        d.extend_from_slice(&0u16.to_le_bytes());
    } else {
        d.extend_from_slice(&0u32.to_le_bytes());
    }
    d
}

/// Parse u"..." or u'...' char16_t literal to UTF-16LE bytes
fn parse_char16_str(raw: &str) -> Vec<u8> {
    // Strip u prefix and quotes: u"..." -> ...
    let inner = if raw.starts_with("u\"") || raw.starts_with("u'") {
        &raw[2..raw.len() - 1]
    } else {
        &raw[1..raw.len() - 1]
    };
    let mut d = vec![];
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        let cp = if c == '\\' {
            match it.next() {
                Some('n') => '\n' as u32,
                Some('t') => '\t' as u32,
                Some('r') => '\r' as u32,
                Some('0') => 0,
                Some('\\') => '\\' as u32,
                Some('"') => '"' as u32,
                Some('\'') => '\'' as u32,
                Some('x') => {
                    let mut val = 0u32;
                    for _ in 0..2 {
                        if let Some(h) = it.clone().next() {
                            if let Some(d) = h.to_digit(16) {
                                val = val * 16 + d;
                                it.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    val
                }
                Some('u') => {
                    let mut val = 0u32;
                    if it.next() == Some('{') {
                        for _ in 0..4 {
                            if let Some(h) = it.clone().next() {
                                if h == '}' {
                                    it.next();
                                    break;
                                }
                                if let Some(d) = h.to_digit(16) {
                                    val = val * 16 + d;
                                    it.next();
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    val
                }
                Some(c) => c as u32,
                None => '\\' as u32,
            }
        } else {
            c as u32
        };
        // Encode as UTF-16LE (char16_t = 2 bytes)
        if cp <= 0xFFFF {
            d.extend_from_slice(&(cp as u16).to_le_bytes());
        } else {
            // Surrogate pair for code points > 0xFFFF
            let cp = cp - 0x10000;
            let high = 0xD800 + (cp >> 10);
            let low = 0xDC00 + (cp & 0x3FF);
            d.extend_from_slice(&(high as u16).to_le_bytes());
            d.extend_from_slice(&(low as u16).to_le_bytes());
        }
    }
    // null terminator (2 bytes of zero)
    d.extend_from_slice(&0u16.to_le_bytes());
    d
}

/// Parse U"..." or U'...' char32_t literal to UTF-32LE bytes
fn parse_char32_str(raw: &str) -> Vec<u8> {
    // Strip U prefix and quotes: U"..." -> ...
    let inner = if raw.starts_with("U\"") || raw.starts_with("U'") {
        &raw[2..raw.len() - 1]
    } else {
        &raw[1..raw.len() - 1]
    };
    let mut d = vec![];
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        let cp = if c == '\\' {
            match it.next() {
                Some('n') => '\n' as u32,
                Some('t') => '\t' as u32,
                Some('r') => '\r' as u32,
                Some('0') => 0,
                Some('\\') => '\\' as u32,
                Some('"') => '"' as u32,
                Some('\'') => '\'' as u32,
                Some('x') => {
                    let mut val = 0u32;
                    for _ in 0..2 {
                        if let Some(h) = it.clone().next() {
                            if let Some(d) = h.to_digit(16) {
                                val = val * 16 + d;
                                it.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    val
                }
                Some('u') => {
                    let mut val = 0u32;
                    if it.next() == Some('{') {
                        for _ in 0..4 {
                            if let Some(h) = it.clone().next() {
                                if h == '}' {
                                    it.next();
                                    break;
                                }
                                if let Some(d) = h.to_digit(16) {
                                    val = val * 16 + d;
                                    it.next();
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    val
                }
                Some('U') => {
                    let mut val = 0u32;
                    for _ in 0..8 {
                        if let Some(h) = it.clone().next() {
                            if let Some(d) = h.to_digit(16) {
                                val = val * 16 + d;
                                it.next();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    val
                }
                Some(c) => c as u32,
                None => '\\' as u32,
            }
        } else {
            c as u32
        };
        // Encode as UTF-32LE (char32_t = 4 bytes)
        d.extend_from_slice(&cp.to_le_bytes());
    }
    // null terminator (4 bytes of zero)
    d.extend_from_slice(&0u32.to_le_bytes());
    d
}

/// Parse a character constant (single or multi-character).
/// C89 6.1.3.4: multi-character constants like 'ab' have type int,
/// value is implementation-defined (GCC: big-endian concatenation).
/// 'ab' = ('a' << 8) | 'b', 'abc' = ('a' << 16) | ('b' << 8) | 'c'
fn parse_chr(raw: &str) -> i128 {
    let inner = &raw[1..raw.len() - 1];
    if inner.is_empty() {
        return 0;
    }
    // Parse each character (handling escape sequences) and combine
    let mut chars_bytes: Vec<i128> = Vec::new();
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            match bytes[i] {
                b'n' => b'\n' as i128,
                b't' => b'\t' as i128,
                b'r' => b'\r' as i128,
                b'0' if i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_digit() => 0,
                b'\\' => b'\\' as i128,
                b'\'' => b'\'' as i128,
                b'a' => 0x07,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'v' => 0x0b,
                b'x' => {
                    // hex escape: \xNN
                    let mut val: i128 = 0;
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                        val = val * 16 + (bytes[i] as i128 - if bytes[i] <= b'9' { b'0' } else { (bytes[i] & 0xdf) - b'A' + 10 } as i128);
                        i += 1;
                    }
                    chars_bytes.push(val);
                    continue;
                }
                b'u' => {
                    // \uNNNN unicode escape
                    let mut val: i128 = 0;
                    i += 1;
                    let count = 4.min(bytes.len() - i);
                    for _ in 0..count {
                        if i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                            val = val * 16 + (bytes[i] as i128 - if bytes[i] <= b'9' { b'0' } else { (bytes[i] & 0xdf) - b'A' + 10 } as i128);
                            i += 1;
                        }
                    }
                    chars_bytes.push(val);
                    continue;
                }
                b'U' => {
                    // \UNNNNNNNN unicode escape
                    let mut val: i128 = 0;
                    i += 1;
                    let count = 8.min(bytes.len() - i);
                    for _ in 0..count {
                        if i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                            val = val * 16 + (bytes[i] as i128 - if bytes[i] <= b'9' { b'0' } else { (bytes[i] & 0xdf) - b'A' + 10 } as i128);
                            i += 1;
                        }
                    }
                    chars_bytes.push(val);
                    continue;
                }
                c if c >= b'0' && c <= b'7' => {
                    // octal escape: \NNN
                    let mut val: i128 = 0;
                    for _ in 0..3 {
                        if i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'7' {
                            val = val * 8 + (bytes[i] - b'0') as i128;
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    chars_bytes.push(val);
                    continue;
                }
                _ => bytes[i] as i128,
            }
        } else {
            bytes[i] as i128
        };
        chars_bytes.push(ch);
        i += 1;
    }
    if chars_bytes.is_empty() {
        return 0;
    }
    // Multi-character constant: big-endian concatenation
    // 'ab' = ('a' << 8) | 'b'
    let mut result: i128 = 0;
    for &ch in &chars_bytes {
        result = (result << 8) | (ch & 0xff);
    }
    result
}

/// Parse a C99 hexadecimal floating-point constant.
/// Format: [0x] hex_digits [.hex_digits] [p/P [+/-] decimal_exp]
/// Value = mantissa * 2^exponent
/// The input string may include the 0x prefix and type/imag suffixes.
fn parse_hex_float(s: &str) -> f64 {
    let s = s.trim();
    // Strip 0x/0X prefix
    let s = if s.starts_with("0x") || s.starts_with("0X") {
        &s[2..]
    } else {
        s
    };
    // Strip type suffixes (f/F/l/L) and imaginary suffixes (i/I/j/J)
    let s = s.trim_end_matches(['f', 'F', 'l', 'L', 'i', 'I', 'j', 'J']);

    // Split into mantissa and exponent at 'p'/'P'
    let (mantissa_str, exp_str) = if let Some(p_pos) = s.find(|c: char| c == 'p' || c == 'P') {
        (&s[..p_pos], Some(&s[p_pos + 1..]))
    } else {
        (s, None)
    };

    // Parse mantissa: integer_part.fractional_part (in hex)
    let (int_part, frac_part) = if let Some(dot_pos) = mantissa_str.find('.') {
        (&mantissa_str[..dot_pos], &mantissa_str[dot_pos + 1..])
    } else {
        (mantissa_str, "")
    };

    let mut mantissa: f64 = 0.0;
    // Parse integer part
    for ch in int_part.chars() {
        let digit = match ch {
            '0'..='9' => ch as u8 - b'0',
            'a'..='f' => ch as u8 - b'a' + 10,
            'A'..='F' => ch as u8 - b'A' + 10,
            '_' | '\'' => continue, // digit separators
            _ => break,
        };
        mantissa = mantissa * 16.0 + digit as f64;
    }
    // Parse fractional part
    let mut frac_scale = 1.0 / 16.0;
    for ch in frac_part.chars() {
        let digit = match ch {
            '0'..='9' => ch as u8 - b'0',
            'a'..='f' => ch as u8 - b'a' + 10,
            'A'..='F' => ch as u8 - b'A' + 10,
            '_' | '\'' => continue,
            _ => break,
        };
        mantissa += digit as f64 * frac_scale;
        frac_scale /= 16.0;
    }

    // Parse binary exponent
    let binary_exp: i32 = if let Some(exp_s) = exp_str {
        exp_s.parse().unwrap_or(0)
    } else {
        0
    };

    mantissa * (2.0f64).powi(binary_exp)
}

#[cfg(test)]
mod tests {
    use super::parse_wide_str;

    #[test]
    fn wide_str_ascii() {
        // L"a" -> 'a' 编码为 UTF-32LE 0x61 + 4 字节 null terminator
        let bytes = parse_wide_str(r#"L"a""#, false);
        assert_eq!(bytes, vec![0x61, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn wide_str_escape() {
        // L"\n" -> '\n' 编码为 0x0a + null terminator
        let bytes = parse_wide_str(r#"L"\n""#, false);
        assert_eq!(bytes, vec![0x0a, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn wide_str_unicode() {
        // L"\u{e9}" -> U+00E9 (UTF-32LE: e9 00 00 00) + null
        let bytes = parse_wide_str(r#"L"\u{e9}""#, false);
        assert_eq!(bytes, vec![0xe9, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn wide_str_multiple_chars() {
        // L"abc" -> 3 字符, 各 4 字节, + null terminator
        let bytes = parse_wide_str(r#"L"abc""#, false);
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[0..4], &[0x61, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &[0x62, 0, 0, 0]);
        assert_eq!(&bytes[8..12], &[0x63, 0, 0, 0]);
        assert_eq!(&bytes[12..16], &[0, 0, 0, 0]);
    }
}
