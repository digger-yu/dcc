//! C++ HIR (High-level Intermediate Representation)
//!
//! 语义化 C++ 中间表示。
//! 与 parser AST 的区别:
//! - 所有名字已解析（指向具体 SymbolId，而非字符串）
//! - 类型已推导并标注
//! - 构造/析构是显式节点，不依赖隐式调用
//! - 成员访问、基类转换、虚函数调用有完整语义信息
//!
//! HIR 不直接输出 C 文本，而是由 lower_ir 降到 dcc typed IR。

use crate::cpp::sema::{ClassId, CxxType, SymbolId};
use std::collections::HashSet;

// === ID 类型 ===

/// HIR 节点通用 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub usize);

/// HIR 表达式 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirExprId(pub usize);

/// HIR 语句 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirStmtId(pub usize);

/// HIR 函数体 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirBodyId(pub usize);

// === G14: Dependent Name Analysis Info ===

/// 依赖名称分析信息（用于模板实例化的两阶段查找）
#[derive(Debug, Clone, Default)]
pub struct DependentInfo {
    /// 依赖模板参数的名称集合（如 T::member 中的 member）
    pub dependent_names: HashSet<String>,
    /// 非依赖名称集合（可在定义点立即解析）
    pub non_dependent_names: HashSet<String>,
}

impl DependentInfo {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn add_dependent(&mut self, name: String) {
        self.dependent_names.insert(name);
    }
    
    pub fn add_non_dependent(&mut self, name: String) {
        self.non_dependent_names.insert(name);
    }
    
    pub fn is_dependent(&self, name: &str) -> bool {
        self.dependent_names.contains(name)
    }
}

// === 析构变体 ===

/// 析构函数变体（Itanium ABI）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtorVariant {
    /// 完整对象析构 D1
    Complete,
    /// 基类析构 D2
    Base,
    /// 删除析构 D0 (operator delete 调用版本)
    Deleting,
}

// === this 调整 ===

/// this 指针调整（多继承/虚继承用）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThisAdjust {
    /// 非虚偏移字节数
    pub non_virtual: i64,
    /// 虚基表索引（虚继承用，None 表示无虚调整）
    pub vbase_index: Option<usize>,
}

// === 被调用者 ===

/// 函数调用的被调用者
#[derive(Debug, Clone)]
pub enum HirCallee {
    /// 直接调用（自由函数或静态成员函数）
    Direct(SymbolId),
    /// 虚函数调用（通过 vtable）
    Virtual {
        method: SymbolId,
        /// vtable slot 索引
        slot: usize,
        /// this 调整
        this_adjust: ThisAdjust,
    },
    /// 函数指针调用
    Indirect(HirExprId),
    /// 构造函数调用
    Constructor { class: ClassId, variant: CtorVariant },
    /// 析构函数调用
    Destructor { class: ClassId, variant: DtorVariant },
}

/// 构造函数变体
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtorVariant {
    /// 完整对象构造 C1
    Complete,
    /// 基类子对象构造 C2
    Base,
    /// 分配构造 C3 (in-charge / 带 vptr 初始化)
    Allocating,
}

// === 表达式 ===

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub id: HirExprId,
    pub kind: HirExprKind,
    pub ty: CxxType,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    /// 局部变量 / 形参引用
    Local(SymbolId),
    /// 全局变量 / 命名空间变量
    Global(SymbolId),
    /// 成员访问（非静态）
    FieldAccess {
        object: HirExprId,
        field: SymbolId,
    },
    /// 方法引用（对象.方法 或 对象->方法），保留对象上下文用于虚调用判定
    MethodRef {
        object: HirExprId,
        method: SymbolId,
    },
    /// 基类转换（非虚）
    BaseCast {
        object: HirExprId,
        from: ClassId,
        to: ClassId,
    },
    /// 虚基类调整
    VBaseAdjust {
        object: HirExprId,
        from: ClassId,
        to: ClassId,
    },
    /// 函数调用
    Call {
        callee: HirCallee,
        args: Vec<HirExprId>,
    },
    /// 构造调用（placement new 风格，在指定地址构造）
    Construct {
        class: ClassId,
        place: HirExprId,
        args: Vec<HirExprId>,
        variant: CtorVariant,
    },
    /// 析构调用
    Destroy {
        class: ClassId,
        place: HirExprId,
        variant: DtorVariant,
    },
    /// 整型字面量
    IntLiteral(i128),
    /// 浮点字面量（字符串保留精度）
    FloatLiteral(String),
    /// 布尔字面量
    BoolLiteral(bool),
    /// 空指针
    NullPtr,
    /// this 指针
    This,
    /// 取址
    AddressOf(HirExprId),
    /// 解引用
    Dereference(HirExprId),
    /// 二元运算
    BinaryOp {
        op: BinaryOp,
        lhs: HirExprId,
        rhs: HirExprId,
    },
    /// 一元运算
    UnaryOp {
        op: UnaryOp,
        operand: HirExprId,
    },
    /// 下标访问 `array[index]`（数组或指针）
    Index {
        array: HirExprId,
        index: HirExprId,
    },
    /// 赋值
    Assign {
        lhs: HirExprId,
        rhs: HirExprId,
    },
    /// 复合赋值
    CompoundAssign {
        op: BinaryOp,
        lhs: HirExprId,
        rhs: HirExprId,
    },
    /// 条件表达式
    Conditional {
        cond: HirExprId,
        then_expr: HirExprId,
        else_expr: HirExprId,
    },
    /// 类型转换（static_cast / 隐式转换）
    Cast {
        expr: HirExprId,
        target: CxxType,
        kind: CastKind,
    },
    /// typeid 表达式
    TypeInfoRef(ClassId),
    /// dynamic_cast
    DynamicCast {
        object: HirExprId,
        src: CxxType,
        dst: CxxType,
    },
    /// throw 表达式
    Throw {
        object: Option<HirExprId>,
        ty: CxxType,
    },
    /// new 表达式
    New {
        ty: CxxType,
        args: Vec<HirExprId>,
        placement: Option<Vec<HirExprId>>,
        is_array: bool,
    },
    /// delete 表达式
    Delete {
        ptr: HirExprId,
        is_array: bool,
    },
    /// sizeof 表达式
    SizeOf(CxxType),
    /// 逗号表达式
    Comma {
        lhs: HirExprId,
        rhs: HirExprId,
    },
    /// 花括号初始化列表 `{a, b, c}`
    InitList {
        elems: Vec<HirExprId>,
        /// 可选的目标类型（TypedInitList 时设置）
        target_ty: Option<CxxType>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    LogicalAnd,
    LogicalOr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Comma,
    // 成员指针访问
    PtrMem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
    BitNot,
    LogicalNot,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
    Deref,
    AddrOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    /// 隐式转换
    Implicit,
    /// static_cast
    Static,
    /// const_cast
    Const,
    /// reinterpret_cast
    Reinterpret,
    /// dynamic_cast
    Dynamic,
}

// === 语句 ===

#[derive(Debug, Clone)]
pub struct HirStmt {
    pub id: HirStmtId,
    pub kind: HirStmtKind,
}

#[derive(Debug, Clone)]
pub enum HirStmtKind {
    /// 空语句
    Null,
    /// 表达式语句
    Expr(HirExprId),
    /// 复合语句（块）
    Compound(Vec<HirStmtId>),
    /// if 语句
    If {
        cond: HirExprId,
        then_body: HirStmtId,
        else_body: Option<HirStmtId>,
    },
    /// while 循环
    While {
        cond: HirExprId,
        body: HirStmtId,
    },
    /// do-while 循环
    DoWhile {
        body: HirStmtId,
        cond: HirExprId,
    },
    /// for 循环
    For {
        init: Option<HirStmtId>,
        cond: Option<HirExprId>,
        inc: Option<HirExprId>,
        body: HirStmtId,
    },
    /// switch
    Switch {
        cond: HirExprId,
        body: HirStmtId,
    },
    /// case 标签
    Case {
        value: i128,
        body: HirStmtId,
    },
    /// default 标签
    Default(HirStmtId),
    /// return
    Return(Option<HirExprId>),
    /// break
    Break,
    /// continue
    Continue,
    /// C++17 [[fallthrough]] / GCC __attribute__((fallthrough))
    Fallthrough,
    /// goto
    Goto(String),
    /// 标签
    Label(String, HirStmtId),
    /// 变量声明
    Decl {
        symbol: SymbolId,
        init: Option<HirExprId>,
    },
    /// 静态局部变量声明（带 guard 的线程安全初始化）
    StaticDecl {
        /// 静态变量符号（实际存储在全局数据区）
        symbol: SymbolId,
        /// guard 变量符号（i64 类型，用于 __cxa_guard_*）
        guard_symbol: SymbolId,
        /// 初始化表达式
        init: Option<HirExprId>,
    },
    /// try/catch
    Try {
        body: HirStmtId,
        handlers: Vec<HirCatchHandler>,
    },
    /// 构造基类 / 成员（ctor initializer list 用）
    CtorInitMember {
        member: SymbolId,
        args: Vec<HirExprId>,
    },
    CtorInitBase {
        base: ClassId,
        args: Vec<HirExprId>,
    },
    /// 设置 vptr 指向指定类的 vtable（构造/析构中更新虚表指针）
    SetVTablePtr {
        class: ClassId,
        place: HirExprId,
    },
}

#[derive(Debug, Clone)]
pub struct HirCatchHandler {
    /// catch 参数类型
    pub param_ty: CxxType,
    /// catch 参数符号（如果有名）
    pub param: Option<SymbolId>,
    /// catch 体
    pub body: HirStmtId,
}

// === 函数 / 翻译单元 ===

/// 函数定义
#[derive(Debug, Clone)]
pub struct HirFunction {
    pub symbol: SymbolId,
    pub body: HirBodyId,
    /// 参数符号列表（按顺序）
    pub params: Vec<SymbolId>,
    /// 返回类型
    pub ret_ty: CxxType,
}

/// 函数体（语句列表）
#[derive(Debug, Clone)]
pub struct HirBody {
    pub id: HirBodyId,
    pub stmts: Vec<HirStmtId>,
    /// 构造函数初始化列表（ctor 才有）
    pub ctor_initializers: Vec<HirStmtId>,
}

/// 翻译单元（整个源文件）
#[derive(Debug, Default, Clone)]
pub struct HirTranslationUnit {
    pub exprs: Vec<HirExpr>,
    pub stmts: Vec<HirStmt>,
    pub functions: Vec<HirFunction>,
    pub bodies: Vec<HirBody>,
    /// 顶层声明（函数、全局变量、类声明等）
    pub top_level: Vec<HirTopLevelDecl>,
}

/// 函数定义的扩展版本（支持依赖名分析）
#[derive(Debug, Clone)]
pub struct HirFunctionExtended {
    pub symbol: SymbolId,
    pub body: HirBodyId,
    /// 参数符号列表（按顺序）
    pub params: Vec<SymbolId>,
    /// 返回类型
    pub ret_ty: CxxType,
    /// G14: 依赖名称信息
    pub dependent_info: DependentInfo,
}

/// 类定义的扩展版本（支持依赖名分析）
#[derive(Debug, Clone)]
pub struct HirClassExtended {
    pub symbol: SymbolId,
    /// 成员列表
    pub members: Vec<(SymbolId, CxxType)>,
    /// G14: 依赖名称信息
    pub dependent_info: DependentInfo,
}

/// 顶层声明种类
#[derive(Debug, Clone)]
pub enum HirTopLevelDecl {
    Function(SymbolId),
    Variable(SymbolId),
    Class(SymbolId),
    Enum(SymbolId),
    Namespace(SymbolId),
    TypeAlias(SymbolId),
    Template(SymbolId),
}

impl HirTranslationUnit {
    pub fn new() -> Self {
        Self::default()
    }
}
