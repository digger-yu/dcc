// src/ast.rs - 抽象语法树
// 第一性原理: AST = 程序的"结构骨架"。类型系统必须能表达 C 的全部类型。
// suckless 准则: 节点种类尽量少;一个节点表示"一个语义概念"。

use std::sync::Arc;
use crate::diagnostics::Span;

#[derive(Debug, Clone)]
pub struct Ident { pub name: String, pub span: Span }

#[derive(Debug, Clone)]
pub struct Program { pub decls: Vec<Decl> }

#[derive(Debug, Clone)]
pub enum Decl {
    Var(VarDecl),
    Func(FuncDecl),
    StructDef(StructDef),
    UnionDef(StructDef),
    EnumDef(EnumDef),
    Typedef { name: Ident, ty: Type, span: Span },
    GlobalAsm { asm_str: String, outputs: Vec<Expr>, output_names: Vec<Option<String>>,
              output_constraints: Vec<String>, inputs: Vec<Expr>, input_names: Vec<Option<String>>,
              input_constraints: Vec<String>, clobbers: Vec<String>, span: Span },
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub ty: Type, pub name: Ident, pub init: Option<Expr>,
    pub span: Span, pub is_extern: bool, pub is_static: bool,
    // Linux 内核属性
    pub section: Option<String>,
    pub alignment: Option<usize>,
    pub is_weak: bool,
    pub visibility: Option<String>,
    pub is_unused: bool,
    pub register_name: Option<String>,  // register asm("rN")
    pub cleanup_fn: Option<String>,     // __attribute__((cleanup(fn)))
    pub is_thread_local: bool,          // _Thread_local / __thread
    /// __attribute__((no_sanitize("address"|"undefined"|"thread"|"kernel-address")))
    pub no_sanitize: Vec<String>,
    /// __attribute__((copy("fn"))): 函数符号别名
    pub copy_fn: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub ret_ty: Type, pub name: Ident, pub params: Vec<Param>,
    pub body: Option<Block>, pub span: Span,
    pub is_extern: bool, pub is_static: bool,
    // Linux 内核属性
    pub section: Option<String>,
    pub alignment: Option<usize>,
    pub is_weak: bool,
    pub visibility: Option<String>,
    pub is_unused: bool,
    pub is_cold: bool,
    pub is_noinline: bool,
    pub is_always_inline: bool,
    pub is_variadic: bool,
    // constructor/destructor/alias
    pub is_constructor: bool,
    pub is_destructor: bool,
    pub alias_name: Option<String>,
    // 嵌套函数静态链
    pub is_nested: bool,              // 是否是嵌套函数
    pub upvars: Vec<(String, i32)>,   // 从外层捕获的变量: (name, offset_in_parent_frame)
    pub is_old_style: bool,           // K&R 风格定义
    pub format_kind: Option<String>,  // __attribute__((format(printf, ...)))
    pub format_string_idx: Option<u32>,
    pub format_first_arg: Option<u32>,
    /// __attribute__((no_sanitize("address"|"undefined"|"thread"|"kernel-address")))
    pub no_sanitize: Vec<String>,
    /// __attribute__((target_clones("default,avx2,...", "sse4.2,default")))
    /// 收集所有目标版本 (忽略实际变体生成, 仅用于兼容)
    pub target_clones: Vec<String>,
    /// __attribute__((returns_nonnull))
    pub returns_nonnull: bool,
}

#[derive(Debug, Clone)]
pub struct Param { pub ty: Type, pub name: Option<Ident>, pub span: Span }

#[derive(Debug, Clone)]
pub struct Block { pub items: Vec<BlockItem>, pub span: Span }

#[derive(Debug, Clone)]
pub enum BlockItem { Stmt(Stmt), Decl(Decl) }

#[derive(Debug, Clone)]
pub enum Stmt {
    Return { val: Option<Expr>, span: Span },
    Expr    { e: Expr, span: Span },
    If      { cond: Expr, then_: Box<Stmt>, else_: Option<Box<Stmt>>, span: Span },
    While   { cond: Expr, body: Box<Stmt>, span: Span },
    Do      { body: Box<Stmt>, cond: Expr, span: Span },
    For     { init: Option<Box<Stmt>>, cond: Option<Expr>,
              step: Option<Expr>, body: Box<Stmt>, span: Span },
    Block   { b: Block, span: Span },
    Break   { span: Span },
    Continue{ span: Span },
    Goto    { target: Ident, span: Span },
    GotoIndirect { expr: Box<Expr>, span: Span },  // goto *expr
    Label   { name: Ident, stmt: Box<Stmt>, span: Span },
    Switch  { expr: Expr, cases: Vec<Case>, span: Span },
    AsmGoto { asm_str: String, outputs: Vec<Expr>, output_names: Vec<Option<String>>,
              output_constraints: Vec<String>, inputs: Vec<Expr>, input_names: Vec<Option<String>>,
              input_constraints: Vec<String>, clobbers: Vec<String>, goto_labels: Vec<Ident>, span: Span },
}

#[derive(Debug, Clone)]
pub struct Case {
    pub val: Option<Expr>,
    /// GNU `case low ... high:` 的上界（仅当 val 为下界时有效）
    pub range_end: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int       { v: i128, span: Span, ty: Type },
    Float     { v: f64, span: Span, ty: Type },
    String    { data: Vec<u8>, span: Span, ty: Type },
    Char      { v: u8, span: Span, ty: Type },
    Ident     { name: Ident, span: Span, ty: Type },
    Binop     { op: BinOp, l: Box<Expr>, r: Box<Expr>, span: Span, ty: Type },
    Unary     { op: UnOp,  e: Box<Expr>, span: Span, ty: Type },
    Call      { callee: Box<Expr>, args: Vec<Expr>, span: Span, ty: Type },
    Assign    { op: BinOp, l: Box<Expr>, r: Box<Expr>, span: Span, ty: Type },
    Cond      { cond: Box<Expr>, then_: Box<Expr>, else_: Box<Expr>, span: Span, ty: Type },
    Cast      { ty: Type, e: Box<Expr>, span: Span },
    SizeofTy  { ty: Type, span: Span },
    SizeofExpr{ e: Box<Expr>, span: Span },
    Index     { arr: Box<Expr>, idx: Box<Expr>, span: Span, ty: Type },
    Member    { expr: Box<Expr>, field: Ident, span: Span, ty: Type, is_arrow: bool },
    AddrOf    { e: Box<Expr>, span: Span, ty: Type },
    Deref     { e: Box<Expr>, span: Span, ty: Type },
    PostInc   { e: Box<Expr>, span: Span, ty: Type },
    PostDec   { e: Box<Expr>, span: Span, ty: Type },
    PreInc    { e: Box<Expr>, span: Span, ty: Type },
    PreDec    { e: Box<Expr>, span: Span, ty: Type },
    CompoundLit { ty: Type, inits: Vec<InitItem>, span: Span },
    /// 初始化列表: {1, 2, 3} 或 {.x = 1, .y = 2}
    InitList  { inits: Vec<InitItem>, span: Span, ty: Type },
    /// _Alignof(type)
    AlignofTy  { ty: Type, span: Span },
    /// _Alignof(expr)
    AlignofExpr{ e: Box<Expr>, span: Span },
    /// 语句表达式 ({...})
    StmtExpr   { block: Box<Block>, span: Span, ty: Type },
    /// 逗号运算符: (lhs, rhs) → 求值 lhs 丢弃, 返回 rhs
    Comma      { l: Box<Expr>, r: Box<Expr>, span: Span, ty: Type },
    /// 内联汇编 __asm__: 字符串列表, 结果类型
    AsmExpr    { asm_str: String, outputs: Vec<Expr>, output_names: Vec<Option<String>>,
                 output_constraints: Vec<String>, inputs: Vec<Expr>, input_names: Vec<Option<String>>,
                 input_constraints: Vec<String>, clobbers: Vec<String>, span: Span, ty: Type },
    /// &&label: label 地址 (GNU 扩展)
    LabelAddr  { label: Ident, span: Span, ty: Type },
    /// __builtin_va_arg(ap, type): 可变参数读取
    VaArg      { ap: Box<Expr>, ty: Type, span: Span },
    /// __builtin_offsetof(type, member)
    Offsetof   { ty: Type, member: String, span: Span, result_ty: Type },
}

/// 初始化项: 普通值 或 指定初始化器
#[derive(Debug, Clone)]
pub enum InitItem {
    /// 普通初始化值
    Expr(Expr),
    /// 指定初始化器: .field = expr 或 [idx] = expr
    Designated { designator: Designator, value: Expr },
}

/// 指定符
#[derive(Debug, Clone)]
pub enum Designator {
    /// .field
    Field(Ident),
    /// [index]
    Index(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    And, Or, Xor, Shl, Shr,
    Lt, Gt, Le, Ge, EqEq, Ne, AndAnd, OrOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp { Neg, Not, BitNot, Plus }

// ===== 类型系统 =====
/// 数组大小: 编译期常量 或 运行时表达式 (VLA)
#[derive(Debug, Clone)]
pub enum ArraySize {
    Const(usize),
    Var(Box<Expr>),  // 运行时表达式 (VLA)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Void, Bool,
    Char, SChar, UChar,
    Short, UShort,
    Int, UInt,
    Long, ULong,
    LongLong, ULongLong,
    Int128, UInt128,          // __int128 / __uint128_t
    Float, Double, LongDouble,
    Fract, Accum,  // 定点数: _Fract (1.31格式), _Accum (16.16格式)
    Complex(Box<Type>),  // _Complex float/double: (real, imag) 两部分
    Ptr(Box<Type>),
    Array(Box<Type>, ArraySize),
    Vector(Box<Type>, usize),  // SIMD 向量: (元素类型, 元素个数)
    Struct(String, Arc<Vec<Field>>),
    Union(String, Arc<Vec<Field>>),
    Enum(String, Arc<Vec<(String, i64)>>),
    Func(Box<Type>, Vec<Type>),
    Named(String),
    Atomic(Box<Type>),     // _Atomic(T): 原子类型, 大小/对齐同 T
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub offset: usize,      // 字节偏移 (位域: 存储单元起始字节)
    pub bit_width: Option<u32>,  // 位域宽度 (None = 普通字段)
    pub bit_offset: u32,    // 位域: 存储单元内位偏移 (非位域=0)
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: Ident, pub fields: Vec<Field>,
    pub is_union: bool, pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: Ident, pub variants: Vec<(Ident, Option<i64>)>, pub span: Span,
}

impl Type {
    pub fn size(&self) -> usize {
        match self {
            Type::Void => 0,
            Type::Bool | Type::Char | Type::SChar | Type::UChar => 1,
            Type::Short | Type::UShort => 2,
            Type::Int | Type::UInt | Type::Float | Type::Enum(_, _) | Type::Fract | Type::Accum => 4,
            Type::Long | Type::ULong | Type::Ptr(_)
            | Type::LongLong | Type::ULongLong | Type::Double => 8,
            Type::Int128 | Type::UInt128 => 16,
            Type::Array(elem, ArraySize::Const(n)) => elem.size() * n,
            Type::Array(_, ArraySize::Var(_)) => 8,  // VLA 退化为指针
            Type::Vector(elem, n) => elem.size() * n,
            Type::LongDouble => 16,
            Type::Complex(inner) => inner.size() * 2,
            Type::Atomic(inner) => inner.size(),
            Type::Struct(_, fields) => {
                fields.last().map(|f| f.offset + f.ty.size()).unwrap_or(0)
            }
            Type::Union(_, fields) => {
                fields.iter().map(|f| f.ty.size()).max().unwrap_or(0)
            }
            _ => 8,
        }
    }

    pub fn align(&self) -> usize {
        match self {
            Type::Void => 1,
            Type::Bool | Type::Char | Type::SChar | Type::UChar => 1,
            Type::Short | Type::UShort => 2,
            Type::Int | Type::UInt | Type::Float | Type::Enum(_, _) | Type::Fract | Type::Accum => 4,
            Type::Long | Type::ULong | Type::Ptr(_)
            | Type::LongLong | Type::ULongLong | Type::Double => 8,
            Type::Int128 | Type::UInt128 => 16,
            Type::Array(elem, ArraySize::Const(_)) => elem.align(),
            Type::Array(elem, ArraySize::Var(_)) => elem.align(),
            Type::Vector(_, _) => 16, // SIMD 向量 16 字节对齐
            Type::LongDouble => 16,
            Type::Complex(inner) => inner.align(),
            Type::Atomic(inner) => inner.align(),
            Type::Struct(_, fields) => fields.iter().map(|f| f.ty.align()).max().unwrap_or(1),
            Type::Union(_, fields) => fields.iter().map(|f| f.ty.align()).max().unwrap_or(1),
            _ => 8,
        }
    }

    pub fn is_ptr(&self) -> bool { matches!(self, Type::Ptr(_)) }
    pub fn is_int(&self) -> bool {
        matches!(self, Type::Bool | Type::Char | Type::SChar | Type::UChar
            | Type::Short | Type::UShort | Type::Int | Type::UInt
            | Type::Long | Type::ULong | Type::LongLong | Type::ULongLong
            | Type::Int128 | Type::UInt128)
    }
    pub fn is_arith(&self) -> bool { self.is_int() || matches!(self, Type::Float | Type::Double | Type::LongDouble | Type::Fract | Type::Accum | Type::Complex(_)) }

    pub fn layout_struct(fields: &mut Vec<Field>, is_union: bool) {
        Self::layout_struct_with_pack(fields, is_union, None);
    }

    /// 按对齐限制布局: max_align = None 用自然对齐, Some(n) 把字段对齐限制为 n
    /// (实现 #pragma pack(n): 把结构体内部所有字段对齐不超过 n 字节)
    pub fn layout_struct_with_pack(fields: &mut Vec<Field>, is_union: bool, max_align: Option<usize>) {
        // 实际应用的对齐: None => 自然对齐, Some(n) => min(n, 自然对齐)
        let eff = |nat: usize| -> usize {
            match max_align { Some(n) if n < nat => n, _ => nat }
        };
        if is_union {
            for f in fields.iter_mut() { f.offset = 0; }
        } else {
            let mut off = 0usize;           // 当前字节偏移
            let mut bit_pos: u32 = 0;       // 当前存储单元内已用位数
            let mut container_bits: u32 = 0; // 当前存储单元总位数

            for f in fields.iter_mut() {
                if let Some(bw) = f.bit_width {
                    // 位域字段
                    let field_bits = bw;
                    let container_ty_bits = (f.ty.size() * 8) as u32;

                    // 第一个位域或容器类型变化: 开始新存储单元
                    if container_bits == 0 || container_ty_bits != container_bits {
                        // 对齐到容器类型边界
                        let align = eff(f.ty.align());
                        off = (off + align - 1) & !(align - 1);
                        container_bits = container_ty_bits;
                        bit_pos = 0;
                    }

                    // 当前单元放不下: 推进到下一单元
                    if bit_pos + field_bits > container_bits {
                        off += (container_bits / 8) as usize;
                        let align = eff(f.ty.align());
                        off = (off + align - 1) & !(align - 1);
                        bit_pos = 0;
                    }

                    f.offset = off;
                    f.bit_offset = bit_pos;
                    bit_pos += field_bits;
                } else {
                    // 非位域字段: 刷新未用完的位域存储单元
                    if container_bits > 0 {
                        off += (container_bits / 8) as usize;
                        container_bits = 0;
                        bit_pos = 0;
                    }
                    // 柔性数组 (零长数组): 不贡献大小
                    if let Type::Array(_, ArraySize::Const(0)) = &f.ty {
                        let nat = f.ty.align();
                        let align = eff(nat);
                        off = (off + align - 1) & !(align - 1);
                        f.offset = off;
                        continue;
                    }
                    let nat = f.ty.align();
                    let a = eff(nat);
                    off = (off + a - 1) & !(a - 1);
                    f.offset = off;
                    off += f.ty.size();
                }
            }
            // 刷新末尾未用完的位域存储单元
            // (size 由 size() 计算, 此处不需要额外处理)
            let _ = (off, bit_pos, container_bits);
        }
    }

    /// packed 结构体布局: 所有字段对齐为 1, 无填充
    pub fn layout_struct_packed(fields: &mut Vec<Field>, is_union: bool) {
        if is_union {
            for f in fields.iter_mut() { f.offset = 0; }
        } else {
            let mut off = 0usize;
            for f in fields.iter_mut() {
                if let Some(bw) = f.bit_width {
                    f.offset = off;
                    f.bit_offset = 0; // 简化: packed 下位域从字节边界开始
                    off += (bw as usize + 7) / 8;
                } else {
                    f.offset = off;
                    off += f.ty.size();
                }
            }
        }
    }
}

pub fn int_t()  -> Type { Type::Int }
pub fn long_t() -> Type { Type::Long }
pub fn void_t() -> Type { Type::Void }
pub fn char_t() -> Type { Type::Char }

/// 简洁的 C 风格类型名 (用于诊断与字符串化)
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Void => write!(f, "void"),
            Type::Bool => write!(f, "_Bool"),
            Type::Char => write!(f, "char"),
            Type::SChar => write!(f, "signed char"),
            Type::UChar => write!(f, "unsigned char"),
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
            Type::Fract => write!(f, "_Fract"),
            Type::Accum => write!(f, "_Accum"),
            Type::Complex(inner) => write!(f, "_Complex {}", inner),
            Type::Ptr(inner) => write!(f, "{}*", inner),
            Type::Array(inner, sz) => match sz {
                ArraySize::Const(n) => write!(f, "{}[{}]", inner, n),
                ArraySize::Var(_) => write!(f, "{}[]", inner),
            }
            Type::Vector(inner, n) => write!(f, "__attribute__((vector_size({}))){}", n * inner.size() * 8, inner),
            Type::Struct(name, _) => write!(f, "struct {}", name),
            Type::Union(name, _) => write!(f, "union {}", name),
            Type::Enum(name, _) => write!(f, "enum {}", name),
            Type::Func(ret, params) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", p)?;
                }
                write!(f, ") -> {}", ret)
            }
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
            if let Type::Struct(_, inner) | Type::Union(_, inner) = &f.ty {
                if let Some((found, inner_off)) = find_field(inner, name) {
                    return Some((found, f.offset + inner_off));
                }
            }
        }
    }
    None
}

impl Expr {
    pub fn ty(&self) -> Type {
        match self {
            Expr::Int { ty, .. } | Expr::Float { ty, .. } | Expr::String { ty, .. } | Expr::Char { ty, .. }
            | Expr::Ident { ty, .. } | Expr::Binop { ty, .. } | Expr::Unary { ty, .. }
            | Expr::Call { ty, .. } | Expr::Assign { ty, .. } | Expr::Cond { ty, .. }
            | Expr::Index { ty, .. } | Expr::Member { ty, .. }
            | Expr::AddrOf { ty, .. } | Expr::Deref { ty, .. }
            | Expr::PostInc { ty, .. } | Expr::PostDec { ty, .. }
            | Expr::PreInc { ty, .. } | Expr::PreDec { ty, .. }
            | Expr::Comma { ty, .. } | Expr::LabelAddr { ty, .. } => ty.clone(),
            Expr::AsmExpr { ty, .. } => ty.clone(),
            Expr::VaArg { ty, .. } => ty.clone(),
            Expr::Cast { ty, .. } => ty.clone(),
            Expr::InitList { ty, .. } => ty.clone(),
            Expr::SizeofTy { .. } | Expr::SizeofExpr { .. } | Expr::CompoundLit { .. }
            | Expr::AlignofTy { .. } | Expr::AlignofExpr { .. }
            | Expr::Offsetof { .. } => Type::Int,
            Expr::StmtExpr { ty, .. } => ty.clone(),
        }
    }
}
