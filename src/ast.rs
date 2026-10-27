// src/ast.rs - 抽象语法树
// 第一性原理: AST = 程序的"结构骨架"。类型系统必须能表达 C 的全部类型。
// suckless 准则: 节点种类尽量少;一个节点表示"一个语义概念"。

use crate::diagnostics::Span;
use std::fmt::Write as _;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub decls: Vec<Decl>,
    /// #pragma STDC FENV_ACCESS 状态: ON/DEFAULT 为 true.
    /// 供 codegen 对浮点操作插入 volatile 语义.
    pub fenv_access: bool,
    /// #pragma STDC FP_CONTRACT 状态: ON 为 true.
    /// true 时 codegen 可把 `a*b+c` 收缩为融合乘加.
    pub fp_contract: bool,
    /// #pragma STDC CX_LIMITED_RANGE 状态: ON 为 true.
    /// true 时 complex 除法使用有限范围公式.
    pub cx_limited_range: bool,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Var(VarDecl),
    Func(FuncDecl),
    StructDef(StructDef),
    UnionDef(StructDef),
    EnumDef(EnumDef),
    Typedef {
        name: Ident,
        ty: Type,
        span: Span,
    },
    GlobalAsm {
        asm_str: String,
        outputs: Vec<Expr>,
        output_names: Vec<Option<String>>,
        output_constraints: Vec<String>,
        inputs: Vec<Expr>,
        input_names: Vec<Option<String>>,
        input_constraints: Vec<String>,
        clobbers: Vec<String>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub ty: Type,
    pub name: Ident,
    pub init: Option<Expr>,
    pub span: Span,
    pub is_extern: bool,
    pub is_static: bool,
    // Linux 内核属性
    pub section: Option<String>,
    pub alignment: Option<usize>,
    pub is_weak: bool,
    pub visibility: Option<String>,
    pub is_unused: bool,
    pub register_name: Option<String>, // register asm("rN")
    /// P12 Phase 4：这个声明的**家**，由 IR 侧的位置表折叠而来（`ssa::regalloc::apply_bindings`
    /// 是唯一写入点）。后端读到它就**照办**，不再自己分配。
    ///
    /// 与上面 `register_name` 的分工不是「两套真源」：`register_name` 是用户写的
    /// **文本名**（与 `section`/`alignment` 同类的声明属性，目标中立），而 `home` 是
    /// **分配结果**（已按通道解释好的号）。两者在 `Codegen` 的入口由同一个解析器
    /// 汇成一张家表（优先级：用户指定 > 自动分配，与今天的 `apply_bindings` 一致）。
    /// 因为 `Location` 是 `Copy`，本字段不打破 `VarDecl: Clone`。
    pub home: Option<crate::location::Location>,
    pub cleanup_fn: Option<String>, // __attribute__((cleanup(fn)))
    pub is_thread_local: bool,      // _Thread_local / __thread
    /// __attribute__((no_sanitize("address"|"undefined"|"thread"|"kernel-address")))
    pub no_sanitize: Vec<String>,
    /// __attribute__((alias("target"))): 变量符号别名
    pub alias_name: Option<String>,
    /// __attribute__((copy("fn"))): 函数符号别名
    pub copy_fn: Option<String>,
    /// __attribute__((retain)): 标记节 SHF_GNU_RETAIN
    pub is_retain: bool,
    /// __attribute__((used)): 防止 gc-sections 移除
    pub is_used_attr: bool,
    /// ISO C 限定符 (const/volatile/restrict) 附着在该变量声明上.
    /// 用于优化 pass 检测 volatile 变量，防止其读写被消除.
    pub quals: QualifierFlags,
    /// 地址空间 (x86 段寄存器: __seg_gs / __seg_fs).
    /// 用于 per-cpu 变量访问: 变量本身位于特定段地址空间.
    pub addr_space: AddrSpace,
    /// __attribute__((tls_model("model")))
    /// TLS 访问模型: "global-dynamic", "local-dynamic",
    /// "initial-exec", "local-exec"
    pub tls_model: Option<String>,
    /// __asm__("label"): GNU asm 标签, 覆盖 ELF 符号名
    pub asm_label: Option<String>,
}

/// C 存储类别 (storage-class specifier).
/// 将分散的 is_static / is_extern / is_thread_local bool 统一为语义明确的枚举.
/// 文件级变量没有显式 static 关键字时也具有静态存储期.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    /// 普通局部变量 (auto)
    Auto,
    /// 静态存储期: 文件级 static 或函数内 static
    Static,
    /// extern 声明
    Extern,
    /// register 提示
    Register,
    /// _Thread_local / __thread
    ThreadLocal,
}

impl VarDecl {
    /// 根据声明上下文和 bool 标志计算存储类别.
    /// `is_local`: 是否在函数/块内部声明 (局部上下文).
    pub fn storage_class(&self, is_local: bool) -> StorageClass {
        if self.is_thread_local {
            return StorageClass::ThreadLocal;
        }
        if self.is_extern {
            return StorageClass::Extern;
        }
        if self.is_static {
            return StorageClass::Static;
        }
        if is_local {
            StorageClass::Auto
        } else {
            // 文件级变量即使没有 static 关键字也具有静态存储期
            StorageClass::Static
        }
    }
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub ret_ty: Type,
    pub name: Ident,
    pub params: Vec<Param>,
    pub body: Option<Block>,
    pub span: Span,
    pub is_extern: bool,
    pub is_static: bool,
    // Linux 内核属性
    pub section: Option<String>,
    pub alignment: Option<usize>,
    pub is_weak: bool,
    pub visibility: Option<String>,
    pub is_unused: bool,
    pub is_cold: bool,
    pub is_hot: bool,
    pub is_noinline: bool,
    pub is_always_inline: bool,
    /// C `inline` 关键字 (GNU89 语义): 非 static inline 函数发射为 weak 符号
    /// 避免多个编译单元包含同一头文件时产生多重定义
    pub is_inline: bool,
    pub is_variadic: bool,
    // constructor/destructor/alias
    pub is_constructor: bool,
    pub is_destructor: bool,
    pub alias_name: Option<String>,
    // 嵌套函数静态链
    pub is_nested: bool,             // 是否是嵌套函数
    pub upvars: Vec<(String, i64)>,  // 从外层捕获的变量: (name, offset_in_parent_frame)
    pub is_old_style: bool,          // K&R 风格定义
    pub format_kind: Option<String>, // __attribute__((format(printf, ...)))
    pub format_string_idx: Option<u32>,
    pub format_first_arg: Option<u32>,
    /// __attribute__((no_sanitize("address"|"undefined"|"thread"|"kernel-address")))
    pub no_sanitize: Vec<String>,
    /// __attribute__((target_clones("default,avx2,...", "sse4.2,default")))
    /// 收集所有目标版本 (忽略实际变体生成, 仅用于兼容)
    pub target_clones: Vec<String>,
    /// __attribute__((returns_nonnull))
    pub returns_nonnull: bool,
    /// __attribute__((constructor(N))) / __attribute__((destructor(N)))
    pub ctor_priority: Option<u16>,
    pub dtor_priority: Option<u16>,
    /// __attribute__((retain)): 标记节 SHF_GNU_RETAIN，gc-sections 保留
    pub is_retain: bool,
    /// __attribute__((used)): 标记 USED，防止 gc-sections 移除
    pub is_used_attr: bool,
    /// __attribute__((flatten)): 所有调用视为 always_inline
    pub is_flatten: bool,
    /// __attribute__((error("msg"))): 调用时发射编译错误
    pub error_msg: Option<String>,
    /// __attribute__((warning("msg"))): 调用时发射编译警告
    pub warning_msg: Option<String>,
    /// __attribute__((deprecated("msg"))): 调用时发射弃用警告
    pub deprecated_msg: Option<String>,
    /// __attribute__((ifunc("resolver"))): 生成 STT_GNU_IFUNC
    pub ifunc_resolver: Option<String>,
    /// __attribute__((asm_output("template"))): GCC15 提案, 指定返回值 asm 输出模板
    pub asm_output_template: Option<String>,
    /// CUDA/HIP: __global__ 函数 (device entry, host 侧可调用)
    pub is_global: bool,
    /// CUDA/HIP: __device__ 函数 (仅 device 侧可见)
    pub is_device: bool,
    /// CUDA/HIP: __host__ 函数 (仅 host 侧可见)
    pub is_host: bool,
    /// __attribute__((naked)): 不生成 prologue/epilogue，函数体完全由内联汇编组成
    pub is_naked: bool,
    /// __attribute__((noreturn)) / C11 _Noreturn: 函数不会正常返回
    /// 编译器应在函数末尾插入 trap 指令（ud2 / brk / 等价指令）防止
    /// 执行流落入后续代码；优化器可利用此信息进行死代码消除。
    pub is_noreturn: bool,
    /// __attribute__((pure)): 函数无副作用, 结果仅依赖参数和/或全局变量
    /// 优化器可消除无副作用的重复调用 (CSE)
    pub is_pure: bool,
    /// __attribute__((const)): 函数结果仅依赖参数, 不读全局内存
    /// 比 pure 更强的约束: 优化器可折叠相同参数的多次调用
    pub is_const_attr: bool,
    /// __attribute__((returns_twice)): 函数可能多次返回 (如 setjmp)
    /// 阻止优化器对调用点做单次返回假设
    pub returns_twice: bool,
    /// __attribute__((no_instrument_function)): 禁止 profiling/fuzzer 插桩
    pub no_instrument: bool,
    /// __attribute__((malloc)): 返回指针不别名已有内存
    pub is_malloc: bool,
    /// __attribute__((malloc (dealloc_fn, argno))): GCC11+ 释放函数提示
    pub malloc_dealloc: Option<String>,
    pub malloc_dealloc_argno: Option<u32>,
    /// __attribute__((alloc_size(N[,M]))): 分配大小参数索引
    pub alloc_size: Option<(u32, Option<u32>)>,
    /// __attribute__((assume_aligned(N))): 返回指针对齐至少 N 字节
    pub assume_aligned: Option<usize>,
    /// __attribute__((warn_unused_result)): 返回值不应被忽略
    pub warn_unused_result: bool,
    /// __attribute__((sentinel)): 可变参函数需 NULL 结尾
    pub is_sentinel: bool,
    /// __attribute__((format_arg(N))): 第 N 个参数是格式字符串
    pub format_arg_idx: Option<u32>,
    /// C23 [[nodiscard]]: 函数返回值不应被忽略
    /// 当该函数的调用结果作为语句被丢弃时, 编译器应发射警告
    pub is_nodiscard: bool,
    /// __asm__("label"): GNU asm 标签, 覆盖 ELF 符号名
    pub asm_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub ty: Type,
    pub name: Option<Ident>,
    pub span: Span,
    /// ISO C 限定符 (const/volatile/restrict) 附着在该参数上
    pub quals: QualifierFlags,
    /// 数组参数退化时丢弃的最外层 VLA 维度表达式。
    /// C11 6.9.1p10: 形参声明中的可变长维度在每次调用入口求值
    /// (副作用生效), 如 `int f(int i, int a[i++])` 的 i++。
    /// 多维时仅最外层被丢弃 (内层维度保留在 Ptr(Array(...)) 中)。
    pub vla_dim: Option<Box<Expr>>,
    /// 形参的「家」：与 [`VarDecl::home`] 同源同含义，只是形参不是 `VarDecl`。
    ///
    /// 为什么需要一个平行字段：解释器类函数（`luaV_execute(lua_State *L, CallInfo *ci)`）
    /// 每步要读的热值往往就是那两个形参，而候选表以 `Function::original_locals` 为「局部」
    /// 真源（`ssa/build.rs` 写明完整集是 `original_locals ∪ params`），只看前者时形参连候选
    /// 都不是，序言只能把入站寄存器存进栈槽、此后每次读都是一次访存。
    ///
    /// 由 `ssa::regalloc::deliver` 在 `DCC_HOME_PARAMS=1` 时写入（第一版只发 callee-saved
    /// 的 POOL 号，与 SysV 入站号 `rdi,rsi,rdx,rcx,r8,r9` 不相交，所以序言里「先存 callee-saved
    /// 再搬形参」不会互相踩）；后端读它把形参直接落在号上，`home_for`/`mov_to_stack` 那套
    /// 现有机制自动跟着走，不需要为形参再造一套寻址。
    pub home: Option<crate::location::Location>,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub items: Vec<BlockItem>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum BlockItem {
    Stmt(Stmt),
    Decl(Decl),
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Stmt {
    Return {
        val: Option<Expr>,
        span: Span,
    },
    Expr {
        e: Expr,
        span: Span,
    },
    If {
        cond: Expr,
        then_: Box<Stmt>,
        else_: Option<Box<Stmt>>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
        span: Span,
    },
    Do {
        body: Box<Stmt>,
        cond: Expr,
        span: Span,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
        span: Span,
    },
    Block {
        b: Block,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Goto {
        target: Ident,
        span: Span,
    },
    GotoIndirect {
        expr: Box<Expr>,
        span: Span,
    }, // goto *expr
    Label {
        name: Ident,
        stmt: Box<Stmt>,
        span: Span,
    },
    Switch {
        expr: Expr,
        cases: Vec<Case>,
        /// 出现在第一个 `case`/`default` 之前的普通声明/语句（含 goto 标签）。
        /// C 允许 `switch (x) { foo: case 1: ... }`，这些前驱语句必须被真实发射
        /// （标签才能被 `goto foo;` 解析到）。以前 parse_cases 把它们按 token 吞掉，
        /// 导致标签从未发射 ⇒ E1102。
        prelude: Vec<BlockItem>,
        span: Span,
    },
    AsmGoto {
        asm_str: String,
        outputs: Vec<Expr>,
        output_names: Vec<Option<String>>,
        output_constraints: Vec<String>,
        inputs: Vec<Expr>,
        input_names: Vec<Option<String>>,
        input_constraints: Vec<String>,
        clobbers: Vec<String>,
        goto_labels: Vec<Ident>,
        /// asm goto is always volatile (GCC semantics)
        volatile: bool,
        span: Span,
    },
    /// C23 `[[fallthrough]]` / GCC `__attribute__((fallthrough))` 语句,
    /// 显式标记 switch case 有意贯穿.
    Fallthrough {
        span: Span,
    },
    /// C++ 零成本异常 (zero-cost EH) 的**受保护调用** (LLVM `invoke` 的等价物)。
    ///
    /// `call` 是一次可能抛出异常的调用 (参数求值与调用序列完全复用普通
    /// `Stmt::Expr(Expr::Call{..})` 的生成); `landing_pad` 是 unwinder 在
    /// 「本调用点抛出并需要交接给当前帧」时**直接跳转**进来的标签; `actions`
    /// 是本调用点在 LSDA call-site 表里的 action 链。
    ///
    /// 为什么必须把「哪一次调用受保护」建模成语句而不是 CFG 的一条边: 表驱动
    /// 展开以 **PC 地址区间** 为键 —— unwinder 只看抛出时的 PC 落在哪个区间,
    /// 因此「区间 = 一条调用指令」就是清理级别的唯一真源。每个调用点有独立的
    /// 区间, 落地垫才能只析构**该点仍然存活**的对象 (少则泄漏, 多则二次析构)。
    Invoke {
        call: Box<Stmt>,
        /// 落地垫标签; `None` = LSDA 里 `landing_pad = 0` 且 `action = 0` 的区间
        /// (受保护但本帧不接管, `_Unwind_Resume` / `__cxa_rethrow` 用)。
        landing_pad: Option<Ident>,
        actions: Vec<EhAction>,
        span: Span,
    },
    /// C++ 零成本异常的**落地垫序言**: `label:` + 把 unwinder 交接的寄存器存入
    /// 两个局部槽。
    ///
    /// x86-64 C++ ABI 规定 personality 返回 `_URC_HANDLER_FOUND` 后, 控制被直接
    /// 交到 landing pad, 此时 `%rax` = 异常对象指针, `%rdx` = type selector
    /// (LSDA 类型表索引, 0 = 纯 cleanup)。这两个值只在落地垫的前几条指令内有效
    /// (后续语句可能覆盖 rax/rdx), 所以后端必须在**标签后的第一条指令**把它们
    /// 溢出到槽位; 之后的分派 (按 selector 比较) / 清理 / `__cxa_begin_catch`
    /// 全是普通 AST 语句。
    EhLandingPad {
        label: Ident,
        object: Box<Expr>,
        selector: Box<Expr>,
        span: Span,
    },
}

/// 受保护调用的 LSDA action 链条目 (按处理顺序排列, 首条即链入口)。
///
/// 与 libstdc++ `__gxx_personality_v0` 读取的 action record 一一对应:
/// 每条记录是 `(sleb128 filter, sleb128 next)`, `next` 是链上下一条记录的
/// **自相对偏移** (从 `next` 字段自身的位置起算, 0 = 链尾); call-site 表里的
/// `action` 是链首记录相对 action 表的 **1-based** 字节偏移 (0 = 无 action)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EhAction {
    /// filter 0: 栈对象析构清理。进入落地垫做完清理后把异常继续交给链上下一条
    /// (因此「先 cleanup 再候选 handler」是析构顺序的唯一真源)。
    Cleanup,
    /// filter > 0: 候选 handler。`type_index` 是本函数 LSDA 类型表 (1-based,
    /// 从表尾向前计数) 的索引; `typeinfo` 是该表项指向的 typeinfo 对象链接名,
    /// `None` = 空表项 (`catch (...)` 无条件捕获任意异常)。
    /// 命中时 personality 把 selector = `type_index` 交给落地垫。
    Catch {
        type_index: u32,
        typeinfo: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Case {
    pub val: Option<Expr>,
    /// GNU `case low ... high:` 的上界（仅当 val 为下界时有效）
    pub range_end: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

/// 浮点字面量的编译期值。
///
/// 变体携带其**源码语义类型**的精确值：`1.0f` → [`FloatConst::F32`]，
/// `1.0` → [`FloatConst::F64`]，`1.0L` → [`FloatConst::LongDouble`]
/// （真 80-bit 位型，直接由十进制文本正确舍入得到，**不经 f64 中转**，
/// 从而消除 `0.1L` 被拓宽成 f64 值的末位精度差异）。
///
/// 消费方必须按变体显式匹配处理；不支持 x87 80-bit 的后端通过
/// [`FloatConst::as_f64`] 显式降级，降级点因此全部可见、可审计。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatConst {
    F32(f32),
    F64(f64),
    LongDouble(crate::float80::Bits),
    /// IEEE binary128（`__float128` / `_Float128`）的 16 字节位模式。
    /// 必须按位保存——binary128 的 113 位尾数 f64（53 位）装不下，经 f64 中转
    /// 会丢精度（见 `crate::float128`）。
    Float128([u8; 16]),
}

impl FloatConst {
    /// 以 f64 读取值。F32 先提升；LongDouble 经真 80-bit → f64
    /// 正确舍入（仅供非 x86 后端与常量折叠等显式降级场景使用）。
    /// Float128 经精确拓宽后再舍入到 f64（对“可精确 f64 表示”的子集完全精确）。
    pub fn as_f64(self) -> f64 {
        match self {
            FloatConst::F32(f) => f as f64,
            FloatConst::F64(f) => f,
            FloatConst::LongDouble(ref b) => crate::float80::to_f64(b),
            FloatConst::Float128(ref bits) => crate::float128::to_f64(bits),
        }
    }

    /// 是否为 long double（80-bit）字面量。
    pub fn is_long_double(self) -> bool {
        matches!(self, FloatConst::LongDouble(_))
    }

    /// 是否为 binary128 字面量。
    pub fn is_float128(self) -> bool {
        matches!(self, FloatConst::Float128(_))
    }

    /// 是否为各精度的（正或负）零。
    pub fn is_zero(self) -> bool {
        match self {
            FloatConst::F32(f) => f == 0.0,
            FloatConst::F64(f) => f == 0.0,
            FloatConst::LongDouble(ref b) => crate::float80::is_zero(b),
            FloatConst::Float128(ref b) => b.iter().all(|&x| x == 0),
        }
    }

    /// 符号翻转（保留各变体自身精度；用于一元 `-` 作用于字面量）。
    pub fn negate(self) -> FloatConst {
        match self {
            FloatConst::F32(f) => FloatConst::F32(-f),
            FloatConst::F64(f) => FloatConst::F64(-f),
            FloatConst::LongDouble(b) => FloatConst::LongDouble(crate::float80::negate(b)),
            FloatConst::Float128(mut bits) => {
                // 仅翻转最高符号位（位 127）。位数组是小端内存镜像，
                // 故符号位在 bits[15] 的最高位。
                bits[15] ^= 0x80;
                FloatConst::Float128(bits)
            },
        }
    }

    /// 指定浮点类型的正零常量。所有“分量缺失 / 纯虚数实部”等场景统一
    /// 经此构造，杜绝随手写 `F64(0.0)` 把 LongDouble 分量静默降级。
    pub fn zero(ty: &Type) -> FloatConst {
        match ty {
            Type::Float => FloatConst::F32(0.0),
            Type::LongDouble => FloatConst::LongDouble(crate::float80::from_f64(0.0)),
            Type::Float128 => FloatConst::Float128([0u8; 16]),
            _ => FloatConst::F64(0.0),
        }
    }

    /// 整型常量 → 指定目标浮点类型，全程精确 RNE。
    ///
    /// 旧路径是 `F64(i as f64)` / `from_f64(i as f64)`：对 `|i| >= 2^53`
    /// 的整数会先丢精度再“拓宽”。统一走 float80 的整数转换入口；
    /// LongDouble 对 `|i| < 2^64` 完全精确。
    pub fn from_int(ty: &Type, i: i128) -> FloatConst {
        match ty {
            Type::Float => FloatConst::F32(crate::float80::i128_to_f32(i)),
            Type::LongDouble => FloatConst::LongDouble(crate::float80::from_i128(i)),
            Type::Float128 => FloatConst::Float128(crate::float128::from_int(i)),
            _ => FloatConst::F64(crate::float80::i128_to_f64(i)),
        }
    }

    /// 把常量转换到目标浮点类型（IEEE 舍入）。
    ///
    /// 跨精度转换必须**显式**做：binary128（113 位尾数）与 x87 80-bit
    /// （64 位）的对象表示完全不同，把 binary128 的 16 字节位型直接写进
    /// `long double` 槽会被读侧当 80-bit 解析成垃圾（`long double g = 1.0q;`
    /// 曾因此读回 0.0）。NaN 跨类型转换按 IEEE 规则 quieting（信令 NaN 变
    /// 安静 NaN）；**同类型**转换不走这里，故信令语义得以保留。
    pub fn coerce_to(self, ty: &Type) -> FloatConst {
        match ty {
            Type::Float => FloatConst::F32(match self {
                FloatConst::F32(f) => f,
                FloatConst::F64(f) => f as f32,
                FloatConst::LongDouble(b) => crate::float80::to_f64(&b) as f32,
                FloatConst::Float128(b) => crate::float128::to_f32(&b),
            }),
            Type::Double => FloatConst::F64(match self {
                FloatConst::F32(f) => f as f64,
                FloatConst::F64(f) => f,
                FloatConst::LongDouble(b) => crate::float80::to_f64(&b),
                FloatConst::Float128(b) => crate::float128::to_f64(&b),
            }),
            Type::LongDouble => FloatConst::LongDouble(match self {
                FloatConst::F32(f) => crate::float80::from_f64(f as f64),
                FloatConst::F64(f) => crate::float80::from_f64(f),
                FloatConst::LongDouble(b) => b,
                FloatConst::Float128(b) => crate::float128::to_f80(&b),
            }),
            Type::Float128 => FloatConst::Float128(match self {
                FloatConst::F32(f) => crate::float128::from_f64(f as f64),
                FloatConst::F64(f) => crate::float128::from_f64(f),
                FloatConst::LongDouble(b) => crate::float128::from_f80(&b),
                FloatConst::Float128(b) => b,
            }),
            _ => self,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Expr {
    Int {
        v: i128,
        span: Span,
        ty: Type,
    },
    Float {
        c: FloatConst,
        span: Span,
        ty: Type,
    },
    /// _Complex 字面量: 4.0i / 3j / 1.0iF (实部 + 虚部; ty = _Complex T)
    ComplexLit {
        real: FloatConst,
        imag: FloatConst,
        span: Span,
        ty: Type,
    },
    /// 整型 _Complex 字面量: 3j / 1i (实部 + 虚部，精确 i128，不经浮点)
    ///
    /// 与 [`Expr::ComplexLit`] 分开：浮点域的 `FloatConst` 无法在
    /// `|i| >= 2^53` 时精确承载整数，旧路径 `F64(v as f64)` 会先丢
    /// 精度再由 codegen 转回整数。
    ComplexIntLit {
        real: i128,
        imag: i128,
        span: Span,
        ty: Type,
    },
    /// __real__ expr: 取 _Complex 表达式实部 (对标量返回自身)
    Real {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    /// __imag__ expr: 取 _Complex 表达式虚部 (对标量返回 0)
    Imag {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    String {
        data: Vec<u8>,
        span: Span,
        ty: Type,
    },
    Char {
        v: i128,
        span: Span,
        ty: Type,
    },
    Ident {
        name: Ident,
        span: Span,
        ty: Type,
    },
    Binop {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
        span: Span,
        ty: Type,
    },
    Unary {
        op: UnOp,
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
        ty: Type,
    },
    Assign {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
        span: Span,
        ty: Type,
    },
    Cond {
        cond: Box<Expr>,
        then_: Box<Expr>,
        else_: Box<Expr>,
        span: Span,
        ty: Type,
    },
    Cast {
        ty: Type,
        e: Box<Expr>,
        span: Span,
    },
    SizeofTy {
        ty: Type,
        span: Span,
    },
    SizeofExpr {
        e: Box<Expr>,
        span: Span,
    },
    Index {
        arr: Box<Expr>,
        idx: Box<Expr>,
        span: Span,
        ty: Type,
    },
    Member {
        expr: Box<Expr>,
        field: Ident,
        span: Span,
        ty: Type,
        is_arrow: bool,
    },
    AddrOf {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    Deref {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    PostInc {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    PostDec {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    PreInc {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    PreDec {
        e: Box<Expr>,
        span: Span,
        ty: Type,
    },
    CompoundLit {
        ty: Type,
        inits: Vec<InitItem>,
        span: Span,
    },
    /// 初始化列表: {1, 2, 3} 或 {.x = 1, .y = 2}
    InitList {
        inits: Vec<InitItem>,
        span: Span,
        ty: Type,
    },
    /// _Alignof(type)
    AlignofTy {
        ty: Type,
        span: Span,
    },
    /// _Alignof(expr)
    AlignofExpr {
        e: Box<Expr>,
        span: Span,
    },
    /// 语句表达式 ({...})
    StmtExpr {
        block: Box<Block>,
        span: Span,
        ty: Type,
    },
    /// 逗号运算符: (lhs, rhs) → 求值 lhs 丢弃, 返回 rhs
    Comma {
        l: Box<Expr>,
        r: Box<Expr>,
        span: Span,
        ty: Type,
    },
    /// 内联汇编 __asm__: 字符串列表, 结果类型
    AsmExpr {
        asm_str: String,
        outputs: Vec<Expr>,
        output_names: Vec<Option<String>>,
        output_constraints: Vec<String>,
        inputs: Vec<Expr>,
        input_names: Vec<Option<String>>,
        input_constraints: Vec<String>,
        clobbers: Vec<String>,
        /// asm volatile: 阻止优化器删除/移动该 asm 语句
        volatile: bool,
        span: Span,
        ty: Type,
    },
    /// &&label: label 地址 (GNU 扩展)
    LabelAddr {
        label: Ident,
        span: Span,
        ty: Type,
    },
    /// __builtin_va_arg(ap, type): 可变参数读取
    VaArg {
        ap: Box<Expr>,
        ty: Type,
        span: Span,
    },
    /// __builtin_va_arg_pack(): 将当前函数所有变参原样转发给被调方
    VaArgPack {
        span: Span,
        ty: Type,
    },
    /// __builtin_offsetof(type, member)
    Offsetof {
        ty: Type,
        member: String,
        /// 数组下标表达式 (按出现顺序), 用于运行时偏移计算
        indices: Vec<Box<Expr>>,
        span: Span,
        result_ty: Type,
    },
}

/// 初始化项: 普通值 或 指定初始化器
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum InitItem {
    /// 普通初始化值
    Expr(Expr),
    /// 指定初始化器: .field = expr 或 [idx] = expr
    Designated { designator: Designator, value: Expr },
}

/// 填满一个该类型的对象需要多少个**标量**初始值 (C11 6.7.9 brace elision 的
/// 「每个元素吞多少标量」口径)。
///
/// 为什么需要: `struct S b[][1] = {3,0,3,4,...}` 里最外层元素类型是
/// `struct S[1]` (一个结构体的数组), 按 C11 6.7.9p14 花括号省略规则, 扁平
/// 标量要继续往**里层结构体**灌, 每 4 个标量填满一个元素 —— 不是 1 个标量
/// 就换行 (pr80622: 那样会把 68 个标量铺成 68 行, 每行只写首字段)。
/// 匿名位域不可初始化, 不计入; 联合只占 1 个标量槽。
pub fn init_scalar_capacity(ty: &Type) -> usize {
    match ty {
        Type::Array(elem, ArraySize::Const(n)) => {
            (*n).max(0) as usize * init_scalar_capacity(&elem.base)
        },
        // VLA / 未知维度: 保守按 1 (调用方遇到 0/1 都不做分组)
        Type::Array(_, _) => 1,
        Type::Struct(_, fields, _, _) => fields
            .iter()
            .filter(|f| !(f.name.is_empty() && f.bit_width.is_some()))
            .map(|f| init_scalar_capacity(&f.ty))
            .sum::<usize>()
            .max(1),
        Type::Union(_, _, _, _) => 1,
        _ => 1,
    }
}

/// 由初始化列表推断最外层数组维度 (C11 6.7.9p22): 按「光标」模型数元素个数 ——
/// 花括号/字符串/指定项各自完结一个元素, 扁平标量按 `init_scalar_capacity`
/// 个一组填满一个元素。
///
/// 例: `struct S b[][1] = {68 个标量}` (每元素 4 标量) → 17;
/// `int a[][2] = {{1,2}, 3}` → 2 (前者 1 个元素, 后者开启新元素)。
pub fn count_init_elements(elem_ty: &Type, inits: &[InitItem]) -> usize {
    let sub = init_scalar_capacity(elem_ty);
    if sub <= 1 {
        // 元素是标量 (或容量未知): 一项一个元素
        return inits.len();
    }
    let mut count = 0usize;
    let mut cursor = 0usize; // 当前元素内已吞的标量数
                             // 字符串字面量只有在**整个元素就是字符数组**时才是自完结项 (一行一个);
                             // 否则它只填字符数组字段那一个叶槽, 按 1 个标量计 (与
                             // group_for_brace_elision 的分组口径一致)。
    let elem_is_char_array = match elem_ty {
        Type::Array(inner, _) => {
            matches!(inner.base, Type::Char | Type::SChar | Type::UChar | Type::Char8)
        },
        _ => false,
    };
    for item in inits {
        // 自完结项: 显式花括号 / 指定初始化器 / (字符数组元素的) 字符串
        let self_contained = match item {
            InitItem::Designated { .. } => true,
            InitItem::Expr(Expr::String { .. }) if elem_is_char_array => true,
            InitItem::Expr(e) => matches!(e, Expr::InitList { .. } | Expr::CompoundLit { .. }),
        };
        if self_contained {
            if cursor > 0 {
                // 之前攒的扁平标量先完结一个元素
                count += 1;
                cursor = 0;
            }
            count += 1;
        } else {
            cursor += 1;
            if cursor >= sub {
                count += 1;
                cursor = 0;
            }
        }
    }
    if cursor > 0 {
        count += 1;
    }
    count
}

/// 由初始化器补齐不完整数组维度（C11 6.7.9p22）。推导规则只有这一处：
/// 数组分支与括号声明符分支都必须走它，否则「谁的维度被补」取决于声明符长什么样。
///
/// 为什么需要单独成函数：这条推导原本内嵌在「数组声明符」分支里，而**经括号声明符**
/// 构造出来的数组走的是函数指针/指针分支，从来没人替它推导，维度就停在
/// `ArraySize::Const(0)`：对象尺寸算成 0，`sizeof(t)/sizeof(t[0])` 也得 0。SQLite 的
/// `static int (*const sqlite3BuiltinExtensions[])(sqlite3*) = {…}` 正是这个形状，
/// 长度为 0 意味着内置扩展一个都不注册，dcc 产物里 `sqlite_dbpage` 一类虚表不存在。
pub fn deduce_incomplete_array_dim(ty: Type, init: Option<&Expr>) -> Type {
    let Some(init) = init else { return ty };
    match (&ty, init) {
        (Type::Array(elem, ArraySize::Const(0)), Expr::InitList { inits, .. }) => {
            // 元素类型是聚合体时按 brace elision 数元素：扁平标量每
            // `init_scalar_capacity` 个填满一个元素（pr80622）。
            let count = count_init_elements(&elem.base, inits);
            Type::Array(elem.clone(), ArraySize::Const(count))
        },
        (Type::Array(elem, ArraySize::Const(0)), Expr::String { data, .. }) => {
            // 宽字符串 L"…" 的 data 是原始字节 (4 bytes/wchar_t)，数组大小应是元素个数。
            let elem_sz = elem.size().max(1);
            let num_elems = if elem_sz > 1 {
                data.len().div_ceil(elem_sz)
            } else {
                data.len()
            };
            Type::Array(elem.clone(), ArraySize::Const(num_elems))
        },
        _ => ty,
    }
}

/// sanitizer（ASan / KASAN）插桩会把 lvalue 包成 `Comma{ check, 真 lvalue }`：先跑
/// shadow 检查调用，再对里面的 lvalue 走 store。发射点在 `Expr::Assign` 臂开头就是逐层
/// 剥开再分派的。**任何按形状判断 lvalue 的代码都必须先过这一道**，否则同一个 LHS 在
/// 两边算出不同答案：帧规划期漏算一个晚分配槽就是 `E1133`（实测 `-fsanitize=address`
/// 编 SQLite 的 `sqlite3ExprListDup` / `sqlite3SrcListDup` 编不外）。
pub fn peel_instrumented_lvalue(e: &Expr) -> &Expr {
    let mut cur = e;
    while let Expr::Comma { r, .. } = cur {
        cur = r;
    }
    cur
}

/// 指定符
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Designator {
    /// .field
    Field(Ident),
    /// [index]
    Index(Expr),
    /// GNU 数组范围指定符: [first ... last] (GCC 扩展,
    /// 范围内所有元素赋同一值). Linux kernel nodemask_t 初始化使用.
    Range(Expr, Expr),
    /// 链式指定符 .a.b.c 或 [0].field[1]
    /// (ISO C99 6.7.8/19: 多个 designator 按顺序应用以定位子对象).
    /// Linux kernel `_ddebug` 初始化使用 `.key.dd_key_true = ...` (进入
    /// union 的具名字段再取 union 成员).
    Chain(Vec<Designator>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    Plus,
}

// ===== 类型系统 =====
// 数组大小: 编译期常量 或 运行时表达式 (VLA)
#[derive(Debug, Clone)]
pub enum ArraySize {
    Const(usize),
    Var(Box<Expr>), // 运行时表达式 (VLA)
}

impl PartialEq for ArraySize {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ArraySize::Const(a), ArraySize::Const(b)) => a == b,
            _ => false, // Var 不参与相等比较
        }
    }
}
impl Eq for ArraySize {}

/// C 类型限定符位标志 (const / volatile / restrict / _Atomic).
///
/// 仿照 Clang Qualifiers 模式: 一组无符号位标志, 通过按位或组合.
/// 由于 dcc 不引入 bitflags 外部依赖, 这里手写一组位运算 helper.
///
/// 编码 (u8):
///   bit 0 (Q_CONST)    = 0x01  const
///   bit 1 (Q_VOLATILE) = 0x02  volatile
///   bit 2 (Q_RESTRICT) = 0x04  restrict
///   bit 3 (Q_ATOMIC)   = 0x08  _Atomic (顶层; _Atomic(T) 走 Atomic 变体)
///   bit 4 (Q_NODEREF)  = 0x10  GCC __attribute__((nodederef)) 内部使用
///
/// Address-space 限定符 (__seg_gs / __seg_fs 等) 不进入此位标志,
/// 因为它们的取值空间大于 1 bit, 走 qualifier 解析阶段的独立字段.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct QualifierFlags(pub u8);
impl QualifierFlags {
    pub const NONE: Self = Self(0);
    pub const CONST: Self = Self(0x01);
    pub const VOLATILE: Self = Self(0x02);
    pub const RESTRICT: Self = Self(0x04);
    pub const ATOMIC: Self = Self(0x08);
    pub const NODEREF: Self = Self(0x10);

    #[inline]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn is_const(self) -> bool {
        (self.0 & Self::CONST.0) != 0
    }

    #[inline]
    pub const fn is_volatile(self) -> bool {
        (self.0 & Self::VOLATILE.0) != 0
    }

    #[inline]
    pub const fn is_restrict(self) -> bool {
        (self.0 & Self::RESTRICT.0) != 0
    }

    #[inline]
    pub const fn is_atomic(self) -> bool {
        (self.0 & Self::ATOMIC.0) != 0
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// 任意非 const 限定符 (volatile / restrict / atomic / nodederef).
    /// 用于 codegen 判断"是否需要把存储视为有副作用 (volatile) /
    /// 别名优化禁用 (restrict)".
    #[inline]
    pub const fn has_non_const(self) -> bool {
        (self.0 & !Self::CONST.0) != 0
    }

    /// 把所有非标准 C 限定符 (atomic / nodederef) 剔除,
    /// 保留 ISO C 11.2.6 规定的 const / volatile / restrict 三件套.
    /// BTF/DWARF 输出时使用标准化形式.
    #[inline]
    pub const fn iso_c(self) -> Self {
        Self(self.0 & (Self::CONST.0 | Self::VOLATILE.0 | Self::RESTRICT.0))
    }
}

impl std::ops::BitOr for QualifierFlags {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for QualifierFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for QualifierFlags {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::fmt::Display for QualifierFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return Ok(());
        }
        if self.is_const() {
            write!(f, "const ")?;
        }
        if self.is_volatile() {
            write!(f, "volatile ")?;
        }
        if self.is_restrict() {
            write!(f, "restrict ")?;
        }
        if self.is_atomic() {
            write!(f, "_Atomic ")?;
        }
        if (self.0 & Self::NODEREF.0) != 0 {
            write!(f, "__nodederef__ ")?;
        }
        Ok(())
    }
}

/// QualType: Clang 风格的"带限定符类型"包装.
///
/// `base` 是原来的扁平 `Type` 枚举 (内部表示不变), `quals` 是该类型
/// 上附着的限定符 (const / volatile / restrict). 仿照 Clang `QualType`:
/// 只在 AST 节点边界 / 公共 API 上包装; 后端匹配仍对 `&qt.base` 进行,
/// 限定符在 codegen / BTF / DWARF 阶段按需读取.
///
/// 通过 `From<Type>` 隐式转换, 现有 `Type` 表达式可以零成本升级到
/// `QualType` (quals = QualifierFlags::NONE); 通过 `qt.base` 取回底层
/// `Type`. 旧的 `is_ptr()` / `is_int()` / `is_arith()` / `size()` /
/// `align()` / Display 等查询接口在 `QualType` 上转发, 业务代码无需
/// 关心当前拿到的是 `Type` 还是 `QualType`.
///
/// 优势:
///   * 限定符完整传播 — `const int *` 表现为 `Ptr(Box<QualType { base: Int, quals: const }>)`,
///     多级指针限定符自然落点.
///   * BTF 可输出限定符节点 (BTF_KIND_CONST/VOLATILE/RESTRICT), 调试信息
///     / vmlinux BTF 校验完整.
///   * 字段级 / 形参级 / 返回值级 const 正确传递, alias 分析可利用.
///
/// 设计选择 (2026-08-26):
///   * `Type` 枚举的内部 Box<Type> 在 Ptr/Array/Func 等变体中后续会
///     改为 Box<QualType>, 以便多级限定符传播; `Type` 本身的"内部结构"
///     (变体 / 字段) 不变.
///   * 不引入外部 `bitflags` 依赖, QualifierFlags 用手写 u8 wrapper 实现.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualType {
    /// 底层类型 (扁平枚举)
    pub base: Type,
    /// 限定符位标志
    pub quals: QualifierFlags,
    /// 地址空间 (x86 段寄存器: __seg_gs / __seg_fs)
    pub addr_space: AddrSpace,
}

impl QualType {
    /// 构造一个无限定符的 QualType (从 Type 直接包装)
    #[inline]
    pub fn new(base: Type) -> Self {
        Self {
            base,
            quals: QualifierFlags::NONE,
            addr_space: AddrSpace::None,
        }
    }

    /// 构造带限定符的 QualType
    #[inline]
    pub fn with_quals(base: Type, quals: QualifierFlags) -> Self {
        Self {
            base,
            quals,
            addr_space: AddrSpace::None,
        }
    }

    /// 构造带地址空间的 QualType
    #[inline]
    pub fn with_addr_space(base: Type, addr_space: AddrSpace) -> Self {
        Self {
            base,
            quals: QualifierFlags::NONE,
            addr_space,
        }
    }

    /// 设置地址空间, 返回新的 QualType
    #[inline]
    pub fn with_addr_space_set(mut self, addr_space: AddrSpace) -> Self {
        self.addr_space = addr_space;
        self
    }

    /// 替换底层类型, 保留限定符和地址空间
    #[inline]
    pub fn with_base(mut self, base: Type) -> Self {
        self.base = base;
        self
    }

    /// 替换限定符, 保留底层类型和地址空间
    #[inline]
    pub fn with_quals_set(mut self, quals: QualifierFlags) -> Self {
        self.quals = quals;
        self
    }

    /// 添加限定符 (按位或)
    #[inline]
    pub fn add_quals(mut self, q: QualifierFlags) -> Self {
        self.quals |= q;
        self
    }

    /// 是否无限定符且无地址空间
    #[inline]
    pub fn is_unqualified(&self) -> bool {
        self.quals.is_empty() && !self.addr_space.is_segment()
    }

    /// 是否带地址空间 (段寄存器限定)
    #[inline]
    pub fn has_addr_space(&self) -> bool {
        self.addr_space.is_segment()
    }

    /// 是否带 const
    #[inline]
    pub fn is_const(&self) -> bool {
        self.quals.is_const()
    }

    /// 是否带 volatile
    #[inline]
    pub fn is_volatile(&self) -> bool {
        self.quals.is_volatile()
    }

    /// 是否带 restrict
    #[inline]
    pub fn is_restrict(&self) -> bool {
        self.quals.is_restrict()
    }

    /// 底层类型是否指针 (限定符不参与)
    #[inline]
    pub fn is_ptr(&self) -> bool {
        self.base.is_ptr()
    }

    /// 底层类型是否整数 (限定符不参与)
    #[inline]
    pub fn is_int(&self) -> bool {
        self.base.is_int()
    }

    /// 底层类型是否算术 (限定符不参与)
    #[inline]
    pub fn is_arith(&self) -> bool {
        self.base.is_arith()
    }

    /// 底层类型大小 (限定符不改变存储大小)
    #[inline]
    pub fn size(&self) -> usize {
        self.base.size()
    }

    /// 底层类型对齐 (限定符不改变对齐)
    #[inline]
    pub fn align(&self) -> usize {
        self.base.align()
    }
}

impl Default for QualType {
    #[inline]
    fn default() -> Self {
        Self::new(Type::Int)
    }
}

impl From<Type> for QualType {
    #[inline]
    fn from(base: Type) -> Self {
        Self::new(base)
    }
}

impl From<&Type> for QualType {
    #[inline]
    fn from(base: &Type) -> Self {
        Self::new(base.clone())
    }
}

/// x86 段寄存器地址空间标识.
///
/// GCC 扩展 `__seg_gs` / `__seg_fs` 等限定符把指针关联到特定段寄存器.
/// Linux 内核用此实现 per-cpu 变量访问: `int __seg_gs *p` 解引用时
/// 生成 `mov (%gs:rax), eax` 而非普通 `mov (%rax), eax`.
///
/// dcc 当前仅实现 x86_64 的 GS/FS 段 (内核 per-cpu 访问所需).
/// 其他段 (CS/DS/ES/SS) 解析器接受但降级为 Generic (不生成段前缀).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AddrSpace {
    /// 默认地址空间 (无段前缀)
    #[default]
    None,
    /// x86_64 %gs 段前缀 (Linux 内核 per-cpu 基址)
    GS,
    /// x86_64 %fs 段前缀 (TLS 基址)
    FS,
    /// 其他段 (CS/DS/ES/SS) — 解析接受但不生成段前缀
    Generic,
}

impl AddrSpace {
    /// 从标识符文本解析地址空间
    pub fn from_ident(text: &str) -> Self {
        match text {
            "__seg_gs" => AddrSpace::GS,
            "__seg_fs" => AddrSpace::FS,
            "__seg_cs" | "__seg_ds" | "__seg_es" | "__seg_ss" => AddrSpace::Generic,
            _ => AddrSpace::None,
        }
    }

    /// 是否为有效段 (非 None)
    #[inline]
    pub fn is_segment(self) -> bool {
        !matches!(self, AddrSpace::None)
    }

    /// x86_64 段前缀字节 (GS=0x65, FS=0x64)
    pub fn prefix_byte(self) -> Option<u8> {
        match self {
            AddrSpace::GS => Some(0x65),
            AddrSpace::FS => Some(0x64),
            _ => None,
        }
    }
}

/// `QualType` 的 C 风格字符串化: 限定符在前, 类型名在后.
/// 例: `const int *` 在最外层 QualType 上输出为 `const int *`;
/// `int * const` 则在 Ptr(...) 节点上输出 `int * const`.
impl std::fmt::Display for QualType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 限定符放在 base 显示之前. 但对于 Ptr 变体, C 风格习惯
        // 是 `int * const` (const 紧贴 *), 所以限定符的精确位置
        // 仍依赖变体; 简单形式: 全部前缀输出.
        if !self.quals.is_empty() {
            write!(f, "{}", self.quals)?;
        }
        if self.addr_space.is_segment() {
            let seg = match self.addr_space {
                AddrSpace::GS => "__seg_gs ",
                AddrSpace::FS => "__seg_fs ",
                AddrSpace::Generic => "__seg ",
                AddrSpace::None => "",
            };
            write!(f, "{}", seg)?;
        }
        write!(f, "{}", self.base)
    }
}

#[derive(Debug, Clone)]
pub enum Type {
    Void,
    Bool,
    Char,
    SChar,
    UChar,
    Char8,  // char8_t (C23, 8-bit UTF-8)
    Char16, // char16_t (C11/C++11, 16-bit)
    Char32, // char32_t (C11/C++11, 32-bit)
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Int128,
    UInt128, // __int128 / __uint128_t
    Float,
    Double,
    LongDouble,
    /// IEEE binary128 (quadruple precision): 128 位 (113 位尾数 + 15 位指数)。
    /// `__float128` / `_Float128`。x86-64 无硬件支持, 算术/比较/转换全部
    /// 委托 libgcc 的 TFmode soft-fp (`__addtf3` / `__netf2` / `__floattitf` ...)。
    /// 值通道是 **xmm0 的 16 字节**（不是 x87 st(0)），见 `float_in_st0`。
    Float128,
    Fract,
    Accum,                    // 定点数: _Fract (1.31格式), _Accum (16.16格式)
    Complex(Box<QualType>),   // _Complex float/double: (real, imag) 两部分
    Imaginary(Box<QualType>), // _Imaginary float/double: 仅虚部, 大小同内型
    Ptr(Box<QualType>),
    Array(Box<QualType>, ArraySize),
    Vector(Box<QualType>, usize), // SIMD 向量: (元素类型, 元素个数)
    Struct(String, Arc<Vec<Field>>, usize, bool), // (name, fields, align, reverse_sso)
    Union(String, Arc<Vec<Field>>, usize, bool), // (name, fields, align, reverse_sso)
    Enum(String, Arc<Vec<(String, i64)>>, Option<Box<QualType>>), // (name, values, C23 底层类型: None=默认 int, Some(t)=底层类型, packed 编码为 Some(SChar))
    Func(Box<QualType>, Vec<QualType>),
    Named(String),
    Atomic(Box<QualType>), // _Atomic(T): 原子类型, 大小/对齐同 T
}

/// 匿名聚合（无 tag 名）只能按结构比；展开层数上限。
/// 具名聚合的环由 `assuming` 栈切断（共归纳相等），此上限只挡住无名字的嵌套。
const ANON_RECORD_CMP_DEPTH: u32 = 16;

/// 一次 `Type` 相等比较中「已经假定相等」的具名聚合对。
///
/// 具名 struct/union 的字段可以回到自身（内核里很常见：`struct irq_desc` 的
/// `handle` 字段是 `irq_flow_handler_t`，而它又是一个接 `struct irq_desc *` 的函数指针），
/// 于是类型图是一张带环的图。按结构无限递归会爆栈，按名字提前返回又会把
/// 「同名的不同作域声明 / 同一个类模板的不同实例」误判为同一类型。所以：
/// 名字相同且该对已在栈上 ⇒ 当场假定相等（共归纳），否则压栈后继续比字段。
type AssumingRecords = Vec<(u8, String, String)>;

/// 类型相等：C 的类型同一律 + 带环类型图的共归纳比较。
///
/// 为什么不能继续用派生 `PartialEq`：派生版对 `Struct(String, Arc<Vec<Field>>, ..)`
/// 按字段**深度结构**比较，而内核的常见写法是「先前向声明、后补全」。同一个 tag
/// 在补全前被复制走的那份字段表是空的，补全后那份是满的 ⇒ 两份额为不相等，于是
/// `extern typeof(sym) sym;`（`EXPORT_SYMBOL_GPL` 的展开形态）误报 E0007
/// “conflicting types for redeclaration”，整个 `kernel/irq/chip.c` 编译失败。
/// 同时，补全后的字段又能指回自身，深度比较还会无限递归。
///
/// 规则：
/// 1. 名字不同的具名聚合一定不同；
/// 2. 名字相同且任一侧字段为空 ⇒ 同一类型（前向声明的占位副本在补全后就是它）；
/// 3. 名字相同且两侧都完整 ⇒ 比字段，但同一对已在本轮比较的栈上时直接假定相等（断环）；
/// 4. 匿名聚合按字段结构比，受 `ANON_RECORD_CMP_DEPTH` 约束（超出则视为不等，保守方向）。
pub fn type_equal(a: &Type, b: &Type) -> bool {
    let mut assuming = AssumingRecords::new();
    type_eq(a, b, &mut assuming, 0)
}

fn record_kind(t: &Type) -> Option<(u8, &str, &Arc<Vec<Field>>)> {
    match t {
        Type::Struct(n, f, _, _) => Some((0, n, f)),
        Type::Union(n, f, _, _) => Some((1, n, f)),
        _ => None,
    }
}

fn type_eq(a: &Type, b: &Type, assuming: &mut AssumingRecords, depth: u32) -> bool {
    if let (Some((ka, na, fa)), Some((kb, nb, fb))) = (record_kind(a), record_kind(b)) {
        if ka != kb || na != nb {
            return false;
        }
        if !na.is_empty() && (fa.is_empty() || fb.is_empty()) {
            return true;
        }
        if Arc::ptr_eq(fa, fb) {
            return true;
        }
        if na.is_empty() && depth >= ANON_RECORD_CMP_DEPTH {
            return false;
        }
        let key = (ka, na.to_string(), nb.to_string());
        if assuming.contains(&key) {
            return true;
        }
        assuming.push(key.clone());
        let eq = fields_eq(fa, fb, assuming, depth + 1);
        assuming.pop();
        return eq;
    }
    if let (Type::Enum(n1, v1, u1), Type::Enum(n2, v2, u2)) = (a, b) {
        if n1 != n2 {
            return false;
        }
        if !n1.is_empty() && (v1.is_empty() || v2.is_empty()) {
            return true;
        }
        return v1 == v2
            && match (u1, u2) {
                (Some(x), Some(y)) => qual_eq(x, y, assuming, depth),
                (None, None) => true,
                _ => false,
            };
    }
    match (a, b) {
        (Type::Ptr(x), Type::Ptr(y))
        | (Type::Complex(x), Type::Complex(y))
        | (Type::Imaginary(x), Type::Imaginary(y))
        | (Type::Atomic(x), Type::Atomic(y)) => qual_eq(x, y, assuming, depth),
        (Type::Array(x, s1), Type::Array(y, s2)) => s1 == s2 && qual_eq(x, y, assuming, depth),
        (Type::Vector(x, n1), Type::Vector(y, n2)) => n1 == n2 && qual_eq(x, y, assuming, depth),
        (Type::Func(r1, p1), Type::Func(r2, p2)) => {
            p1.len() == p2.len()
                && qual_eq(r1, r2, assuming, depth)
                && p1
                    .iter()
                    .zip(p2.iter())
                    .all(|(x, y)| qual_eq(x, y, assuming, depth))
        },
        (Type::Named(n1), Type::Named(n2)) => n1 == n2,
        // 其余都是无载荷的叶子标量：变体不同即不同类型。
        _ => core::mem::discriminant(a) == core::mem::discriminant(b),
    }
}

fn qual_eq(a: &QualType, b: &QualType, assuming: &mut AssumingRecords, depth: u32) -> bool {
    a.quals == b.quals && a.addr_space == b.addr_space && type_eq(&a.base, &b.base, assuming, depth)
}

fn fields_eq(a: &[Field], b: &[Field], assuming: &mut AssumingRecords, depth: u32) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.name == y.name
                && x.offset == y.offset
                && x.bit_width == y.bit_width
                && x.bit_offset == y.bit_offset
                && x.quals == y.quals
                && x.ptr_quals == y.ptr_quals
                && x.field_align == y.field_align
                && x.field_packed == y.field_packed
                && type_eq(&x.ty, &y.ty, assuming, depth)
        })
}

impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        type_equal(self, other)
    }
}

impl Eq for Type {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub offset: usize,          // 字节偏移 (位域: 存储单元起始字节)
    pub bit_width: Option<u32>, // 位域宽度 (None = 普通字段)
    pub bit_offset: u32,        // 位域: 存储单元内位偏移 (非位域=0)
    /// ISO C 限定符 (const/volatile/restrict) 附着在该字段类型上.
    /// BTF/DWARF 输出时据此生成 BTF_KIND_CONST/VOLATILE/RESTRICT 节点.
    pub quals: QualifierFlags,
    /// 指针层级限定符: 每个元素对应一层指针的限定符.
    /// 例如 `int * const * volatile p` → ptr_quals = [CONST, VOLATILE].
    /// BTF 输出时据此在指针链中插入限定符节点.
    pub ptr_quals: Vec<QualifierFlags>,
    /// 字段级 __attribute__((aligned(N))) 显式指定的对齐值.
    /// None 表示使用类型的自然对齐.
    pub field_align: Option<usize>,
    /// 字段级 __attribute__((packed)) 标记.
    /// 对位域: 允许跨容器类型边界连续排列.
    /// 对非位域: 对齐降为 1 字节.
    pub field_packed: bool,
}

impl Field {
    /// 位域读/写时应加载与回写的**存储单元字节宽度**（单一真源）。
    ///
    /// `offset` 落在存储单元起点、`bit_offset` 是单元内位偏移（见字段注释），
    /// 因此容器必须覆盖到 `bit_offset + bit_width` 位，而不是字段声明类型自身
    /// 的宽度。混装声明类型时二者会不一致：
    ///   `uint64_t mant_high:47; unsigned exponent:15;`
    /// gcc 把小类型的 `exponent` 继续打包进那个 8 字节单元的 48..62 位
    /// （`offset=8, bit_offset=48`）。若仍按 `unsigned` 取 4 字节容器，读时
    /// `mov eax,[+8]` 只拿到低 32 位再 `shr 48` 得到 0，写时也只回写低 4 字节
    /// 抹掉同单元的 `mant_high` 高位 —— float128-nan.c 的 `u.nan.exponent`
    /// 正卡在这里。取「覆盖到最高位所需、且不小于声明类型、向上对齐到 1/2/4/8」
    /// 的宽度即可命中真实单元；普通位域（字段在自身类型单元内）结果不变。
    pub fn bitfield_unit_size(&self) -> usize {
        let bw = self.bit_width.unwrap_or(0);
        let end_bits = self.bit_offset + bw;
        let needed = (end_bits as usize + 7) / 8;
        needed.max(self.ty.size()).next_power_of_two().min(8)
    }
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: Ident,
    pub fields: Vec<Field>,
    pub is_union: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: Ident,
    pub variants: Vec<(Ident, Option<i64>)>,
    pub span: Span,
}

/// `QualType` 的身份串：限定符与地址空间必须入串，否则 `const int *` 与 `int *`
/// 会撞名 —— 值编号按串复用副本，撞名就是错值。
fn qual_identity(q: &QualType, out: &mut String) -> Option<()> {
    let _ = write!(out, "q{:02x}a{:?}:", q.quals.0, q.addr_space);
    q.base.identity_tag_into(out)
}

impl Type {
    pub fn aggregate_align_from_offsets(fields: &[Field]) -> usize {
        // Packed union 哨兵: 零宽度位域字段名 "\0__packed__" 表示该
        // union 使用了 __attribute__((packed)). 此时对齐为 1.
        if fields.iter().any(|f| f.name == "\0__packed__") {
            return 1;
        }
        let mut align = fields
            .iter()
            .map(|f| f.ty.align())
            .max()
            .unwrap_or(1)
            .max(1);
        for f in fields {
            let nat = f.ty.align().max(1);
            if nat > 1 && f.offset % nat != 0 {
                let observed = if f.offset == 0 {
                    1
                } else {
                    1usize << f.offset.trailing_zeros()
                };
                align = align.min(observed.max(1).min(nat));
            }
        }
        align.max(1)
    }

    /// Compute struct/union alignment from fields.
    /// - Packed (sentinel `\0__packed__`): 1
    /// - Otherwise: max of field natural alignments, capped by `pack` if any
    pub fn compute_struct_align(fields: &[Field], packed: bool, pack: Option<usize>) -> usize {
        if packed {
            return 1;
        }
        let max_field = fields
            .iter()
            .map(|f| f.ty.align())
            .max()
            .unwrap_or(1)
            .max(1);
        match pack {
            Some(n) if n < max_field => n,
            _ => max_field,
        }
    }

    /// 身份串的长度上限。匿名聚合只能按字段展开，宽度×深度会成倍增长，这里给**串本身**
    /// 定一个上限：超限就放弃（返回 `None`），绝不返回一个可能撞名的截断串。
    /// 上限按字节计，因此无论类型图多大，单次调用的成本都被这一条挡住。
    const TYPE_TAG_MAX_BYTES: usize = 4096;

    /// 类型「身份串」：给值编号、CFI 哈希这类「同类型同串、异类型异串」的场合。
    ///
    /// 为什么不用派生 `Debug`：`Struct`/`Union` 里带着 `Arc<Vec<Field>>`，派生
    /// `Debug` 会把字段树整棵展开，而内核里的 `struct page` 一类类型是一张**共享
    /// DAG**（匿名 union 套匿名 struct，成员又指回同一个 tag），把 DAG 当树展开每层
    /// 翻倍 —— 实测单个类型串要 3.4 GB，`format!` 当场分配失败，整个 TU 表现为被 OOM
    /// 杀掉。`Display`（见本文件 `impl std::fmt::Display for Type`）对匿名聚合又只输出
    /// `struct `（无名），两个不同的匿名类型会撞名 —— 所以两者都不能直接用。
    ///
    /// 本函数按 C 自己的类型同一律走：具名聚合以 **tag 名 + 尺寸 + 对齐 + 字段数**
    /// 为身份（一次命中即返回，不展开字段，成本与聚合大小无关）；匿名聚合才按字段
    /// 展开，长度超过 `TYPE_TAG_MAX_BYTES` ⇒ `None`，由调用方放弃那一次编号。
    pub fn identity_tag(&self) -> Option<String> {
        let mut out = String::new();
        self.identity_tag_into(&mut out)?;
        Some(out)
    }

    fn identity_tag_into(&self, out: &mut String) -> Option<()> {
        if out.len() > Self::TYPE_TAG_MAX_BYTES {
            return None;
        }
        match self {
            Type::Struct(name, fields, align, rev_sso)
            | Type::Union(name, fields, align, rev_sso) => {
                let is_struct = matches!(self, Type::Struct(..));
                if !name.is_empty() {
                    // 名义同一律：具名聚合以名字 + 布局摘要为身份，不展开字段。
                    out.push_str(if is_struct { "struct " } else { "union " });
                    out.push_str(name);
                    let _ = write!(
                        out,
                        "#sz{}#al{}#nf{}#r{}",
                        self.size(),
                        align,
                        fields.len(),
                        rev_sso
                    );
                    return Some(());
                }
                // 匿名聚合：名字为空，只能按字段（名/偏移/位域/限定符/类型）展开。
                out.push_str(if is_struct { "anon struct{" } else { "anon union{" });
                for f in fields.iter() {
                    let _ = write!(out, "{}@{}", f.name, f.offset);
                    if let Some(w) = f.bit_width {
                        let _ = write!(out, ":b{}/{}", f.bit_offset, w);
                    }
                    let _ = write!(out, "q{:02x}fa{}:", f.quals.0, f.field_align.unwrap_or(0));
                    f.ty.identity_tag_into(out)?;
                    out.push(';');
                }
                out.push('}');
                Some(())
            },
            Type::Enum(name, _, under) => {
                let _ = write!(out, "enum {}", name);
                if let Some(t) = under {
                    out.push('<');
                    qual_identity(t, out)?;
                    out.push('>');
                }
                Some(())
            },
            Type::Complex(q) | Type::Imaginary(q) | Type::Ptr(q) | Type::Atomic(q) => {
                out.push_str(match self {
                    Type::Complex(_) => "complex ",
                    Type::Imaginary(_) => "imaginary ",
                    Type::Ptr(_) => "ptr ",
                    _ => "atomic ",
                });
                qual_identity(q, out)
            },
            Type::Array(elem, sz) => {
                match sz {
                    ArraySize::Const(n) => {
                        let _ = write!(out, "array {}<", n);
                    },
                    ArraySize::Var(_) => out.push_str("vla <"),
                }
                qual_identity(elem, out)?;
                out.push('>');
                Some(())
            },
            Type::Vector(elem, n) => {
                let _ = write!(out, "vector {}<", n);
                qual_identity(elem, out)?;
                out.push('>');
                Some(())
            },
            Type::Func(ret, params) => {
                out.push_str("func ");
                qual_identity(ret, out)?;
                out.push('(');
                for p in params.iter() {
                    qual_identity(p, out)?;
                    out.push(',');
                }
                out.push(')');
                Some(())
            },
            Type::Named(name) => {
                let _ = write!(out, "named {}", name);
                Some(())
            },
            // 叶子标量：变体名就是身份，`Debug` 在此是 O(1)。
            scalar => {
                let _ = write!(out, "{:?}", scalar);
                Some(())
            },
        }
    }

    pub fn size(&self) -> usize {
        match self {
            Type::Void => 0,
            Type::Bool | Type::Char | Type::SChar | Type::UChar | Type::Char8 => 1,
            Type::Char16 | Type::Short | Type::UShort => 2,
            Type::Char32 | Type::Int | Type::UInt | Type::Float | Type::Fract | Type::Accum => 4,
            Type::Enum(_, _, under) => match under {
                // C23 底层类型: enum E : unsigned char 用底层类型大小;
                // 未指定时默认 int (4 字节); packed 编码为 Some(SChar) (1 字节)
                Some(t) => t.size(),
                None => 4,
            },
            Type::Long
            | Type::ULong
            | Type::Ptr(_)
            | Type::LongLong
            | Type::ULongLong
            | Type::Double => 8,
            Type::Int128 | Type::UInt128 => 16,
            Type::Array(elem, ArraySize::Const(n)) => {
                let elem_sz = elem.size();
                // 防止超大数组大小导致内存分配失败 (如 [100000000000000001][3])
                elem_sz.saturating_mul(*n)
            },
            Type::Array(_, ArraySize::Var(_)) => 8, // VLA 退化为指针
            Type::Vector(elem, n) => elem.size() * n,
            Type::LongDouble | Type::Float128 => 16,
            Type::Complex(inner) => inner.size() * 2,
            Type::Imaginary(inner) => inner.size(),
            Type::Atomic(inner) => inner.size(),
            Type::Struct(_, fields, align, _) => {
                // 计算结构体大小: 考虑所有字段 (包括零宽度位域的位置)
                // 零宽度位域不贡献存储, 但其位置决定结构体最小大小
                let size = fields
                    .iter()
                    .map(|f| {
                        if f.bit_width == Some(0) {
                            // 零宽度位域: 其偏移量决定结构体最小大小
                            f.offset
                        } else if let Some(bw) = f.bit_width {
                            // 位域字段: 结束位置 = offset + ceil((bit_offset + bw) / 8)
                            f.offset + (f.bit_offset + bw).div_ceil(8) as usize
                        } else {
                            f.offset + f.ty.size()
                        }
                    })
                    .max()
                    .unwrap_or(0);

                if *align > 1 {
                    (size + align - 1) & !(align - 1)
                } else {
                    size
                }
            },
            Type::Union(_, fields, align, _) => {
                let size = fields.iter().map(|f| f.ty.size()).max().unwrap_or(0);
                if *align > 1 {
                    (size + align - 1) & !(align - 1)
                } else {
                    size
                }
            },
            _ => 8,
        }
    }

    /// Get the element type of an array type.
    /// Returns None if the type is not an array.
    pub fn get_array_element_type(&self) -> Option<&Type> {
        match self {
            Type::Array(elem, _) => Some(&elem.base),
            _ => None,
        }
    }

    pub fn align(&self) -> usize {
        match self {
            Type::Void => 1,
            Type::Bool | Type::Char | Type::SChar | Type::UChar | Type::Char8 => 1,
            Type::Char16 | Type::Short | Type::UShort => 2,
            Type::Char32 | Type::Int | Type::UInt | Type::Float | Type::Fract | Type::Accum => 4,
            Type::Enum(_, _, under) => match under {
                // C23 底层类型: enum E : unsigned char 用底层类型大小;
                // 未指定时默认 int (4 字节); packed 编码为 Some(SChar) (1 字节)
                Some(t) => t.size(),
                None => 4,
            },
            Type::Long
            | Type::ULong
            | Type::Ptr(_)
            | Type::LongLong
            | Type::ULongLong
            | Type::Double => 8,
            Type::Int128 | Type::UInt128 => 16,
            Type::Array(elem, ArraySize::Const(_)) => elem.align(),
            Type::Array(elem, ArraySize::Var(_)) => elem.align(),
            // SIMD 向量的自然对齐 = 其总字节数 (GCC: V16QI=128b, V8DF=256b,
            // V16SF=512b)。旧实现硬编码 16 会让 32/64 字节向量在 ABI 分类里
            // 逃过 "对齐 > 16 字节 ⇒ 整体走内存" 规则 (SysV ABI 3.2.3 与 GCC
            // `classify_argument` 的 `if (align > 128) return CLASS_MEMORY`)。
            Type::Vector(elem, n) => (elem.size() * n).max(1),
            Type::LongDouble | Type::Float128 => 16,
            Type::Complex(inner) => inner.align(),
            Type::Imaginary(inner) => inner.align(),
            Type::Atomic(inner) => inner.align(),
            Type::Struct(_, _, align, _) => *align,
            Type::Union(_, _, align, _) => *align,
            _ => 8,
        }
    }

    /// 类型是否含 VLA 维度 (任意层数组出现 ArraySize::Var)。
    /// 外层常量但内层可变的多维数组 (int a[2][N] →
    /// Array(Array(Int, Var(N)), Const(2))) 也算 VLA: 总大小是
    /// 运行时值, sizeof 必须运行时求值 (C11 6.5.3.4p2), 静态
    /// size() 的 Var 维度 fallback (8 字节) 会得出错误结果。
    /// 结构体/联合体含 VLA 字段 (variable-sized struct, GCC 扩展)
    /// 同理: 整体大小是运行时值。
    pub fn has_vla(&self) -> bool {
        match self {
            Type::Array(_, ArraySize::Var(_)) => true,
            Type::Array(elem, ArraySize::Const(_)) => elem.base.has_vla(),
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                fields.iter().any(|f| f.ty.has_vla())
            },
            _ => false,
        }
    }

    pub fn is_ptr(&self) -> bool {
        matches!(self, Type::Ptr(_))
    }
    pub fn is_int(&self) -> bool {
        matches!(
            self,
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
        )
    }
    pub fn is_arith(&self) -> bool {
        self.is_int()
            || matches!(
                self,
                Type::Float
                    | Type::Double
                    | Type::LongDouble
                    | Type::Float128
                    | Type::Fract
                    | Type::Accum
                    | Type::Complex(_)
                    | Type::Imaginary(_)
            )
    }

    /// 是否为**实数浮点**标量类型（不含复数/虚数）。
    ///
    /// binary128（`__float128` / `_Float128`）与 float/double/long double 同属
    /// 实数浮点：常量求值必须走 `eval_full` 取**位模式**，走 `eval`（截断成
    /// i128）会把 `4.5` 变成 `4`，全局/静态初始化就会写错值。
    ///
    /// 复数/虚数另走 `Type::Complex` / `Type::Imaginary` 的分量路径，不在此列。
    pub fn is_real_float(&self) -> bool {
        matches!(self, Type::Float | Type::Double | Type::LongDouble | Type::Float128)
    }

    /// 整数提升 (C11 6.3.1.1p2): 比 `int` 秩低的整数类型一律转为 `int`
    /// (或 `unsigned int`, 当 `int` 无法表示其全部值时)。
    ///
    /// 非整数类型原样返回, 便于在混合表达式里直接套用。
    pub fn int_promotion(&self) -> Type {
        match self {
            Type::Bool
            | Type::Char
            | Type::SChar
            | Type::UChar
            | Type::Char8
            | Type::Short
            | Type::UShort => Type::Int,
            other => other.clone(),
        }
    }

    /// C11 6.3.1.8「通常算术转换」的整数部分: 两个操作数的公共类型。
    ///
    /// 全项目**单一真源** —— C 前端 (`parser.rs`)、SSA→AST 重建
    /// (`ssa::expr_rebuild::infer_binop_type`) 与 codegen 共用本函数, 避免任一
    /// 路径自己拍一个「结果都是 int」的近似口径 —— 那会把 `long * int`
    /// 降格成 32 位运算, 后续 `long` 赋值变成 sext(i32), 高 32 位静默丢失。
    ///
    /// 秩表按 ISO C 由低到高; 浮点/复数/指针不在本函数职责内 (调用方需
    /// 先判别, 非整数类型只会在同秩时命中「返回自身」这条保守路径)。
    pub fn usual_arith_type(&self, other: &Type) -> Type {
        use Type::*;
        let a = self.int_promotion();
        let b = other.int_promotion();
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
        let a = &a;
        let b = &b;
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
        // C11 6.3.1.8p1 末两条: 无符号侧秩不低于有符号侧 ⇒ 有符号侧升到无符号侧类型
        // (上面 `is_uns(higher)` 与同秩分支已覆盖); 有符号侧秩**严格更高**时, 只有当它
        // 能表示无符号侧的**全部值**才停在有符号类型, 否则两侧一起升到「有符号侧对应的
        // 无符号类型」。
        //
        // 旧写法只看秩 (`rank(higher) > rank(lower)`), 把「秩更高」当成了「更宽」—— LP64
        // 上 `long long`(秩 5) 与 `unsigned long`(秩 4) 同为 64 位, `long long` 表示不了
        // `unsigned long` 的 [2^63, 2^64) 那些值, 结果类型却留在有符号侧 ⇒ 比较编成有符号
        // 条件码, `if ((unsigned long)x > 0xffffffffUL)` 对 x<0 静默判假 (实测 -O0/-O2 都错,
        // gcc 给 32 dcc 给 0)。秩相同那一档本来就走「升到对应无符号类型」, 与本判据一致。
        //
        // 可表示性用宽度比较即足够: 标准整型里秩严格更高者的宽度只会 ≥ 秩低者, 故
        // `size()` 严格更大 ⇔ 「能表示无符号侧全部值」; `__int128`(16) vs `unsigned long
        // long`(8) 这类跨档因此仍停在有符号侧, 与 gcc 一致。
        if rank(higher) > rank(lower) && higher.size() > lower.size() {
            return higher.clone();
        }
        // 同秩时: 有符号无法表示无符号的全部值, 转为对应无符号类型
        match higher {
            Int => UInt,
            Long => ULong,
            LongLong => ULongLong,
            Int128 => UInt128,
            _ => higher.clone(),
        }
    }

    /// 复数 / 虚数算术的**实型**公共类型 (C11 6.3.1.8 + G.6.4.4)。
    ///
    /// `_Complex` / `_Imaginary` 运算的结果 shape 由它的**实型**决定, 而实型
    /// 就是两侧实型参加通常算术转换的结果 —— 浮点秩高于任何整数秩, 故只要
    /// 一侧是浮点, 公共实型必为两者中较宽的浮点族类型; 两侧皆整数时退回
    /// 整数【usual_arith_type】这条单一真源。本函数是前端 `_Complex` /
    /// `_Imaginary` 推导与标量浮点臂共用的唯一口径。
    ///
    /// 旧写法在复数/虚数推导里按 `size()` 比较内型, 而 `int` 与 `float` 同为
    /// 4 字节 —— `4 * _Complex_I` 的实型被判成 `int`, 于是浮点分量的**位模式**
    /// 当成整数值 (`zf = 3 + 4*_Complex_I` 虚部读出 0x40800000 = 1082130432)。
    pub fn common_real_type(&self, other: &Type) -> Type {
        let frank = |t: &Type| -> i32 {
            match t {
                Type::LongDouble => 3,
                // binary128 的秩高于 x87 80-bit 扩展精度。
                Type::Float128 => 4,
                Type::Double => 2,
                Type::Float => 1,
                _ => 0,
            }
        };
        if frank(self) > 0 || frank(other) > 0 {
            // 至少一侧是浮点: 整数侧的值表示必须先进入浮点域, 不得当候选实型
            return if frank(self) >= frank(other) {
                self.clone()
            } else {
                other.clone()
            };
        }
        self.usual_arith_type(other)
    }

    pub fn layout_struct(fields: &mut [Field], is_union: bool) {
        Self::layout_struct_with_pack(fields, is_union, None);
    }

    /// 按对齐限制布局: max_align = None 用自然对齐, Some(n) 把字段对齐限制为 n
    /// (实现 #pragma pack(n): 把结构体内部所有字段对齐不超过 n 字节)
    pub fn layout_struct_with_pack(fields: &mut [Field], is_union: bool, max_align: Option<usize>) {
        // 实际应用的对齐: 考虑字段级 aligned 属性, 然后应用 pack 限制
        // field_align > natural alignment, 然后 min(max_align, alignment)
        let eff = |f: &Field| -> usize {
            let nat = match f.field_align {
                Some(a) => a, // 字段级 aligned 覆盖自然对齐
                None => f.ty.align(),
            };
            let nat = nat.max(1); // 防止对齐为 0 导致后续 underflow
            match max_align {
                Some(n) if n < nat => n,
                _ => nat,
            }
        };
        if is_union {
            for f in fields.iter_mut() {
                f.offset = 0;
            }
        } else {
            // SysV AMD64 位域 "存储单元 (storage unit)" 模型 (与 gcc 一致):
            //
            // 单元大小 = 位域声明类型的大小 (int -> 4 字节)。单元起点必须对齐到
            // 该类型的对齐, 但**不**要求紧接在前一个普通字段之后另开对齐单元 —
            // 只要字段从结构体起点算起能落在某个自然对齐的单元内, 就允许从当前
            // 字节开始打包。例如 `char c; int bf:1;` 中 bf 放在字节 1 位 0 (与 c
            // 后面的尾字节共享 0..4 这个 int 单元), 而不是对齐到字节 4。
            //
            // 游标:
            //   next_bit        下一个空闲位 (自结构体起点, 普通字段后按字节取整)
            //   unit_start_bit  当前打开单元的起始位; u128::MAX 表示无打开单元
            //   unit_bits       当前单元总位数; unit_used 单元内已占用位数
            //
            // 位游标用 u128 而不是 u64: 普通字段的 next_bit = (byte + size) * 8,
            // 含超大数组字段的结构体 (991014-1: `short buf[2^62-256]`) 在 u64
            // 下必然溢出 —— 旧写法静默回绕, 把后续字段排进数组内部 (offset/size
            // 全错), saturating_mul 也只是把回绕换成"塌到 u64::MAX"另一种错法。
            // u128 下 byte+size < 2^64, *8 < 2^67, 不溢出。
            let mut next_bit: u128 = 0;
            let mut unit_start_bit: u128 = u128::MAX;
            let mut unit_bits: u32 = 0;
            let mut unit_used: u32 = 0;

            // 结束当前打开的位域单元: 把游标推进到单元末尾。
            let close_unit = |next_bit: &mut u128,
                              unit_start_bit: &mut u128,
                              unit_used: &mut u32,
                              unit_bits: &mut u32| {
                if *unit_start_bit != u128::MAX {
                    let end = *unit_start_bit + u128::from(*unit_bits);
                    if end > *next_bit {
                        *next_bit = end;
                    }
                    *unit_start_bit = u128::MAX;
                    *unit_used = 0;
                    *unit_bits = 0;
                }
            };

            for f in fields.iter_mut() {
                if let Some(bw) = f.bit_width {
                    let field_bits = bw;
                    let want_bits = (f.ty.size() * 8) as u32;

                    // 零宽度位域: 结束当前单元, 再对齐到声明类型边界
                    if field_bits == 0 {
                        close_unit(
                            &mut next_bit,
                            &mut unit_start_bit,
                            &mut unit_used,
                            &mut unit_bits,
                        );
                        let a = eff(f) as u128;
                        let aligned = ((next_bit + a * 8 - 1) / (a * 8)) * (a * 8);
                        next_bit = aligned;
                        f.offset = (next_bit / 8) as usize;
                        f.bit_offset = 0;
                        continue;
                    }

                    let start_bit: u128; // 字段位偏移 (自结构体起点)

                    if unit_start_bit != u128::MAX {
                        // 已有打开单元
                        let same_fits =
                            want_bits == unit_bits && unit_used + field_bits <= unit_bits;
                        // gcc: 更小类型的位域可与当前大单元共享剩余空间;
                        // 同类型仅在剩余位足够时复用 (如 int:31; int:2 必须另起单元)。
                        let smaller_fits =
                            want_bits < unit_bits && unit_used + field_bits <= unit_bits;
                        let packed_fits =
                            f.field_packed && unit_used + field_bits <= want_bits.max(unit_bits);
                        if same_fits || smaller_fits || packed_fits {
                            // 复用当前单元剩余位
                            start_bit = unit_start_bit + u128::from(unit_used);
                            if packed_fits && want_bits > unit_bits {
                                unit_bits = want_bits;
                            }
                        } else {
                            // 放不下或容器更大: 关闭当前单元, 另开新单元
                            close_unit(
                                &mut next_bit,
                                &mut unit_start_bit,
                                &mut unit_used,
                                &mut unit_bits,
                            );
                            let unit_bytes = u128::from(want_bits / 8);
                            // 新单元起点对齐到声明类型
                            let aligned =
                                ((next_bit / 8 + unit_bytes - 1) / unit_bytes) * unit_bytes;
                            unit_start_bit = aligned * 8;
                            start_bit = unit_start_bit;
                            unit_bits = want_bits;
                            unit_used = 0;
                        }
                    } else {
                        // 无打开单元: 候选起点为当前字节 (向下取整到字节),
                        // 再向下落到包含该位的、自然对齐的存储单元起点。
                        let cur_byte = next_bit / 8;
                        let unit_bytes = u128::from(want_bits / 8);
                        let start_byte = cur_byte - (cur_byte % unit_bytes);
                        let bit_in_unit = ((cur_byte - start_byte) * 8) as u32;
                        if bit_in_unit + field_bits <= want_bits {
                            unit_start_bit = start_byte * 8;
                            start_bit = next_bit;
                            unit_bits = want_bits;
                            unit_used = bit_in_unit;
                        } else {
                            // 该单元剩余位不足: 推进到下一对齐单元
                            let next_start_byte =
                                ((cur_byte + unit_bytes - 1) / unit_bytes) * unit_bytes;
                            unit_start_bit = next_start_byte * 8;
                            start_bit = unit_start_bit;
                            unit_bits = want_bits;
                            unit_used = 0;
                        }
                    }

                    // (f.offset, f.bit_offset) = (容器字节偏移, 容器内位偏移):
                    // 各后端把这一对当作「容器地址 + 容器内移位」用, 因此容器
                    // 必须落在**单元起点** (按声明类型大小对齐), 不能落在字段
                    // 自己的字节偏移上。`long long pad:12; long long field:52`
                    // 的 field 位偏移是 12: 旧布局给出 offset=1/bit_offset=4,
                    // 后端于是从 base+1 起读 8 字节 —— 既越过 8 字节结构体末尾
                    // (踩坏相邻字段/结构体, bf64-1/bitfld-5 abort), 又让两个位域
                    // 看起来分居不同容器 (静态/运行期打包互相覆盖, pr98366)。
                    f.offset = (unit_start_bit / 8) as usize;
                    f.bit_offset = (start_bit - unit_start_bit) as u32;
                    // 更新单元占用与全局游标
                    let field_end = start_bit + u128::from(field_bits);
                    let in_unit_end = (field_end - unit_start_bit) as u32;
                    if in_unit_end > unit_used {
                        unit_used = in_unit_end;
                    }
                    if field_end > next_bit {
                        next_bit = field_end;
                    }
                } else {
                    // 非位域字段: 若前有未关闭的位域, gcc 只把已占用位向上取整到
                    // 字节边界, 剩余字节仍可放本普通字段 (如 `char x; int:1..:5;
                    // short s;` 的 s 落在字节 6, 而不是单元末尾字节 8)。
                    if unit_start_bit != u128::MAX {
                        let used_bytes = (unit_used + 7) / 8;
                        let flush_bit = (unit_start_bit + u128::from(used_bytes * 8)).max(next_bit);
                        next_bit = (flush_bit + 7) & !7;
                        unit_start_bit = u128::MAX;
                        unit_used = 0;
                        unit_bits = 0;
                    }
                    let mut byte = (next_bit / 8) as usize;
                    // 柔性数组 (零长数组): 不贡献大小
                    if let Type::Array(_, ArraySize::Const(0)) = &f.ty {
                        let align = eff(f);
                        byte = (byte + align - 1) & !(align - 1);
                        f.offset = byte;
                        next_bit = (byte as u128) * 8;
                        continue;
                    }
                    let a = eff(f);
                    byte = (byte + a - 1) & !(a - 1);
                    f.offset = byte;
                    next_bit = (byte as u128 + f.ty.size() as u128) * 8;
                }
            }
        }
    }

    /// packed 结构体布局: 所有字段对齐为 1, 无填充
    pub fn layout_struct_packed(fields: &mut [Field], is_union: bool) {
        if is_union {
            for f in fields.iter_mut() {
                f.offset = 0;
            }
        } else {
            // packed 模式: 位域连续排列, 不强制容器类型切换时对齐.
            // 使用全局位位置 (global_bit_pos) 跟踪下一个可用位.
            let mut global_bit_pos: u32 = 0;
            for f in fields.iter_mut() {
                if let Some(bw) = f.bit_width {
                    if bw == 0 {
                        // 零宽度位域: 对齐到容器类型的自然对齐边界
                        // (packed 模式下, 容器的对齐为 1, 但零宽度位域仍需要
                        // 对齐到其类型的自然边界, 如 long long 对齐到 8 字节)
                        let container_align = f.ty.align();
                        let container_bits = (container_align * 8) as u32;
                        let align_bits = container_bits;
                        if !global_bit_pos.is_multiple_of(align_bits) {
                            global_bit_pos = (global_bit_pos + align_bits - 1) & !(align_bits - 1);
                        }
                        f.offset = (global_bit_pos / 8) as usize;
                        f.bit_offset = 0;
                        continue;
                    }
                    f.offset = (global_bit_pos / 8) as usize;
                    f.bit_offset = global_bit_pos % 8;
                    global_bit_pos += bw;
                } else {
                    // 非位域字段: 对齐到下一个字节边界
                    if !global_bit_pos.is_multiple_of(8) {
                        global_bit_pos = (global_bit_pos + 7) & !7;
                    }
                    f.offset = (global_bit_pos / 8) as usize;
                    global_bit_pos += (f.ty.size() * 8) as u32;
                }
            }
        }
    }
}

pub fn int_t() -> Type {
    Type::Int
}

/// Compact one-line summary of a Type for debug prints (avoids noisy {:?} dumps).
pub fn type_kind_summary(ty: &Type) -> String {
    match ty {
        Type::Void => "void".into(),
        Type::Bool => "bool".into(),
        Type::Char => "char".into(),
        Type::SChar => "schar".into(),
        Type::UChar => "uchar".into(),
        Type::Char8 => "char8".into(),
        Type::Char16 => "char16".into(),
        Type::Char32 => "char32".into(),
        Type::Short => "short".into(),
        Type::UShort => "ushort".into(),
        Type::Int => "int".into(),
        Type::UInt => "uint".into(),
        Type::Long => "long".into(),
        Type::ULong => "ulong".into(),
        Type::LongLong => "longlong".into(),
        Type::ULongLong => "ulonglong".into(),
        Type::Int128 => "int128".into(),
        Type::UInt128 => "uint128".into(),
        Type::Float => "float".into(),
        Type::Double => "double".into(),
        Type::LongDouble => "longdouble".into(),
        Type::Float128 => "float128".into(),
        Type::Fract => "fract".into(),
        Type::Accum => "accum".into(),
        Type::Complex(inner) => format!("complex({})", type_kind_summary(&inner.base)),
        Type::Imaginary(inner) => format!("imag({})", type_kind_summary(&inner.base)),
        Type::Ptr(inner) => format!("*({})", type_kind_summary(&inner.base)),
        Type::Array(inner, n) => format!("[{:?}]({})", n, type_kind_summary(&inner.base)),
        Type::Vector(inner, n) => format!("vec{}({})", n, type_kind_summary(&inner.base)),
        Type::Func(ret, params) => {
            let params: Vec<String> = params.iter().map(|p| type_kind_summary(&p.base)).collect();
            format!("Func({})({})", type_kind_summary(&ret.base), params.join(","))
        },
        Type::Struct(name, _, _, _) => format!("struct {}", name),
        Type::Union(name, _, _, _) => format!("union {}", name),
        Type::Enum(name, _, _) => format!("enum {}", name),
        Type::Named(name) => format!("named {}", name),
        Type::Atomic(inner) => format!("atomic({})", type_kind_summary(&inner.base)),
    }
}
pub fn long_t() -> Type {
    Type::Long
}
pub fn void_t() -> Type {
    Type::Void
}
pub fn char_t() -> Type {
    Type::Char
}

/// 简洁的 C 风格类型名 (用于诊断与字符串化)
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Void => write!(f, "void"),
            Type::Bool => write!(f, "_Bool"),
            Type::Char => write!(f, "char"),
            Type::SChar => write!(f, "signed char"),
            Type::UChar => write!(f, "unsigned char"),
            Type::Char8 => write!(f, "char8_t"),
            Type::Char16 => write!(f, "char16_t"),
            Type::Char32 => write!(f, "char32_t"),
            Type::Short => write!(f, "short"),
            Type::UShort => write!(f, "unsigned short"),
            Type::Int => write!(f, "int"),
            Type::UInt => write!(f, "unsigned int"),
            Type::Long => write!(f, "long"),
            Type::ULong => write!(f, "unsigned long"),
            Type::LongLong => write!(f, "long long"),
            Type::ULongLong => write!(f, "unsigned long long"),
            Type::Int128 => write!(f, "__int128"),
            Type::UInt128 => write!(f, "__uint128_t"),
            Type::Float => write!(f, "float"),
            Type::Double => write!(f, "double"),
            Type::LongDouble => write!(f, "long double"),
            Type::Float128 => write!(f, "__float128"),
            Type::Fract => write!(f, "_Fract"),
            Type::Accum => write!(f, "_Accum"),
            Type::Complex(inner) => write!(f, "_Complex {}", inner),
            Type::Imaginary(inner) => write!(f, "_Imaginary {}", inner),
            Type::Ptr(inner) => write!(f, "{}*", inner),
            Type::Array(inner, sz) => match sz {
                ArraySize::Const(n) => write!(f, "{}[{}]", inner, n),
                ArraySize::Var(_) => write!(f, "{}[]", inner),
            },
            Type::Vector(inner, n) => {
                write!(f, "__attribute__((vector_size({}))){}", n * inner.size() * 8, inner)
            },
            Type::Struct(name, _, _, _) => write!(f, "struct {}", name),
            Type::Union(name, _, _, _) => write!(f, "union {}", name),
            Type::Enum(name, _, _) => write!(f, "enum {}", name),
            Type::Func(ret, params) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ") -> {}", ret)
            },
            Type::Named(name) => write!(f, "{}", name),
            Type::Atomic(inner) => write!(f, "_Atomic({})", inner),
        }
    }
}

/// 在字段列表中递归查找字段 (支持匿名 struct/union 透明穿透)
/// 返回 (找到的 Field, 累计偏移)
pub fn find_field<'a>(fields: &'a [Field], name: &str) -> Option<(&'a Field, usize)> {
    // 1. 直接查找
    if let Some(f) = fields.iter().find(|f| f.name == name) {
        return Some((f, 0));
    }
    // 2. 递归查找匿名 struct/union 字段
    for f in fields.iter() {
        if f.name.is_empty() {
            if let Type::Struct(_, inner, _, _) | Type::Union(_, inner, _, _) = &f.ty {
                if let Some((found, inner_off)) = find_field(inner, name) {
                    return Some((found, f.offset + inner_off));
                }
            }
        }
    }
    None
}

/// 是否为零长度数组 (Flexible Array Member 形式 `T a[]` 或 `T a[0]`).
/// ISO C99 6.7.2.1 规定这种成员必须出现在结构体末尾且结构体必须至少含
/// 一个具名字段 (`more than one named member`). 实际验证在 parser.rs 完成.
pub fn is_zero_sized_array(ty: &Type) -> bool {
    matches!(ty, Type::Array(_, ArraySize::Const(0)))
}

/// 统计字段列表中具名字段数 (非匿名). 用于验证 FAM 约束.
pub fn count_named_fields(fields: &[Field]) -> usize {
    fields.iter().filter(|f| !f.name.is_empty()).count()
}

impl Expr {
    pub fn ty(&self) -> Type {
        match self {
            Expr::Int { ty, .. }
            | Expr::Float { ty, .. }
            | Expr::ComplexLit { ty, .. }
            | Expr::ComplexIntLit { ty, .. }
            | Expr::Real { ty, .. }
            | Expr::Imag { ty, .. }
            | Expr::String { ty, .. }
            | Expr::Char { ty, .. }
            | Expr::Ident { ty, .. }
            | Expr::Binop { ty, .. }
            | Expr::Unary { ty, .. }
            | Expr::Call { ty, .. }
            | Expr::Assign { ty, .. }
            | Expr::Cond { ty, .. }
            | Expr::Index { ty, .. }
            | Expr::Member { ty, .. }
            | Expr::AddrOf { ty, .. }
            | Expr::Deref { ty, .. }
            | Expr::PostInc { ty, .. }
            | Expr::PostDec { ty, .. }
            | Expr::PreInc { ty, .. }
            | Expr::PreDec { ty, .. }
            | Expr::Comma { ty, .. }
            | Expr::LabelAddr { ty, .. }
            | Expr::CompoundLit { ty, .. } => ty.clone(),
            Expr::AsmExpr { ty, .. } => ty.clone(),
            Expr::VaArg { ty, .. } => ty.clone(),
            Expr::VaArgPack { ty, .. } => ty.clone(),
            Expr::Cast { ty, .. } => ty.clone(),
            Expr::InitList { ty, .. } => ty.clone(),
            Expr::SizeofTy { .. }
            | Expr::SizeofExpr { .. }
            | Expr::AlignofTy { .. }
            | Expr::AlignofExpr { .. }
            | Expr::Offsetof { .. } => Type::Int,
            Expr::StmtExpr { ty, .. } => ty.clone(),
        }
    }
}

/// Returns true if the type or any nested QualType carries volatile qualifier.
/// Used by optimization passes to protect volatile accesses from elimination.
pub fn type_has_volatile(ty: &Type) -> bool {
    match ty {
        Type::Ptr(inner)
        | Type::Array(inner, _)
        | Type::Atomic(inner)
        | Type::Complex(inner)
        | Type::Imaginary(inner)
        | Type::Vector(inner, _) => inner.quals.is_volatile() || type_has_volatile(&inner.base),
        Type::Func(ret, params) => {
            ret.quals.is_volatile()
                || type_has_volatile(&ret.base)
                || params
                    .iter()
                    .any(|p| p.quals.is_volatile() || type_has_volatile(&p.base))
        },
        Type::Enum(_, _, Some(inner)) => {
            inner.quals.is_volatile() || type_has_volatile(&inner.base)
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::Type;

    /// C11 6.3.1.8「通常算术转换」的整数部分矩阵。
    ///
    /// 期望值**不是手推的**：每条都来自本机 gcc 13 (x86_64 LP64) 的
    /// `_Generic((__typeof__(a OP b)){0}, …)` 实测（把 a、b 声明成各自的类型后取
    /// `a + b` 的类型），故这张表同时是「本函数与 gcc 口径一致」的证据，而不只是
    /// 「本函数自洽」。
    ///
    /// 为什么必须有这张表：[`Type::usual_arith_type`] 曾被写成「有符号侧秩严格更高就
    /// 停在有符号类型」，把「秩更高」当成了「更宽」。LP64 上 `long long`(秩 5) 与
    /// `unsigned long`(秩 4) 同宽，`long long` 表示不了 `unsigned long` 的高半轴 ⇒
    /// 公共类型必须升到 `unsigned long long`；留在有符号侧意味着无符号比较拿到有符号
    /// 条件码（`if ((unsigned long)x > 0xffffffffUL)` 对负数静默判假，实测 gcc 给 32、
    /// dcc 给 0）。判据是「秩严格更高 **且** 更宽」，两个条件缺一不可 —— 下面既有
    /// 「同宽必须升无符号」的样本，也有「更宽才允许停在有符号」的样本（`long` ×
    /// `unsigned int`、`__int128` × `unsigned long long`），所以把判据改成「只看看宽度」
    /// 或「混合符号一律无符号」都会红。
    #[test]
    fn usual_arith_type_matches_c11_conversion_rank_and_range() {
        use Type::*;
        let cases: &[(&Type, &Type, &Type)] = &[
            // —— 同宽混合符号: 有符号侧表示不了无符号侧全部值 ⇒ 升到对应无符号类型 ——
            (&ULong, &LongLong, &ULongLong),
            (&LongLong, &ULong, &ULongLong),
            (&LongLong, &ULongLong, &ULongLong),
            (&ULongLong, &LongLong, &ULongLong),
            (&Long, &ULong, &ULong),
            (&ULong, &Long, &ULong),
            (&Int, &UInt, &UInt),
            (&UInt, &Int, &UInt),
            // —— 有符号侧确实更宽: 能表示无符号侧全部值 ⇒ 停在有符号类型 ——
            (&Long, &UInt, &Long),
            (&UInt, &Long, &Long),
            (&LongLong, &UInt, &LongLong),
            (&Int128, &ULongLong, &Int128),
            (&Int128, &Int, &Int128),
            // —— 无符号侧秩不低于对方 ⇒ 无符号胜 ——
            (&UInt128, &Int128, &UInt128),
            (&Int128, &UInt128, &UInt128),
            // —— 两侧同符号 ⇒ 取秩高者 (与「宽度」无关, 不得被上面的分支误伤) ——
            (&Long, &Int, &Long),
            (&ULongLong, &ULong, &ULongLong),
            // —— 整数提升先行 (6.3.1.1p2): 比 int 秩低的都先变成 int ——
            (&Short, &UShort, &Int),
            (&UShort, &Int, &Int),
            (&UShort, &Long, &Long),
            (&SChar, &UChar, &Int),
        ];
        for (a, b, want) in cases {
            let got = a.usual_arith_type(b);
            assert!(
                std::mem::discriminant(&got) == std::mem::discriminant(want),
                "usual_arith_type({:?}, {:?}) = {:?}, gcc 实测为 {:?}",
                a,
                b,
                got,
                want
            );
            // 转换是对称运算: 交换实参不得改变结果。
            let flipped = b.usual_arith_type(a);
            assert!(
                std::mem::discriminant(&flipped) == std::mem::discriminant(want),
                "usual_arith_type({:?}, {:?}) 与正向不一致: {:?}",
                b,
                a,
                flipped
            );
        }
    }

    /// 关系/相等运算的**结果类型**永远是 `int`（6.5.8），公共类型只决定比较的有符号性。
    ///
    /// 这条与上一条分开钉：`dispatch.rs` 的 Binop 臂要的正是「公共类型的有符号性」，
    /// 而前端给节点标注的是「结果的类型」—— 两者被混为一谈时，`a < b` 会带着
    /// `unsigned long long` 的类型往下走，后续赋值/截断口径跟着错。
    #[test]
    fn comparison_result_type_is_int_and_does_not_leak_the_common_type() {
        use Type::*;
        // 公共类型是无符号 (决定条件码), 但表达式类型仍是 int。
        let common = ULong.usual_arith_type(&LongLong);
        assert!(std::mem::discriminant(&common) == std::mem::discriminant(&ULongLong));
        assert!(matches!(common, ULongLong));
        // 整数提升入口本身也必须是类型, 不是「结果 int」的捷径。
        assert!(matches!(UChar.int_promotion(), Int));
        assert!(matches!(UShort.int_promotion(), Int));
        assert!(matches!(ULong.int_promotion(), ULong));
    }
}
