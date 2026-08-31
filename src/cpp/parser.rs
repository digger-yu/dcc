//! C++ 解析器 (M7 + M8).
//!
//! M7 范围: class 声明 / 成员函数 / 成员变量 / this / :: / 引用 /
//! 构造 / 析构.
//!
//! M8 扩展:
//!   - 命名空间: `namespace foo {}` / `using namespace bar` / `::name`
//!   - 运算符重载: `operator + (T const&)` / `operator () (T)` / `operator T*`
//!   - 继承: `class Derived : public Base {}` (单 / 多 / 虚)
//!   - 虚函数 + vtable 生成
//!   - RTTI: `typeid(e)` / `dynamic_cast<T*>(e)` + `_ZTI` 符号
//!   - 异常: `try {} catch (T& e) {}` / `throw e` (调用 __cxa_*)
//!   - Lambda: `[](T x) { return x; }` (降级为匿名 struct + operator())
//!   - 模板: `template<typename T> T id(T x) { return x; }` (textual sub)
//!   - constexpr: 简单常量表达式求值
//!
//! 解析结果用 CppDecl 枚举表示, 供 lower.rs 降级到 dcc C IR.

use crate::cpp::lexer::{CppKw, CppLexer, CppTok};
use crate::cpp::mangle::{MangleType, NestedName, Operator};
use crate::cpp::template_depth::MAX_TEMPLATE_DEPTH;
use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Tok, TokKind};

/// 解析错误
#[derive(Debug, Clone)]
pub enum CppParseError {
    Unexpected(&'static str, String, Span),
    Unsupported(&'static str, Span),
    TooDeep(Span),
    /// 模板实例化嵌套深度超限 (见 template_depth.rs)
    TemplateDepthExceeded {
        depth: u32,
        max: u32,
        span: Span,
    },
}

impl CppParseError {
    pub fn to_diag(&self, file: &str) -> Diagnostic {
        let mut span = self.span();
        span.file = file.to_string();
        match self {
            Self::Unexpected(want, got, _) => {
                Diagnostic::err("C0001", format!("expected {}, got '{}'", want, got), span)
            }
            Self::Unsupported(what, _) => Diagnostic::err(
                "C0002",
                format!("C++ feature not yet supported: {}", what),
                span,
            )
            .with_hint("M8 implements most C++ features; some advanced patterns may still fail"),
            Self::TooDeep(_) => Diagnostic::err("C0003", "nesting too deep", span),
            Self::TemplateDepthExceeded { depth, max, .. } => Diagnostic::err(
                "C0004",
                format!("template nesting depth {} exceeds limit {}", depth, max),
                span,
            )
            .with_hint("reduce template argument nesting (GCC equivalent: -ftemplate-depth)"),
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Unexpected(_, _, s) | Self::Unsupported(_, s) | Self::TooDeep(s) => s.clone(),
            Self::TemplateDepthExceeded { span: s, .. } => s.clone(),
        }
    }
}

/// 继承基类描述
#[derive(Debug, Clone)]
pub struct BaseSpec {
    /// `public` / `private` / `protected` (M8 简化: 仅记录)
    pub access: Access,
    /// `virtual` 标记 (虚基类)
    pub is_virtual: bool,
    /// 基类名
    pub name: String,
    /// 模板实参 (如 `Base<int>` 中的 `int`)
    pub template_args: Option<Vec<crate::cpp::mangle::MangleType>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Private,
    Protected,
}

/// 类 / 结构体声明
#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
    /// `class` 或 `struct` (M8: 统一为 struct 在 C 中表达)
    pub is_struct: bool,
    /// M30.2: `union` 聚合 - lower 发射 union 关键字 (成员重叠语义).
    pub is_union: bool,
    /// 基类列表 (M8: 含普通 / 虚基)
    pub bases: Vec<BaseSpec>,
    pub members: Vec<CppDecl>,
}

/// 模板参数 (M8 简化: 仅类型参数)
#[derive(Debug, Clone)]
pub struct TemplateParam {
    pub name: String,
    /// 默认实参 (M8: 简化, 解析但不强求)
    pub default: Option<MangleType>,
    /// M30.1: 非类型形参种类 (`template <typename T, bool B>` 的 B 为
    /// Some(Bool)). None = 类型形参. 种类仅用于解析/默认值求值,
    /// 实例化管道统一用 MangleType::NonType 表示值.
    pub non_type: Option<MangleType>,
}

#[derive(Debug, Clone)]
pub struct TemplateArgs {
    /// 形参名 (T / U / ...) 列表
    pub params: Vec<TemplateParam>,
}

/// Catch 子句
#[derive(Debug, Clone)]
pub struct CatchClause {
    /// `T& e` 中的类型与名; None 表示 `catch (...)`
    pub ty: Option<MangleType>,
    pub name: Option<String>,
    pub body: CppBody,
}

/// Lambda 捕获列表
#[derive(Debug, Clone)]
pub enum Capture {
    /// `[this]`
    This,
    /// `[&]`
    ByRefAll,
    /// `[=]`
    ByValAll,
    /// `[x]`
    ByVal(String),
    /// `[&x]`
    ByRef(String),
    /// `[&x = expr]` (M8: 简化为 by-ref capture)
    Init(String, Box<CppExpr>),
}

#[derive(Debug, Clone)]
pub struct LambdaExpr {
    pub captures: Vec<Capture>,
    pub params: Vec<(String, MangleType)>,
    pub body: CppBody,
    /// 降级时分配的闭包类型名 (lower 阶段填充)
    pub closure_name: String,
}

/// 解析后的顶层 / 类内声明
#[derive(Debug, Clone)]
pub enum CppDecl {
    /// 自由函数
    FreeFn {
        ret: MangleType,
        name: String,
        params: Vec<(String, MangleType)>,
        body: CppBody,
        /// M26.1: `noexcept` / `noexcept(expr)` 修饰 - 异常不外传
        is_noexcept: bool,
    },
    /// 类外成员函数定义
    MemberFn {
        class: String,
        ret: MangleType,
        name: String,
        params: Vec<(String, MangleType)>,
        body: CppBody,
        /// 是否有 `virtual` 修饰
        is_virtual: bool,
        /// 是否有 `override` / `final` 修饰 (M17.0: 区分 `override` vs `final`)
        is_override: bool,
        /// M17.0: `final` 修饰 - vtable 项关闭, 阻止派生类覆盖
        is_final: bool,
        /// 是否 const 方法
        is_const: bool,
        /// M17.0: `noexcept` 修饰 - 异常不外传
        is_noexcept: bool,
        /// M17.0: `volatile` 成员函数 - 体内 this 访问走 volatile load/store
        is_volatile: bool,
        /// M17.0: `friend` 标记 - 不属于 this 类成员, 但有访问权限
        is_friend: bool,
    },
    /// 构造函数 (类外)
    Ctor {
        class: String,
        params: Vec<(String, MangleType)>,
        /// 成员初始化列表: `name(value)` 对 (M9: 仅简单表达式 / 字面量)
        inits: Vec<(String, CppExpr)>,
        body: CppBody,
        is_virtual: bool,
        /// M17.0: `explicit` 标记 - 阻止隐式转换
        is_explicit: bool,
        /// M17.0: `noexcept` 标记
        is_noexcept: bool,
    },
    /// 析构函数 (类外)
    Dtor {
        class: String,
        body: CppBody,
        is_virtual: bool,
        /// M17.0: `noexcept` 标记
        is_noexcept: bool,
    },
    /// 成员变量
    MemberVar {
        ty: MangleType,
        name: String,
        /// M17.0: `mutable` 标记 - 即使在 const 方法中也可修改
        is_mutable: bool,
    },
    /// M30.2: 匿名聚合实例成员 `union { ... } temp;` / `struct {...} x;`
    /// (obstack temp/chunkfun/freefun, cpp_hash_table val) - 定义与
    /// 实例变量一体, lower 原样发射 C 匿名聚合 (字段重叠保留).
    AnonAggr {
        is_union: bool,
        members: Vec<CppDecl>,
        name: String,
    },
    /// 类 / 结构体声明
    Class(ClassDecl),
    /// 命名空间 (M8)
    Namespace {
        /// 命名空间名; None 表示匿名
        name: Option<String>,
        body: Vec<CppDecl>,
    },
    /// using 声明 / using namespace
    Using {
        /// 是否是 `using namespace X` (true) / `using X::Y` (false)
        is_namespace: bool,
        /// 限定路径
        path: Vec<String>,
    },
    /// C++20: `using enum Color;` - 将枚举值引入当前作用域
    /// 降级: C 枚举值已在同一作用域, 故为 no-op.
    UsingEnum { name: String },
    /// 运算符重载函数 (M8)
    OperatorFn {
        /// 限定名 (含 class 与 operator 名)
        class: Option<String>,
        op: crate::cpp::mangle::Operator,
        /// 一元 / 二元 / 转型 / 函数调用
        params: Vec<(String, MangleType)>,
        body: CppBody,
        ret: MangleType,
        /// 一元 / 转型才用: 形参列表为空
        is_unary: bool,
    },
    /// 模板声明 (M8: 简化, 仅作语法占位 + 立即展开)
    /// requires: 模板头后的 `requires 约束` 文本 (M26: 实例化时求值过滤)
    /// is_export: `export template` 时为 true (M26: BMF 导出标记)
    Template {
        params: TemplateArgs,
        decl: Box<CppDecl>,
        requires: Option<String>,
        is_export: bool,
    },
    /// C++20 partial specialization (M10+):
    ///   `template <typename T> class Foo<T*> { ... };`
    ///   `template <typename T> void foo<T>(T) { ... }` 不被支持
    PartialSpec {
        params: TemplateArgs,
        /// 特化模式, 字符串形式: `T*`, `int`, `std::vector<T>` 等
        pattern_text: String,
        decl: Box<CppDecl>,
    },
    /// `using Base::Base;` 继承构造函数 (M10+)
    InheritingCtor { class: String, base: String },
    /// C++20 concept 定义 (M9: 占位, 立即展开为 no-op)
    Concept {
        name: String,
        /// constraint expression 文本 (M10+ 解析为简单 constraint 树)
        body_text: String,
    },
    /// C++20 module 声明 (M9: 占位, 立即展开为 no-op)
    /// `module foo;` / `export module foo;` / `module;`
    /// M10+: 支持分区 `module foo:part1;`
    Module {
        name: Option<String>,
        partition: Option<String>,
        is_export: bool,
    },
    /// C++20 import 声明 (M9: 占位, 立即展开为 no-op)
    /// `import foo;` / `import <iostream>;` / `import "header.h";`
    Import { path: Vec<String> },
    /// M26.4: `module :private;` 私有片段 - 之后的声明不导出
    PrivateFragment,
    /// C++20 export 块声明 (M9: 占位, 立即展开为 no-op)
    ExportBlock(Vec<CppDecl>),
    /// M11: extern "C" { ... } 块 (M18: 透传原文到 lower 输出)
    /// 原始文本作为字符串保存, lower 时整体 re-emit 为 extern "C" 块
    RawExternC(String),
    /// M30: 顶层全局变量声明 (`int g = 5;` / `Tpl<Args>::member g;`).
    /// 之前 Ident 起始的变量声明误入 parse_free_fn (要求 `(`), 失败后
    /// 整个文件降级 strip-keywords 模式; 现在显式解析并输出 C 全局变量.
    GlobalVar {
        ty: MangleType,
        name: String,
        init: Option<CppExpr>,
        /// `static` 修饰 (C 同义: 内部链接)
        is_static: bool,
    },
    /// M30: typedef 声明. M30: 带目标类型与别名, 使模板实例别名
    /// (`typedef Name<Args> alias`) 和模板类内 typedef
    /// (`typedef T type;`) 可被记录与实例化替换; lower 对普通
    /// typedef 仍不产生 C 输出 (类型由后续引用处展开).
    Typedef { target: MangleType, name: String },
    /// M18.0: 空占位声明 (enum / 修饰后无实际声明时使用)
    Empty,
    /// C++ enum declaration with scoped members and optional underlying type.
    Enum {
        name: String,
        scoped: bool,
        underlying: Option<String>,
        variants: Vec<(String, Option<String>)>,
    },
}

#[derive(Debug, Clone)]
pub struct CppBody {
    pub stmts: Vec<CppStmt>,
    /// M30.2: true = 纯声明 (类内 `Ret m() const;` 无 body). lower 端据此
    /// 输出原型而非空 weak 定义 (空定义会与类外定义产生重复符号, 链接器
    /// 可能选空的那个, 静默吞掉正确实现).
    pub is_decl: bool,
}

impl Default for CppBody {
    fn default() -> Self {
        CppBody {
            stmts: Vec::new(),
            is_decl: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CppStmt {
    Expr(CppExpr),
    Return(Option<CppExpr>),
    If {
        cond: CppExpr,
        then: Box<CppBody>,
        els: Option<Box<CppBody>>,
    },
    /// M17.0: `if constexpr (cond) { ... }` 编译期分支 - lower 时 const 求值并选分支
    IfConstexpr {
        cond: CppExpr,
        then: Box<CppBody>,
        els: Option<Box<CppBody>>,
    },
    While {
        cond: CppExpr,
        body: Box<CppBody>,
    },
    For {
        init: Option<Box<CppStmt>>,
        cond: Option<CppExpr>,
        step: Option<Box<CppStmt>>,
        body: Box<CppBody>,
    },
    Block(CppBody),
    /// `this->member = expr` 或 `this->member(args)`
    ThisMember(String, Box<CppStmt>),
    /// M9: 局部变量声明 `T name;` / `T name = init;` / `auto name = init;`
    LocalVar {
        ty: MangleType,
        name: String,
        init: Option<CppExpr>,
    },
    /// M10+: 静态局部变量 `static T name = init;`
    /// 触发 `__cxa_guard_*` 调用 (C++ ABI 线程安全单次初始化)
    StaticLocal {
        ty: MangleType,
        name: String,
        init: Option<CppExpr>,
    },
    /// `try { ... } catch (T& e) { ... }` (M8)
    Try {
        body: Box<CppBody>,
        catches: Vec<CatchClause>,
    },
    /// `throw expr;` (M8)
    Throw(Option<CppExpr>),
    /// `break;` (M8)
    Break,
    /// `continue;` (M8)
    Continue,
    /// `switch (expr) { case val: stmts... default: stmts... }`
    Switch {
        expr: CppExpr,
        /// Each arm: None = default, Some(expr) = case expr
        arms: Vec<(Option<CppExpr>, CppBody)>,
    },
    /// `co_await expr;` (C++20 协程, M9 占位)
    CoAwait(CppExpr),
    /// `co_yield expr;` (C++20 协程, M9 占位)
    CoYield(CppExpr),
    /// `co_return expr;` (C++20 协程, M9 占位)
    CoReturn(Option<CppExpr>),
    /// 空声明 `;` 已被调用方过滤
    Decl(CppDecl),
}

#[derive(Debug, Clone)]
pub enum CppExpr {
    IntLit(i64),
    /// M26.1: 浮点字面量, 保留源文本 (含 0x/指数/后缀) 供 lower 透传.
    FloatLit(String),
    StrLit(String),
    Ident(String),
    This,
    /// this->member
    ThisMember(String),
    /// 限定名访问: ns1::ns2::name
    QualifiedAccess(Vec<String>, String),
    /// 成员访问: obj.member
    Member(Box<CppExpr>, String),
    /// `obj->member` 指针访问
    PtrMember(Box<CppExpr>, String),
    /// M10+: 成员函数指针 `&Class::method` (简化: 形参 self, 传 this)
    MemberPtr {
        class: String,
        name: String,
    },
    /// M26: 带显式模板实参的模板名 `foo<int>` / `Box<T>` (作为 callee 出现)
    /// lower 阶段按实参实例化模板函数并调用.
    TemplateIdent {
        name: String,
        args: Vec<MangleType>,
    },
    /// M29: 模板实例静态成员/枚举常量访问 `Tpl<Args>::member`.
    /// 类内 enum 常量降级为全局名, 引用处按短名输出; 访问前需触发实例化.
    TplStaticAccess {
        name: String,
        args: Vec<MangleType>,
        member: String,
    },
    /// M10+: 大括号初始化列表 `{1, 2, 3}` (initializer_list)
    InitList(Vec<CppExpr>),
    /// M10+: 类型化的大括号初始化列表 `T{1, 2, 3}` (initializer_list<T>)
    TypedInitList {
        ty: MangleType,
        elems: Vec<CppExpr>,
    },
    /// M17.0: `obj.*ptr_to_member` 真解引用 (指针到数据成员)
    /// 成员指针以 size_t 偏移表示; lower 阶段查表得到 offsetof(struct T, f)
    MemberStar(Box<CppExpr>, String),
    /// M17.0: `obj->*ptr_to_member` 真解引用 (obj 是指针, member 是偏移)
    PtrMemberStar(Box<CppExpr>, String),
    Call(Box<CppExpr>, Vec<CppExpr>),
    /// M17.0: `new T[n]` 数组分配
    NewArr {
        ty: String,
        count: Box<CppExpr>,
    },
    New(String, Vec<CppExpr>),
    /// M17.0: `new (ptr) T(args)` placement new
    PlacementNew {
        ptr: Box<CppExpr>,
        ty: String,
        args: Vec<CppExpr>,
    },
    Delete(Box<CppExpr>),
    /// M17.0: `delete[] p` 数组释放
    DeleteArr(Box<CppExpr>),
    /// M17.0: `noexcept(expr)` 编译期 bool
    Noexcept(Box<CppExpr>),
    BinOp(BinOpKind, Box<CppExpr>, Box<CppExpr>),
    UnaryOp(UnaryOpKind, Box<CppExpr>),
    Ref(Box<CppExpr>),
    AddrOf(Box<CppExpr>),
    Cast(MangleType, Box<CppExpr>),
    /// `sizeof(T)` 或 `sizeof expr` (M8: 仅作常量返回)
    Sizeof(SizeofArg),
    /// `typeid(e)` (M8) -> M17.0 真 RTTI
    Typeid(Box<CppExpr>),
    /// M17.0: `typeid(T)` 静态类型 RTTI (无运行时, 取静态 typeinfo)
    TypeidType(MangleType),
    /// `dynamic_cast<T*>(e)` (M8)
    DynamicCast {
        ty: MangleType,
        expr: Box<CppExpr>,
    },
    /// M18: C++23 多维下标 `obj[i, j, k]` -> `MultiIndex(obj, [i, j, k])`
    /// lower 阶段改写为 C 函数调用 `__dcc_multi_index_<n>(obj, ...)`
    MultiIndex(Box<CppExpr>, Vec<CppExpr>),
    /// `static_cast<T>(e)` (M8)
    StaticCast {
        ty: MangleType,
        expr: Box<CppExpr>,
    },
    /// `const_cast<T>(e)` (M8)
    ConstCast {
        ty: MangleType,
        expr: Box<CppExpr>,
    },
    /// `reinterpret_cast<T>(e)` (M8)
    ReinterpretCast {
        ty: MangleType,
        expr: Box<CppExpr>,
    },
    /// Lambda 表达式 (M8: 降级为闭包类型)
    Lambda(LambdaExpr),
    /// 运算符调用 (M8: 解析时合成, lower 时按运算符分派)
    OpCall {
        op: crate::cpp::mangle::Operator,
        /// 显式形参 (含左操作数); 一元/转型时只 1 个
        args: Vec<CppExpr>,
    },
    /// 三元 `cond ? a : b` (M8: 解析器支持, 留作降级)
    Ternary {
        cond: Box<CppExpr>,
        then: Box<CppExpr>,
        els: Box<CppExpr>,
    },
    /// 逗号 `(a, b)` (M8: 表达式级)
    Comma(Vec<CppExpr>),
    /// 字符串字面量拼接: `"a" "b"` -> "ab"
    StrConcat(Vec<String>),
}

#[derive(Debug, Clone)]
pub enum SizeofArg {
    Type(MangleType),
    Expr(Box<CppExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    EqEq,
    Lt,
    Assign,
    Shl,
    Shr,
    Gt,
    Le,
    Ge,
    Ne,
    And,
    Or,
    Xor,
    LAnd,
    LOr,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ShlAssign,
    ShrAssign,
    AndAssign,
    OrAssign,
    XorAssign,
    /// 索引下标 `a[b]` (M8: 解析为 BinOp)
    Index,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOpKind {
    Neg,
    Not,
    Plus,
    LNot,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
    Deref,
}

/// M17.0: skip_specifiers 返回三态 (override / final / noexcept)
#[derive(Debug, Clone, Copy, Default)]
pub struct SpecKinds {
    pub is_override: bool,
    pub is_final: bool,
    pub is_noexcept: bool,
}

/// C++ 解析器
pub struct CppParser<'a> {
    pub lex: CppLexer<'a>,
    pub peek: Option<CppTok>,
    pub file: String,
    pub diags: Vec<Diagnostic>,
    /// 解析期间记下的 lambda (M8: lower 时分配闭包类型名)
    pub lambdas: Vec<LambdaExpr>,
    /// 嵌套模板 `>>` 拆出的待闭合 `>` 计数 (M18.x, 见 parse_mangle_type_depth)
    deferred_gt: u32,
    /// M29: 模板实例 typedef 别名 `typedef Tpl<Args> Alias;`
    /// (Alias -> 模板名 + 实参). 表达式 `Alias::member` 展开为
    /// TplStaticAccess, 以触发实例化并引用带前缀的常量.
    pub typedef_alias: std::collections::HashMap<String, (String, Vec<MangleType>)>,
    /// M30.4: 匿名聚合体计数器, 为每个匿名 union/struct 生成唯一名
    /// (`__anon_aggr_1`, `__anon_aggr_2`, ...) 避免同名导致构造函数/
    /// typeinfo/typedef 符号重复.
    anon_counter: u32,
}

impl<'a> CppParser<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        let mut lex = CppLexer::new(src, file);
        let peek = lex.next_tok();
        Self {
            lex,
            peek,
            file: file.to_string(),
            diags: vec![],
            lambdas: vec![],
            deferred_gt: 0,
            typedef_alias: std::collections::HashMap::new(),
            anon_counter: 0,
        }
    }

    fn bump(&mut self) -> Option<CppTok> {
        let cur = self.peek.take();
        self.peek = self.lex.next_tok();
        cur
    }

    fn span(&self) -> Span {
        match &self.peek {
            Some(t) => t.span(),
            None => Span {
                file: self.file.clone(),
                line: 0,
                col: 0,
                len: 1,
            },
        }
    }

    fn at(&self, kw: CppKw) -> bool {
        matches!(&self.peek, Some(CppTok::Kw(k, _)) if *k == kw)
    }

    /// class/struct 聚合关键字嗅探 (C++ 中 class 与 struct 等价;
    /// struct 可能来自 C lexer 的 KwStruct 或裸 Ident)
    fn at_aggr_kw(&self) -> bool {
        match &self.peek {
            Some(CppTok::Kw(CppKw::Class, _)) => true,
            Some(CppTok::C(Tok {
                kind: TokKind::KwStruct,
                ..
            })) => true,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => text == "struct",
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn at_c_kw(&self, _kind: TokKind) -> bool {
        matches!(&self.peek, Some(CppTok::C(Tok { .. })))
    }

    /// 当前 token 是否为指定 C token 种类.
    fn at_tok(&self, kind: TokKind) -> bool {
        matches!(&self.peek, Some(CppTok::C(Tok { kind: k, .. })) if *k == kind)
    }

    /// 当前 token 是否是 `[`.
    fn peek_is_lbracket(&self) -> bool {
        self.at_tok(TokKind::LBracket)
    }

    /// 期望指定 C token 并消耗; 不匹配时返回 Unexpected 错误.
    fn expect_tok(&mut self, kind: TokKind) -> Result<(), CppParseError> {
        if self.at_tok(kind) {
            self.bump();
            Ok(())
        } else {
            Err(CppParseError::Unexpected(
                "expected token",
                format!("{:?}", self.peek),
                self.span(),
            ))
        }
    }

    /// 取 token 的源文本 (C token 用 text; 关键字用关键字名).
    fn token_text(&self, t: &CppTok) -> String {
        match t {
            CppTok::C(Tok { text, .. }) => text.clone(),
            CppTok::Kw(kw, _) => kw.as_str().to_string(),
            CppTok::ColonColon(_) => "::".to_string(),
        }
    }

    /// M30.2: 跳过 GNU 声明尾缀 `__attribute__ (( ... ))` / `__asm__ (
    /// ... )` (可连续多组; glibc 头的 noexcept/leaf/mode/asm 重命名等,
    /// 尾缀不参与语义, 仅需从 token 流移除).
    fn skip_gnu_suffixes(&mut self) {
        loop {
            let is_gnu = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) => {
                    text == "__attribute__"
                        || text == "__asm__"
                        || text == "__asm"
                        || text == "asm"
                }
                _ => false,
            };
            if !is_gnu {
                break;
            }
            self.bump();
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                let mut depth = 0i32;
                while let Some(t) = self.peek.clone() {
                    match t {
                        CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        }) => {
                            depth += 1;
                            self.bump();
                        }
                        CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {
                            self.bump();
                        }
                    }
                }
            }
        }
    }

    fn expect_ident(&mut self) -> Result<String, CppParseError> {
        match self.bump() {
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => Ok(text),
            Some(other) => {
                let got = format!("{:?}", other);
                Err(CppParseError::Unexpected("identifier", got, other.span()))
            }
            None => Err(CppParseError::Unexpected(
                "identifier",
                "EOF".into(),
                self.span(),
            )),
        }
    }

    fn expect_kw(&mut self, kw: CppKw) -> Result<Span, CppParseError> {
        if self.at(kw) {
            let sp = self.peek.as_ref().unwrap().span();
            self.bump();
            Ok(sp)
        } else {
            let got = format!("{:?}", self.peek);
            Err(CppParseError::Unexpected(kw.as_str(), got, self.span()))
        }
    }

    fn parse_qualified_name(&mut self) -> Result<Vec<String>, CppParseError> {
        let mut parts = vec![];
        // 支持 `::name` (全局作用域) 与 `std::name` (命名空间) 等
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump();
            parts.push("".to_string());
        }
        let first = self.expect_ident()?;
        parts.push(first);
        while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump();
            parts.push(self.expect_ident()?);
        }
        Ok(parts)
    }

    /// 窥探 lexer 中第 n 个 token (n=0 表示当前 token, 1 表示下一个, ...)
    fn peek_token_at(&self, n: usize) -> Option<CppTok> {
        if n == 0 {
            return self.peek.clone();
        }
        let mut clone = self.lex.clone();
        let mut result = None;
        for _ in 0..n {
            result = clone.next_tok();
            if result.is_none() {
                return result;
            }
        }
        result
    }

    /// `<` 在 ident 后是否像模板实参 `T<U>` 而非比较 `a < b`.
    /// 向前扫描:
    /// - 跨过嵌套 `<`/`>` (Shr=`>>` 视为两层 `>`)
    /// - 遇到 `;` / unmatched `)` / EOF 在 depth==1 仍未闭合 `>` → 比较
    /// - 遇到合法类型 token (ident / C 类型关键字 / `*` / `&` / `,` / `const`)
    ///   不直接判否; 只有破坏 C++ 表达式的 token 才判否
    fn looks_like_template_args_after_lt(&self) -> bool {
        let mut depth: i32 = 1;
        let mut i = 1usize; // self.peek 已是 `<`
        loop {
            match self.peek_token_at(i) {
                None => return false,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                })) => {
                    depth += 1;
                    // 嵌套深度超过限制: 提前判定为模板实参,
                    // 由 parse_mangle_type_depth 报深度错误 (防 O(n^2) 扫描挂起)
                    if depth > MAX_TEMPLATE_DEPTH as i32 {
                        return true;
                    }
                    i += 1;
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                })) => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                    i += 1;
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Shr, ..
                })) => {
                    // `>>` 当两层 `>`
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                    i += 1;
                }
                // 仅在 depth==1 遇到语句/表达式终结符才判否
                // 逗号在 depth==1 是模板实参分隔符, 合法
                Some(CppTok::C(Tok {
                    kind:
                        TokKind::Semi
                        | TokKind::RParen
                        | TokKind::LBrace
                        | TokKind::RBrace
                        | TokKind::Semi,
                    ..
                })) if depth == 1 => {
                    return false;
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign | TokKind::Question | TokKind::Colon,
                    ..
                })) if depth == 1 => {
                    // `a < b ? c : d` / `a < b = c` 是表达式
                    return false;
                }
                _ => {
                    i += 1;
                }
            }
        }
    }

    /// 顶层解析
    pub fn parse_top_level(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let mut decls = vec![];
        while self.peek.is_some() {
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
                continue;
            }
            match self.parse_decl() {
                Ok(d) => decls.push(d),
                Err(e) => {
                    // Record error diagnostic for fallback detection
                    self.diags.push(e.to_diag(&self.file));
                    // 容错: 跳过无法解析的顶层构造, 到下一个 ';' 或 '}'
                    // 使 C++ 前端能处理包含复杂系统头文件的 GCC 构建
                    self.skip_to_next_toplevel();
                }
            }
        }
        Ok(decls)
    }

    /// 跳过 token 直到下一个顶层声明起点 (';' 或匹配所有括号后)
    fn skip_to_next_toplevel(&mut self) {
        let mut depth = 0i32;
        while let Some(tok) = &self.peek {
            match tok {
                CppTok::C(Tok { kind: TokKind::Semi, .. }) if depth <= 0 => {
                    self.bump();
                    return;
                }
                CppTok::C(Tok { kind: TokKind::LBrace, .. })
                | CppTok::C(Tok { kind: TokKind::LParen, .. })
                | CppTok::C(Tok { kind: TokKind::Lt, .. }) => {
                    depth += 1;
                    self.bump();
                }
                CppTok::C(Tok { kind: TokKind::RBrace, .. })
                | CppTok::C(Tok { kind: TokKind::RParen, .. })
                | CppTok::C(Tok { kind: TokKind::Gt, .. }) => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.bump();
                    if depth <= 0 {
                        return;
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// M30.2: `struct/union [Name]` 后是否是聚合定义/前向/继承形态
    /// (`{` / `:` / `;`)；`struct tm *gmtime(...)` 等类型使用形态返回
    /// false，交由函数/变量声明解析。
    fn aggr_looks_like_def(&self) -> bool {
        match self.peek_token_at(1) {
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace | TokKind::Colon | TokKind::Semi,
                ..
            })) => true,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })) => matches!(
                self.peek_token_at(2),
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace | TokKind::Colon | TokKind::Semi,
                    ..
                }))
            ),
            _ => false,
        }
    }

    /// M30.2: 跳过 token 直到成员声明结束 (`;` 或平衡所有括号后的 `}`).
    /// 用于类/结构体成员解析失败时的容错恢复.
    fn skip_to_member_end(&mut self) {
        let mut depth = 0i32;
        while let Some(tok) = &self.peek {
            match tok {
                CppTok::C(Tok { kind: TokKind::Semi, .. }) if depth <= 0 => {
                    self.bump();
                    return;
                }
                CppTok::C(Tok { kind: TokKind::RBrace, .. }) if depth <= 0 => {
                    // 不消耗 `}`, 留给外层循环
                    return;
                }
                CppTok::C(Tok { kind: TokKind::LBrace, .. })
                | CppTok::C(Tok { kind: TokKind::LParen, .. })
                | CppTok::C(Tok { kind: TokKind::Lt, .. }) => {
                    depth += 1;
                    self.bump();
                }
                CppTok::C(Tok { kind: TokKind::RBrace, .. })
                | CppTok::C(Tok { kind: TokKind::RParen, .. })
                | CppTok::C(Tok { kind: TokKind::Gt, .. }) => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// 解析一条声明
    fn parse_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // typedef <type> <name>; (M11 stdlib: 解析为 no-op 占位)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwTypedef,
                ..
            }))
        ) {
            return self.parse_typedef_decl();
        }
        // 模板声明 (M8: 简化为占位)
        if self.at(CppKw::Template) {
            return self.parse_template_decl(false);
        }
        // 命名空间 (M8)
        if self.at(CppKw::Namespace) {
            return self.parse_namespace_decl();
        }
        // using 声明 (M8)
        if self.at(CppKw::Using) {
            return self.parse_using_decl();
        }
        // C++20 module / export module (M9: 占位)
        if self.at(CppKw::Module) {
            return self.parse_module_decl();
        }
        // C++20 import (M9: 占位)
        if self.at(CppKw::Import) {
            return self.parse_import_decl();
        }
        // C++20 export { ... } (M9: 占位)
        if self.at(CppKw::Export) {
            return self.parse_export_decl();
        }
        // M18.0: enum (含 scoped enum class) 占位解析
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwEnum,
                ..
            }))
        ) {
            return self.parse_enum_decl();
        }
        // C++20 concept (M9: 占位, 仅顶层)
        if self.at(CppKw::Concept) {
            return self.parse_concept_decl();
        }
        // extern "C" { ... } 块 (M11 stdlib: C 头引用)
        if self.is_extern_c_block_start() {
            return self.parse_extern_c_block();
        }
        // M30.2: `extern` 链接性说明 + 普通声明 (`extern void f(...);`,
        // `extern T g;`) - 无 "C" 字面量时跳过 extern, 按普通声明解析.
        // 之前 KwExtern 不在任何分发分支, 直接落到 declaration form 错误.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwExtern,
                ..
            }))
        ) && !matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::StringLit,
                ..
            }))
        ) {
            self.bump();
            return self.parse_decl();
        }
        // M30.2: `__extension__` GNU 前缀 - 无语义, 跳过后按普通声明
        // 解析 (`__extension__ typedef struct {...} lldiv_t;` 等).
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            if text == "__extension__" {
                self.bump();
                return self.parse_decl();
            }
        }
        // class / struct Name (M8: 可继承)
        // M30.2: `class Name *fn()` / `class Name var` 是 elaborated type
        // 说明符 (函数声明/变量声明), 不是类定义. 只有 `class Name {` /
        // `class Name :` / `class Name ;` 才是类定义/前向声明.
        if self.at(CppKw::Class) {
            let is_def = match self.peek_token_at(1) {
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace | TokKind::Colon | TokKind::Semi,
                    ..
                })) => true,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) => matches!(
                    self.peek_token_at(2),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBrace | TokKind::Colon | TokKind::Semi,
                        ..
                    }))
                ),
                _ => false,
            };
            if is_def {
                return self.parse_class_decl();
            }
            // elaborated type: fall through 到 free fn / global var
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            if text == "struct" {
                if self.aggr_looks_like_def() {
                    return self.parse_aggr_decl(false);
                }
                self.bump();
            }
        }
        // `struct` 作为 C 关键字直接出现 (dcc C lexer 把 struct 标为 KwStruct)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwStruct,
                ..
            }))
        ) && self.aggr_looks_like_def()
        {
            return self.parse_aggr_decl(false);
        }
        // M30.2: 顶层 union 定义 (`union U { ... };`) - 复用聚合解析,
        // is_union 标记 lower 发射 union 关键字. 之前 KwUnion 只在类型
        // 关键字列表里走变量声明回退, 遇 `{` 直接 declaration form 错误.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwUnion,
                ..
            }))
        ) && self.aggr_looks_like_def()
        {
            return self.parse_aggr_decl(true);
        }
        // 运算符重载: Type operator op ( ... )
        if self.is_operator_fn_start() {
            return self.parse_operator_fn();
        }
        // M18.0: 顶层限定名返回类型的自由函数 `views::filter_view f()` /
        // `inline std::ranges::filter_view f()` / `::std::foo f()`.
        if self.is_qualified_return_fn() {
            return self.parse_free_fn();
        }
        // 顶层: 全局函数 (返回类型 + 名 + 形参)
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            ..
        })) = &self.peek
        {
            return self.parse_free_fn();
        }
        // M9: 顶层声明以类型关键字开头 (`void f()`, `int g(int)`)
        // M11 stdlib: 支持返回类型带指针/引用, 如 `char* strcpy(char* dst, ...)`
        // 同时支持 C++ 关键字 (CppTok::Kw) 与 C 关键字 (CppTok::C)
        let is_type_kw_or_spec = matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwInt
                    | TokKind::KwChar
                    | TokKind::KwShort
                    | TokKind::KwLong
                    | TokKind::KwVoid
                    | TokKind::KwFloat
                    | TokKind::KwDouble
                    | TokKind::KwSigned
                    | TokKind::KwUnsigned
                    | TokKind::KwStruct
                    | TokKind::KwUnion
                    | TokKind::KwEnum
                    | TokKind::KwConst
                    | TokKind::KwVolatile
                    | TokKind::KwStatic
                    | TokKind::KwInline
                    | TokKind::KwBool
                    | TokKind::KwAuto
                    | TokKind::KwAtomic
                    | TokKind::KwRestrict,
                ..
            }))
        ) || matches!(
            &self.peek,
            Some(CppTok::Kw(
                CppKw::Inline | CppKw::Static | CppKw::Const | CppKw::Volatile | CppKw::Class,
                _
            ))
        );
        if is_type_kw_or_spec {
            // 区分: 顶层是自由函数 vs 顶层是带类型的变量声明
            // 跳过类型后所有可能的指针/引用修饰, 找到 ident 名字, 然后看是不是 `(`.
            let mut j = 1;
            // 跳过 const/volatile
            while matches!(
                self.peek_token_at(j),
                Some(CppTok::Kw(CppKw::Const | CppKw::Volatile, _))
            ) {
                j += 1;
            }
            // 跳过额外的类型关键字 (例: `inline void*` 中的 void, `long long`,
            // `unsigned char` 等) 或 typedef 标识符 (例: `FILE*`, `size_t`)
            while let Some(tok) = self.peek_token_at(j) {
                // C++ 关键字: inline, static, const, volatile, class
                if let CppTok::Kw(k, _) = &tok {
                    if matches!(
                        k,
                        CppKw::Inline | CppKw::Static | CppKw::Const | CppKw::Volatile
                    ) {
                        j += 1;
                        continue;
                    }
                    // M30.2: elaborated type `class Name` 在返回类型位置
                    // (`class mkdeps *cpp_get_deps(...)`).
                    if matches!(k, CppKw::Class) {
                        j += 1;
                        // 吞类名标识符
                        if matches!(
                            self.peek_token_at(j),
                            Some(CppTok::C(Tok {
                                kind: TokKind::Ident,
                                ..
                            }))
                        ) {
                            j += 1;
                        }
                        continue;
                    }
                }
                // C 关键字: void, int, char, ...
                if let CppTok::C(t) = &tok {
                    if matches!(
                        t.kind,
                        TokKind::KwVoid
                            | TokKind::KwInt
                            | TokKind::KwChar
                            | TokKind::KwShort
                            | TokKind::KwLong
                            | TokKind::KwFloat
                            | TokKind::KwDouble
                            | TokKind::KwSigned
                            | TokKind::KwUnsigned
                            | TokKind::KwInline
                            | TokKind::KwBool
                    ) {
                        j += 1;
                        continue;
                    }
                    // M30.2: elaborated type `struct S` / `union U` / `enum E`
                    // 在返回类型位置 (如 `const struct line_map *fn(...)`).
                    // 吞关键字 + 后续标识符 (类型名).
                    if matches!(
                        t.kind,
                        TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum
                    ) {
                        j += 1;
                        // 吞可选的类型名标识符
                        if matches!(
                            self.peek_token_at(j),
                            Some(CppTok::C(Tok {
                                kind: TokKind::Ident,
                                ..
                            }))
                        ) {
                            j += 1;
                        }
                        continue;
                    }
                }
                // typedef / 用户类型 ident: 仅当后面紧跟 `*` / `&` / 另一个 ident
                // 或模板实参 `<...>` 时认为是类型 (例: `FILE*`, `size_t f`, `MyType&`, `Holder<int> f`).
                if let CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }) = &tok
                {
                    let next = self.peek_token_at(j + 1);
                    if matches!(
                        next,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star | TokKind::Amp,
                            ..
                        })) | Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        }))
                    ) {
                        j += 1;
                        continue;
                    }
                    // M26: `Holder<int>` 模板类型 (跳过后再验证是函数名)
                    if matches!(
                        next,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Lt,
                            ..
                        }))
                    ) {
                        if let Some(after) = self.skip_template_args_at(j + 1) {
                            j = after;
                            continue;
                        }
                    }
                }
                break;
            }
            // 跳过 * 和 &
            loop {
                while matches!(
                    self.peek_token_at(j),
                    Some(CppTok::Kw(CppKw::Const | CppKw::Volatile, _))
                ) {
                    j += 1;
                }
                if matches!(
                    self.peek_token_at(j),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    }))
                ) {
                    j += 1;
                } else {
                    break;
                }
            }
            let name_pos = j;
            let name_tok = self.peek_token_at(name_pos);
            let is_name_ident = matches!(
                name_tok,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            );
            // M30.2: 类外成员定义 `Ret Class::method(` / `Ret C::~C(` -
            // 名字段为 `Ident (:: Ident)+ (` (析构中间带 ~); 此前只认
            // `Ident (` 导致 libcpp/line-map.cc 等整文件降级 strip.
            let is_qualified_fn = is_name_ident
                && matches!(self.peek_token_at(name_pos + 1), Some(CppTok::ColonColon(_)))
                && (matches!(
                    self.peek_token_at(name_pos + 2),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        ..
                    }))
                ) || matches!(
                    self.peek_token_at(name_pos + 2),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Tilde,
                        ..
                    }))
                ))
                && matches!(
                    self.peek_token_at(name_pos + 3),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident | TokKind::LParen,
                        ..
                    }))
                );
            let is_fn = (is_name_ident
                && matches!(
                    self.peek_token_at(name_pos + 1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                ))
                || is_qualified_fn;
            if is_fn {
                return self.parse_free_fn();
            }
            if self.is_operator_fn_start() {
                return self.parse_operator_fn();
            }
        }
        // M30: 类型关键字起始的全局变量声明 `int g = 5;` /
        // `static unsigned long h = 7;`. 之前直接 Unsupported → 整个
        // 文件降级 strip-keywords 模式; 现在显式解析 (失败仍报原错).
        {
            let saved_lex = self.lex.clone();
            let saved_peek = self.peek.clone();
            match self.parse_global_var_decl() {
                Ok(v) => return Ok(v),
                Err(_) => {
                    self.lex = saved_lex;
                    self.peek = saved_peek;
                }
            }
        }
        Err(CppParseError::Unsupported("declaration form", self.span()))
    }

    /// 解析 template <typename T> ...
    /// is_export: `export template` 时为 true
    fn parse_template_decl(&mut self, is_export: bool) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Template)?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "<",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut params = vec![];
        loop {
            // class / typename Name (= default)?
            // M18.0: 接受 CppKw::Class (C++ 词法) 和 Ident "class" (C 词法)
            let has_kw = self.at(CppKw::Typename)
                || self.at(CppKw::Class)
                || matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) if text == "class");
            if has_kw {
                self.bump();
            }
            // M18.x: `typename = enable_if_t<...>` 匿名默认形参 (SFINAE 惯用法),
            // 形参名缺省时生成内部占位名.
            if has_kw
                && matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Assign,
                        ..
                    }))
                )
            {
                self.bump();
                let anon = format!("_anon{}", params.len());
                let default = Some(self.parse_mangle_type()?);
                params.push(TemplateParam {
                    name: anon,
                    default,
                    non_type: None,
                });
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Comma,
                        ..
                    }))
                ) {
                    self.bump();
                    continue;
                }
                break;
            }
            // M30.1: 非类型形参 (`template <typename T, bool B>` /
            // `<int N>` / `<unsigned long N>` / `<size_t N>`).
            // 尝试 <类型> <名> 形态; 失败回退原路径 (保留旧容忍度).
            let non_type = if has_kw {
                None
            } else {
                let saved_lex = self.lex.clone();
                let saved_peek = self.peek.clone();
                match self.parse_mangle_type() {
                    Ok(ty) if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok { kind: TokKind::Ident, .. }))
                    ) => Some(ty),
                    _ => {
                        self.lex = saved_lex;
                        self.peek = saved_peek;
                        None
                    }
                }
            };
            let name = self.expect_ident()?;
            let default = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                // M30.1: 非类型形参默认值是编译期字面量 (`= true` / `= 42`)
                if non_type.is_some() {
                    Some(self.parse_non_type_value()?)
                } else {
                    Some(self.parse_mangle_type()?)
                }
            } else {
                None
            };
            params.push(TemplateParam {
                name,
                default,
                non_type,
            });
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }))
            ) {
                self.bump();
            } else {
                break;
            }
        }
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Gt,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                ">",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        // M18.0: `template <T> requires Concept<T>` 后跟 `requires` 子句
        // M26: 约束文本保存到 CppDecl::Template, 实例化时求值过滤
        let requires = self.parse_requires_clause();
        // M10+: 检测 partial specialization 模式
        // `class Foo<T*>` 或 `class Foo<int>` (区别于普通 `class Foo`)
        // class 与 struct 等价, 两者都要支持偏特化
        if self.at_aggr_kw() {
            // 窥探: class Name <   (偏特化候选)
            let saved_lex = self.lex.clone();
            let saved_peek = self.peek.clone();
            let is_struct = !self.at(CppKw::Class);
            self.bump(); // class/struct
            let _name = self.expect_ident()?;
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) {
                // 是 partial specialization
                // 收集 <...> 内的所有 token 文本为 pattern_text
                self.bump(); // <
                let mut pattern_text = String::new();
                let mut depth: i32 = 1;
                while depth > 0 {
                    let t = self.bump();
                    match t {
                        Some(CppTok::C(Tok {
                            kind: TokKind::Lt, ..
                        })) => {
                            pattern_text.push('<');
                            depth += 1;
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Gt, ..
                        })) => {
                            depth -= 1;
                            if depth > 0 {
                                pattern_text.push('>');
                            }
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Shr, ..
                        })) => {
                            depth -= 1;
                            if depth > 0 {
                                pattern_text.push_str(">>");
                            }
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Colon,
                            ..
                        })) => {
                            pattern_text.push(':');
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        })) => {
                            pattern_text.push(',');
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star,
                            ..
                        })) => {
                            pattern_text.push('*');
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            text,
                            ..
                        })) => {
                            pattern_text.push_str(&text);
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::Number,
                            text,
                            ..
                        })) => {
                            pattern_text.push_str(&text);
                        }
                        Some(CppTok::ColonColon(_)) => {
                            pattern_text.push_str("::");
                        }
                        // M30.1: true/false 字面量转回文本 (非类型偏特化
                        // pattern 如 `picker<T, false>` 需要与 pick_partial
                        // 的 NonType key 匹配; Debug 格式含逗号会破坏
                        // pattern_text 的 split(',') 约定)
                        Some(CppTok::Kw(CppKw::True, _)) => {
                            pattern_text.push_str("true");
                        }
                        Some(CppTok::Kw(CppKw::False, _)) => {
                            pattern_text.push_str("false");
                        }
                        Some(other) => {
                            // 其他 token 转为文本
                            let s = format!("{:?}", other);
                            pattern_text.push_str(&s);
                        }
                        None => break,
                    }
                    if depth > 0 {
                        pattern_text.push(' ');
                    }
                }
                // 继续解析类体 (或函数模板特化)
                // 复用 parse_class_decl 流程
                let bases = self.parse_base_list()?;
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBrace,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        "{",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump(); // {
                let mut members = vec![];
                while let Some(t) = &self.peek {
                    if matches!(
                        t,
                        CppTok::C(Tok {
                            kind: TokKind::RBrace,
                            ..
                        })
                    ) {
                        break;
                    }
                    members.push(self.parse_member_decl_with_class(&_name)?);
                }
                self.bump(); // }
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    }))
                ) {
                    self.bump();
                }
                let decl = CppDecl::Class(ClassDecl {
                    name: _name,
                    is_struct,
                    is_union: false,
                    bases,
                    members,
                });
                return Ok(CppDecl::PartialSpec {
                    params: TemplateArgs { params },
                    pattern_text: pattern_text.trim().to_string(),
                    decl: Box::new(decl),
                });
            } else {
                // 不是 partial spec, 恢复 lex 状态
                self.lex = saved_lex;
                self.peek = saved_peek;
            }
        }
        let decl = self.parse_decl()?;
        Ok(CppDecl::Template {
            params: TemplateArgs { params },
            decl: Box::new(decl),
            requires,
            is_export,
        })
    }

    /// 解析 namespace foo { ... } 或 namespace { ... }
    fn parse_namespace_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Namespace)?;
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            // inline 关键字 (C++17) 跳过
            if text == "inline" {
                self.bump();
            }
        }
        let real_name: Option<String> = if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            if text != "{" {
                Some(self.expect_ident()?)
            } else {
                None
            }
        } else {
            None
        };
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut body = vec![];
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
            ) {
                break;
            }
            body.push(self.parse_decl()?);
        }
        self.bump();
        Ok(CppDecl::Namespace {
            name: real_name,
            body,
        })
    }

    /// 解析 using namespace std; 或 using std::cout; 或 using Base::Base;
    /// 或 using Name = Type; (C++11 类型别名)
    /// 或 using enum Color; (C++20)
    fn parse_using_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Using)?;
        // C++20: using enum Name;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwEnum,
                ..
            }))
        ) {
            self.bump(); // consume 'enum'
            let name = self.expect_ident()?;
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                self.bump();
            }
            return Ok(CppDecl::UsingEnum { name });
        }
        let is_namespace = if self.at(CppKw::Namespace) {
            self.bump();
            true
        } else {
            false
        };
        let path = self.parse_qualified_name()?;
        // C++11: using Name = Type; (类型别名声明)
        if !is_namespace && path.len() == 1 {
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Assign, .. }))) {
                self.bump(); // consume '='
                let target = self.parse_mangle_type()?;
                if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                    self.bump();
                }
                // Record in typedef_alias for template resolution
                self.typedef_alias.insert(path[0].clone(),
                    (format!("{:?}", target), vec![]));
                return Ok(CppDecl::Typedef { target, name: path[0].clone() });
            }
        }
        // M10+: using Base::Base 形式 (继承构造)
        if path.len() == 2 && path[0] == path[1] {
            // `using Base::Base;` 继承构造
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppDecl::InheritingCtor {
                class: String::new(), // 占位, 实际由类外解析注入
                base: path[0].clone(),
            });
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::Using { is_namespace, path })
    }

    /// 解析 `module foo;` / `module foo:part1;` / `module;` / `module :private;` / `export module foo;` (M9 占位)
    fn parse_module_decl(&mut self) -> Result<CppDecl, CppParseError> {
        let is_export = false; // 顶层 parse_decl 不带 export 前缀
        self.expect_kw(CppKw::Module)?;
        let mut name: Option<String> = None;
        let mut partition: Option<String> = None;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            // M26.4: `module :private;` - 私有片段 (之后声明不导出)
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Colon,
                    ..
                }))
            ) {
                self.bump(); // 吃掉 ':'
                if self.at(CppKw::Private) {
                    self.bump();
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Semi,
                            ..
                        }))
                    ) {
                        self.bump();
                    }
                    return Ok(CppDecl::PrivateFragment);
                }
                // `module :part;` 无名分区声明 (标准中少见于主模块接口)
                partition = Some(self.expect_ident()?);
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    }))
                ) {
                    self.bump();
                }
                return Ok(CppDecl::Module {
                    name,
                    partition,
                    is_export,
                });
            }
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) = &self.peek
            {
                if text == ":" {
                    // 跳过 : 之后的 partition 名 (M9 占位)
                    let _ = self.parse_qualified_name();
                } else {
                    name = Some(self.expect_ident()?);
                    // M10+: 检查是否有 : 分区符 (注意: 是 `:` 单冒号, 不是 `::`)
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Colon,
                            ..
                        }))
                    ) {
                        self.bump(); // 吃掉 :
                        partition = Some(self.expect_ident()?);
                    }
                }
            }
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::Module {
            name,
            partition,
            is_export,
        })
    }

    /// 解析 `import foo;` / `import <iostream>;` / `import "header.h";` / `import :part;` (M9 占位)
    fn parse_import_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Import)?;
        let path = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            // M26.4: `import :part;` - 分区导入 (path 记作 ":part")
            self.bump(); // ':'
            vec![format!(":{}", self.expect_ident()?)]
        } else if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) {
            self.bump();
            let mut parts = vec![];
            loop {
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Gt,
                        ..
                    }))
                ) {
                    break;
                }
                parts.push(text_of(&self.peek).to_string());
                self.bump();
            }
            self.bump(); // eat >
            vec![parts.join("")]
        } else if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::StringLit,
                ..
            }))
        ) {
            let s = text_of(&self.peek).to_string();
            self.bump();
            vec![s]
        } else {
            self.parse_qualified_name()?
        };
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::Import { path })
    }

    /// 解析 `export { ... }` / `export module foo;` / `export template <...>` (M9 占位)
    fn parse_export_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Export)?;
        // M26: `export template <...>` - export 修饰模板声明, 标记 is_export=true
        if self.at(CppKw::Template) {
            return self.parse_template_decl(true);
        }
        // M18.0: `export int f() {}` - export 修饰函数声明, 透传.
        // 形式: export 后面不是 `module` 或 `{`, 让上层重新解析.
        if !self.at(CppKw::Module)
            && !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }))
            )
        {
            // 重新进入 parse_decl 继续解析声明体 (e.g. `export int f() {...}`)
            return self.parse_decl();
        }
        // export module foo; / export module foo:part1;
        if self.at(CppKw::Module) {
            self.bump();
            let mut name: Option<String> = None;
            let mut partition: Option<String> = None;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) = &self.peek
                {
                    if text == ":" {
                        let _ = self.parse_qualified_name();
                    } else {
                        name = Some(self.expect_ident()?);
                        // M10+: 支持 export module foo:part1;
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Colon,
                                ..
                            }))
                        ) {
                            self.bump();
                            partition = Some(self.expect_ident()?);
                        }
                    }
                }
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppDecl::Module {
                name,
                partition,
                is_export: true,
            });
        }
        // export { decls }
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut body = vec![];
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
            ) {
                break;
            }
            body.push(self.parse_decl()?);
        }
        self.bump();
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::ExportBlock(body))
    }

    /// 解析 `concept Name = ...;` (M9 占位)
    fn parse_concept_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Concept)?;
        let name = self.expect_ident()?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "=",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        // 简化: 把 = 之后到 ; 之前的 token 收为 body_text (M9 不解析表达式)
        let mut body_text = String::new();
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                })
            ) {
                break;
            }
            body_text.push_str(text_of(&self.peek));
            body_text.push(' ');
            self.bump();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::Concept {
            name,
            body_text: body_text.trim().to_string(),
        })
    }

    /// 解析 `requires` 后缀 (M9 占位: 跳过 requires expr)
    /// M18.0: 修正为跟踪所有括号深度 (< > ( [ { ), 顶层遇到 ; / { / } 才停止.
    /// 旧版只跟踪 () [] {} 三种, 对 `requires Integral<T>` 后的
    /// `T identity(T x) { ... }` 会错误地把整个函数体吃掉.
    /// 进一步修正: 顶层遇到 `>` 且后跟 ident / * / & 视作 requires expr
    /// 结束 (因为 function 形参/返回类型可能也含 `>`, 但不会紧跟 `>` 后是 ident).
    /// M26: 解析 requires 子句并返回约束文本 (未命中返回 None).
    /// 约束文本从 token 重建 (去掉空白, 保留 `&&` / `||` / `<` / `>`).
    fn parse_requires_clause(&mut self) -> Option<String> {
        if !self.at(CppKw::Requires) {
            return None;
        }
        self.bump();
        let mut text = String::new();
        // 跳过到顶层 ; / { / } 为止. 跟踪 () [] {} <> 深度.
        let mut depth = 0i32;
        loop {
            let t = self.peek.clone();
            match t {
                None => break,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                })) if depth == 0 => break,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })) if depth == 0 => break,
                Some(CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })) if depth == 0 => break,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen | TokKind::LBracket | TokKind::LBrace,
                    ..
                })) => {
                    depth += 1;
                    self.bump();
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                })) => {
                    depth += 1;
                    self.bump();
                    text.push('<');
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen | TokKind::RBracket | TokKind::RBrace,
                    ..
                })) => {
                    depth -= 1;
                    self.bump();
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                })) => {
                    depth -= 1;
                    self.bump();
                    // 顶层 > 后若紧跟 ident/* /& 视作 function 声明开始, 停止 requires
                    if depth == 0
                        && matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Ident,
                                ..
                            })) | Some(CppTok::C(Tok {
                                kind: TokKind::Star,
                                ..
                            })) | Some(CppTok::C(Tok {
                                kind: TokKind::Amp,
                                ..
                            }))
                        )
                    {
                        break;
                    }
                    text.push('>');
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text: id,
                    ..
                })) => {
                    self.bump();
                    text.push_str(&id);
                    // 在 ident 之间加空格, 避免 `T::value` 粘连成 `T::value` 之外的拼写
                    if !matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        })) | Some(CppTok::ColonColon(_))
                    ) {
                        text.push(' ');
                    }
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Number,
                    text: n,
                    ..
                })) => {
                    self.bump();
                    text.push_str(&n);
                }
                Some(CppTok::ColonColon(_)) => {
                    self.bump();
                    text.push_str("::");
                }
                _ => {
                    self.bump();
                }
            }
        }
        Some(text.trim().to_string())
    }

    fn parse_class_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Class)?;
        let name = self.expect_ident()?;
        // M30.2: 前置声明 `class X;` - no-op (Typedef 占位, lower 不输出;
        // 后续完整定义正常处理). GCC 头大量使用前向声明.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppDecl::Typedef {
                target: MangleType::Class(name.clone()),
                name,
            });
        }
        let bases = self.parse_base_list()?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut members = vec![];
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
            ) {
                break;
            }
            match self.parse_member_decl_with_class(&name) {
                Ok(d) => members.push(d),
                Err(e) => {
                    // M30.2: 记录错误但继续解析, 跳过无法解析的成员到下一个
                    // `;` 或 `}` (与 parse_top_level 容错一致).
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                }
            }
        }
        self.bump();
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::Class(ClassDecl {
            name,
            is_struct: false,
            is_union: false,
            bases,
            members,
        }))
    }

    /// 解析 struct/union 聚合声明 (M30.2: is_union 区分 union 关键字)
    fn parse_aggr_decl(&mut self, is_union: bool) -> Result<CppDecl, CppParseError> {
        // 'struct'/'union' already peeked
        self.bump();
        // M30.2: tag 名可选 (`union { ... } temp;` 匿名定义实例成员)
        let name = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) {
            Some(self.expect_ident()?)
        } else {
            None
        };
        // M30.2: 前置声明 `struct X;` - 同 class, no-op 占位.
        if let Some(n) = &name {
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
                return Ok(CppDecl::Typedef {
                    target: MangleType::Class(n.clone()),
                    name: n.clone(),
                });
            }
        }
        let bases = self.parse_base_list()?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        // M30.4: 匿名聚合体分配唯一名 (`__anon_aggr_1`, `__anon_aggr_2`, ...)
        // 避免多个匿名 union/struct 共享同名导致构造函数/typeinfo 重复定义.
        let anon_name = name.clone().unwrap_or_else(|| {
            self.anon_counter += 1;
            format!("__anon_aggr_{}", self.anon_counter)
        });
        let ctx_name = anon_name.clone();
        let mut members = vec![];
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
            ) {
                break;
            }
            match self.parse_member_decl_with_class(&ctx_name) {
                Ok(d) => members.push(d),
                Err(e) => {
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                }
            }
        }
        self.bump();
        // M30.2: 定义后实例名 (`union { ... } temp;` / `struct X {} x;`)
        let inst = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) {
            Some(self.expect_ident()?)
        } else {
            None
        };
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        match (name, inst) {
            // 匿名定义 + 实例: AnonAggr 一体发射 (实例名保留, 成员
            // 字段引用 `h->temp.i` 在 C 中合法).
            (None, Some(inst)) => Ok(CppDecl::AnonAggr {
                is_union,
                members,
                name: inst,
            }),
            // 带 tag 定义 (变量实例罕见, 忽略实例名)
            (Some(n), _) => Ok(CppDecl::Class(ClassDecl {
                name: n,
                is_struct: true,
                is_union,
                bases,
                members,
            })),
            // M30.4: 匿名纯定义: 使用唯一占位名 (避免同名构造函数冲突)
            (None, None) => Ok(CppDecl::Class(ClassDecl {
                name: anon_name,
                is_struct: true,
                is_union,
                bases,
                members,
            })),
        }
    }

    /// 带类上下文的成员解析: 用于类内 inline 成员函数
    fn parse_member_decl_with_class(&mut self, class: &str) -> Result<CppDecl, CppParseError> {
        let saved = self.peek.clone();
        let r = self.parse_member_decl()?;
        // 注入类名到 MemberFn / Ctor / Dtor / OperatorFn
        Ok(inject_class_name(r, class, &saved))
    }

    /// 解析基类列表: `: public Base, private virtual B2`
    fn parse_base_list(&mut self) -> Result<Vec<BaseSpec>, CppParseError> {
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            return Ok(vec![]);
        }
        self.bump();
        let mut bases = vec![];
        loop {
            let mut access = Access::Private; // struct default
            let mut is_virtual = false;
            // access spec
            if self.at(CppKw::Public) {
                self.bump();
                access = Access::Public;
            } else if self.at(CppKw::Private) {
                self.bump();
                access = Access::Private;
            } else if self.at(CppKw::Protected) {
                self.bump();
                access = Access::Protected;
            }
            // virtual keyword
            if self.at(CppKw::Virtual) {
                self.bump();
                is_virtual = true;
            }
            let name = self.expect_ident()?;
            // 解析模板实参: Base<T, U>
            let template_args = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt()
            {
                self.bump(); // 吞 `<`
                let mut targs: Vec<crate::cpp::mangle::MangleType> = Vec::new();
                loop {
                    let arg = self.parse_template_arg(1)?;
                    targs.push(arg);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                        continue;
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Gt,
                            ..
                        }))
                    ) {
                        self.bump();
                        break;
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Shr,
                            ..
                        }))
                    ) {
                        self.bump();
                        self.deferred_gt += 1;
                        break;
                    }
                    return Err(CppParseError::Unexpected(
                        ">",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                Some(targs)
            } else {
                None
            };
            bases.push(BaseSpec {
                access,
                is_virtual,
                name,
                template_args,
            });
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }))
            ) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(bases)
    }

    fn parse_member_decl(&mut self) -> Result<CppDecl, CppParseError> {
        if let Some(t) = &self.peek {
            if t.is_access_spec() {
                self.bump();
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Colon,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        ":",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                return self.parse_member_decl();
            }
        }
        // M18.0: 类内允许 template 成员 (member template), 透传到 parse_template_decl
        if self.at(CppKw::Template) {
            return self.parse_template_decl(false);
        }
        // 类内 enum 成员: `enum { v = 0 };` / `enum Name { ... };` /
        // `enum class E : T { ... };`. type_traits 等 trait 惯用法
        // (`struct X { enum { value = N }; }`) 依赖它; 之前此分支缺失,
        // enum 落入 parse_member_var 后 expect_ident 失败, 导致整个
        // 类/模板声明被顶层容错逻辑静默丢弃.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwEnum,
                ..
            }))
        ) {
            // M30.2: 仅当 `enum [Name] {` 才是定义; `enum Name member`
            // (elaborated 类型成员, 可带位域) 落到成员变量解析.
            let defines = match self.peek_token_at(1) {
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })) => true,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) => matches!(
                    self.peek_token_at(2),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBrace,
                        ..
                    }))
                ),
                _ => false,
            };
            if defines {
                return self.parse_enum_decl();
            }
            self.bump();
        }
        // M30.2: 类内 `struct/union [Tag] { ... };` 嵌套定义成员 -
        // defines 前瞻 (同 enum 模式); `struct IOB *_chain;` (elaborated
        // 类型指针成员) bump struct 落成员变量解析.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwStruct | TokKind::KwUnion,
                ..
            }))
        ) {
            let defines = match self.peek_token_at(1) {
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })) => true,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) => matches!(
                    self.peek_token_at(2),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBrace,
                        ..
                    }))
                ),
                _ => false,
            };
            if defines {
                let is_u = matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::KwUnion,
                        ..
                    }))
                );
                return self.parse_aggr_decl(is_u);
            }
            self.bump();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Tilde,
                ..
            }))
        ) {
            return self.parse_dtor();
        }
        // 运算符重载
        if self.is_operator_fn_start() {
            return self.parse_operator_fn();
        }
        // M30: 类内 typedef 成员 (`typedef T type;`) — 解析并记录
        // 目标类型 (实例化时形参替换), 供 TplMember 引用展开;
        // 本身不产生 C 输出.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwTypedef,
                ..
            }))
        ) {
            return self.parse_typedef_decl();
        }
        // 跳过 `typedef` 简单形式
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            if text == "typedef" {
                // 简单 typedef 跳过
                while let Some(t) = &self.peek {
                    if matches!(
                        t,
                        CppTok::C(Tok {
                            kind: TokKind::Semi,
                            ..
                        })
                    ) {
                        break;
                    }
                    self.bump();
                }
                self.bump();
                return self.parse_member_decl();
            }
        }
        // M17.0: `mutable` 是成员变量修饰, 跳过然后 parse_member_var
        if self.at(CppKw::Mutable) {
            self.bump();
            return self.parse_member_var();
        }
        // M30.2: `__extension__` GNU 前缀成员 - 跳过后按普通成员解析
        // (`__extension__ unsigned long long int __a;` rand48 组合).
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            if text == "__extension__" {
                self.bump();
                return self.parse_member_decl();
            }
        }
        // M17.0: `explicit` 是 ctor 修饰, 跳过然后 parse_ctor
        if self.at(CppKw::Explicit) {
            self.bump();
            return self.parse_ctor();
        }
        // M17.0: `friend` 是自由函数/类声明标记, 跳过然后递归
        if self.at(CppKw::Friend) {
            self.bump();
            return self.parse_friend_decl();
        }
        // M17.0: `using Base::Base;` 继承构造 - 类内合法
        if self.at(CppKw::Using) {
            return self.parse_using_decl();
        }
        // M17.0: `volatile` 单独出现时可能是 volatile 成员函数限定,
        // 但通常与 const 连用 (const volatile); 这里当 cv 限定跳过
        if self.at(CppKw::Volatile) {
            self.bump();
            return self.parse_member_decl();
        }
        // M17.0: `noexcept` / `override` / `final` / `constexpr` / `inline` 单独出现
        // 是成员函数后置修饰, 跳过
        if self.at(CppKw::Noexcept)
            || self.at(CppKw::Override)
            || self.at(CppKw::Final)
            || self.at(CppKw::Constexpr)
            || self.at(CppKw::Inline)
            || self.at(CppKw::Consteval)
        {
            self.bump();
            return self.parse_member_decl();
        }
        // `virtual` -> 跳到下个非 virtual 修饰, 由 parse_member_fn_inline / parse_dtor 决定
        if self.at(CppKw::Virtual) {
            // 跳过后看是 ~ (dtor) 还是 type (fn)
            let next_is_tilde = if let Some(t2) = self.peek_token_at(1) {
                matches!(
                    t2,
                    CppTok::C(Tok {
                        kind: TokKind::Tilde,
                        ..
                    })
                )
            } else {
                false
            };
            if next_is_tilde {
                return self.parse_dtor();
            }
            return self.parse_member_fn_inline();
        }
        // M30.2: 通用函数指针成员检测 `RetType (*name)(params);`
        // 支持关键字返回类型 (`bool (*f)(...)`, `int (*f)(...)`,
        // `void (*f)(...)`) 和多 token 返回类型 (`const char *(*f)(...)`,
        // `cpp_hashnode *(*f)(...)`).  扫描 `(*` 模式, 在 `;` 前找到
        // 则判定为函数指针成员.
        {
            let mut k = 0;
            loop {
                match self.peek_token_at(k) {
                    Some(CppTok::C(Tok { kind: TokKind::Semi, .. })) | None => break,
                    Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) => {
                        if matches!(
                            self.peek_token_at(k + 1),
                            Some(CppTok::C(Tok { kind: TokKind::Star, .. }))
                        ) {
                            return self.parse_member_fnptr();
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            ..
        })) = &self.peek
        {
            let second = self.peek_token_at(1);
            match second {
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                })) => {
                    // M30.2: `Ret (*name)(args);` 类内函数指针成员
                    // (cpp_hash_table::alloc_node 等) - ctor 形态是 `Name(`,
                    // fnptr 形态是 `Name (*`, 以 `(` 后是否 `*` 区分.
                    if matches!(
                        self.peek_token_at(2),
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star,
                            ..
                        }))
                    ) {
                        return self.parse_member_fnptr();
                    }
                    return self.parse_ctor();
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) => {
                    let third = self.peek_token_at(2);
                    match third {
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        })) => {
                            return self.parse_member_fn_inline();
                        }
                        _ => return self.parse_member_var(),
                    }
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                })) => {
                    // M11 stdlib: typedef 指针类型成员变量 `dccpp_ostream* p_;`
                    // M18.0: 但是 `Type* name(...)` 是函数声明, 要先看 peek[2..]
                    let third = self.peek_token_at(2);
                    if let Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        ..
                    })) = third
                    {
                        let fourth = self.peek_token_at(3);
                        if matches!(
                            fourth,
                            Some(CppTok::C(Tok {
                                kind: TokKind::LParen,
                                ..
                            }))
                        ) {
                            return self.parse_member_fn_inline();
                        }
                    }
                    return self.parse_member_var();
                }
                Some(CppTok::Kw(_, _)) => {
                    // 第二 token 是关键字: 可能是 `class operator+` (operator fn)
                    if self.is_operator_fn_start() {
                        return self.parse_operator_fn();
                    }
                    return Err(CppParseError::Unsupported("class member", self.span()));
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Amp, ..
                })) => {
                    // M11 stdlib: `T& operator=()` - 返回引用的 operator
                    // 必须确保 & 后面是 `operator` 关键字, 不是普通 ident
                    // (避免 `ostream& cout();` 这种自由函数被误判)
                    // M26.1: 引用成员 `T& m_;` 保留类型 T (不再丢失为占位)
                    let ty_text = if let Some(CppTok::C(Tok { text, .. })) = &self.peek {
                        text.clone()
                    } else {
                        String::new()
                    };
                    self.bump(); // 跳过 T
                    self.bump(); // 跳过 &
                    if matches!(&self.peek, Some(CppTok::Kw(CppKw::Operator, _))) {
                        return self.parse_operator_fn();
                    }
                    // M18.0: `T& m_;` 类内引用成员变量
                    if let Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        text,
                        ..
                    })) = &self.peek
                    {
                        let name = text.clone();
                        self.bump();
                        // 默认初始化 0; lower 时引用降级为指针
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Semi,
                                ..
                            }))
                        ) {
                            self.bump();
                        }
                        return Ok(CppDecl::MemberVar {
                            ty: MangleType::Reference(Box::new(MangleType::Class(ty_text))),
                            name,
                            is_mutable: false,
                        });
                    }
                    return Err(CppParseError::Unsupported("T& member", self.span()));
                }
                _ => return Err(CppParseError::Unsupported("class member", self.span())),
            }
        }
        // M9: 成员函数以类型关键字开头 (`int get() ...`, `void set(int) ...` 等)
        // M11 stdlib: 同样支持 C++ 关键字开头 (`const T& method()`) 与类型关键字 +
        // * + 名 (`const void* data()`)
        let is_type_kw_or_qual = matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwInt
                    | TokKind::KwChar
                    | TokKind::KwShort
                    | TokKind::KwLong
                    | TokKind::KwVoid
                    | TokKind::KwFloat
                    | TokKind::KwDouble
                    | TokKind::KwSigned
                    | TokKind::KwUnsigned
                    | TokKind::KwStruct
                    | TokKind::KwUnion
                    | TokKind::KwEnum
                    | TokKind::KwConst
                    | TokKind::KwVolatile
                    | TokKind::KwStatic
                    | TokKind::KwInline
                    | TokKind::KwBool
                    | TokKind::KwAuto
                    | TokKind::KwAtomic
                    | TokKind::KwRestrict,
                ..
            }))
        ) || matches!(
            &self.peek,
            Some(CppTok::Kw(
                CppKw::Const | CppKw::Volatile | CppKw::Static,
                _
            ))
        );
        if is_type_kw_or_qual {
            // M11 stdlib: 如果是 `Type operator ...` 模式, 走 parse_operator_fn
            if self.is_operator_fn_start() {
                return self.parse_operator_fn();
            }
            // M18.0: 跳过 const/volatile/static 限定后再判定 fn/var
            // 否则 `static dccpp_time_point now()` 会被错判为 var.
            // 流程: 跳限定 -> 跳 typedef 类型 ident (后跟 ident/* /&)
            //      -> 跳 * 和 & -> 看 name_pos+1 是否是 `(`.
            let mut j = 1;
            // 跳过 const/volatile/static 限定
            while matches!(
                self.peek_token_at(j),
                Some(CppTok::Kw(
                    CppKw::Const | CppKw::Volatile | CppKw::Static,
                    _
                ))
            ) {
                j += 1;
            }
            // 跳过额外的类型关键字 (例: `void*`, `long long`, `unsigned char` 等)
            // 或 typedef 标识符 (例: `FILE*`, `size_t f`, `MyType&`,
            // `dccpp_time_point now()`)
            while let Some(tok) = self.peek_token_at(j) {
                if let CppTok::Kw(k, _) = &tok {
                    if matches!(
                        k,
                        CppKw::Inline | CppKw::Static | CppKw::Const | CppKw::Volatile
                    ) {
                        j += 1;
                        continue;
                    }
                }
                if let CppTok::C(t) = &tok {
                    if matches!(
                        t.kind,
                        TokKind::KwVoid
                            | TokKind::KwInt
                            | TokKind::KwChar
                            | TokKind::KwShort
                            | TokKind::KwLong
                            | TokKind::KwFloat
                            | TokKind::KwDouble
                            | TokKind::KwSigned
                            | TokKind::KwUnsigned
                            | TokKind::KwInline
                            | TokKind::KwBool
                    ) {
                        j += 1;
                        continue;
                    }
                }
                // typedef / 用户类型 ident: 仅当后面紧跟 `*` / `&` / 另一个 ident
                // 时才认为是类型, 单 ident 视为函数名
                if let CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }) = &tok
                {
                    let next = self.peek_token_at(j + 1);
                    if matches!(
                        next,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star | TokKind::Amp,
                            ..
                        }))
                    ) | matches!(
                        next,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        }))
                    ) {
                        j += 1;
                        continue;
                    }
                }
                break;
            }
            // 跳过 * 和 &
            loop {
                while matches!(
                    self.peek_token_at(j),
                    Some(CppTok::Kw(
                        CppKw::Const | CppKw::Volatile | CppKw::Static,
                        _
                    ))
                ) {
                    j += 1;
                }
                if matches!(
                    self.peek_token_at(j),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    }))
                ) {
                    j += 1;
                } else {
                    break;
                }
            }
            // 此时 token[j] 应是 ident 函数名
            let name_tok = self.peek_token_at(j);
            let is_fn = matches!(
                name_tok,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) && matches!(
                self.peek_token_at(j + 1),
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            );
            // M30.2: `Ret (*name)(args);` 函数指针成员 (类型词起始形态)
            let is_fnptr = matches!(
                name_tok,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) && matches!(
                self.peek_token_at(j + 1),
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                }))
            );
            return if is_fn {
                self.parse_member_fn_inline()
            } else if is_fnptr {
                self.parse_member_fnptr()
            } else {
                self.parse_member_var()
            };
        }
        Err(CppParseError::Unsupported("class member", self.span()))
    }

    /// 判断当前位置是否是 `extern "C" { ... }` 块
    fn is_extern_c_block_start(&self) -> bool {
        // extern "C" { ... }
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwExtern,
                ..
            }))
        ) {
            return false;
        }
        // 第二个 token 必须是字符串字面量 "C" 或 "c"
        match self.peek_token_at(1) {
            Some(CppTok::C(Tok {
                kind: TokKind::StringLit,
                text,
                ..
            })) => text == "\"C\"" || text == "\"c\"",
            _ => false,
        }
    }

    /// 解析 `extern "C" { ... }` 块
    ///
    /// M11 stdlib: 解析 extern "C" 块内的所有声明, 透传给外层.
    /// 这些是 C 函数的声明, 会在 lower 阶段作为 extern 转发.
    fn parse_extern_c_block(&mut self) -> Result<CppDecl, CppParseError> {
        // M18.0: 透传原文到 lower 输出.
        // 因为 C 头语法 (struct xxx; 等) 不一定被 dcc++ 完整支持,
        // 整体保留为字符串, lower 时直接 re-emit.
        // 记下 `extern` 关键字前的位置, 之后截取时直接拿原文整段
        let full_start = self.lex.pos - "extern".len();
        // 跳过 `extern`
        self.bump();
        // 跳过字符串字面量 (例如 "C")
        if let Some(CppTok::C(Tok {
            kind: TokKind::StringLit,
            ..
        })) = &self.peek
        {
            self.bump();
        }
        // 期望 `{`
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        // 跳过 `extern "C" {` 部分
        self.bump();
        // 跟踪花括号深度, 找到匹配的 `}` 为止
        let mut depth = 1i32;
        while let Some(t) = self.peek.clone() {
            match t {
                CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }) => {
                    self.bump();
                    depth += 1;
                }
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                }) => {
                    self.bump();
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
        // 截取原文: 整段 `extern "C" { ... }`
        let body_end = self.lex.pos;
        // full_start 是 `extern` 关键字起始, body_end 是 `}` 之后
        let full_text = if full_start <= body_end && body_end <= self.lex.src.len() {
            String::from_utf8_lossy(&self.lex.src[full_start..body_end]).to_string()
        } else {
            String::new()
        };
        Ok(CppDecl::RawExternC(full_text))
    }

    /// 解析 typedef <type> <name>; (M11 stdlib: 简化, 作为占位)
    fn parse_typedef_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // 跳过 `typedef`
        self.bump();
        // M30.2: `typedef __typeof__(expr) Name;` (glibc nullptr_t) -
        // depth 跳过 __typeof__ 的括号表达式, 与 decltype 同等处理.
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            if text == "__typeof__" || text == "__typeof" || text == "typeof" {
                self.bump();
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                ) {
                    let mut depth = 0i32;
                    while let Some(t) = self.peek.clone() {
                        match t {
                            CppTok::C(Tok {
                                kind: TokKind::LParen,
                                ..
                            }) => {
                                depth += 1;
                                self.bump();
                            }
                            CppTok::C(Tok {
                                kind: TokKind::RParen,
                                ..
                            }) => {
                                self.bump();
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                // M30.2: __typeof__(expr) 即目标类型本身; 跳过后的下一
                // 个 ident 是 typedef 名 (glibc nullptr_t), 直接早退.
                let name = self.expect_ident()?;
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    }))
                ) {
                    self.bump();
                }
                return Ok(CppDecl::Typedef {
                    target: MangleType::Int,
                    name,
                });
            }
        }
        // M30: 解析目标类型 (parse_mangle_type 覆盖 builtin/类名/
        // 模板实例/限定名/指针). 失败时回退 lex 状态并退化为旧跳过
        // 逻辑: 匿名 enum/struct 定义等无法简单解析的 typedef 仍走
        // no-op 路径 (target 置 Int, 不被引用).
        let saved_lex = self.lex.clone();
        let saved_peek = self.peek.clone();
        let mut fp_name: Option<String> = None;
        let target = match self.parse_mangle_type() {
            Ok(t) => {
                // M30.2: 函数指针 typedef `Ret (*name)(args);` - 返回类型
                // 解析成功后名字在括号内 (`void (*new_handler)()`).
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                ) && matches!(
                    self.peek_token_at(1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star,
                        ..
                    }))
                ) {
                    self.bump(); // (
                    self.bump(); // *
                    fp_name = Some(self.expect_ident()?);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }))
                    ) {
                        self.bump(); // )
                    }
                    // M30.2: 参数组 `(Args)` - depth 跳过 (嵌套括号内的
                    // 逗号不影响). 例: `typedef int (*cb)(void*, void*);`
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        }))
                    ) {
                        let mut depth = 0i32;
                        while let Some(t) = self.peek.clone() {
                            match t {
                                CppTok::C(Tok {
                                    kind: TokKind::LParen,
                                    ..
                                }) => {
                                    depth += 1;
                                    self.bump();
                                }
                                CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                }) => {
                                    self.bump();
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {
                                    self.bump();
                                }
                            }
                        }
                    }
                }
                // M30.2: `typedef struct Tag { ... } Name;` - 带 tag 的
                // 定义形态 parse_mangle_type 消费 `struct Tag` 后返回 Ok,
                // typedef 真名在 brace 定义之后; depth 跳过成员体.
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBrace,
                        ..
                    }))
                ) {
                    let mut depth = 0i32;
                    while let Some(t) = self.peek.clone() {
                        match t {
                            CppTok::C(Tok {
                                kind: TokKind::LBrace,
                                ..
                            }) => {
                                depth += 1;
                                self.bump();
                            }
                            CppTok::C(Tok {
                                kind: TokKind::RBrace,
                                ..
                            }) => {
                                self.bump();
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                t
            }
            Err(_) => {
                self.lex = saved_lex;
                self.peek = saved_peek;
                // M30.2: 回退路径统一 "定位 typedef 名". 形态覆盖:
                //   a. `struct/union/enum [Tag] { ... } Name;` (定义,
                //      glibc max_align_t / __fsid_t; body 内成员名会提前
                //      终止简单跳过, 必须先按定义形态处理)
                //   b. `decltype ( ... ) Name;` (C++11 nullptr_t)
                //   c. `Ret (* Name) (Args);` (函数指针, 名在括号内)
                //   d. 其余: 跳过类型关键字 / `*` / `&` / 括号 depth.
                // a: struct/union/enum 定义.
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum,
                        ..
                    }))
                ) {
                    self.bump();
                    // 可选 tag 名: 仅当下一个是 `{` 才吃掉.
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        }))
                    ) && matches!(
                        self.peek_token_at(1),
                        Some(CppTok::C(Tok {
                            kind: TokKind::LBrace,
                            ..
                        }))
                    ) {
                        self.bump();
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::LBrace,
                            ..
                        }))
                    ) {
                        let mut depth = 0i32;
                        loop {
                            match &self.peek {
                                Some(CppTok::C(Tok {
                                    kind: TokKind::LBrace,
                                    ..
                                })) => {
                                    depth += 1;
                                    self.bump();
                                }
                                Some(CppTok::C(Tok {
                                    kind: TokKind::RBrace,
                                    ..
                                })) => {
                                    depth -= 1;
                                    self.bump();
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                None => break,
                                _ => {
                                    self.bump();
                                }
                            }
                        }
                    }
                }
                // b: decltype ( ... ).
                if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
                    if text == "decltype"
                        && matches!(
                            self.peek_token_at(1),
                            Some(CppTok::C(Tok {
                                kind: TokKind::LParen,
                                ..
                            }))
                        )
                    {
                        self.bump();
                        let mut depth = 1i32;
                        while depth > 0 {
                            match self.bump() {
                                Some(CppTok::C(Tok {
                                    kind: TokKind::LParen,
                                    ..
                                })) => depth += 1,
                                Some(CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                })) => depth -= 1,
                                None => break,
                                _ => {}
                            }
                        }
                    }
                }
                // c+d: 跳过类型关键字 / `*` / `&`; 括号 depth 跳过,
                // `(* Name)` 函数指针形态在括号内取名.
                loop {
                    match &self.peek {
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star | TokKind::Amp,
                            ..
                        })) => {
                            self.bump();
                        }
                        Some(CppTok::C(Tok {
                            kind:
                                TokKind::KwChar
                                | TokKind::KwInt
                                | TokKind::KwShort
                                | TokKind::KwLong
                                | TokKind::KwFloat
                                | TokKind::KwDouble
                                | TokKind::KwVoid
                                | TokKind::KwBool
                                | TokKind::KwSigned
                                | TokKind::KwUnsigned
                                | TokKind::KwStruct
                                | TokKind::KwUnion
                                | TokKind::KwEnum
                                | TokKind::KwConst
                                | TokKind::KwVolatile
                                | TokKind::KwStatic
                                | TokKind::KwAtomic
                                | TokKind::KwRestrict,
                            ..
                        })) => {
                            self.bump();
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        })) => {
                            // 前瞻 `(` cv* `*` cv* Ident → 函数指针.
                            let mut k = 1usize;
                            while matches!(
                                self.peek_token_at(k),
                                Some(CppTok::C(Tok {
                                    kind: TokKind::KwConst | TokKind::KwVolatile,
                                    ..
                                }))
                            ) {
                                k += 1;
                            }
                            if matches!(
                                self.peek_token_at(k),
                                Some(CppTok::C(Tok {
                                    kind: TokKind::Star,
                                    ..
                                }))
                            ) {
                                k += 1;
                                while matches!(
                                    self.peek_token_at(k),
                                    Some(CppTok::C(Tok {
                                        kind: TokKind::KwConst | TokKind::KwVolatile,
                                        ..
                                    }))
                                ) {
                                    k += 1;
                                }
                                if let Some(CppTok::C(Tok {
                                    kind: TokKind::Ident,
                                    text,
                                    ..
                                })) = self.peek_token_at(k)
                                {
                                    fp_name = Some(text);
                                }
                            }
                            // depth 跳过整组 `(...)`, 再 continue 处理尾随组.
                            self.bump();
                            let mut depth = 1i32;
                            while depth > 0 {
                                match self.bump() {
                                    Some(CppTok::C(Tok {
                                        kind: TokKind::LParen,
                                        ..
                                    })) => depth += 1,
                                    Some(CppTok::C(Tok {
                                        kind: TokKind::RParen,
                                        ..
                                    })) => depth -= 1,
                                    None => break,
                                    _ => {}
                                }
                            }
                            if fp_name.is_some() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                // c 续: 函数指针的参数列表 `(...)` (可能多组).
                if fp_name.is_some() {
                    while matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        }))
                    ) {
                        self.bump();
                        let mut depth = 1i32;
                        while depth > 0 {
                            match self.bump() {
                                Some(CppTok::C(Tok {
                                    kind: TokKind::LParen,
                                    ..
                                })) => depth += 1,
                                Some(CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                })) => depth -= 1,
                                None => break,
                                _ => {}
                            }
                        }
                    }
                }
                MangleType::Int
            }
        };
        // 期望 ident (typedef 名). 模板实例类型 `Name<Args> alias`
        // 已由 parse_mangle_type 解析为 Template; 记录别名映射,
        // 表达式 `alias::member` 可展开为 TplStaticAccess (触发实例化
        // 并引用带实例前缀的常量).
        let name = match fp_name {
            Some(n) => n,
            None => self.expect_ident()?,
        };
        // M30.2: 函数类型 typedef `typedef Ret Name(Args);` (无 `*`,
        // glibc cookie_read_function_t) - 名字后是参数组, depth 跳过;
        // 函数签名 typedef 仅作占位, 不产生 C 输出.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            let mut depth = 0i32;
            while let Some(t) = self.peek.clone() {
                match t {
                    CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }) => {
                        depth += 1;
                        self.bump();
                    }
                    CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }) => {
                        self.bump();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {
                        self.bump();
                    }
                }
            }
        }
        if let MangleType::Template { name: tname, args } = &target {
            self.typedef_alias
                .insert(name.clone(), (tname.clone(), args.clone()));
        }
        // M30.2: typedef 尾部 GNU 尾缀 (`typedef int register_t
        // __attribute__ ((__mode__ (__word__)));`)
        self.skip_gnu_suffixes();
        // 跳过 `;`
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        // M30: typedef 仍不在 lower 阶段产生 C 输出; 目标类型随声明
        // 保留, 模板类内的 typedef 成员经 instantiate_decl 替换形参后
        // 由 TplMember 类型引用展开 (见 Lower::inst_typedefs).
        Ok(CppDecl::Typedef { target, name })
    }

    /// Parse `enum [class] Name [: underlying] { ... };` and retain
    /// enumerator names and initializer token text for lowering.
    fn parse_enum_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // 跳过 `enum`
        self.bump();
        // Consume `class` or `struct` for a scoped enum.
        let scoped = if self.at(CppKw::Class) || self.at_tok(TokKind::KwStruct) {
            self.bump();
            true
        } else {
            false
        };
        // Collect the enum name when present.
        let name = if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            text.clone()
        } else {
            "_anon".to_string()
        };
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            ..
        })) = &self.peek
        {
            self.bump();
        }
        let mut underlying = None;
        // Preserve the underlying type spelling up to the opening brace.
        if self.at_tok(TokKind::Colon) {
            self.bump();
            let mut parts = Vec::new();
            while !self.at_tok(TokKind::LBrace) {
                if let Some(tok) = self.peek.clone() {
                    parts.push(self.token_text(&tok));
                    self.bump();
                } else {
                    break;
                }
            }
            underlying = Some(parts.join(" "));
        }
        // 必须有 `{`
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump(); // {
                     // Parse the enumerator list: `name [= init-expr]`, comma separated,
                     // terminated by the matching `}`.  Initializer token text is kept so
                     // the lowerer can re-emit the constant expression verbatim.
        let mut variants: Vec<(String, Option<String>)> = Vec::new();
        let mut depth = 1i32;
        let mut cur_name: Option<String> = None;
        let mut cur_init: Option<Vec<String>> = None;
        while let Some(t) = self.peek.clone() {
            let open = matches!(
                &t,
                CppTok::C(Tok {
                    kind: TokKind::LParen | TokKind::LBracket | TokKind::LBrace,
                    ..
                })
            );
            let close = matches!(
                &t,
                CppTok::C(Tok {
                    kind: TokKind::RParen | TokKind::RBracket | TokKind::RBrace,
                    ..
                })
            );
            match &t {
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                }) if depth == 1 => {
                    // 枚举列表结束
                    self.bump();
                    break;
                }
                _ => {}
            }
            // 所有括号种类都计入 depth, 保证 init 表达式内的逗号不会切分 enumerator.
            if open {
                depth += 1;
            } else if close {
                depth -= 1;
            }
            match &t {
                CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }) if depth == 1 => {
                    if let Some(n) = cur_name.take() {
                        variants.push((n, cur_init.take().map(|v| v.join(" "))));
                    }
                    cur_init = None;
                    self.bump();
                }
                CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                }) if cur_name.is_none() && depth == 1 => {
                    cur_name = Some(text.clone());
                    self.bump();
                }
                CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }) if cur_name.is_some() && depth == 1 => {
                    cur_init = Some(Vec::new());
                    self.bump();
                }
                _ => {
                    if let Some(init) = cur_init.as_mut() {
                        init.push(self.token_text(&t));
                    }
                    self.bump();
                }
            }
        }
        if let Some(n) = cur_name.take() {
            variants.push((n, cur_init.take().map(|v| v.join(" "))));
        }
        // 跳过 `;`
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::Enum {
            name,
            scoped,
            underlying,
            variants,
        })
    }

    /// 判断当前位置是否是 `Type operator ... (...)`
    /// 形式: `T operator+(...)`, `T operator()(...)`, `operator()(...)` (无返回类型)
    fn is_operator_fn_start(&self) -> bool {
        let mut j = 0;
        while matches!(
            self.peek_token_at(j),
            Some(CppTok::Kw(
                CppKw::Inline | CppKw::Static | CppKw::Virtual | CppKw::Explicit,
                _
            ))
        ) {
            j += 1;
        }
        self.is_operator_fn_start_at(j)
    }

    fn is_operator_fn_start_at(&self, j: usize) -> bool {
        // 形式 A: ident operator...  (例: string operator+(...))
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            ..
        })) = self.peek_token_at(j)
        {
            if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(j + 1) {
                return true;
            }
            // M18.0: `Class& operator...` / `Class* operator...` 也算
            if matches!(
                self.peek_token_at(j + 1),
                Some(CppTok::C(Tok {
                    kind: TokKind::Amp | TokKind::Star,
                    ..
                }))
            ) {
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) = self.peek_token_at(j + 2)
                {
                    if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(j + 3) {
                        return true;
                    }
                }
            }
        }
        // M26.1: 形式 B/C: 类型关键字开头, 中间可有 * / & / const / 类型 token,
        // 之后是 `operator`. 例: `int operator+`, `void* operator new`,
        // `const T& operator=`.
        if self.is_type_start_token(self.peek_token_at(j)) {
            let mut k = j + 1;
            for _ in 0..5 {
                match self.peek_token_at(k) {
                    Some(CppTok::Kw(CppKw::Operator, _)) => return true,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    })) => k += 1,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        ..
                    })) => k += 1,
                    Some(CppTok::C(Tok { kind: ty, .. })) if is_c_type_kw(ty) => k += 1,
                    Some(CppTok::Kw(CppKw::Const | CppKw::Volatile | CppKw::Static, _)) => k += 1,
                    _ => break,
                }
            }
        }
        false
    }

    /// 是否类型起始 token (C 类型关键字 或 C++ const/volatile).
    fn is_type_start_token(&self, tok: Option<CppTok>) -> bool {
        match tok {
            Some(CppTok::C(Tok { kind, .. })) => is_c_type_kw(kind),
            Some(CppTok::Kw(CppKw::Const | CppKw::Volatile, _)) => true,
            _ => false,
        }
    }

    /// 解析运算符重载函数
    fn parse_operator_fn(&mut self) -> Result<CppDecl, CppParseError> {
        // 跳过 inline/static 等说明符 (例: `inline string operator+`)
        loop {
            let is_spec = matches!(
                &self.peek,
                Some(CppTok::Kw(
                    CppKw::Inline | CppKw::Static | CppKw::Virtual | CppKw::Explicit,
                    _
                ))
            );
            if is_spec {
                self.bump();
            } else {
                break;
            }
        }
        // 形式 1: Type Class::operator+(...)  (类外定义)
        // 形式 2: Class::operator+(...)        (返回类型被略)
        // 形式 3: Type operator+(...)          (全局)
        // 形式 4: Class::operator()(...)        (类内 inline)
        // 形式 5: operator()(...)               (类内 inline, 无返回类型)
        // 形式 6: const Type operator()(...)    (M11 stdlib: 限定符 + 类型)
        // 形式 7: Class& operator/(...)         (M18: 类引用返回)
        // 形式 8: Class* operator()(...)        (M18: 类指针返回)
        let mut ret = MangleType::Void;
        // 判断是否需要解析返回类型
        // 1) Ident 后面是 `operator` (不是 `:: operator`)  -> 解析为返回类型
        // 2) Ident 后面是 `::` `operator`  -> 不解析 (Ident 是类名)
        // 3) Ident 后面是 `&` / `*` `operator`  -> 解析为返回类型 (M18)
        // 4) 关键字后面是 `operator`  -> 解析为返回类型
        let need_ret = match &self.peek {
            Some(CppTok::C(Tok {
                kind:
                    TokKind::KwInt
                    | TokKind::KwChar
                    | TokKind::KwShort
                    | TokKind::KwLong
                    | TokKind::KwVoid
                    | TokKind::KwFloat
                    | TokKind::KwDouble
                    | TokKind::KwSigned
                    | TokKind::KwUnsigned
                    | TokKind::KwStruct
                    | TokKind::KwUnion
                    | TokKind::KwEnum
                    | TokKind::KwConst
                    | TokKind::KwVolatile
                    | TokKind::KwInline
                    | TokKind::KwBool
                    | TokKind::KwAuto
                    | TokKind::KwAtomic
                    | TokKind::KwRestrict,
                ..
            })) => true,
            // M30.2: Cpp lexer 的 Kw 形式限定符 (`const T& operator[]`)
            // 同样是返回类型起始; 之前仅认 C(Tok) 形式导致
            // "expected operator, got Const".
            Some(CppTok::Kw(CppKw::Const | CppKw::Volatile, _)) => true,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })) => {
                // ident 后面是 operator 关键字 (不是 `::`): ident 必为返回类型
                if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(1) {
                    true
                }
                // ident 后面是 & / * 然后 ident 然后 operator: 返回类型
                else if matches!(
                    self.peek_token_at(1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Amp | TokKind::Star,
                        ..
                    }))
                ) {
                    matches!(
                        self.peek_token_at(2),
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        }))
                    ) && matches!(self.peek_token_at(3), Some(CppTok::Kw(CppKw::Operator, _)))
                } else {
                    false
                }
            }
            _ => false,
        };
        if need_ret {
            ret = self.parse_mangle_type()?;
            // M18: 跳过返回类型后的 * / & (例: `Class& operator/=`)
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Star | TokKind::Amp,
                    ..
                }))
            ) {
                self.bump();
                ret = MangleType::Pointer(Box::new(ret));
            }
        }
        let mut class: Option<String> = None;
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            ..
        })) = &self.peek
        {
            if let Some(CppTok::ColonColon(_)) = self.peek_token_at(1) {
                class = Some(text_of(&self.peek).to_string());
                self.bump();
                self.bump();
            }
        }
        self.expect_kw(CppKw::Operator)?;
        let op = self.parse_operator_name()?;
        let params = self.parse_param_list()?;
        let is_unary = params.is_empty() || matches!(op, Operator::CastTo(_));
        let _is_const = self.skip_cv_qual();
        // M18.0: `= 0` / `= delete` (deleted / pure virtual operator)
        let peek_after_paren = self.peek.clone();
        if matches!(
            &peek_after_paren,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            self.bump(); // 跳过 =
            let tok0 = self.peek.clone();
            let is_value = matches!(
                &tok0,
                Some(CppTok::C(Tok {
                    kind: TokKind::Number,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) | Some(CppTok::Kw(CppKw::Delete, _))
            );
            if is_value {
                self.bump();
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppDecl::OperatorFn {
                class,
                op,
                params,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                ret,
                is_unary,
            });
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            // 仅声明, 无函数体
            self.bump();
            return Ok(CppDecl::OperatorFn {
                class,
                op,
                params,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                ret,
                is_unary,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::OperatorFn {
            class,
            op,
            params,
            body,
            ret,
            is_unary,
        })
    }

    /// 解析 operator 后的运算符名 (如 + - * / << >> < > == != () [] -> etc.)
    fn parse_operator_name(&mut self) -> Result<Operator, CppParseError> {
        // 优先解析多元 (>=2 char)
        let saved = self.peek.clone();
        let t1 = self.bump();
        if let Some(t1) = t1 {
            // 先看 t1 自己是否已经是组合 token (如 PlusEq, Shl, EqEq 等)
            // M11 stdlib: 直接按 TokKind 映射, 避免多字符组合错误.
            if let CppTok::C(tok) = &t1 {
                if let Some(op) = crate::cpp::mangle::from_tok_kind(&tok.kind) {
                    // M30.2: operator[] / operator() 的 `[`/`(` 在
                    // from_tok_kind 命中提前返回, 需补吃配对的 `]` / `)`. 之前
                    // parse_param_list 撞上 RBracket 报 "expected (".
                    if matches!(tok.kind, TokKind::LBracket | TokKind::LParen) {
                        self.bump();
                    }
                    return Ok(op);
                }
            }
            let s1 = match &t1 {
                CppTok::C(Tok {
                    kind: TokKind::Plus,
                    ..
                }) => "+",
                CppTok::C(Tok {
                    kind: TokKind::Minus,
                    ..
                }) => "-",
                CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                }) => "*",
                CppTok::C(Tok {
                    kind: TokKind::Slash,
                    ..
                }) => "/",
                CppTok::C(Tok {
                    kind: TokKind::Percent,
                    ..
                }) => "%",
                CppTok::C(Tok {
                    kind: TokKind::Caret,
                    ..
                }) => "^",
                CppTok::C(Tok {
                    kind: TokKind::Amp, ..
                }) => "&",
                CppTok::C(Tok {
                    kind: TokKind::Pipe,
                    ..
                }) => "|",
                CppTok::C(Tok {
                    kind: TokKind::Tilde,
                    ..
                }) => "~",
                CppTok::C(Tok {
                    kind: TokKind::Not, ..
                }) => "!",
                CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                }) => "<",
                CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                }) => ">",
                CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }) => "=",
                CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }) => ",",
                CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }) => "()",
                CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }) => "[]",
                _ => "",
            };
            // 尝试 2-char 组合 (只当 s1 是单字符时)
            if s1.len() == 1 {
                let two = format!("{}{}", s1, peek_two_str(&self.peek));
                if let Some(op) = Operator::from_token(&two) {
                    // 吃掉第二个字符 (s1 已 bump, 还要再 bump 第二个)
                    if two.len() == 2 {
                        self.bump();
                    }
                    return Ok(op);
                }
            }
            if let Some(op) = Operator::from_token(s1) {
                // M30.2: operator[] / operator() 的 `[`/`(` 已消费,
                // 补吃配对的 `]` / `)` (否则后续 parse_param_list 撞上
                // RBracket 报 "expected (").
                if s1 == "[]" || s1 == "()" {
                    self.bump();
                }
                return Ok(op);
            }
        }
        // M26.1: `operator new` / `operator delete` (含 `[]` 数组形式)
        // 注意: t1 已被上面的 if-let 部分移走, 这里回看保存的 peek.
        if matches!(&saved, Some(CppTok::Kw(CppKw::New, _))) {
            if self.peek_is_lbracket() {
                self.bump(); // [
                self.expect_tok(TokKind::RBracket)?;
                return Ok(Operator::NewArr);
            }
            return Ok(Operator::New);
        }
        if matches!(&saved, Some(CppTok::Kw(CppKw::Delete, _))) {
            if self.peek_is_lbracket() {
                self.bump(); // [
                self.expect_tok(TokKind::RBracket)?;
                return Ok(Operator::DeleteArr);
            }
            return Ok(Operator::Delete);
        }
        let _ = saved;
        Err(CppParseError::Unsupported("operator name", self.span()))
    }

    fn parse_param_list(&mut self) -> Result<Vec<(String, MangleType)>, CppParseError> {
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "(",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut params = vec![];
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                })
            ) {
                break;
            }
            // 跳过 const 限定 (例如 `T const&` 表达成 const T& )
            if self.at(CppKw::Const) {
                self.bump();
                continue;
            }
            // M11 stdlib: variadic `...` (C 风格变参)
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ellipsis,
                    ..
                }))
            ) {
                self.bump();
                params.push(("...".to_string(), MangleType::Void));
                break;
            }
            let mut ty = self.parse_mangle_type()?;
            // M30.2: 数组形参 `char[20]` / `int a[2][3]` - 下标 depth
            // 跳过 (glibc tmpnam 等; 形参退化为指针).
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }))
            ) {
                self.bump();
                let mut depth = 1i32;
                while let Some(t) = self.peek.clone() {
                    match t {
                        CppTok::C(Tok {
                            kind: TokKind::LBracket,
                            ..
                        }) => {
                            depth += 1;
                            self.bump();
                        }
                        CppTok::C(Tok {
                            kind: TokKind::RBracket,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {
                            self.bump();
                        }
                    }
                }
            }
            // 跳过 const 后缀限定
            if self.at(CppKw::Const) {
                self.bump();
            }
            // 跳过 & / && / * 后缀 (C++ 中可放在标识符前);
            // M30.2: `T&&` 右值引用同按引用处理 (移动语义降级).
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Amp | TokKind::AndAnd,
                    ..
                }))
            ) {
                self.bump();
                ty = MangleType::Reference(Box::new(ty));
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                }))
            ) {
                self.bump();
                ty = MangleType::Pointer(Box::new(ty));
            }
            // M18.0: 函数指针形参 `int (*name)(args)` 解析为 Pointer
            //   形式: 紧跟 `(` `*` ident `)` 然后 `(` ... `)`. lower 用
            //   `__fnptr_*` 别名占位 (具体 typedef 在 stdlib/runtime 提供).
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                // 透传整段 `(*name)(args)`, 名字为 name, 类型标记为 Pointer 占位
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                })) = self.peek_token_at(1)
                {
                    self.bump(); // (
                    self.bump(); // *
                    let name = self.expect_ident().unwrap_or_else(|_| "_".into());
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }))
                    ) {
                        self.bump(); // )
                    }
                    // 跳过 args: `(` ... `)`
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        }))
                    ) {
                        self.bump();
                        while let Some(tt) = &self.peek {
                            if matches!(
                                tt,
                                CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                })
                            ) {
                                break;
                            }
                            self.bump();
                        }
                        self.bump(); // )
                    }
                    params.push((name, MangleType::Pointer(Box::new(ty))));
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                    }
                    continue;
                }
            }
            let name = if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })) = &self.peek
            {
                Some(self.expect_ident()?)
            } else {
                None
            };
            // M30.2: 带名数组形参 `char __s[20]` (tmpnam_r) - 名字后
            // 下标 depth 跳过.
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }))
            ) {
                self.bump();
                let mut depth = 1i32;
                while let Some(t) = self.peek.clone() {
                    match t {
                        CppTok::C(Tok {
                            kind: TokKind::LBracket,
                            ..
                        }) => {
                            depth += 1;
                            self.bump();
                        }
                        CppTok::C(Tok {
                            kind: TokKind::RBracket,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {
                            self.bump();
                        }
                    }
                }
            }
            params.push((name.unwrap_or_else(|| "_".into()), ty));
            // M30.2: 默认实参 `= expr` - depth 跳到 `,` / `)` (值不需要).
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                let mut depth = 0i32;
                while let Some(tt) = &self.peek {
                    match tt {
                        CppTok::C(Tok {
                            kind: TokKind::LParen | TokKind::LBrace,
                            ..
                        }) => {
                            depth += 1;
                            self.bump();
                        }
                        CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }) => {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                            self.bump();
                        }
                        CppTok::C(Tok {
                            kind: TokKind::RBrace,
                            ..
                        }) => {
                            depth -= 1;
                            self.bump();
                        }
                        CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }) if depth == 0 => break,
                        _ => {
                            self.bump();
                        }
                    }
                }
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }))
            ) {
                self.bump();
            }
        }
        self.bump();
        Ok(params)
    }

    /// 解析成员变量 (支持 `int a, b, c;`)
    /// M17.0: 识别 `mutable int n;` 修饰, 标记 is_mutable=true.
    fn parse_member_var(&mut self) -> Result<CppDecl, CppParseError> {
        let is_mutable = if self.at(CppKw::Mutable) {
            self.bump();
            true
        } else {
            false
        };
        let ty = self.parse_mangle_type()?;
        // M30.2: 无名位域 `int :32;` (结构体 padding, glibc __fd_set) -
        // 无名字, 仅跳过位宽; 成员名占位避免 expect_ident 撞 `:`.
        let name = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            "_".to_string()
        } else {
            self.expect_ident()?
        };
        // M30.2: 数组成员 `char _shortbuf[1];` (_IO_FILE) - 下标 depth
        // 跳过 (数组成员退化为指针; 与位域同位置处理).
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            self.bump();
            let mut depth = 1i32;
            while let Some(t) = self.peek.clone() {
                match t {
                    CppTok::C(Tok {
                        kind: TokKind::LBracket,
                        ..
                    }) => {
                        depth += 1;
                        self.bump();
                    }
                    CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }) => {
                        self.bump();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {
                        self.bump();
                    }
                }
            }
        }
        // M30.2: 位域 `int x : 3;` / `enum E e : 8;` - 跳过位宽表达式.
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            self.bump();
            let mut depth = 0i32;
            while let Some(t) = &self.peek {
                match t {
                    CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }) => {
                        depth += 1;
                        self.bump();
                    }
                    CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }) => {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                        self.bump();
                    }
                    CppTok::C(Tok {
                        kind: TokKind::Semi | TokKind::Comma,
                        ..
                    }) if depth == 0 => break,
                    _ => {
                        self.bump();
                    }
                }
            }
        }
        // 简单: 只取第一个名字, 后续 `, name` 直接吃掉
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Comma,
                ..
            }))
        ) {
            self.bump();
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })) = &self.peek
            {
                self.bump(); // 消耗 ident
            }
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            // 简单值 (单字面量)
            self.bump();
            while let Some(t) = &self.peek {
                if matches!(
                    t,
                    CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    })
                ) {
                    break;
                }
                self.bump();
            }
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::MemberVar {
            ty,
            name,
            is_mutable,
        })
    }

    /// M30.2: 类内函数指针成员 `Ret (*name) (args);`
    /// (cpp_hash_table::alloc_node 等). dcc Cpp AST 无函数指针类型,
    /// 降级为 void* 成员 (声明/赋值场景兼容).
    fn parse_member_fnptr(&mut self) -> Result<CppDecl, CppParseError> {
        // 入口: peek 从返回类型开始 (`Ret (*name)(...)` / `(*name)(...)`),
        // 调用方前瞻已确认 `( * name` 形态; 此处先吃掉返回类型词到 `(`.
        let mut guard = 0;
        while guard < 8 {
            match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
                | None => break,
                _ => {
                    self.bump();
                }
            }
            guard += 1;
        }
        self.bump(); // (
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Star | TokKind::KwConst | TokKind::KwVolatile,
                ..
            }))
        ) {
            self.bump();
        }
        let name = self.expect_ident()?;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RParen,
                ..
            }))
        ) {
            self.bump(); // )
        }
        // 参数列表 `(...)` (可能多组, 返回函数指针)
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            self.bump();
            let mut depth = 1i32;
            while depth > 0 {
                match self.bump() {
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    })) => depth += 1,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    })) => depth -= 1,
                    None => break,
                    _ => {}
                }
            }
        }
        // `noexcept` / `= 0` / `__attribute__` 后缀
        loop {
            if self.at(CppKw::Noexcept) {
                self.bump();
                continue;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                continue;
            }
            // M30.2: `__attribute__ ((...))` 后缀 - 跳过整个属性
            // (gcc 回调函数指针成员大量使用 __format__/__nonnull__ 等).
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) = &self.peek
            {
                if text == "__attribute__" || text == "__attribute" {
                    self.bump();
                    // 跳过 ((...))
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        }))
                    ) {
                        let mut depth = 0i32;
                        while let Some(t) = self.peek.clone() {
                            match t {
                                CppTok::C(Tok {
                                    kind: TokKind::LParen,
                                    ..
                                }) => {
                                    depth += 1;
                                    self.bump();
                                }
                                CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                }) => {
                                    self.bump();
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {
                                    self.bump();
                                }
                            }
                        }
                    }
                    continue;
                }
            }
            break;
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppDecl::MemberVar {
            ty: MangleType::Pointer(Box::new(MangleType::Void)),
            name,
            is_mutable: false,
        })
    }

    /// M30.2: 类外成员函数 / 构造 / 析构定义
    ///   `Ret Class::method(params) [const] [noexcept] { ... }`
    ///   `Class::Class(params) [: init, ...] { ... }`
    ///   `Class::~Class(params) { ... }`
    /// 调用点: parse_free_fn 已消费返回类型与限定名首段, peek 为 `::`.
    /// GCC 主体 (libcpp/rich_location 等) 大量使用类外定义, 此前不识别
    /// 导致整文件降级 strip 管道 (尾部 const / this 改写全部丢失, 产出
    /// 非法 C).
    fn parse_out_of_class_member(
        &mut self,
        first: String,
        ret: MangleType,
    ) -> Result<CppDecl, CppParseError> {
        let mut parts = vec![first];
        // 收集完整限定链 `A::B::m` (嵌套命名空间 / 外层类)
        loop {
            if !matches!(&self.peek, Some(CppTok::ColonColon(_))) {
                break;
            }
            self.bump();
            // 析构 `Class::~Class`
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Tilde,
                    ..
                }))
            ) {
                self.bump();
                let nm = self.expect_ident()?;
                let cls = parts.pop().unwrap_or(nm.clone());
                let _params = self.parse_param_list()?;
                let spec = self.skip_specifiers();
                let body = if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    }))
                ) {
                    self.bump();
                    CppBody {
                    stmts: vec![],
                    is_decl: true,
                }
                } else {
                    self.parse_body()?
                };
                return Ok(CppDecl::Dtor {
                    class: cls,
                    body,
                    is_virtual: false,
                    // C++11 [except.spec]/3: destructors are implicitly noexcept
                    is_noexcept: true,
                });
            }
            parts.push(self.expect_ident()?);
        }
        let name = parts.pop().unwrap_or_default();
        let class = parts.join("::");
        // 构造 `Class::Class(params) [: inits] {...}` - 方法名与类名相同
        if !class.is_empty() && name == parts.join("::") {
            let params = self.parse_param_list()?;
            let spec = self.skip_specifiers();
            let inits = self.parse_ctor_init_list()?;
            let body = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
                CppBody {
                    stmts: vec![],
                    is_decl: true,
                }
            } else {
                self.parse_body()?
            };
            return Ok(CppDecl::Ctor {
                class,
                params,
                inits,
                body,
                is_virtual: false,
                is_explicit: false,
                is_noexcept: spec.is_noexcept,
            });
        }
        // 普通成员函数
        let params = self.parse_param_list()?;
        let is_const = self.skip_cv_qual();
        let is_volatile = if self.at(CppKw::Volatile) {
            self.bump();
            true
        } else {
            false
        };
        let spec = self.skip_specifiers();
        let body = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            CppBody {
                    stmts: vec![],
                    is_decl: true,
                }
        } else {
            self.parse_body()?
        };
        Ok(CppDecl::MemberFn {
            class,
            ret,
            name,
            params,
            body,
            is_virtual: false,
            is_override: spec.is_override,
            is_const,
            is_final: spec.is_final,
            is_noexcept: spec.is_noexcept,
            is_volatile,
            is_friend: false,
        })
    }

    /// M30.2: 解析构造函数初始化列表 `: a(1), b{2}` (类内外定义共用).
    /// 无列表时返回空 vec. 逻辑自 parse_ctor 抽出.
    fn parse_ctor_init_list(&mut self) -> Result<Vec<(String, CppExpr)>, CppParseError> {
        let mut inits: Vec<(String, CppExpr)> = vec![];
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            return Ok(inits);
        }
        self.bump();
        loop {
            let member = self.expect_ident()?;
            let init = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                self.bump();
                let e = if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        ")",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                e
            } else if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }))
            ) {
                self.bump();
                let e = if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBrace,
                        ..
                    }))
                ) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBrace,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        "}",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                e
            } else {
                None
            };
            inits.push((member, init.unwrap_or(CppExpr::IntLit(0))));
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }))
            ) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(inits)
    }

    fn parse_ctor(&mut self) -> Result<CppDecl, CppParseError> {
        // M17.0: 检测 `explicit` 修饰
        let is_explicit = if self.at(CppKw::Explicit) {
            self.bump();
            true
        } else {
            false
        };
        let class = self.expect_ident()?;
        let params = self.parse_param_list()?;
        let is_virtual = self.skip_cv_qual() || false;
        // M17.0: 可能有 `noexcept` / `final` (override 对 ctor 无意义)
        let spec = self.skip_specifiers();
        let is_noexcept = spec.is_noexcept;
        // 解析 ctor 初始化列表 `: re(r), im(i)` (M30.2: 逻辑抽出共用)
        let inits = self.parse_ctor_init_list()?;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppDecl::Ctor {
                class,
                params,
                inits,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_virtual,
                is_explicit,
                is_noexcept,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Ctor {
            class,
            params,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
        })
    }

    fn parse_dtor(&mut self) -> Result<CppDecl, CppParseError> {
        let is_virtual = if self.at(CppKw::Virtual) {
            self.bump();
            true
        } else {
            false
        };
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Tilde,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "~",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let class = self.expect_ident()?;
        let _params = self.parse_param_list()?;
        let spec = self.skip_specifiers();
        let _is_noexcept = spec.is_noexcept; // C++11: dtors always implicitly noexcept
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppDecl::Dtor {
                class,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_virtual,
                // C++11 [except.spec]/3: destructors are implicitly noexcept
                is_noexcept: true,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Dtor {
            class,
            body,
            is_virtual,
            // C++11 [except.spec]/3: destructors are implicitly noexcept
            is_noexcept: true,
        })
    }

    /// M30.1: 解析单个模板实参 (类型或非类型字面量).
    /// 实参位置的统一入口: true/false/整数走 NonType, 其余走类型解析.
    /// parse_mangle_type 内部循环 / 局部变量 qualified 分支 /
    /// 基类列表 / 函数显式实参共用.
    fn parse_template_arg(&mut self, depth: u32) -> Result<MangleType, CppParseError> {
        if self.at(CppKw::True)
            || self.at(CppKw::False)
            || matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Number | TokKind::Minus,
                    ..
                }))
            )
        {
            return self.parse_non_type_value();
        }
        self.parse_mangle_type_depth(depth)
    }

    /// M30.1: 解析非类型模板实参 / 非类型形参默认值.
    /// 接受 true / false / 整数字面量 (含负号), 返回 MangleType::NonType.
    fn parse_non_type_value(&mut self) -> Result<MangleType, CppParseError> {
        if self.at(CppKw::True) {
            self.bump();
            return Ok(MangleType::NonType(1));
        }
        if self.at(CppKw::False) {
            self.bump();
            return Ok(MangleType::NonType(0));
        }
        let neg = matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Minus,
                ..
            }))
        );
        if neg {
            self.bump();
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Number,
            text,
            ..
        })) = &self.peek
        {
            // 去掉整数后缀 (u/U/l/L 组合); hex/octal 交给 i64 解析
            let raw: String = text
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == 'x' || *c == 'X')
                .collect();
            let v: i64 = if let Some(hex) = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
            {
                i64::from_str_radix(hex, 16).map_err(|_| {
                    CppParseError::Unexpected("non-type template argument", raw, self.span())
                })?
            } else {
                raw.parse::<i64>().map_err(|_| {
                    CppParseError::Unexpected("non-type template argument", raw, self.span())
                })?
            };
            self.bump();
            return Ok(MangleType::NonType(if neg { -v } else { v }));
        }
        Err(CppParseError::Unexpected(
            "non-type template argument",
            format!("{:?}", self.peek),
            self.span(),
        ))
    }

    fn parse_mangle_type(&mut self) -> Result<MangleType, CppParseError> {
        self.parse_mangle_type_depth(0)
    }

    /// 带模板嵌套深度的 parse_mangle_type.
    /// depth 随递归递增; 超过 MAX_TEMPLATE_DEPTH 时报错,
    /// 避免超深嵌套模板 (T<T<...>>) 挂起解析器 (见 template_depth.rs).
    fn parse_mangle_type_depth(&mut self, depth: u32) -> Result<MangleType, CppParseError> {
        // 提前检查深度限制，避免在深度递归后才检测
        if depth > MAX_TEMPLATE_DEPTH {
            return Err(CppParseError::TemplateDepthExceeded {
                depth,
                max: MAX_TEMPLATE_DEPTH,
                span: self.span(),
            });
        }
        // M30.2: `__restrict` 仅是优化提示, 类型上等同无限制 (glibc
        // string.h 参数列表大量使用); 作为类型前缀/`*` 后缀跳过.
        // M30.2: `volatile` 前缀限定同理 (`typedef volatile int ...`,
        // Kw/Volatile 与 C 形式双 token 形态都要覆盖).
        loop {
            match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::KwRestrict,
                    ..
                })) => {
                    self.bump();
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::KwVolatile,
                    ..
                })) => {
                    self.bump();
                }
                Some(CppTok::Kw(CppKw::Volatile, _)) => {
                    self.bump();
                }
                _ => break,
            }
        }
        // M17.0: `decltype(expr)` 是类型, 需要整体解析
        // 形式: decltype ( expr )
        // 注: C++ lexer 把 decltype 映射为 CppTok::Kw(CppKw::Decltype),
        // 而非 CppTok::C(TokKind::KwDecltype).
        if self.at(CppKw::Decltype) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            // decltype(auto) 占位: 推出 DecltypeAuto, 后续规则同 C++14
            if self.at(CppKw::Constexpr)
                || matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::KwAuto,
                        ..
                    }))
                )
            {
                self.bump();
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        ")",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                let mut ty = MangleType::DecltypeAuto;
                while matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star,
                        ..
                    }))
                ) {
                    self.bump();
                    ty = MangleType::Pointer(Box::new(ty));
                }
                return Ok(ty);
            }
            // decltype(expr): 解析表达式, 但仅取其"形状"映射为 MangleType
            let _expr = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            // 简化: 我们对 decltype(expr) 的 int 字面量 / Ident / 函数调用都映射为 Int.
            // 更精确的规则要求"未求值操作数"类型推导, 在 lower 阶段结合 const_eval 完成.
            // 此处保守返回 Int, 实际值由 lower 在声明点处理.
            let mut ty = MangleType::Int;
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                }))
            ) {
                self.bump();
                ty = MangleType::Pointer(Box::new(ty));
            }
            return Ok(ty);
        }
        // const 是限定符, 不进类型
        let mut is_const = false;
        if self.at(CppKw::Const) {
            self.bump();
            is_const = true;
        }
        // M30.1: `typename` dependent-type 前缀 (`typename T::type` /
        // `typename std::enable_if<B, T>::type`) 同样是限定符, 吞掉后
        // 按普通限定名解析.
        if self.at(CppKw::Typename) {
            self.bump();
        }
        // 接受普通 ident (含 'auto') 和 C 类型关键字 (int, void, char, ...)
        let name = match self.peek.clone() {
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => text,
            Some(CppTok::C(Tok {
                kind: TokKind::KwAuto,
                ..
            })) => "auto".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwVoid,
                ..
            })) => "void".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwBool,
                ..
            })) => "bool".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwChar,
                ..
            })) => "char".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwShort,
                ..
            })) => "short".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwInt,
                ..
            })) => "int".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwLong,
                ..
            })) => "long".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwFloat,
                ..
            })) => "float".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwDouble,
                ..
            })) => "double".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwSigned,
                ..
            })) => "signed".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwUnsigned,
                ..
            })) => "unsigned".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwStruct,
                ..
            })) => "struct".to_string(),
            // M30.2: elaborated type specifier `class X` 参数/成员
            // (Cpp lexer 把 class 标为 Kw(Class), 非 C(Tok))
            Some(CppTok::Kw(CppKw::Class, _)) => "class".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwUnion,
                ..
            })) => "union".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwEnum,
                ..
            })) => "enum".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwAtomic,
                ..
            })) => "atomic".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwInt128,
                ..
            })) => "__int128".to_string(),
            Some(CppTok::C(Tok {
                kind: TokKind::KwUInt128,
                ..
            })) => "__uint128_t".to_string(),
            _ => {
                return Err(CppParseError::Unexpected(
                    "type",
                    format!("{:?}", self.peek),
                    self.span(),
                ))
            }
        };
        self.bump();
        // M26: 处理限定名 `std::vector` / `::std::vector` 等
        let mut full_name = name.clone();
        while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump(); // 吞 `::`
                         // 读取下一部分标识符
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) = &self.peek
            {
                full_name = format!("{}::{}", full_name, text);
                self.bump();
            } else {
                break;
            }
        }
        // enum/struct/union 后跟类型名标识符时, 消费标识符作为类型名的一部分
        // 例如 `enum location_resolution_kind` → "enum location_resolution_kind"
        // 而非仅 "enum" (否则后续标识符被误认为参数名/变量名)
        if matches!(name.as_str(), "enum" | "struct" | "union" | "class") {
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) = &self.peek
            {
                // M30.2: `class X` 的 C 输出无 class 关键字 - full_name
                // 只取类型名 (否则 lower 输出 `struct class X` 非法).
                full_name = if name == "class" {
                    text.clone()
                } else {
                    format!("{} {}", full_name, text)
                };
                self.bump();
                // 也支持限定名: enum ns::Kind
                while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
                    self.bump();
                    if let Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        text,
                        ..
                    })) = &self.peek
                    {
                        full_name = format!("{}::{}", full_name, text);
                        self.bump();
                    } else {
                        break;
                    }
                }
            } else if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }))
            ) {
                // M30.2: 匿名 tag 定义 `struct { ... }` 在类型位置
                // (typedef struct {...} N;) 不是可命名类型, 之前被当
                // Class("struct") 返回 Ok 导致 typedef 取名撞上 `{`.
                // 返回 Err 让调用方 (parse_typedef_decl 回退) 跳过定义体.
                return Err(CppParseError::Unexpected(
                    "type name",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
        }
        // 跳过 const 限定后缀 (`int const*` 等)
        if self.at(CppKw::Const) {
            self.bump();
        }
        // M30: 吞组合关键字类型并保留宽度 (`unsigned long` → Ulong,
        // `long long` → LongLong). 之前仅跳过丢弃, `unsigned long` 被
        // 降级为 32 位 Uint, 导致形参/展开类型宽度错误.
        // 仅当首个类型词是内建基类型时才组合 (避免吞类名后的类型词).
        let is_builtin_base = matches!(
            full_name.as_str(),
            "void"
                | "bool"
                | "char"
                | "short"
                | "int"
                | "long"
                | "float"
                | "double"
                | "signed"
                | "unsigned"
        );
        if is_builtin_base {
            while let Some(CppTok::C(Tok { kind, .. })) = &self.peek {
                let txt = match kind {
                    TokKind::KwLong => " long",
                    TokKind::KwShort => " short",
                    TokKind::KwChar => " char",
                    TokKind::KwInt => " int",
                    TokKind::KwSigned => " signed",
                    TokKind::KwUnsigned => " unsigned",
                    // M30.2: `long double` 组合 (strtold 等) - 缺失时
                    // double 被误认为下一个 ident 名.
                    TokKind::KwDouble => " double",
                    _ => break,
                };
                full_name.push_str(txt);
                self.bump();
            }
        }
        let mut ty = match full_name.as_str() {
            "void" => MangleType::Void,
            "bool" => MangleType::Bool,
            "char" => MangleType::Char,
            "signed char" | "char signed" => MangleType::Schar,
            "unsigned char" | "char unsigned" => MangleType::Uchar,
            "short" | "short int" | "int short" => MangleType::Short,
            "unsigned short" | "short unsigned" | "unsigned short int" => MangleType::Ushort,
            "int" | "signed" | "signed int" => MangleType::Int,
            "unsigned" | "unsigned int" => MangleType::Uint,
            "long" | "long int" | "int long" => MangleType::Long,
            "unsigned long" | "long unsigned" | "unsigned long int" | "long unsigned int" => {
                MangleType::Ulong
            }
            "long long" | "long long int" => MangleType::LongLong,
            "unsigned long long" | "long unsigned long" | "unsigned long long int" => {
                MangleType::UlongLong
            }
            "float" => MangleType::Float,
            "double" => MangleType::Double,
            "long double" | "double long" => MangleType::LongDouble,
            // M17.0: `auto` 改为 Auto 占位, 由 lower 阶段基于 init 推导
            "auto" => MangleType::Auto,
            other => MangleType::Class(other.to_string()),
        };
        // M18: `T<U1, U2, ...>` 模板实例化语法
        // 判别: ident/类名 后紧跟 `<` 且形参合法 (递归到匹配 `>`)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) && self.looks_like_template_args_after_lt()
        {
            // 嵌套深度限制: 超限直接报错 (防解析器挂起 / 栈溢出)
            if depth + 1 > MAX_TEMPLATE_DEPTH {
                return Err(CppParseError::TemplateDepthExceeded {
                    depth: depth + 1,
                    max: MAX_TEMPLATE_DEPTH,
                    span: self.span(),
                });
            }
            self.bump(); // 吞 `<`
            let mut args = Vec::new();
            // 至少 1 个实参; 跨过嵌套 `<`/`>`
            loop {
                let arg = self.parse_template_arg(depth + 1)?;
                args.push(arg);
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Comma,
                        ..
                    }))
                ) {
                    self.bump();
                    continue;
                }
                // 内层模板 `>>` 拆出的第二个 `>`: 闭合当前层
                if self.deferred_gt > 0 {
                    self.deferred_gt -= 1;
                    break;
                }
                // 期望 `>` 或 `>>` (嵌套模板)
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Gt,
                        ..
                    }))
                ) {
                    self.bump();
                    break;
                }
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Shr,
                        ..
                    }))
                ) {
                    // Shr = `>>`: 第一个 `>` 闭合当前层,
                    // 第二个 `>` 记入 deferred_gt 由外层模板消费
                    self.bump();
                    self.deferred_gt += 1;
                    break;
                }
                return Err(CppParseError::Unexpected(
                    ">",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            // 升级为 Template
            // M30: 限定名 `std::vector<...>` 用最后一段作模板名,
            // 与 register (namespace 内声明的裸名) 和
            // parse_local_var_decl (last segment) 对齐; 否则
            // 参数/成员位置的 `std::vector<int>` 实例名错误.
            let tpl_name = full_name.rsplit("::").next().unwrap_or(&name).to_string();
            ty = MangleType::Template {
                name: tpl_name.clone(),
                args,
            };
            // M30: 类型语境的 `Tpl<Args>::member` (模板类内 typedef 成员,
            // 如 std::underlying_type<E>::type). lower 阶段展开为形参
            // 替换后的真实类型.
            if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
                self.bump(); // ::
                let member = self.expect_ident()?;
                let args = match ty {
                    MangleType::Template { args, .. } => args,
                    _ => Vec::new(),
                };
                ty = MangleType::TplMember {
                    name: tpl_name,
                    args,
                    member,
                };
            }
        }
        loop {
            // M30.2: `*` 之间可有 cv 限定 (`char * const p` 合法 C),
            // 之前 break 导致 expect_ident 撞上 Const.
            if self.at(CppKw::Const) || self.at(CppKw::Volatile) {
                self.bump();
                continue;
            }
            // M30.2: `*` 后的 __restrict 同 cv 位置 (`char * __restrict p`)
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwRestrict,
                    ..
                }))
            ) {
                self.bump();
                continue;
 }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                }))
            ) {
                self.bump();
                ty = MangleType::Pointer(Box::new(ty));
                continue;
            }
            break;
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Amp,
                ..
            }))
        ) {
            self.bump();
            ty = MangleType::Reference(Box::new(ty));
            // const T& 已经由 & 创建了 Reference, is_const 不再重复包装
            is_const = false;
        }
        if is_const {
            ty = MangleType::Reference(Box::new(ty));
        } // 简化: const T (无 &) -> T* (按值传递, const 无影响)
        let _ = is_const;
        Ok(ty)
    }

    fn parse_member_fn_inline(&mut self) -> Result<CppDecl, CppParseError> {
        let mut is_virtual = false;
        if self.at(CppKw::Virtual) {
            self.bump();
            is_virtual = true;
        }
        // M17.0: 显式检测 inline / static 修饰 (类内允许)
        if self.at(CppKw::Inline) || self.at(CppKw::Static) {
            self.bump();
        }
        let ret = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        let is_const = self.skip_cv_qual();
        // M17.0: volatile 成员函数 - 在 const 之后可加 volatile
        let is_volatile = if self.at(CppKw::Volatile) {
            self.bump();
            true
        } else {
            false
        };
        let spec = self.skip_specifiers();
        // M18.0: `= 0` / `= delete` (deleted / pure virtual) - 接受为占位空体
        let peek_after_paren = self.peek.clone();
        let is_eq_zero = matches!(
            &peek_after_paren,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        );
        if is_eq_zero {
            self.bump(); // 跳过 =
            let tok0 = self.peek.clone();
            let is_value = matches!(
                &tok0,
                Some(CppTok::C(Tok {
                    kind: TokKind::Number,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) | Some(CppTok::Kw(CppKw::Delete, _))
            );
            if is_value {
                self.bump();
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppDecl::MemberFn {
                class: String::new(),
                ret,
                name,
                params,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_virtual,
                is_override: spec.is_override,
                is_const,
                is_final: spec.is_final,
                is_noexcept: spec.is_noexcept,
                is_volatile,
                is_friend: false,
            });
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppDecl::MemberFn {
                class: String::new(),
                ret,
                name,
                params,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_virtual,
                is_override: spec.is_override,
                is_const,
                is_final: spec.is_final,
                is_noexcept: spec.is_noexcept,
                is_volatile,
                is_friend: false,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::MemberFn {
            class: String::new(),
            ret,
            name,
            params,
            body,
            is_virtual,
            is_override: spec.is_override,
            is_const,
            is_final: spec.is_final,
            is_noexcept: spec.is_noexcept,
            is_volatile,
            is_friend: false,
        })
    }

    /// M17.0: 解析 `friend` 声明
    /// 形式:
    ///   friend class Foo;       -> 接受 (无 mangle)
    ///   friend int peek(S&);    -> 自由函数声明 (无 this 形参, 不 mangle)
    ///   friend int peek(S&) {}  -> 自由函数定义 (无 this 形参, 不 mangle)
    fn parse_friend_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // `friend class Foo;` 或 `friend class Foo<T>;` 简化: 接受, 跳过; 不生成任何代码
        // 注: `struct` 在 C++ lexer 中走 C 关键字分支, 不是 CppKw
        if self.at(CppKw::Class)
            || matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwStruct,
                    ..
                }))
            )
        {
            self.bump();
            // 读取类名 (如有)
            let _ = self.expect_ident();
            // 解析模板实参: friend class Foo<T>
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt()
            {
                self.bump(); // 吞 `<`
                             // 跳过模板实参 (不存储, friend 声明不影响 lower)
                let mut depth = 1;
                while depth > 0 {
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Lt,
                            ..
                        }))
                    ) {
                        depth += 1;
                        self.bump();
                    } else if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Gt,
                            ..
                        }))
                    ) {
                        depth -= 1;
                        self.bump();
                    } else if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Shr,
                            ..
                        }))
                    ) {
                        depth -= 2;
                        self.bump();
                    } else if self.peek.is_none() {
                        break;
                    } else {
                        self.bump();
                    }
                }
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            // 接受为 free fn 形式 (无 body, 不影响 lower)
            return Ok(CppDecl::FreeFn {
                ret: MangleType::Void,
                name: String::new(),
                params: vec![],
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_noexcept: false,
            });
        }
        // `friend <type> <name>(args) [const] [{...} | ;]`
        let ret = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        let _is_const = self.skip_cv_qual();
        let _is_volatile = if self.at(CppKw::Volatile) {
            self.bump();
            true
        } else {
            false
        };
        let spec = self.skip_specifiers();
        // 函数体 或 ;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppDecl::FreeFn {
                ret,
                name,
                params,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_noexcept: spec.is_noexcept,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
            is_noexcept: spec.is_noexcept,
        })
    }

    fn parse_free_fn(&mut self) -> Result<CppDecl, CppParseError> {
        // 跳过函数说明符 (inline, static, virtual, explicit, ...)
        // M30: 记录 static (Ident 起始的顶层变量声明需要内部链接信息)
        let mut is_static = false;
        loop {
            // M30.2: 说明符同认 C 形式关键字 (KwStatic/KwInline/KwAuto/
            // KwRegister) 与 GNU 宏 Ident `__inline` / `__extension__`
            // (glibc bits/byteswap.h `static __inline __uint16_t ...`).
            let is_spec = matches!(
                &self.peek,
                Some(CppTok::Kw(
                    CppKw::Inline | CppKw::Static | CppKw::Virtual | CppKw::Explicit,
                    _
                ))
            ) || matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwStatic
                        | TokKind::KwInline
                        | TokKind::KwAuto
                        | TokKind::KwRegister
                        | TokKind::KwExtern,
                    ..
                }))
            )
                || match &self.peek {
                    Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) => {
                        text == "__inline" || text == "__inline__" || text == "__extension__"
                    }
                    _ => false,
                };
            if is_spec {
                let is_c_static = matches!(
                    &self.peek,
                    Some(CppTok::Kw(CppKw::Static, _))
                ) || matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::KwStatic,
                        ..
                    }))
                );
                if is_c_static {
                    is_static = true;
                }
                self.bump();
            } else {
                break;
            }
        }
        // M18.0: 解析限定名返回类型 `views::filter_view` / `std::ranges::foo`
        // 形式: [::] ident (:: ident)*  后跟 ident 名字和 (...
        let ret = if self.is_qualified_type_start() {
            let parts = self.parse_qualified_name()?;
            let last = parts.last().cloned().unwrap_or_default();
            let mut ty = MangleType::Class(last);
            // 跳过多 token 类型后缀
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwLong
                        | TokKind::KwShort
                        | TokKind::KwChar
                        | TokKind::KwInt
                        | TokKind::KwSigned
                        | TokKind::KwUnsigned,
                    ..
                }))
            ) {
                self.bump();
            }
            // * 或 & 修饰
            loop {
                if self.at(CppKw::Const) {
                    self.bump();
                }
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    }))
                ) {
                    self.bump();
                    ty = MangleType::Pointer(Box::new(ty));
                } else {
                    break;
                }
            }
            ty
        } else {
            self.parse_mangle_type()?
        };
        let name = self.expect_ident()?;
        // M30.2: 名字后是 `::` → 类外成员定义 `Ret Class::method(...)` /
        // `Class::Class(...)` / `Class::~Class(...)`.
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            return self.parse_out_of_class_member(name, ret);
        }
        // M30: 名字后不是 `(` → 全局变量声明 (`Tpl<Args>::member g = 5;`
        // / `MyType g;`). 变量与函数共用 Ident 起始的声明入口, 变量
        // 尾部 (init / `;`) 由 finish_global_var 完成.
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return self.finish_global_var(ret, name, is_static);
        }
        let params = self.parse_param_list()?;
        self.skip_cv_qual();
        // M30.2: GNU 尾缀 `__attribute__ ((__nothrow__ ...))` /
        // `__asm__ ("" "__isoc23_scanf")` (glibc 头大量使用).
        self.skip_gnu_suffixes();
        // M26.1: 自由函数 noexcept 修饰 (noexcept / noexcept(expr))
        let spec = self.skip_specifiers();
        // M18.0: 函数声明 (无 body) `int foo(...);` 接受为声明, 不强制 body
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppDecl::FreeFn {
                ret,
                name,
                params,
                body: CppBody {
                stmts: vec![],
                is_decl: true,
            },
                is_noexcept: spec.is_noexcept,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
            is_noexcept: spec.is_noexcept,
        })
    }

    /// M30: 解析顶层全局变量声明 `<type> name (= expr)? ;`
    /// 由 parse_free_fn (Ident 起始类型) 与 parse_decl (类型关键字起始)
    /// 共用. 仅支持单个声明符 (`,` 多声明符仍走降级路径).
    fn parse_global_var_decl(&mut self) -> Result<CppDecl, CppParseError> {
        let mut is_static = false;
        loop {
            if self.at(CppKw::Static) {
                is_static = true;
                self.bump();
            } else if self.at(CppKw::Const) || self.at(CppKw::Volatile) {
                self.bump();
            } else {
                break;
            }
        }
        let mut ty = self.parse_mangle_type()?;
        // `&` 后缀兜底 (`*` 已由 parse_mangle_type 处理)
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Amp,
                ..
            }))
        ) {
            self.bump();
            ty = MangleType::Reference(Box::new(ty));
        }
        let name = self.expect_ident()?;
        // M30.2: 全局变量数组下标 `extern char *__tzname[2];` - depth 跳
        // (数组退化为指针).
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            self.bump();
            let mut depth = 1i32;
            while let Some(t) = self.peek.clone() {
                match t {
                    CppTok::C(Tok {
                        kind: TokKind::LBracket,
                        ..
                    }) => {
                        depth += 1;
                        self.bump();
                    }
                    CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }) => {
                        self.bump();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {
                        self.bump();
                    }
                }
            }
        }
        self.finish_global_var(ty, name, is_static)
    }

    /// M30: 完成全局变量声明尾部 (`= init` / `;`)
    fn finish_global_var(
        &mut self,
        ty: MangleType,
        name: String,
        is_static: bool,
    ) -> Result<CppDecl, CppParseError> {
        let init = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        } else {
            // `,` 多声明符等尾部不支持, 保持旧降级行为 (报 Unsupported)
            return Err(CppParseError::Unsupported(
                "global variable declarator",
                self.span(),
            ));
        }
        Ok(CppDecl::GlobalVar {
            ty,
            name,
            init,
            is_static,
        })
    }

    /// M18.0: 探测当前位置是否是带限定返回类型的自由函数声明
    /// 形式: [inline|static]* [::] ident (:: ident)* ident '('
    ///   例如: `views::filter_view f()` / `inline std::foo bar()`
    ///   不要求 `*`/`&` 修饰 (那是 ident 后的事)
    fn is_qualified_return_fn(&self) -> bool {
        let mut i = 0;
        // 跳过说明符
        while matches!(
            self.peek_token_at(i),
            Some(CppTok::Kw(
                CppKw::Inline | CppKw::Static | CppKw::Virtual | CppKw::Explicit,
                _
            ))
        ) {
            i += 1;
        }
        // 必须有 ident (返回类型第一段)
        let start = i;
        if matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
            i += 1;
        } else if !matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) {
            return false;
        }
        i += 1;
        // 必须有 :: ident 至少一段
        if !matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
            return false;
        }
        i += 1;
        if !matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) {
            return false;
        }
        i += 1;
        // 继续跳过更多 `:: ident`
        while matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
            i += 1;
            if !matches!(
                self.peek_token_at(i),
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) {
                return false;
            }
            i += 1;
        }
        // 跳过 * 和 &
        while matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Star | TokKind::Amp,
                ..
            }))
        ) {
            i += 1;
        }
        // 现在 i 必须指向 ident (函数名) + (下一 token)
        if !matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) {
            return false;
        }
        if !matches!(
            self.peek_token_at(i + 1),
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return false;
        }
        let _ = start;
        true
    }

    fn parse_body(&mut self) -> Result<CppBody, CppParseError> {
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut stmts = vec![];
        while let Some(t) = &self.peek {
            if matches!(
                t,
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
            ) {
                break;
            }
            stmts.push(self.parse_stmt()?);
        }
        self.bump();
        Ok(CppBody {
            stmts,
            is_decl: false,
        })
    }

    /// 解析 body 或单个 stmt (用于 if/while 等的 body, 支持无 `{}` 的单语句)
    fn parse_body_or_stmt(&mut self) -> Result<CppBody, CppParseError> {
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            self.parse_body()
        } else {
            let stmt = self.parse_stmt()?;
            Ok(CppBody {
            stmts: vec![stmt],
            is_decl: false,
        })
        }
    }

    fn parse_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            return Ok(CppStmt::Block(CppBody {
                stmts: vec![],
                is_decl: false,
            }));
        }
        if self.at(CppKw::Try) {
            return self.parse_try_stmt();
        }
        if self.at(CppKw::Throw) {
            return self.parse_throw_stmt();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwIf,
                ..
            }))
        ) {
            return self.parse_if_stmt();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwWhile,
                ..
            }))
        ) {
            return self.parse_while_stmt();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwFor,
                ..
            }))
        ) {
            return self.parse_for_stmt();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwReturn,
                ..
            }))
        ) {
            self.bump();
            let val = if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::Return(val));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwBreak,
                ..
            }))
        ) {
            self.bump();
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::Break);
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwContinue,
                ..
            }))
        ) {
            self.bump();
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::Continue);
        }
        // switch (expr) { case val: stmts... default: stmts... }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwSwitch,
                ..
            }))
        ) {
            return self.parse_switch_stmt();
        }
        // C++20 协程 (M9: 占位, 直接走对应 C 表达式)
        if self.at(CppKw::CoAwait) {
            self.bump();
            let e = self.parse_expr()?;
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::CoAwait(e));
        }
        if self.at(CppKw::CoYield) {
            self.bump();
            let e = self.parse_expr()?;
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::CoYield(e));
        }
        if self.at(CppKw::CoReturn) {
            self.bump();
            let val = if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::CoReturn(val));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            let body = self.parse_body()?;
            return Ok(CppStmt::Block(body));
        }
        // M9: 局部变量声明 `Type name;` / `Type name = expr;` / `auto name = expr;`
        // 严格说应单独走 CppStmt::LocalVar, 这里降级为 Lower 阶段识别:
        // 暂作为 Expr 处理 (Lower 阶段根据 ty/=expr 重写)
        if self.is_local_var_decl_start() {
            return self.parse_local_var_decl();
        }
        let e = self.parse_expr()?;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppStmt::Expr(e))
    }

    fn parse_try_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.expect_kw(CppKw::Try)?;
        let body = self.parse_body()?;
        let mut catches = vec![];
        while self.at(CppKw::Catch) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            // catch (...) 或 catch (T& e)
            let (ty, name) = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ellipsis,
                    ..
                }))
            ) {
                self.bump();
                (None, None)
            } else {
                let ty = self.parse_mangle_type()?;
                let name = if let Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) = &self.peek
                {
                    Some(self.expect_ident()?)
                } else {
                    None
                };
                (Some(ty), name)
            };
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let b = self.parse_body()?;
            catches.push(CatchClause { ty, name, body: b });
        }
        Ok(CppStmt::Try {
            body: Box::new(body),
            catches,
        })
    }

    fn parse_throw_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.expect_kw(CppKw::Throw)?;
        let val = if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(CppStmt::Throw(val))
    }

    fn parse_if_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        // if 已在 peek 中
        self.bump();
        // M17.0: `if constexpr (cond) { ... }` 编译期分支
        let is_constexpr = if self.at(CppKw::Constexpr) {
            self.bump();
            true
        } else {
            false
        };
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "(",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let cond = self.parse_expr()?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                ")",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let then = self.parse_body_or_stmt()?;
        let els = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwElse,
                ..
            }))
        ) {
            self.bump();
            Some(Box::new(self.parse_body_or_stmt()?))
        } else {
            None
        };
        if is_constexpr {
            Ok(CppStmt::IfConstexpr {
                cond,
                then: Box::new(then),
                els,
            })
        } else {
            Ok(CppStmt::If {
                cond,
                then: Box::new(then),
                els,
            })
        }
    }

    fn parse_while_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.bump();
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "(",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let cond = self.parse_expr()?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                ")",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let body = self.parse_body_or_stmt()?;
        Ok(CppStmt::While {
            cond,
            body: Box::new(body),
        })
    }

    fn parse_for_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.bump();
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "(",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let init = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
            None
        } else {
            let s = self.parse_stmt()?;
            Some(Box::new(s))
        };
        let cond = if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        let step = if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RParen,
                ..
            }))
        ) {
            Some(Box::new(CppStmt::Expr(self.parse_expr()?)))
        } else {
            None
        };
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                ")",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let body = self.parse_body()?;
        Ok(CppStmt::For {
            init,
            cond,
            step,
            body: Box::new(body),
        })
    }

    /// Parse `switch (expr) { case val: stmts... default: stmts... }`
    /// case/default are label prefixes inside the brace body, not standalone stmts.
    fn parse_switch_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        // bump past `switch`
        self.bump();
        // expect `(`
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "(",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let expr = self.parse_expr()?;
        // expect `)`
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RParen,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                ")",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        // expect `{`
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "{",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        // Parse body: collect case/default arms
        let mut arms: Vec<(Option<CppExpr>, CppBody)> = Vec::new();
        let mut cur_expr: Option<CppExpr> = None;
        let mut cur_stmts: Vec<CppStmt> = Vec::new();
        while !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RBrace,
                ..
            }))
        ) {
            // Check for `case expr:`
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwCase,
                    ..
                }))
            ) {
                // Push previous arm if any
                if cur_expr.is_some() || !cur_stmts.is_empty() {
                    arms.push((
                        cur_expr.take(),
                        CppBody {
                            stmts: std::mem::take(&mut cur_stmts),
                            is_decl: false,
                        },
                    ));
                }
                self.bump(); // past `case`
                let case_expr = self.parse_expr()?;
                // expect `:`
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Colon,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        ":",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                cur_expr = Some(case_expr);
                continue;
            }
            // Check for `default:`
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwDefault,
                    ..
                }))
            ) {
                // Push previous arm if any
                if cur_expr.is_some() || !cur_stmts.is_empty() {
                    arms.push((
                        cur_expr.take(),
                        CppBody {
                            stmts: std::mem::take(&mut cur_stmts),
                            is_decl: false,
                        },
                    ));
                }
                self.bump(); // past `default`
                // expect `:`
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Colon,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        ":",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                cur_expr = None; // None = default arm
                continue;
            }
            // Regular statement inside an arm
            cur_stmts.push(self.parse_stmt()?);
        }
        // Push last arm
        if cur_expr.is_some() || !cur_stmts.is_empty() {
            arms.push((
                cur_expr,
                CppBody {
                    stmts: cur_stmts,
                    is_decl: false,
                },
            ));
        }
        // bump past `}`
        self.bump();
        Ok(CppStmt::Switch { expr, arms })
    }

    fn parse_expr(&mut self) -> Result<CppExpr, CppParseError> {
        // 三元: a ? b : c
        let e = self.parse_assignment()?;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Question,
                ..
            }))
        ) {
            self.bump();
            let then = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Colon,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ":",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let els = self.parse_expr()?;
            return Ok(CppExpr::Ternary {
                cond: Box::new(e),
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        Ok(e)
    }

    fn parse_assignment(&mut self) -> Result<CppExpr, CppParseError> {
        let l = self.parse_or()?;
        let asop = match &self.peek {
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            })) => Some(BinOpKind::Assign),
            Some(CppTok::C(Tok {
                kind: TokKind::PlusEq,
                ..
            })) => Some(BinOpKind::AddAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::MinusEq,
                ..
            })) => Some(BinOpKind::SubAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::StarEq,
                ..
            })) => Some(BinOpKind::MulAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::SlashEq,
                ..
            })) => Some(BinOpKind::DivAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::PercentEq,
                ..
            })) => Some(BinOpKind::ModAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::ShlEq,
                ..
            })) => Some(BinOpKind::ShlAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::ShrEq,
                ..
            })) => Some(BinOpKind::ShrAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::AmpEq,
                ..
            })) => Some(BinOpKind::AndAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::PipeEq,
                ..
            })) => Some(BinOpKind::OrAssign),
            Some(CppTok::C(Tok {
                kind: TokKind::CaretEq,
                ..
            })) => Some(BinOpKind::XorAssign),
            _ => None,
        };
        if let Some(op) = asop {
            self.bump();
            let r = self.parse_assignment()?;
            return Ok(CppExpr::BinOp(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn parse_or(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_and()?;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::OrOr,
                ..
            }))
        ) {
            self.bump();
            let r = self.parse_and()?;
            l = CppExpr::BinOp(BinOpKind::LOr, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_bitor()?;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::AndAnd,
                ..
            }))
        ) {
            self.bump();
            let r = self.parse_bitor()?;
            l = CppExpr::BinOp(BinOpKind::LAnd, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitor(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_bitxor()?;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Pipe,
                ..
            }))
        ) {
            self.bump();
            let r = self.parse_bitxor()?;
            l = CppExpr::BinOp(BinOpKind::Or, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitxor(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_bitand()?;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Caret,
                ..
            }))
        ) {
            self.bump();
            let r = self.parse_bitand()?;
            l = CppExpr::BinOp(BinOpKind::Xor, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitand(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_equality()?;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Amp,
                ..
            }))
        ) {
            self.bump();
            let r = self.parse_equality()?;
            l = CppExpr::BinOp(BinOpKind::And, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_equality(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_relational()?;
        loop {
            let op = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::EqEq,
                    ..
                })) => BinOpKind::EqEq,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ne, ..
                })) => BinOpKind::Ne,
                _ => break,
            };
            self.bump();
            let r = self.parse_relational()?;
            l = CppExpr::BinOp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_relational(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_shift()?;
        loop {
            let op = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                })) => BinOpKind::Lt,
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                })) => BinOpKind::Gt,
                Some(CppTok::C(Tok {
                    kind: TokKind::Le, ..
                })) => BinOpKind::Le,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ge, ..
                })) => BinOpKind::Ge,
                _ => break,
            };
            self.bump();
            let r = self.parse_shift()?;
            l = CppExpr::BinOp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_shift(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_additive()?;
        loop {
            let op = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Shl, ..
                })) => BinOpKind::Shl,
                Some(CppTok::C(Tok {
                    kind: TokKind::Shr, ..
                })) => BinOpKind::Shr,
                _ => break,
            };
            self.bump();
            let r = self.parse_additive()?;
            l = CppExpr::BinOp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_additive(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_multiplicative()?;
        loop {
            let op = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Plus,
                    ..
                })) => BinOpKind::Add,
                Some(CppTok::C(Tok {
                    kind: TokKind::Minus,
                    ..
                })) => BinOpKind::Sub,
                _ => break,
            };
            self.bump();
            let r = self.parse_multiplicative()?;
            l = CppExpr::BinOp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_multiplicative(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_unary()?;
        loop {
            let op = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                })) => BinOpKind::Mul,
                Some(CppTok::C(Tok {
                    kind: TokKind::Slash,
                    ..
                })) => BinOpKind::Div,
                Some(CppTok::C(Tok {
                    kind: TokKind::Percent,
                    ..
                })) => BinOpKind::Mod,
                _ => break,
            };
            self.bump();
            let r = self.parse_unary()?;
            l = CppExpr::BinOp(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_unary(&mut self) -> Result<CppExpr, CppParseError> {
        // 前缀 ++ -- + - ! ~ * &
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::PlusPlus,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::PreInc, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::MinusMinus,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::PreDec, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Plus,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Plus, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Minus,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Neg, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Not,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::LNot, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Tilde,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Not, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Star,
                ..
            }))
        ) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Deref, Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Amp,
                ..
            }))
        ) {
            // M10+: 检查是否是 `&Class::method` (成员函数指针)
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })) = self.peek_token_at(1)
            {
                // 看是否是 qualified name: &Foo::bar
                let mut clone = self.lex.clone();
                clone.next_tok(); // skip &
                let mut parts = vec![];
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) = clone.next_tok()
                {
                    parts.push(text);
                    if let Some(CppTok::ColonColon(_)) = clone.next_tok() {
                        if let Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            text,
                            ..
                        })) = clone.next_tok()
                        {
                            parts.push(text);
                            // &Foo::bar
                            self.bump(); // &
                            let class = self.expect_ident()?;
                            self.bump(); // ::
                            let name = self.expect_ident()?;
                            return Ok(CppExpr::MemberPtr { class, name });
                        }
                    }
                }
            }
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::AddrOf(Box::new(e)));
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwSizeof,
                ..
            }))
        ) {
            self.bump();
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                self.bump();
                let ty = self.parse_mangle_type()?;
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        ")",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                return Ok(CppExpr::Sizeof(SizeofArg::Type(ty)));
            }
            let e = self.parse_unary()?;
            return Ok(CppExpr::Sizeof(SizeofArg::Expr(Box::new(e))));
        }
        if self.at(CppKw::Typeid) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            // M17.0: typeid(T) 静态类型 RTTI
            // 试探: 若 LParen 后是 type-id (decltype 开头 / struct/typename 关键字 / 已知类型关键字)
            // 走静态路径. 否则按 typeid(e) 路径.
            if let Some(t) = self.peek.clone() {
                let is_type_id = matches!(
                    t,
                    CppTok::C(Tok {
                        kind: TokKind::KwVoid
                            | TokKind::KwInt
                            | TokKind::KwChar
                            | TokKind::KwShort
                            | TokKind::KwLong
                            | TokKind::KwFloat
                            | TokKind::KwDouble
                            | TokKind::KwSigned
                            | TokKind::KwUnsigned
                            | TokKind::KwBool
                            | TokKind::KwAuto
                            | TokKind::KwStruct
                            | TokKind::KwUnion
                            | TokKind::KwEnum
                            | TokKind::KwConst
                            | TokKind::KwInt128
                            | TokKind::KwUInt128,
                        ..
                    })
                ) || self.at(CppKw::Decltype)
                    || matches!(
                        t,
                        CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        })
                    );
                if is_type_id {
                    let ty = self.parse_mangle_type()?;
                    if !matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }))
                    ) {
                        return Err(CppParseError::Unexpected(
                            ")",
                            format!("{:?}", self.peek),
                            self.span(),
                        ));
                    }
                    self.bump();
                    return Ok(CppExpr::TypeidType(ty));
                }
            }
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(CppExpr::Typeid(Box::new(e)));
        }
        // M17.0: `noexcept(expr)` 一元操作符
        if self.at(CppKw::Noexcept) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(CppExpr::Noexcept(Box::new(e)));
        }
        if self.at(CppKw::DynamicCast) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "<",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ">",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(CppExpr::DynamicCast {
                ty,
                expr: Box::new(e),
            });
        }
        if self.at(CppKw::StaticCast) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "<",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ">",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(CppExpr::StaticCast {
                ty,
                expr: Box::new(e),
            });
        }
        if self.at(CppKw::ConstCast) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "<",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ">",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(CppExpr::ConstCast {
                ty,
                expr: Box::new(e),
            });
        }
        if self.at(CppKw::ReinterpretCast) {
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "<",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ">",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "(",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(CppExpr::ReinterpretCast {
                ty,
                expr: Box::new(e),
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<CppExpr, CppParseError> {
        let mut e = self.parse_primary()?;
        loop {
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                self.bump();
                let mut a = vec![];
                while !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    a.push(self.parse_expr()?);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                    }
                }
                self.bump();
                e = CppExpr::Call(Box::new(e), a);
                continue;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }))
            ) {
                self.bump();
                // M18: 检测多维下标 `obj[i, j, k]`
                let mut indices = vec![self.parse_expr()?];
                while matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Comma,
                        ..
                    }))
                ) {
                    self.bump();
                    indices.push(self.parse_expr()?);
                }
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        "]",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                if indices.len() == 1 {
                    e = CppExpr::BinOp(BinOpKind::Index, Box::new(e), Box::new(indices.remove(0)));
                } else {
                    e = CppExpr::MultiIndex(Box::new(e), indices);
                }
                continue;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Dot,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::DotStar,
                    ..
                }))
            ) {
                // M17.0: `.*` 成员指针访问 (区别于 `.` 成员访问)
                // P2.1: 接受 lexer 产生的 `DotStar` 单 token 以及
                // 手动合成的 `Dot` + `Star` (老路径).
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::DotStar,
                        ..
                    }))
                ) {
                    self.bump();
                    let m = self.expect_ident()?;
                    e = CppExpr::MemberStar(Box::new(e), m);
                } else {
                    self.bump();
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star,
                            ..
                        }))
                    ) {
                        self.bump();
                        let m = self.expect_ident()?;
                        e = CppExpr::MemberStar(Box::new(e), m);
                    } else {
                        let m = self.expect_ident()?;
                        e = CppExpr::Member(Box::new(e), m);
                    }
                }
                continue;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Arrow,
                    ..
                }))
            ) {
                self.bump();
                // M17.0: `->*` 成员指针访问
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star,
                        ..
                    }))
                ) {
                    self.bump();
                    let m = self.expect_ident()?;
                    e = CppExpr::PtrMemberStar(Box::new(e), m);
                } else {
                    let m = self.expect_ident()?;
                    e = CppExpr::PtrMember(Box::new(e), m);
                }
                continue;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::PlusPlus,
                    ..
                }))
            ) {
                self.bump();
                e = CppExpr::UnaryOp(UnaryOpKind::PostInc, Box::new(e));
                continue;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::MinusMinus,
                    ..
                }))
            ) {
                self.bump();
                e = CppExpr::UnaryOp(UnaryOpKind::PostDec, Box::new(e));
                continue;
            }
            break;
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<CppExpr, CppParseError> {
        if self.at(CppKw::This) {
            self.bump();
            return Ok(CppExpr::This);
        }
        if self.at(CppKw::True) {
            self.bump();
            return Ok(CppExpr::IntLit(1));
        }
        if self.at(CppKw::False) {
            self.bump();
            return Ok(CppExpr::IntLit(0));
        }
        if self.at(CppKw::Nullptr) {
            self.bump();
            return Ok(CppExpr::IntLit(0));
        }
        if self.at(CppKw::New) {
            return self.parse_new();
        }
        if self.at(CppKw::Delete) {
            self.bump();
            // M17.0: `delete[] p` 数组释放
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }))
            ) {
                self.bump();
                if !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }))
                ) {
                    return Err(CppParseError::Unexpected(
                        "]",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                self.bump();
                let p = self.parse_expr()?;
                return Ok(CppExpr::DeleteArr(Box::new(p)));
            }
            let p = self.parse_expr()?;
            return Ok(CppExpr::Delete(Box::new(p)));
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Number,
            text,
            ..
        })) = &self.peek
        {
            // M26.1: 区分浮点字面量与整数字面量; 整数支持 0x/0b/0o 前缀与
            // u/l/ll 等后缀 (之前 text.parse::<i64>() 只支持十进制, 其余全变 0).
            if is_float_number(text) {
                let t = text.clone();
                self.bump();
                return Ok(CppExpr::FloatLit(t));
            }
            let v = parse_int_text(text);
            self.bump();
            return Ok(CppExpr::IntLit(v));
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::StringLit,
            text,
            ..
        })) = &self.peek
        {
            // 字符串字面量 (拼接支持)
            let mut parts = vec![strip_quotes(text)];
            self.bump();
            while let Some(CppTok::C(Tok {
                kind: TokKind::StringLit,
                text,
                ..
            })) = &self.peek
            {
                parts.push(strip_quotes(text));
                self.bump();
            }
            if parts.len() == 1 {
                return Ok(CppExpr::StrLit(parts.remove(0)));
            }
            return Ok(CppExpr::StrConcat(parts));
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::CharLit,
            text,
            ..
        })) = &self.peek
        {
            let s = strip_quotes(text);
            self.bump();
            // M26.1: 支持转义字符 (\n \t \\ \' \" \0 \xNN 等), 之前只取首个字符.
            let v = char_lit_value(&s);
            return Ok(CppExpr::IntLit(v));
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::LParen,
            ..
        })) = &self.peek
        {
            // M11 stdlib: 区分 C 风格转换 `(T)expr` 与括号表达式 `(expr)`.
            // 先保存 lexer 状态 + 当前 LParen token, 尝试解析类型, 如果类型后是 `)` 则认为是转换.
            // 否则回退为括号表达式.
            // 但: 若 `(` 后第一个 token 是 `!` / 字面量 / ident-with-following-EqEq 等
            // 表达式标志, 直接走括号表达式, 避免误把 `!(p == "/tmp")` 当成 cast.
            let lparen = self.peek.clone();
            let nxt = self.peek_token_at(1);
            let starts_with_expr_token = matches!(
                nxt,
                Some(CppTok::C(Tok {
                    kind: TokKind::Not,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Number,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::StringLit,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::CharLit,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Minus,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Plus,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Tilde,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::Amp,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::PlusPlus,
                    ..
                })) | Some(CppTok::C(Tok {
                    kind: TokKind::MinusMinus,
                    ..
                })) | Some(CppTok::Kw(CppKw::This, _))
                    | Some(CppTok::Kw(CppKw::True, _))
                    | Some(CppTok::Kw(CppKw::False, _))
                    | Some(CppTok::Kw(CppKw::Nullptr, _))
            );
            let mut did_cast_detection = false;
            if !starts_with_expr_token {
                did_cast_detection = true;
                let saved = self.lex.clone();
                self.bump();
                match self.parse_mangle_type() {
                    Ok(ty) => {
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::RParen,
                                ..
                            }))
                        ) {
                            // 确认是 C 风格转换
                            self.bump(); // 吞 `)`
                            let e = self.parse_unary()?;
                            return Ok(CppExpr::Cast(ty, Box::new(e)));
                        }
                        // 不是转换, 恢复 lexer 状态, 重新解析为括号表达式
                        self.lex = saved;
                        self.peek = lparen.clone();
                    }
                    Err(_) => {
                        // 解析类型失败, 恢复 lexer 状态
                        self.lex = saved;
                        self.peek = lparen.clone();
                    }
                }
            }
            // 普通括号表达式
            // 若已经做过 cast 检测, self.peek 已是 LParen, bump 一下吞掉它
            // 否则直接 bump 吞 LParen
            if !did_cast_detection {
                self.bump(); // 吞 `(`
            } else {
                // cast 检测恢复后 self.peek = LParen, 这里 bump 一下
                self.bump();
            }
            let e = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            return Ok(e);
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::LBrace,
            ..
        })) = &self.peek
        {
            // M10+: 大括号初始化列表 `{1, 2, 3}` (initializer_list)
            return self.parse_init_list();
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::LBracket,
            ..
        })) = &self.peek
        {
            // Lambda
            return self.parse_lambda();
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek.clone()
        {
            // qualified name?
            if matches!(&self.peek_token_at(1), Some(CppTok::ColonColon(_))) {
                // M29: 模板实例 typedef 别名 `alias::member` -> TplStaticAccess
                if let Some(CppTok::C(Tok { text, .. })) = &self.peek {
                    if let Some((tname, targs)) = self.typedef_alias.get(text).cloned() {
                        self.bump(); // alias
                        self.bump(); // ::
                        let member = self.expect_ident()?;
                        return Ok(CppExpr::TplStaticAccess {
                            name: tname,
                            args: targs,
                            member,
                        });
                    }
                }
                let parts = self.parse_qualified_name()?;
                // M30.1: 限定名 + 模板实参 + `::member`
                // (`std::conditional<true, T, F>::type` /
                // `std::is_trivially_destructible<char>::value`).
                // 此前 qualified 分支直接返回 QualifiedAccess, `<` 落到
                // 比较运算符导致整个表达式解析失败.
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Lt,
                        ..
                    }))
                ) && self.looks_like_template_args_after_lt()
                {
                    let last = parts.last().cloned().unwrap_or_default();
                    self.bump(); // 吞 `<`
                    let mut targs: Vec<MangleType> = Vec::new();
                    loop {
                        let arg = self.parse_template_arg(1)?;
                        targs.push(arg);
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Comma,
                                ..
                            }))
                        ) {
                            self.bump();
                            continue;
                        }
                        if self.deferred_gt > 0 {
                            self.deferred_gt -= 1;
                            break;
                        }
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Gt,
                                ..
                            }))
                        ) {
                            self.bump();
                            break;
                        }
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Shr,
                                ..
                            }))
                        ) {
                            self.bump();
                            self.deferred_gt += 1;
                            break;
                        }
                        return Err(CppParseError::Unexpected(
                            ">",
                            format!("{:?}", self.peek),
                            self.span(),
                        ));
                    }
                    // 限定模板实例名后跟 `::`: 静态成员 / 枚举常量访问,
                    // 与裸名分支 (TplStaticAccess) 同构
                    if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
                        self.bump(); // ::
                        let member = self.expect_ident()?;
                        return Ok(CppExpr::TplStaticAccess {
                            name: last,
                            args: targs,
                            member,
                        });
                    }
                    // 无 `::` 尾缀的限定模板实例表达式: 回退为 TemplateIdent
                    return Ok(CppExpr::TemplateIdent {
                        name: last,
                        args: targs,
                    });
                }
                if let Some(last) = parts.last().cloned() {
                    let path = parts[..parts.len() - 1].to_vec();
                    return Ok(CppExpr::QualifiedAccess(path, last));
                }
            }
            let name = text.clone();
            self.bump();
            // 模板实例化: Name<...> (须与比较 a < b 区分)
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt()
            {
                // M26: 解析并保存显式模板实参 (供 lower 阶段实例化模板函数)
                self.bump(); // 吞 `<`
                let mut targs: Vec<MangleType> = Vec::new();
                loop {
                    let arg = self.parse_template_arg(1)?;
                    targs.push(arg);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                        continue;
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Gt,
                            ..
                        }))
                    ) {
                        self.bump();
                        break;
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Shr,
                            ..
                        }))
                    ) {
                        self.bump();
                        self.deferred_gt += 1;
                        break;
                    }
                    return Err(CppParseError::Unexpected(
                        ">",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                // 模板实例名后跟 `::`: 静态成员 / 枚举常量访问
                // (`S1<int,int>::v`). 保留实参以便 lower 触发实例化.
                if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
                    self.bump(); // ::
                    let member = self.expect_ident()?;
                    return Ok(CppExpr::TplStaticAccess {
                        name,
                        args: targs,
                        member,
                    });
                }
                return Ok(CppExpr::TemplateIdent { name, args: targs });
            }
            return Ok(CppExpr::Ident(name));
        }
        // M11 stdlib: `::name` 全局作用域访问
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            let parts = self.parse_qualified_name()?;
            if let Some(last) = parts.last().cloned() {
                let path = parts[..parts.len() - 1].to_vec();
                return Ok(CppExpr::QualifiedAccess(path, last));
            }
        }
        // 把 `auto` 当作类型占位 (M8: 不做实际类型推断)
        if let Some(CppTok::C(Tok {
            kind: TokKind::KwAuto,
            ..
        })) = &self.peek
        {
            self.bump();
            return Ok(CppExpr::Ident("auto".into()));
        }
        Err(CppParseError::Unsupported("expression", self.span()))
    }

    /// 解析 lambda: [captures](params){ body }
    fn parse_lambda(&mut self) -> Result<CppExpr, CppParseError> {
        // 已经在 [
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "[",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        let mut captures = vec![];
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RBracket,
                ..
            }))
        ) {
            // 默认捕获 [&] / [=]
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Amp,
                    ..
                }))
            ) {
                self.bump();
                captures.push(Capture::ByRefAll);
            } else if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                captures.push(Capture::ByValAll);
            } else {
                loop {
                    if self.at(CppKw::This) {
                        self.bump();
                        captures.push(Capture::This);
                    } else if let Some(CppTok::C(Tok {
                        kind: TokKind::Amp, ..
                    })) = &self.peek
                    {
                        self.bump();
                        let n = self.expect_ident()?;
                        captures.push(Capture::ByRef(n));
                    } else if let Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        text,
                        ..
                    })) = &self.peek.clone()
                    {
                        let n = text.clone();
                        self.bump();
                        captures.push(Capture::ByVal(n));
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
        }
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RBracket,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "]",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        // 形参列表 (可选)
        let params = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            self.parse_param_list()?
        } else {
            vec![]
        };
        // const 等限定
        self.skip_cv_qual();
        // body
        let body = self.parse_body()?;
        let closure_name = format!("__lambda_{}", self.lambdas.len());
        let lam = LambdaExpr {
            captures,
            params,
            body,
            closure_name: closure_name.clone(),
        };
        self.lambdas.push(lam);
        Ok(CppExpr::Lambda(LambdaExpr {
            captures: self.lambdas.last().unwrap().captures.clone(),
            params: self.lambdas.last().unwrap().params.clone(),
            body: self.lambdas.last().unwrap().body.clone(),
            closure_name,
        }))
    }

    /// new 表达式中类型名: Ident (class) 或 C 类型关键字 (int/float/...) 或 char
    fn parse_new_type(&mut self) -> Result<String, CppParseError> {
        match self.bump() {
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => Ok(text),
            Some(CppTok::C(Tok {
                kind: TokKind::KwInt,
                ..
            })) => Ok("int".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwChar,
                ..
            })) => Ok("char".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwLong,
                ..
            })) => Ok("long".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwShort,
                ..
            })) => Ok("short".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwFloat,
                ..
            })) => Ok("float".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwDouble,
                ..
            })) => Ok("double".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwVoid,
                ..
            })) => Ok("void".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwBool,
                ..
            })) => Ok("bool".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwSigned,
                ..
            })) => Ok("int".to_string()),
            Some(CppTok::C(Tok {
                kind: TokKind::KwUnsigned,
                ..
            })) => Ok("unsigned".to_string()),
            Some(CppTok::Kw(CppKw::Typename, _)) => {
                // 简化: typename 后面直接接 ident
                self.expect_ident()
            }
            Some(other) => {
                let got = format!("{:?}", other);
                Err(CppParseError::Unexpected("type-name", got, other.span()))
            }
            None => Err(CppParseError::Unexpected(
                "type-name",
                "EOF".into(),
                self.span(),
            )),
        }
    }

    /// 解析 new T(args) 或 new T[](args)
    fn parse_new(&mut self) -> Result<CppExpr, CppParseError> {
        self.bump(); // new
                     // M17.0: placement new - `new (ptr) T(args)`
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            self.bump();
            let ptr = self.parse_expr()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            let ty = self.parse_new_type()?;
            let args = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                self.bump();
                let mut a = vec![];
                while !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    a.push(self.parse_expr()?);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                    }
                }
                self.bump();
                a
            } else {
                vec![]
            };
            return Ok(CppExpr::PlacementNew {
                ptr: Box::new(ptr),
                ty,
                args,
            });
        }
        // 标准 new T(args) / new T[n] - T 可能是 class 名 (Ident) 或内建类型 (int/float/...)
        let ty = self.parse_new_type()?;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            self.bump();
            let count = if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RBracket,
                    ..
                }))
            ) {
                self.parse_expr()?
            } else {
                CppExpr::IntLit(0)
            };
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RBracket,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "]",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            // 可选的 ctor 调用 `new T[n]()`
            let _args = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                self.bump();
                let mut a = vec![];
                while !matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                ) {
                    a.push(self.parse_expr()?);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                    }
                }
                self.bump();
                a
            } else {
                vec![]
            };
            return Ok(CppExpr::NewArr {
                ty,
                count: Box::new(count),
            });
        }
        // 标准 new T(args)
        let args = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            self.bump();
            let mut a = vec![];
            while !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                a.push(self.parse_expr()?);
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Comma,
                        ..
                    }))
                ) {
                    self.bump();
                }
            }
            self.bump();
            a
        } else {
            vec![]
        };
        Ok(CppExpr::New(ty, args))
    }

    /// 解析大括号初始化列表 `{1, 2, 3}` (M10+: initializer_list)
    fn parse_init_list(&mut self) -> Result<CppExpr, CppParseError> {
        // 已经在 {
        self.bump();
        let mut elems = vec![];
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RBrace,
                ..
            }))
        ) {
            loop {
                elems.push(self.parse_expr()?);
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Comma,
                        ..
                    }))
                ) {
                    self.bump();
                    // 允许尾随逗号: {1, 2, 3,}
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::RBrace,
                            ..
                        }))
                    ) {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::RBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected(
                "}",
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        Ok(CppExpr::InitList(elems))
    }

    /// 跳过 const / override / final / noexcept / 0
    fn skip_cv_qual(&mut self) -> bool {
        let mut is_const = false;
        loop {
            let is = match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) if text == "const" => {
                    is_const = true;
                    true
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::KwConst,
                    ..
                })) => {
                    is_const = true;
                    true
                }
                Some(CppTok::Kw(CppKw::Const, _)) => {
                    is_const = true;
                    true
                }
                _ => false,
            };
            if is {
                self.bump();
            } else {
                break;
            }
        }
        is_const
    }

    /// 跳过 override / final / noexcept / = 0 / 纯虚
    /// M17.0: 区分 override / final / noexcept 三种状态, 改返回元组.
    fn skip_specifiers(&mut self) -> SpecKinds {
        let mut kinds = SpecKinds::default();
        loop {
            if self.at(CppKw::Override) {
                self.bump();
                kinds.is_override = true;
                continue;
            }
            if self.at(CppKw::Final) {
                self.bump();
                kinds.is_final = true;
                continue;
            }
            if self.at(CppKw::Noexcept) {
                self.bump();
                kinds.is_noexcept = true;
                // 处理 noexcept(expr) 形式: 消耗括号表达式
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                ) {
                    self.bump();
                    let mut depth = 1i32;
                    while depth > 0 {
                        if let Some(t) = self.bump() {
                            if matches!(
                                t,
                                CppTok::C(Tok {
                                    kind: TokKind::LParen,
                                    ..
                                })
                            ) {
                                depth += 1;
                            } else if matches!(
                                t,
                                CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                })
                            ) {
                                depth -= 1;
                            }
                        } else {
                            break;
                        }
                    }
                }
                continue;
            }
            // = 0 纯虚标记, 跳过
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Number,
                    text,
                    ..
                })) = &self.peek
                {
                    if text == "0" {
                        self.bump();
                    }
                }
                continue;
            }
            break;
        }
        kinds
    }

    /// M9: 判断当前位置是否是局部变量声明
    /// 形式: `Type name;` / `Type name = init;` / `Type name(args);`
    /// 要求: 第一段是类型关键字 / auto / 用户自定义类型 ident (允许 const/volatile
    ///      限定, char/short/int/long 等类型说明符, * 与 & 修饰), 后面跟 ident
    ///      作为变量名, 再跟 `;` `=` 或 `(`.
    /// 用户自定义类型 ident 不能是 C/C++ 关键字或保留字 (避免与 `if (...)` 冲突).
    /// M10+: 支持 `static Type name = init;` 静态局部变量
    /// M11: 支持多 token 类型如 `const char* p = ...;` 与 `ostream& r = ...;`
    /// 同时识别 C++ 关键字 (CppTok::Kw: const, volatile, static) 和 C 关键字
    /// (CppTok::C: int, char, void 等).
    /// M26: 从 token 索引 i 起跳过 `ident<...>` 模板实参 (尖括号平衡扫描).
    /// 返回模板实参后的索引; `<` 后不是平衡尖括号时返回 None.
    fn skip_template_args_at(&self, mut i: usize) -> Option<usize> {
        if !matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) {
            return Some(i);
        }
        let mut depth: i32 = 1;
        i += 1;
        loop {
            match self.peek_token_at(i) {
                None => return None,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                })) => {
                    depth += 1;
                    i += 1;
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                })) => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                Some(CppTok::C(Tok {
                    kind: TokKind::Shr, ..
                })) => {
                    // `>>` 视作两层 `>`
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return Some(i);
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                // 语句/表达式终结符在未闭合时出现: 视为比较表达式
                Some(CppTok::C(Tok {
                    kind:
                        TokKind::Semi
                        | TokKind::RParen
                        | TokKind::LBrace
                        | TokKind::RBrace
                        | TokKind::Assign
                        | TokKind::Question
                        | TokKind::Colon,
                    ..
                })) if depth == 1 => return None,
                _ => {
                    i += 1;
                }
            }
        }
    }

    /// 局部变量声明探测: `Type name;` / `Type name = expr;` 等
    fn is_local_var_decl_start(&self) -> bool {
        let mut i = 0;
        // 跳过 static
        let is_static = self.at(CppKw::Static);
        if is_static {
            i += 1;
        }
        // 跳过 const/volatile 限定符
        while self.is_qualifier_at(i) {
            i += 1;
        }
        // M18.0: 支持限定名 `std::duration` / `::foo` 作为类型.
        // 检测两种形式:
        //   1) `::ident[::ident]*`  (全局作用域开头)
        //   2) `ident::ident[::ident]*`  (命名空间)
        let starts_with_qualified_name =
            if matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
                true
            } else if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            })) = self.peek_token_at(i)
            {
                // ident :: 形式: 必须是 ident :: ident 才算
                matches!(self.peek_token_at(i + 1), Some(CppTok::ColonColon(_)))
                    && matches!(
                        self.peek_token_at(i + 2),
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            ..
                        }))
                    )
            } else {
                false
            };
        if starts_with_qualified_name {
            if matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
                i += 1;
            }
            // 跳过第一个 ident
            if !matches!(
                self.peek_token_at(i),
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) {
                return false;
            }
            i += 1;
            // 继续吞 ::ident
            while matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
                i += 1;
                if !matches!(
                    self.peek_token_at(i),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        ..
                    }))
                ) {
                    return false;
                }
                i += 1;
            }
            // M26: 限定名后的模板实参 `std::vector<int>`:
            // 跳过 `<...>` 尖括号
            if let Some(after) = self.skip_template_args_at(i) {
                i = after;
            }
        } else {
            // 第一个类型说明符
            let first = self.peek_token_at(i);
            if !self.is_type_specifier(first.as_ref()) {
                return false;
            }
            i += 1;
            // M26: 模板实例化类型 `Holder<int>` / `Holder<Holder<int>>`:
            // 类型名后紧跟 `<` 且尖括号平衡 → 跳过模板实参
            if let Some(after) = self.skip_template_args_at(i) {
                i = after;
            } else {
                // 不平衡: 是表达式 (如 `a < b`) 而非声明
                return false;
            }
            // M17.0: `decltype(expr)` 整体作为一个类型说明符
            // 需要跳过 `(` expr `)` (含 auto 占位 + 嵌套括号) 才能继续看下一段类型
            if matches!(first, Some(CppTok::Kw(CppKw::Decltype, _))) {
                // 必须是 (
                if !matches!(
                    self.peek_token_at(i),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                ) {
                    return false;
                }
                i += 1;
                // 跳过 expr, 平衡括号 (含嵌套)
                let mut depth: i32 = 1;
                while depth > 0 {
                    match self.peek_token_at(i) {
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        })) => {
                            depth += 1;
                            i += 1;
                        }
                        Some(CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        })) => {
                            depth -= 1;
                            i += 1;
                        }
                        None => return false,
                        _ => {
                            i += 1;
                        }
                    }
                }
            }
        }
        // M30: `Tpl<Args>::member` 类型 (模板类内 typedef 成员):
        // 模板实参后可拖 `::member`, 同样属于类型部分
        // (两个分支共享: qualified 与裸 Ident + 模板实参)
        while matches!(self.peek_token_at(i), Some(CppTok::ColonColon(_))) {
            if !matches!(
                self.peek_token_at(i + 1),
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) {
                break;
            }
            i += 2;
        }
        // 允许 multi-token 类型: long long, short int, unsigned char, signed int 等
        // 只要下一个 token 是另一个类型说明符 (char/short/long/int/float/double/signed/unsigned)
        // 或后置限定 (const/volatile) 或指针/引用, 就继续
        loop {
            if self.is_qualifier_at(i) {
                i += 1;
                continue;
            }
            if self.is_extra_type_spec_at(i) {
                i += 1;
                continue;
            }
            break;
        }
        // * 或 & 修饰 (允许交错 const/volatile)
        loop {
            while self.is_qualifier_at(i) {
                i += 1;
            }
            if self.is_star_amp_at(i) {
                i += 1;
            } else {
                break;
            }
        }
        // 此时第 i 个 token 应是变量名 (ident)
        let name_tok = self.peek_token_at(i);
        if !matches!(
            name_tok,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) {
            return false;
        }
        // 第 i+1 个 token 必须是 `;` `=` `(` 之一
        let after_name = self.peek_token_at(i + 1);
        matches!(
            after_name,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            })) | Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            })) | Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        )
    }

    /// 第 i 个 token 是否是除第一个类型说明符之外的其他类型说明符
    /// 用于 multi-token 类型如 `unsigned char` / `long long` / `long int`
    fn is_extra_type_spec_at(&self, i: usize) -> bool {
        matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::KwChar
                    | TokKind::KwShort
                    | TokKind::KwLong
                    | TokKind::KwInt
                    | TokKind::KwFloat
                    | TokKind::KwDouble
                    | TokKind::KwSigned
                    | TokKind::KwUnsigned,
                ..
            }))
        )
    }

    /// 第 i 个 token 是否是 const/volatile/restrict 限定符
    fn is_qualifier_at(&self, i: usize) -> bool {
        matches!(
            self.peek_token_at(i),
            Some(CppTok::Kw(CppKw::Const | CppKw::Volatile, _))
        )
    }

    /// 第 i 个 token 是否是 long / short
    fn is_long_short_at(&self, i: usize) -> bool {
        matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::KwLong | TokKind::KwShort,
                ..
            }))
        )
    }

    /// 第 i 个 token 是否是 * 或 &
    fn is_star_amp_at(&self, i: usize) -> bool {
        matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Star | TokKind::Amp,
                ..
            }))
        )
    }

    /// 第 i 个 token 是否是类型说明符 (int/char/.../auto/用户类型 ident)
    /// M17.0: 包含 CppKw::Decltype 用于 `decltype(expr)` 类型查询
    fn is_type_specifier(&self, t: Option<&CppTok>) -> bool {
        match t {
            Some(CppTok::C(Tok {
                kind:
                    TokKind::KwInt
                    | TokKind::KwChar
                    | TokKind::KwShort
                    | TokKind::KwLong
                    | TokKind::KwVoid
                    | TokKind::KwFloat
                    | TokKind::KwDouble
                    | TokKind::KwSigned
                    | TokKind::KwUnsigned
                    | TokKind::KwStruct
                    | TokKind::KwUnion
                    | TokKind::KwEnum
                    | TokKind::KwInline
                    | TokKind::KwBool
                    | TokKind::KwAuto
                    | TokKind::KwAtomic,
                ..
            })) => true,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => !is_reserved_word(text),
            Some(CppTok::Kw(CppKw::Decltype, _)) => true,
            _ => false,
        }
    }

    /// M18.0: 探测当前位置是否是限定类型名
    /// 即: `::ident`, `ident::ident`, `std::duration` 等形式
    fn is_qualified_type_start(&self) -> bool {
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            return true;
        }
        // ident :: ident 形式
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            ..
        })) = &self.peek
        {
            if matches!(self.peek_token_at(1), Some(CppTok::ColonColon(_))) {
                // 进一步要求后面 (:: 后) 也是 ident
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })) = self.peek_token_at(2)
                {
                    return true;
                }
            }
        }
        false
    }

    /// M9: 解析局部变量声明
    fn parse_local_var_decl(&mut self) -> Result<CppStmt, CppParseError> {
        // M10+: 检测 static 关键字
        let is_static = self.at(CppKw::Static);
        if is_static {
            self.bump();
        }
        // M18.0: 支持限定类型 `std::duration`, `::foo` 等
        // 检测 :: 开头 (全局作用域) 或 ident + :: (命名空间)
        // M30.2: 先跳过 const/volatile 前缀, 否则 `const bidi::context &x`
        // 因 const 挡住 is_qualified_type_start 检测而走 parse_mangle_type,
        // 丢失命名空间上下文.
        while self.at(CppKw::Const) || self.at(CppKw::Volatile) {
            self.bump();
        }
        let is_qualified_type = self.is_qualified_type_start();
        let ty = if is_qualified_type {
            // 解析限定名为 class 类型, 存为 (last segment, qualified path)
            let parts = self.parse_qualified_name()?;
            let last = parts.last().cloned().unwrap_or_default();
            // 用 last segment 当 Class 名 (lower 时根据 namespace ctx 还原)
            let mut ty = MangleType::Class(last.clone());
            // M26: 限定名后的模板实参 `std::vector<int>`
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt()
            {
                self.bump(); // 吞 `<`
                let mut args = Vec::new();
                loop {
                    let arg = self.parse_template_arg(1)?;
                    args.push(arg);
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }))
                    ) {
                        self.bump();
                        continue;
                    }
                    if self.deferred_gt > 0 {
                        self.deferred_gt -= 1;
                        break;
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Gt,
                            ..
                        }))
                    ) {
                        self.bump();
                        break;
                    }
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::Shr,
                            ..
                        }))
                    ) {
                        self.bump();
                        self.deferred_gt += 1;
                        break;
                    }
                    return Err(CppParseError::Unexpected(
                        ">",
                        format!("{:?}", self.peek),
                        self.span(),
                    ));
                }
                ty = MangleType::Template {
                    name: last.clone(),
                    args: args.clone(),
                };
                // M30: `Tpl<Args>::member` 拖尾 (模板类内 typedef 成员)
                if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
                    self.bump(); // ::
                    let member = self.expect_ident()?;
                    ty = MangleType::TplMember {
                        name: last,
                        args,
                        member,
                    };
                }
            }
            // 跳过 const/volatile 后缀
            if self.at(CppKw::Const) {
                self.bump();
            }
            // 跳过多 token 类型后缀 (例: long long)
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwLong
                        | TokKind::KwShort
                        | TokKind::KwChar
                        | TokKind::KwInt
                        | TokKind::KwSigned
                        | TokKind::KwUnsigned,
                    ..
                }))
            ) {
                self.bump();
            }
            // * 或 & 修饰
            loop {
                if self.at(CppKw::Const) {
                    self.bump();
                }
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    }))
                ) {
                    self.bump();
                    ty = MangleType::Pointer(Box::new(ty));
                } else {
                    break;
                }
            }
            ty
        } else {
            self.parse_mangle_type()?
        };
        let name = self.expect_ident()?;
        // 跳过可能的 `()` (如 `int x(0);` C++ 风格构造)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            // 简化: 支持 `int x();` / `int x(0);` / 类直接初始化 `Widget w(7)`
            self.bump();
            let mut args: Vec<CppExpr> = Vec::new();
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                args.push(self.parse_expr()?);
                while matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Comma,
                        ..
                    }))
                ) {
                    self.bump();
                    args.push(self.parse_expr()?);
                }
            }
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    ")",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump();
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            // M26.4: 类类型直接初始化 `Widget w(7)` 包成 ctor 调用
            // Call(Ident("Widget"), [7]), 让 lower 走 ctor 路径而非普通赋值.
            let init = match &ty {
                MangleType::Class(cls) if !args.is_empty() => {
                    Some(CppExpr::Call(Box::new(CppExpr::Ident(cls.clone())), args))
                }
                _ if args.len() == 1 => Some(args.into_iter().next().unwrap()),
                _ => None,
            };
            return if is_static {
                Ok(CppStmt::StaticLocal { ty, name, init })
            } else {
                Ok(CppStmt::LocalVar { ty, name, init })
            };
        }
        let init = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        if is_static {
            Ok(CppStmt::StaticLocal { ty, name, init })
        } else {
            Ok(CppStmt::LocalVar { ty, name, init })
        }
    }
}

/// 给定声明 d, 注入类名 (用于类内 inline 函数)
fn inject_class_name(d: CppDecl, class: &str, _saved: &Option<CppTok>) -> CppDecl {
    match d {
        CppDecl::MemberFn {
            name,
            ret,
            params,
            body,
            is_virtual,
            is_override,
            is_const,
            is_final,
            is_noexcept,
            is_volatile,
            is_friend,
            ..
        } => {
            // 已经是定义形式: 不再注入
            CppDecl::MemberFn {
                class: class.to_string(),
                ret,
                name,
                params,
                body,
                is_virtual,
                is_override,
                is_const,
                is_final,
                is_noexcept,
                is_volatile,
                is_friend,
            }
        }
        CppDecl::Ctor {
            params,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
            ..
        } => CppDecl::Ctor {
            class: class.to_string(),
            params,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
        },
        CppDecl::Dtor {
            body,
            is_virtual,
            is_noexcept,
            ..
        } => CppDecl::Dtor {
            class: class.to_string(),
            body,
            is_virtual,
            is_noexcept,
        },
        CppDecl::OperatorFn {
            op,
            params,
            body,
            ret,
            is_unary,
            ..
        } => CppDecl::OperatorFn {
            class: Some(class.to_string()),
            op,
            params,
            body,
            ret,
            is_unary,
        },
        other => other,
    }
}

/// 获取当前 token 的文本 (若存在)
fn text_of(t: &Option<CppTok>) -> &str {
    match t {
        Some(CppTok::C(Tok { text, .. })) => text,
        _ => "",
    }
}

fn is_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "class"
            | "struct"
            | "enum"
            | "union"
            | "template"
            | "namespace"
            | "using"
            | "if"
            | "else"
            | "while"
            | "for"
            | "do"
            | "return"
            | "break"
            | "continue"
            | "try"
            | "catch"
            | "throw"
            | "const"
            | "static"
            | "extern"
            | "typedef"
            | "virtual"
            | "explicit"
            | "friend"
            | "inline"
            | "operator"
            | "sizeof"
            | "alignof"
            | "typeid"
            | "constexpr"
            | "consteval"
            | "constinit"
            | "noexcept"
            | "mutable"
            | "volatile"
            | "register"
            | "auto"
            | "decltype"
            | "switch"
            | "case"
            | "default"
    )
}

fn peek_two_str(t: &Option<CppTok>) -> &str {
    match t {
        Some(CppTok::C(Tok {
            kind: TokKind::Plus,
            ..
        })) => "+",
        Some(CppTok::C(Tok {
            kind: TokKind::Minus,
            ..
        })) => "-",
        Some(CppTok::C(Tok {
            kind: TokKind::Star,
            ..
        })) => "*",
        Some(CppTok::C(Tok {
            kind: TokKind::Slash,
            ..
        })) => "/",
        Some(CppTok::C(Tok {
            kind: TokKind::Percent,
            ..
        })) => "%",
        Some(CppTok::C(Tok {
            kind: TokKind::Caret,
            ..
        })) => "^",
        Some(CppTok::C(Tok {
            kind: TokKind::Amp, ..
        })) => "&",
        Some(CppTok::C(Tok {
            kind: TokKind::Pipe,
            ..
        })) => "|",
        Some(CppTok::C(Tok {
            kind: TokKind::Lt, ..
        })) => "<",
        Some(CppTok::C(Tok {
            kind: TokKind::Gt, ..
        })) => ">",
        Some(CppTok::C(Tok {
            kind: TokKind::Assign,
            ..
        })) => "=",
        _ => "",
    }
}

fn strip_quotes(s: &str) -> String {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// M26.1: 是否 C 类型关键字 token.
fn is_c_type_kw(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::KwInt
            | TokKind::KwChar
            | TokKind::KwShort
            | TokKind::KwLong
            | TokKind::KwVoid
            | TokKind::KwFloat
            | TokKind::KwDouble
            | TokKind::KwSigned
            | TokKind::KwUnsigned
            | TokKind::KwStruct
            | TokKind::KwUnion
            | TokKind::KwEnum
            | TokKind::KwInline
            | TokKind::KwBool
            | TokKind::KwAuto
            | TokKind::KwAtomic
            | TokKind::KwRestrict
            | TokKind::KwConst
            | TokKind::KwVolatile
            | TokKind::KwStatic
    )
}

/// M26.1: 解析字符字面量值 (含转义). 失败返回 0.
fn char_lit_value(s: &str) -> i64 {
    let mut chars = s.chars();
    let Some(c) = chars.next() else {
        return 0;
    };
    if c != '\\' {
        return c as i64;
    }
    // 转义序列
    match chars.next() {
        Some('n') => 0x0a,
        Some('t') => 0x09,
        Some('r') => 0x0d,
        Some('0') => 0x00,
        Some('\\') => 0x5c,
        Some('\'') => 0x27,
        Some('"') => 0x22,
        Some('a') => 0x07,
        Some('b') => 0x08,
        Some('f') => 0x0c,
        Some('v') => 0x0b,
        Some('x') => {
            let hex: String = chars.take_while(|c| c.is_ascii_hexdigit()).collect();
            i64::from_str_radix(&hex, 16).unwrap_or(0)
        }
        _ => 0,
    }
}

/// M26.1: 数字字面量是否浮点 (含 `.` / 指数 `e`/`E` / 后缀 `f`/`F`).
fn is_float_number(text: &str) -> bool {
    let t = text.trim_end_matches(['u', 'U', 'l', 'L', 'f', 'F']);
    t.contains('.') || t.contains('e') || t.contains('E')
}

/// M26.1: 解析整数数字字面量 (支持 0x/0b/0o 前缀与 u/l/ll 后缀).
/// 失败时返回 0 (与旧行为一致, 但旧行为对合法 C++ 字面量几乎全部失败).
fn parse_int_text(text: &str) -> i64 {
    let t = text.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).unwrap_or(0);
    }
    if let Some(oct) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return i64::from_str_radix(oct, 8).unwrap_or(0);
    }
    if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return i64::from_str_radix(bin, 2).unwrap_or(0);
    }
    // 旧的 8 进制写法 `0755`
    if t.len() > 1 && t.starts_with('0') {
        return i64::from_str_radix(&t[1..], 8).unwrap_or(0);
    }
    t.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_class_with_ctor_and_dtor() {
        let src = b"class Foo { public: Foo(); ~Foo(); int bar(); };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "Foo");
                assert_eq!(c.members.len(), 3);
            }
            _ => panic!("expected class"),
        }
    }

    #[test]
    fn mangle_ctor_via_mangle_module() {
        let n = NestedName::from_dotted(vec!["Foo".into()]);
        let m = crate::cpp::mangle::mangle_ctor(&n, &[MangleType::Int], 1);
        assert_eq!(m, "_ZN3FooC1Ei");
    }

    #[test]
    fn parse_namespace() {
        let src = b"namespace std { class vector { public: int x; }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Namespace { name, body } => {
                assert_eq!(name.as_deref(), Some("std"));
                assert_eq!(body.len(), 1);
            }
            _ => panic!("expected namespace, got {:?}", d[0]),
        }
    }

    #[test]
    fn parse_inherit() {
        let src = b"class Base { public: int x; }; class Derived : public Base { public: int y; };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[1] {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "Derived");
                assert_eq!(c.bases.len(), 1);
                assert_eq!(c.bases[0].name, "Base");
                assert_eq!(c.bases[0].access, Access::Public);
            }
            _ => panic!("expected class"),
        }
    }

    #[test]
    fn parse_try_catch_throw() {
        let src = b"void f() { try { throw 1; } catch (int x) { } }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        // 这里仅验证解析不报错
        assert!(d.len() == 1);
    }

    #[test]
    fn parse_template_decl() {
        let src = b"template <typename T> T id(T x) { return x; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(matches!(&d[0], CppDecl::Template { .. }));
    }

    #[test]
    fn parse_lambda() {
        let src = b"void f() { auto g = [](int x) { return x; }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(d.len() == 1);
        assert_eq!(p.lambdas.len(), 1);
    }

    #[test]
    fn parse_operator_overload() {
        let src = b"class Vec { public: Vec operator+(Vec const& other) { return *this; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(d.len() == 1);
    }

    #[test]
    fn parse_concept() {
        let src = b"template <typename T> concept Integral = sizeof(T) <= 8;";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        // outer is Template wrapping Concept
        if let CppDecl::Template { decl, .. } = &d[0] {
            assert!(matches!(decl.as_ref(), CppDecl::Concept { .. }));
        } else {
            panic!("expected Template wrapping Concept");
        }
    }

    #[test]
    fn parse_module() {
        let src = b"module foo;";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(matches!(&d[0], CppDecl::Module { name: Some(_), .. }));
    }

    #[test]
    fn parse_export_module() {
        let src = b"export module foo;";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(matches!(
            &d[0],
            CppDecl::Module {
                is_export: true,
                ..
            }
        ));
    }

    #[test]
    fn parse_import() {
        let src = b"import std;";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(matches!(&d[0], CppDecl::Import { .. }));
    }

    #[test]
    fn parse_import_angle() {
        let src = b"import <iostream>;";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        if let CppDecl::Import { path } = &d[0] {
            assert!(!path.is_empty());
        } else {
            panic!("expected Import");
        }
    }

    #[test]
    fn parse_coroutine_basic() {
        let src = b"void f() { co_await 1; co_yield 2; co_return 3; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(d.len() == 1);
    }

    // M26: enum 保留枚举名 + 枚举值 + 底层类型 + scoped 标记
    #[test]
    fn parse_enum_keeps_variants() {
        let src = b"enum Color { RED, GREEN = 5, BLUE };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Enum {
                name,
                scoped,
                underlying,
                variants,
            } => {
                assert_eq!(name, "Color");
                assert!(!scoped);
                assert!(underlying.is_none());
                assert_eq!(
                    variants,
                    &vec![
                        ("RED".to_string(), None),
                        ("GREEN".to_string(), Some("5".to_string())),
                        ("BLUE".to_string(), None),
                    ]
                );
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    #[test]
    fn parse_enum_scoped_with_underlying() {
        let src = b"enum class Scoped : unsigned char { A = 1, B = A + 2 };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Enum {
                name,
                scoped,
                underlying,
                variants,
            } => {
                assert_eq!(name, "Scoped");
                assert!(scoped);
                assert_eq!(underlying.as_deref(), Some("unsigned char"));
                assert_eq!(variants.len(), 2);
                assert_eq!(variants[0], ("A".to_string(), Some("1".to_string())));
                // 初始化表达式保留 token 文本 (含前值引用)
                assert_eq!(variants[1], ("B".to_string(), Some("A + 2".to_string())));
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    #[test]
    fn parse_enum_anonymous() {
        let src = b"enum { ANON_X = 42 };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Enum { name, variants, .. } => {
                assert_eq!(name, "_anon");
                assert_eq!(
                    variants,
                    &vec![("ANON_X".to_string(), Some("42".to_string()))]
                );
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    #[test]
    fn parse_enum_initializer_with_parens_keeps_comma() {
        // 初始化表达式内的逗号不应切分 enumerator
        let src = b"enum E { A = f(1, 2), B = 3 };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Enum { variants, .. } => {
                assert_eq!(variants.len(), 2);
                assert_eq!(
                    variants[0],
                    ("A".to_string(), Some("f ( 1 , 2 )".to_string()))
                );
                assert_eq!(variants[1], ("B".to_string(), Some("3".to_string())));
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    // M26.1: 数字 / 字符字面量解析 (hex/oct/bin/float/转义)
    #[test]
    fn parse_number_literals_hex_oct_bin() {
        let src = b"int f() {
            int a = 0x41; int b = 0755; int c = 0b1010; int d = 42ULL;
            char n = '\\n'; char h = '\\x41'; double x = 1.5;
            return (int)(a + b + c + d + n + h + (int)x);
        }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        // 解析成功且字面量值正确
        match &d[0] {
            CppDecl::FreeFn { body, .. } => {
                // 简化验证: 找 IntLit 值
                let mut vals: Vec<i64> = vec![];
                let mut stmts: Vec<&CppStmt> = body.stmts.iter().collect();
                while let Some(s) = stmts.pop() {
                    match s {
                        CppStmt::LocalVar { init: Some(e), .. } => {
                            if let CppExpr::IntLit(v) = e {
                                vals.push(*v);
                            }
                        }
                        CppStmt::Return(Some(CppExpr::IntLit(v))) => vals.push(*v),
                        _ => {}
                    }
                }
                assert!(vals.contains(&65), "hex 0x41: {:?}", vals);
                assert!(vals.contains(&493), "oct 0755: {:?}", vals);
                assert!(vals.contains(&10), "bin 0b1010: {:?}", vals);
                assert!(vals.contains(&42), "suffix 42ULL: {:?}", vals);
                assert!(vals.contains(&10), "\\n: {:?}", vals);
                assert!(vals.contains(&65), "\\x41: {:?}", vals);
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn parse_reference_member_keeps_type() {
        let src = b"class Holder { public: Widget& ref; };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Class(c) => match &c.members[0] {
                CppDecl::MemberVar { ty, name, .. } => {
                    assert_eq!(name, "ref");
                    assert!(matches!(
                        ty,
                        MangleType::Reference(inner) if matches!(&**inner, MangleType::Class(n) if n == "Widget")
                    ));
                }
                other => panic!("expected MemberVar, got {:?}", other),
            },
            other => panic!("expected Class, got {:?}", other),
        }
    }

    #[test]
    fn parse_operator_new_delete_names() {
        let src = b"class A {
            public:
              void* operator new(unsigned long sz);
              void operator delete(void* p);
              int* operator new[](unsigned long sz);
              void operator delete[](void* p);
        };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level();
        assert!(
            d.is_ok(),
            "parse failed: {:?}",
            d.as_ref().map(|v| v[0].clone())
        );
    }

    // M18.1: T<U1, U2, ...> 模板实参语法解析
    // 注: 模板类型出现在自由函数参数中, 类成员声明 parser 尚不支持.
    #[test]
    fn parse_template_arg_single() {
        let src = b"int f(vector<int> v) { return 0; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level();
        assert!(d.is_ok(), "parse failed: {:?}", d.err());
    }

    #[test]
    fn parse_template_arg_multi() {
        let src = b"int f(map<string, int> m) { return 0; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level();
        assert!(d.is_ok(), "parse failed: {:?}", d.err());
    }

    #[test]
    fn parse_template_arg_pointer() {
        let src = b"int f(vector<int*> v) { return 0; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level();
        assert!(d.is_ok(), "parse failed: {:?}", d.err());
    }

    #[test]
    fn lt_after_ident_is_template_not_compare() {
        // 表达式 `a < b` 不应被误判为模板实参
        // 这里是函数体内部的二元比较: 解析应不报"期望 >"错误
        let src = b"int f(int a, int b) { return a < b; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level();
        assert!(d.is_ok(), "parse failed: {:?}", d.err());
    }

    #[test]
    fn parse_qualified_template_type() {
        // M26: std::vector<int> 等限定名模板实参
        // 测试函数参数中的限定名模板类型
        let src = b"void f(std::vector<int> v) {}";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level();
        assert!(d.is_ok(), "parse failed: {:?}", d.err());
        let decls = d.unwrap();
        // 验证解析成功
        assert!(!decls.is_empty());
    }

    #[test]
    fn parse_using_type_alias() {
        // C++11: using Name = Type;
        let src = b"using int_alias = int;";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert_eq!(d.len(), 1);
        match &d[0] {
            CppDecl::Typedef { name, .. } => {
                assert_eq!(name, "int_alias");
            }
            _ => panic!("expected Typedef, got {:?}", d[0]),
        }
    }
}
