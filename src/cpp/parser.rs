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

use crate::cpp::cpp_const_eval::{
    eval_cpp_const_full_with_calls, ConstexprContext, ConstexprFunction, CppStmt as ConstEvalStmt,
};
use crate::cpp::lexer::{CppKw, CppLexer, CppTok};
use crate::cpp::mangle::{MangleType, NestedName, Operator};
use crate::cpp::template_depth::MAX_TEMPLATE_DEPTH;
use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Tok, TokKind};
use std::sync::Arc;

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
    /// 硬错误: 不参与"跳到一个分隔符后继续"的容错恢复.
    /// 模板/嵌套深度超限若被容错吞掉, 会把整棵超深类型降级成
    /// Opaque 原文 (长度随嵌套线性膨胀), 既丢失诊断码, 又拖慢后续阶段.
    pub fn is_fatal(&self) -> bool {
        matches!(self, CppParseError::TemplateDepthExceeded { .. } | CppParseError::TooDeep(_))
    }

    pub fn to_diag(&self, file: &str) -> Diagnostic {
        let mut span = self.span();
        span.file = Arc::from(file);
        match self {
            Self::Unexpected(want, got, _) => {
                Diagnostic::err("C0001", format!("expected {}, got '{}'", want, got), span)
            },
            Self::Unsupported(what, _) => {
                Diagnostic::err("C0002", format!("C++ feature not yet supported: {}", what), span)
                    .with_hint(
                        "M8 implements most C++ features; some advanced patterns may still fail",
                    )
            },
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
    /// G22: 类体内每个成员的访问级别 `(成员名, access)`.
    /// 访问说明符 (`public:` / `private:` / `protected:`) 只影响其**后**面的
    /// 成员, 起点默认 `struct`/`union` = public, `class` = private.
    /// 表按声明顺序记录, 允许同名 (重载) 多项; 未记录 = 该成员访问级别未知,
    /// sema 按类的默认访问回退 (不臆造 private).
    pub member_access: Vec<(String, Access)>,
}

/// 从成员声明取出用于 `ClassDecl::member_access` 索引的键.
/// 构造函数用类名, 析构函数用 `~类名` (与 sema 侧登记的名字一致).
pub fn member_access_key(d: &CppDecl) -> Option<String> {
    match d {
        CppDecl::MemberVar { name, .. } => Some(name.clone()),
        CppDecl::MemberFn { name, .. } => Some(name.clone()),
        CppDecl::Ctor { class, .. } => Some(class.clone()),
        CppDecl::Dtor { class, .. } => Some(format!("~{}", class)),
        CppDecl::OperatorFn { op, .. } => Some(format!("operator{}", op.encode_name())),
        CppDecl::Class(cd) => Some(cd.name.clone()),
        CppDecl::Typedef { name, .. } => Some(name.clone()),
        CppDecl::Using { path, .. } => path.last().cloned(),
        _ => None,
    }
}

/// 查一个成员在类体内的声明访问级别.
///
/// `member_access` 表只记录类体循环里真正碰到的访问说明符; 合成/重建的
/// `ClassDecl` (模板实例、模块接口) 表可能为空, 此时回退到语言默认
/// (`struct`/`union` -> public, `class` -> private), 而不是臆造 private ——
/// 历史实现把所有成员写死为 private, 导致 `struct` 的公开字段在外部访问时
/// 被误判为越权.
/// 同名 (重载) 多项时取最后一条 (后声明的可见性覆盖先声明的).
pub fn member_decl_access(cd: &ClassDecl, member: &CppDecl) -> Access {
    let default = if cd.is_struct { Access::Public } else { Access::Private };
    let key = match member_access_key(member) {
        Some(k) => k,
        None => return default,
    };
    cd.member_access
        .iter()
        .rev()
        .find(|(n, _)| *n == key)
        .map(|(_, a)| *a)
        .unwrap_or(default)
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
    /// C++11 参数包 (typename... Args). 解析时容忍, lower 时忽略 `...`.
    pub is_pack: bool,
    /// G11: 是否为模板模板参数 (`template<typename> class C`)
    /// Some(n) = 接受 n 个类型形参的模板模板参数 (如 `vector` 接受 1 个)
    /// None = 普通类型参数
    pub template_params: Option<usize>,
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

/// 构造函数初始化列表 (ctor-initializer) 的一个条目。
///
/// `Class() : base(1, 2), member(x) {}` 里每个 `name(arg-list)` 都是一条。
/// 实参**必须**保留为列表而不是压成单个表达式:
/// - 空列表 `s()` 是「默认初始化」, 与「没有初始化条目」意义不同 (类类型成员
///   会走零参构造, 标量成员会值初始化为 0);
/// - 多实参 `B(1, 2)` 是基类/类成员的构造器重载选择依据, 早期把它折叠成
///   逗号表达式的最后一个值会丢掉实参个数, 选不到正确的重载。
#[derive(Debug, Clone)]
pub struct CppCtorInit {
    /// 基类名或非静态数据成员名 (未解析, 允许 `ns::Base` 形式的限定名)
    pub name: String,
    /// 圆括号/花括号内的实参列表; 空 = 默认初始化
    pub args: Vec<CppExpr>,
    /// 是否使用花括号形式 `name{...}` (列表初始化)
    pub braces: bool,
}

impl CppCtorInit {
    /// `name(args...)` 形式 (空 args 即默认初始化 `name()`)。
    pub fn direct(name: impl Into<String>, args: Vec<CppExpr>) -> Self {
        CppCtorInit {
            name: name.into(),
            args,
            braces: false,
        }
    }

    /// `name{args...}` 列表初始化形式。
    pub fn braced(name: impl Into<String>, args: Vec<CppExpr>) -> Self {
        CppCtorInit {
            name: name.into(),
            args,
            braces: true,
        }
    }
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
        /// M30.5: trailing return type `auto f() -> Ret`
        trailing_ret_type: Option<MangleType>,
    },
    /// M30.9: static_assert(constant-expr, "message")
    StaticAssert {
        condition: CppExpr,
        message: Option<String>,
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
        /// `static` 标记 - 类作用域成员函数, **没有 `this`**
        ///
        /// 静态性必须进入 AST: 它在 [class.static.data] 里决定调用形态
        /// (`S::f()` 不能递对象), 也与 cv 限定一样是重载决议的输入。以前
        /// 它在 parser 里被吐掉, sema/hir/lowering 全链都只能把 `S::f()`
        /// 当成要 `this` 的成员调用 (报 member function call missing this argument)。
        is_static: bool,
    },
    /// 构造函数 (类外)
    Ctor {
        class: String,
        params: Vec<(String, MangleType)>,
        /// 成员初始化列表 (每个条目携带完整实参列表, 见 `CppCtorInit`)
        inits: Vec<CppCtorInit>,
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
        /// `volatile` 标记 - 成员变量为 volatile 限定
        is_volatile: bool,
        /// `const` 标记 - 成员对象为 const 限定 (重载决议要看它)
        is_const: bool,
        /// `static` 标记 - 类作用域的**静态**数据成员 (不属于子对象布局)
        ///
        /// 它是 [class.static.data] 的声明, 不是字段: `struct integral_constant<T,v>
        /// { static const T value = v; }` 这类 traits 的 `value` 就靠它进入语义层。
        is_static: bool,
        /// 类内初值 (`static constexpr int N = 8;` 的 `8`)。非静态数据成员的类内
        /// 初值是**默认初值** (构造时用), 静态成员的则是常量定义。
        init: Option<CppExpr>,
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
    ///   `template <typename T> class Foo<T*> { ... }`
    ///   `template <typename T> void foo<T>(T) { ... }` 不被支持
    PartialSpec {
        params: TemplateArgs,
        /// 特化模式，字符串形式：`T*`, `int`, `std::vector<T>` 等
        pattern_text: String,
        decl: Box<CppDecl>,
    },
    /// G13: Full specialization (显式特化)
    /// `template<> class Foo<void> { ... }`
    /// `template<> struct Allocator<int> { typedef int value_type; };`
    /// 注意：<>() 为空表示全特化；有具体参数也是全特化
    /// 与偏特化的区别：无模板形参列表，直接指定具体类型
    FullSpec {
        /// 特化实参 (写在**名字之后**的 `<...>` 里), 如 `[void]` / `[int, double]`.
        /// `template<>` 的形参表对全特化恒为空, 不是实参来源.
        specific_args: Vec<MangleType>,
        /// 特化体本身就是一个完整的类声明:
        /// `name` 是被特化的主模板名, `bases` 是特化体**自己的**基类列表
        /// (全特化不从主模板继承基类, `template<> struct A<int> : B {}` 的
        /// 基类就是 `B`), `members`/`member_access` 原样保留全部成员种类
        /// (成员函数 / 变量 / typedef / enum / 嵌套类) —— 早前压成 CppBody
        /// 后只余 MemberVar 与 Typedef, `enum { value = 1 }` 这类 trait
        /// 惯用法整个丢失, 基类也凭空消失。
        class: ClassDecl,
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
        /// `volatile` 修饰
        is_volatile: bool,
        /// `const` 修饰 (重载决议 / 常量性)
        is_const: bool,
    },
    /// M30: typedef 声明。M30: 带目标类型与别名，使模板实例别名
    /// (`typedef Name<Args> alias`) 和模板类内 typedef
    /// (`typedef T type;`) 可被记录与实例化替换; lower 对普通
    /// typedef 仍不产生 C 输出 (类型由后续引用处展开).
    Typedef { target: MangleType, name: String },
    /// G9: inline namespace (for ABI versioning like __cxx11)
    InlineNamespace {
        name: Option<String>,
        body: Vec<CppDecl>,
    },
    /// G10: extern template (suppresses implicit instantiation)
    ExternTemplate(Box<CppDecl>),
    /// M18.0: 空占位声明 (enum / 修饰后无实际声明时使用)
    Empty,
    /// C++ enum declaration with scoped members and optional underlying type.
    Enum {
        name: String,
        scoped: bool,
        underlying: Option<String>,
        variants: Vec<(String, Option<String>)>,
    },
    /// 无法解析的声明，保留原始源文本，lower 时原样输出。
    /// parser 容错恢复时，将跳过区域的原文保存于此，
    /// 避免声明内容在降级过程中丢失。
    Opaque(String),
}

#[derive(Debug, Clone, Default)]
pub struct CppBody {
    pub stmts: Vec<CppStmt>,
    /// M30.2: true = 纯声明 (类内 `Ret m() const;` 无 body). lower 端据此
    /// 输出原型而非空 weak 定义 (空定义会与类外定义产生重复符号, 链接器
    /// 可能选空的那个, 静默吞掉正确实现).
    pub is_decl: bool,
}

/// 一条声明里被多个 init-declarator **共享**的 decl-specifier-seq.
///
/// 关键约束 ([dcl.name]/[dcl.aoffset]): 指针 / 引用 / 数组层级属于 **declarator**,
/// 不属于 decl-specifier —— `int *p, q;` 中 `q` 的类型是 `int`, 不是 `int*`。
/// 所以这里只保存剥掉所有 `*` / `&` / `[N]` 之后的**基础类型**加上存储期与 cv
/// 限定, 每个 declarator 各自携带自己的层级 (见 [`InitDeclarator::layers`])。
#[derive(Debug, Clone)]
pub struct DeclSpec {
    /// decl-specifier 的类型部分, **不含**任何 declarator 层级.
    pub ty: MangleType,
    /// `static` 存储期 (`static int a = 1, b = 2;`).
    pub is_static: bool,
    /// `volatile` 限定 (与 [`CppStmt::LocalVar::is_volatile`] 同一语义).
    pub is_volatile: bool,
    /// `const` 限定 (与 [`CppStmt::LocalVar::is_const`] 同一语义).
    ///
    /// 对象类型的顶层 cv 是**重载决议**的一部分 (`f()` 与 `f() const` 选哪个),
    /// 因此不能像以前那样在说明符循环里被直接吐掉。
    pub is_const: bool,
}

/// declarator 名前的 `*` / `&` 层级.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaratorLayer {
    /// `*` —— 指针
    Pointer,
    /// `&` —— 引用
    Reference,
}

/// 同一条声明里的单个 init-declarator: `int a = 1, *b[2];` 里的 `a = 1` 与 `*b[2]`.
#[derive(Debug, Clone)]
pub struct InitDeclarator {
    /// 名前的 `*` / `&` 层级, 按源码书写顺序 (`int *&p` -> `[Pointer, Reference]`).
    pub layers: Vec<DeclaratorLayer>,
    /// 名后的数组维度, 按源码书写顺序 (`a[2][3]` -> `[2, 3]`).
    pub array_dims: Vec<usize>,
    pub name: String,
    pub init: Option<CppExpr>,
}

impl InitDeclarator {
    /// 该 declarator 的完整类型 = [`DeclSpec::ty`] 叠加自己的层级.
    ///
    /// 包装顺序与旧的单 declarator 路径逐字一致: 先按源码顺序套 `*` / `&`,
    /// 再把数组维度**从内向外**套 (源顺序 `[2][3]` => `Array(Array(T, 3), 2)`).
    pub fn full_ty(&self, spec: &DeclSpec) -> MangleType {
        let mut ty = spec.ty.clone();
        for layer in &self.layers {
            ty = match layer {
                DeclaratorLayer::Pointer => MangleType::Pointer(Box::new(ty)),
                DeclaratorLayer::Reference => MangleType::Reference(Box::new(ty)),
            };
        }
        for n in self.array_dims.iter().rev() {
            ty = MangleType::Array(Box::new(ty), *n);
        }
        ty
    }

    /// 展开成语义等价的单 declarator 语句 ([`CppStmt::LocalVar`] /
    /// [`CppStmt::StaticLocal`]) —— 下游「按变量逐个处理」的 pass 统一走这条归约,
    /// 不必各自复刻 `LocalVar` 的语义.
    pub fn to_stmt(&self, spec: &DeclSpec) -> CppStmt {
        let ty = self.full_ty(spec);
        if spec.is_static {
            CppStmt::StaticLocal {
                ty,
                name: self.name.clone(),
                init: self.init.clone(),
                is_volatile: spec.is_volatile,
                is_const: spec.is_const,
            }
        } else {
            CppStmt::LocalVar {
                ty,
                name: self.name.clone(),
                init: self.init.clone(),
                is_volatile: spec.is_volatile,
                is_const: spec.is_const,
            }
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
        is_volatile: bool,
        /// `const` 限定 (重载决议用)
        is_const: bool,
    },
    /// M10+: 静态局部变量 `static T name = init;`
    /// 触发 `__cxa_guard_*` 调用 (C++ ABI 线程安全单次初始化)
    StaticLocal {
        ty: MangleType,
        name: String,
        init: Option<CppExpr>,
        is_volatile: bool,
        /// `const` 限定 (重载决议用)
        is_const: bool,
    },
    /// 同一条声明里的**多个** init-declarator: `int i = 0, j = 1;` /
    /// `int *p, q;` / `for (int i = 0, j = n; ...)`。
    ///
    /// 单个 declarator 的声明仍用 [`Self::LocalVar`] / [`Self::StaticLocal`] 表示,
    /// 本变体只承载 2 个以上的情形。
    ///
    /// 为什么必须是**一条**语句: C++ 语法里 `for` 的 init 只能是一个
    /// declaration-statement ([stmt.init]/[dcl.dcls]), 拆成 N 条 `LocalVar` 既生不出
    /// 合法的 `for (int i = 0, j = n; ...)`, 也丢掉了「这些变量共享同一组
    /// decl-specifier」这一事实。因此这里按 C++ 文法建模 (共享 [`DeclSpec`] +
    /// 各自带指针层级的 declarator 列表); 需要逐个变量处理的下游 pass 统一用
    /// [`Self::as_local_var_stmts`] 归约成单 declarator 语句。
    LocalVarList {
        spec: DeclSpec,
        declarators: Vec<InitDeclarator>,
    },
    /// `try { ... } catch (T& e) { ... }` (M8)
    Try {
        body: Box<CppBody>,
        catches: Vec<CatchClause>,
    },
    /// `throw expr;` (M8)
    Throw(Option<CppExpr>),
    /// `do stmt while (expr);`
    DoWhile {
        body: Box<CppBody>,
        cond: CppExpr,
    },
    /// `goto label;`
    Goto(String),
    /// `label: stmt` — 带标签的语句
    Labeled {
        label: String,
        stmt: Box<CppStmt>,
    },
    /// `break;` (M8)
    Break,
    /// `continue;` (M8)
    Continue,
    /// C++17 `[[fallthrough]];` / GCC `__attribute__((fallthrough));`
    /// 显式标记 switch case 有意贯穿.
    Fallthrough {
        span: Span,
    },
    /// `switch (expr) { case val: stmts... default: stmts... }`
    Switch {
        expr: CppExpr,
        /// Each arm: None = default, Some(expr) = case expr
        arms: Vec<(Option<CppExpr>, CppBody)>,
        /// 每个 arm 的 case/default 标签 span, 与 `arms` 一一对应.
        /// 用于 `-Wimplicit-fallthrough` 把 W0501 定位到 case 标签 (对齐 C 端).
        label_spans: Vec<Span>,
    },
    /// `co_await expr;` (C++20 协程, M9 占位)
    CoAwait(CppExpr),
    /// `co_yield expr;` (C++20 协程, M9 占位)
    CoYield(CppExpr),
    /// `co_return expr;` (C++20 协程，M9 占位)
    CoReturn(Option<CppExpr>),
    /// C++17 结构化绑定 `auto [a, b, c] = expr;`
    StructuredBinding {
        names: Vec<String>,
        ty: MangleType,
        init: CppExpr,
        is_volatile: bool,
    },
    /// 空声明 `;` 已被调用方过滤
    Decl(CppDecl),
    /// 无法解析的语句，保留原始源文本。
    /// parse_body 容错恢复时，将跳过区域的原文保存于此，
    /// 避免函数体内后续语句因单条错误全部丢失。
    OpaqueStmt(String),
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
    /// G12: 参数包展开 `args...` —— 出现在实参列表 / 初始化列表 / 基类实参
    /// 等**列表**语境, 由模板单态化 (`template::substitute_*`) 按推导出的包
    /// 大小摊平成 N 个元素. 孤立于列表之外 (如 `args... + 1`) 是非法 C++,
    /// 摊平阶段会硬报错, 不静默丢代码.
    PackExpansion(Box<CppExpr>),
}

#[derive(Debug, Clone)]
pub enum SizeofArg {
    Type(MangleType),
    Expr(Box<CppExpr>),
    /// G12: `sizeof...(Pack)` —— 包中元素个数, 单态化时替成字面量.
    Pack(String),
}

impl CppExpr {
    /// G12: 借出所有**直接子表达式**, 供重写式遍历使用.
    ///
    /// 穷尽 match (无 `_` 兼底): 新增变体时编译失败会强制这里同步更新,
    /// 否则模板体里的依赖名字 / 包展开会被遍历静默漏过 —— 与禁止
    /// Opaque 静默降级同一理由.
    pub fn sub_exprs_mut(&mut self) -> Vec<&mut CppExpr> {
        match self {
            CppExpr::IntLit(_)
            | CppExpr::FloatLit(_)
            | CppExpr::StrLit(_)
            | CppExpr::Ident(_)
            | CppExpr::This
            | CppExpr::ThisMember(_)
            | CppExpr::QualifiedAccess(_, _)
            | CppExpr::MemberPtr { .. }
            | CppExpr::TemplateIdent { .. }
            | CppExpr::TplStaticAccess { .. }
            | CppExpr::StrConcat(_)
            | CppExpr::TypeidType(_)
            | CppExpr::Lambda(_) => Vec::new(),
            CppExpr::Member(o, _)
            | CppExpr::PtrMember(o, _)
            | CppExpr::MemberStar(o, _)
            | CppExpr::PtrMemberStar(o, _)
            | CppExpr::Delete(o)
            | CppExpr::DeleteArr(o)
            | CppExpr::Noexcept(o)
            | CppExpr::Ref(o)
            | CppExpr::AddrOf(o) => vec![o],
            CppExpr::NewArr { count, .. } => vec![count],
            CppExpr::BinOp(_, a, b) => vec![a, b],
            CppExpr::UnaryOp(_, a) => vec![a],
            CppExpr::Cast(_, a)
            | CppExpr::StaticCast { expr: a, .. }
            | CppExpr::ConstCast { expr: a, .. }
            | CppExpr::ReinterpretCast { expr: a, .. }
            | CppExpr::DynamicCast { expr: a, .. } => vec![a],
            CppExpr::Sizeof(arg) => match arg {
                SizeofArg::Expr(x) => vec![x],
                SizeofArg::Type(_) | SizeofArg::Pack(_) => Vec::new(),
            },
            CppExpr::Typeid(a) => vec![a],
            CppExpr::InitList(v) | CppExpr::Comma(v) => v.iter_mut().collect(),
            CppExpr::TypedInitList { elems, .. } => elems.iter_mut().collect(),
            CppExpr::OpCall { args, .. } => args.iter_mut().collect(),
            CppExpr::MultiIndex(o, idx) => {
                let mut v = vec![&mut **o];
                v.extend(idx.iter_mut());
                v
            },
            CppExpr::Call(f, as_) => {
                let mut v = vec![&mut **f];
                v.extend(as_.iter_mut());
                v
            },
            CppExpr::New(_, as_) => as_.iter_mut().collect(),
            CppExpr::PlacementNew { ptr, args, .. } => {
                let mut v = vec![&mut **ptr];
                v.extend(args.iter_mut());
                v
            },
            CppExpr::Ternary { cond, then, els } => vec![cond, then, els],
            CppExpr::PackExpansion(o) => vec![o],
        }
    }

    /// G12: 借出所有**允许参数包展开的列表语境** (实参列表 / 初始化列表 /
    /// 函数式转换实参). `T{a, args...}` / `f(a, args...)` 里的 `args...` 就
    /// 在这类位置被摊平成 N 个元素.
    pub fn pack_list_mut(&mut self) -> Vec<&mut Vec<CppExpr>> {
        match self {
            CppExpr::Call(_, as_) | CppExpr::New(_, as_) => vec![as_],
            CppExpr::InitList(v)
            | CppExpr::Comma(v)
            | CppExpr::MultiIndex(_, v)
            | CppExpr::OpCall { args: v, .. } => vec![v],
            CppExpr::TypedInitList { elems, .. } => vec![elems],
            CppExpr::PlacementNew { args, .. } => vec![args],
            _ => Vec::new(),
        }
    }
}

impl CppStmt {
    /// 归约关系: 把多 declarator 的一条声明 ([`Self::LocalVarList`]) 展开成语义
    /// 等价的单 declarator 语句序列 (每个元素都是 [`Self::LocalVar`] 或
    /// [`Self::StaticLocal`], 按声明顺序)。
    ///
    /// 下游那些「按变量逐个处理」的位置 (名字登记 / 类型采集 / 模板类型替换 /
    /// 帧变量提升 …) 统一用本函数拿到**每一个** declarator 的名字与类型, 而不是
    /// 只认第一个。非 `LocalVarList` 语句返回 `None`。
    pub fn as_local_var_stmts(&self) -> Option<Vec<CppStmt>> {
        match self {
            CppStmt::LocalVarList { spec, declarators } => Some(
                declarators
                    .iter()
                    .map(|d| d.to_stmt(spec))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        }
    }

    /// G12: 同 [`CppExpr::sub_exprs_mut`], 语句版.
    pub fn sub_exprs_mut(&mut self) -> Vec<&mut CppExpr> {
        match self {
            CppStmt::Expr(e)
            | CppStmt::Return(Some(e))
            | CppStmt::CoAwait(e)
            | CppStmt::CoYield(e)
            | CppStmt::Throw(Some(e)) => vec![e],
            CppStmt::Return(None) | CppStmt::Throw(None) | CppStmt::Goto(_) => Vec::new(),
            CppStmt::If { cond, .. } | CppStmt::IfConstexpr { cond, .. } => vec![cond],
            CppStmt::While { cond, .. } | CppStmt::DoWhile { cond, .. } => vec![cond],
            CppStmt::For { cond, .. } => cond.iter_mut().collect(),
            CppStmt::Break | CppStmt::Continue | CppStmt::Fallthrough { .. } => Vec::new(),
            CppStmt::Switch { expr, arms, .. } => {
                let mut v = vec![expr];
                for (_, body) in arms {
                    for s in &mut body.stmts {
                        v.extend(s.sub_exprs_mut());
                    }
                }
                v
            },
            CppStmt::CoReturn(e) => e.iter_mut().collect(),
            CppStmt::StructuredBinding { init, .. } => vec![init],
            CppStmt::LocalVar { init, .. } | CppStmt::StaticLocal { init, .. } => {
                init.iter_mut().collect()
            },
            CppStmt::LocalVarList { declarators, .. } => declarators
                .iter_mut()
                .flat_map(|d| d.init.iter_mut())
                .collect(),
            CppStmt::Try { .. } | CppStmt::Block(_) | CppStmt::Decl(_) => Vec::new(),
            CppStmt::ThisMember(_, inner) => inner.sub_exprs_mut(),
            CppStmt::Labeled { stmt, .. } => stmt.sub_exprs_mut(),
            CppStmt::OpaqueStmt(_) => Vec::new(),
        }
    }

    /// G12: 同 [`CppExpr::pack_list_mut`], 语句版 (语句本身不含列表, 递归到
    /// 子表达式里的列表由 [`Self::sub_exprs_mut`] 驱动).
    pub fn pack_list_mut(&mut self) -> Vec<&mut Vec<CppExpr>> {
        Vec::new()
    }

    /// G12: 借出所有**嵌套语句体** (含 try/catch 的处理器体).
    pub fn sub_bodies_mut(&mut self) -> Vec<&mut CppBody> {
        match self {
            CppStmt::If { then, els, .. } | CppStmt::IfConstexpr { then, els, .. } => {
                let mut v = vec![&mut **then];
                v.extend(els.iter_mut().map(|b| &mut **b));
                v
            },
            CppStmt::While { body, .. } | CppStmt::For { body, .. } => vec![body],
            CppStmt::Block(body) => vec![body],
            CppStmt::DoWhile { body, .. } => vec![body],
            CppStmt::Try { body, catches } => {
                let mut v = vec![&mut **body];
                v.extend(catches.iter_mut().map(|c| &mut c.body));
                v
            },
            CppStmt::Switch { arms, .. } => arms.iter_mut().map(|(_, b)| b).collect(),
            CppStmt::ThisMember(_, inner) => inner.sub_bodies_mut(),
            CppStmt::Labeled { stmt, .. } => stmt.sub_bodies_mut(),
            _ => Vec::new(),
        }
    }

    /// G12: 借出子**语句** (`for` 的 init/step, 标签语句).
    pub fn sub_stmts_mut(&mut self) -> Vec<&mut CppStmt> {
        match self {
            CppStmt::For { init, step, .. } => {
                let mut v: Vec<&mut CppStmt> = Vec::new();
                if let Some(i) = init {
                    v.push(i);
                }
                if let Some(s) = step {
                    v.push(s);
                }
                v
            },
            CppStmt::Labeled { stmt, .. } => vec![stmt],
            CppStmt::ThisMember(_, inner) => vec![inner],
            _ => Vec::new(),
        }
    }
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
/// `= 0` / `= delete` / `= default` 三种函数尾缀同样由 `skip_specifiers` 这个
/// **单一真源**消化并回传 (M30.6): 它们都意味着"无函数体的声明", ctor/dtor /
/// 成员函数 / 自由函数四条路径共用同一份判定, 不再各自实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct SpecKinds {
    pub is_override: bool,
    pub is_final: bool,
    pub is_noexcept: bool,
    /// `= delete`: 显式删除的特殊成员函数
    pub is_deleted: bool,
    /// `= default`: 要求编译器合成默认实现
    pub is_defaulted: bool,
    /// `= 0`: 纯虚函数
    pub is_pure_virtual: bool,
}

/// M53: 别名模板表项: (形参名表, 形参默认值表, 目标类型).
/// 抽出类型别名是为了避免在字段上写嵌套过深的泛型
/// (`HashMap<String, (Vec<String>, Vec<Option<MangleType>>, MangleType)>`).
type AliasTemplate = (Vec<String>, Vec<Option<MangleType>>, MangleType);

/// C++ 解析器
pub struct CppParser<'a> {
    pub lex: CppLexer<'a>,
    pub peek: Option<CppTok>,
    pub file: Arc<str>,
    pub diags: Vec<Diagnostic>,
    /// -Wimplicit-fallthrough: switch case 隐式贯穿警告
    pub warn_implicit_fallthrough: bool,
    /// 解析期间记下的 lambda (M8: lower 时分配闭包类型名)
    pub lambdas: Vec<LambdaExpr>,
    /// 嵌套模板 `>>` 拆出的待闭合 `>` 计数 (M18.x, 见 parse_mangle_type_depth)
    deferred_gt: u32,
    /// M53: 正在解析**非类型模板实参里的常量表达式** ([temp.arg.nontype]) 的深度.
    /// > 0 时 `>` / `>>` 是模板实参列表的收尾而不是二元运算符: 否则
    /// > `parse_expr` 会把 `S<(2 + 5)> s;` 吃成 `(2 + 5) > s` 、把
    /// > `buffer<sizeof(int) * 2>` 的闭合 `>` 吞掉.
    nttp_angle_stop: u32,
    /// M29: 模板实例 typedef 别名 `typedef Tpl<Args> Alias;`
    /// (Alias -> 模板名 + 实参). 表达式 `Alias::member` 展开为
    /// TplStaticAccess, 以触发实例化并引用带前缀的常量.
    pub typedef_alias: std::collections::HashMap<String, (String, Vec<MangleType>)>,
    /// M53: 别名模板 `template <params> using Name = Type;` ([temp.alias]).
    /// Name → (形参名表, 形参默认值, 目标类型). `<type_traits>` 里的
    /// `enable_if_t` / `conditional_t` / `remove_reference_t` 等 `*_t` 全由它展开.
    /// 展开在**类型解析点**完成 (见 [`CppParser::expand_alias_type`]), 下游
    /// (实例化工作表 / sema / 布局) 只看到真实类模板与其成员类型,
    /// 不需要知道别名存在过 —— 与手写 `typename enable_if<B,T>::type` 同形.
    alias_templates: std::collections::HashMap<String, AliasTemplate>,
    /// M30.4: 匿名聚合体计数器, 为每个匿名 union/struct 生成唯一名
    /// (`__anon_aggr_1`, `__anon_aggr_2`, ...) 避免同名导致构造函数/
    /// typeinfo/typedef 符号重复.
    anon_counter: u32,
    /// 原始源文本引用, 用于容错恢复时提取被跳过区域的原文.
    /// 与 lex.src 相同, 但 parser 级别直接访问更方便.
    src: &'a [u8],
    /// M31: 已知类型名集合 (class/struct/union/enum tag、typedef/using 别名、
    /// 模板类型参数、namespace 名). `typeid(T)` / `sizeof(T)` 括号内是 type-id
    /// 还是 expression 的消歧必须有这个信息: 语法上 `typeid(a)` 与 `typeid(T)`
    /// 无法区分, 只有"该标识符是否是一个已声明类型的名字"能决定
    /// ([expr.typeid] / [basic.type.ident] 的 type-id 优先规则).
    type_names: std::collections::HashSet<String>,
    /// `type_names` 是否已从源文本扫描填充 (惰性: 不含 typeid/sizeof 的 TU
    /// 不为该信息多跑一遍词法分析).
    type_names_seeded: bool,
    /// 编译期整型常量表: 枚举量与 `const/constexpr T X = <字面量>;`.
    /// `MangleType::Array(ty, n)` 在 parser 就要拿到定长 `n: usize`, 而局部
    /// 数组声明 `int t[ANON_X];` / `int t[2 + 3];` 的维度不是字面量; 之前只
    /// 认 `CppExpr::IntLit`, 失败后整条声明降级为 OpaqueStmt (字段/语句丢失).
    /// 值冲突时存 `None` (歧义), 宁可折叠失败也不给错的长度.
    int_consts: Vec<(String, Option<i128>)>,
    /// M54: **constexpr 自由函数**表 (名字 → 可解释执行的函数体).
    ///
    /// 常量表达式里出现函数调用 (`char buf[square(12)];`) 时,
    /// [`CppParser::fold_int_const_expr`] 查这张表把调用折成字面量 ——
    /// 折叠发生在**解析点**, 因此下游 (sema / native / legacy Lower) 看到的
    /// 已经是整型字面量, 不需要各自复刻一张表.
    ///
    /// 只在函数体**完整转换成功**时才登记 ([`Self::to_constexpr_body_stmts`]
    /// 返回 `None` 就不登记): 宁可不折叠, 也绝不给出错的常量值.
    constexpr_fns: std::collections::HashMap<String, ConstexprFunction>,
    /// M54: **重载歧义**的 constexpr 函数名. 表只以名字为键, 而同一名字的两个
    /// 重载 (`constexpr int f(int)` / `constexpr int f(int, int)`) 无法共处一格;
    /// 一旦冲突就把名字放进本集合并从 [`Self::constexpr_fns`] 删掉,
    /// 之后该名的调用永不折叠 (而不是拿错签名去算)。
    constexpr_fn_ambiguous: std::collections::HashSet<String>,
    /// M54: 当前声明的 decl-specifier-seq 里**已剥掉但仍有语义**的说明符.
    /// `parse_decl_list` 过去把 `constexpr`/`consteval`/`constinit` 当成纯语法
    /// 噪声丢掉 ("剥离即遗忘"), 于是同一关键字承载的两条语义 —— 对象的
    /// 常量性 (可登记进 [`Self::int_consts`]) 与函数的可编译期求值性 (可登记进
    /// [`Self::constexpr_fns`]) —— 全部丢失. 现在由本字段把信息带到终端解析函数
    /// ( [`CppParser::parse_global_var_decl`] / [`CppParser::parse_free_fn`]),
    /// 由它们 `take()` 消费; 每条**新**声明入口处清零, 保证不会漏到下一条.
    pending_decl_spec: PendingDeclSpec,
    /// M54: 全局 / 命名空间作用域对象的**声明类型**表 (声明名 → 完整类型).
    ///
    /// `sizeof(全局数组)` 需要表达式类型信息, 而 [cpp_const_eval] 的公共求值器
    /// 只做 `sizeof(类型)` ([expr.sizeof] 的两种形式里 `sizeof(e)` 必须知道
    /// `e` 的类型)。本表让 [`Self::fold_int_const_expr`] 在解析点就能折出
    /// `sizeof(g)` / `sizeof(g[0])` / `sizeof(*g)`, 于是
    /// `static_assert(sizeof(g) == 40)` 是**真检查**而不是“求不出就放行”。
    /// 只登记声明名 (不带命名空间前缀): 同名多条声明类型不一致时按
    /// “歧义”从表里删掉 —— 宁可折不出, 也绝不拿别人的类型算尺寸。
    global_obj_tys: std::collections::HashMap<String, MangleType>,
    /// M54: 函数体嵌套深度 (>0 表示正在解析某个函数/lambda 体内的一条语句).
    /// 局部声明会**遮蔽**全局对象 (`void f(){ int g[3]; char b[sizeof(g)]; }`
    /// 里的 `g` 不是全局 `g`), 而本解析器没有作用域链, 故函数体内一律
    /// **不**使用 [`Self::global_obj_tys`] 折 sizeof 表达式形式。
    fn_body_depth: u32,
}

/// M54: 一条声明里被提前剥掉、需交给终端解析函数消费的 decl-specifier.
#[derive(Debug, Clone, Copy, Default)]
pub struct PendingDeclSpec {
    /// `constexpr` / `consteval` / `constinit`
    pub constexpr: Option<ConstexprSpec>,
    /// `static` (内部链接) —— 只在 `constexpr` 已被剥掉的穿插情形下记录,
    /// 其余情形仍由终端自己的说明符循环处理 (不改变既有行为).
    pub is_static: bool,
    /// `const` —— 同上.
    pub is_const: bool,
}

/// M54: 一条全局变量声明的存储期 / cv / 常量性说明符集合.
///
/// 旧签名用四个位旗参数 (`is_static`, `is_volatile`, ...) 传递, 新增“常量性”
/// 需再加两个平行参数且很容易在某个调用点漏一个 —— 改为一个构造器参数。
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobalVarQuals {
    /// `static` —— 内部链接.
    pub is_static: bool,
    /// `volatile` 限定.
    pub is_volatile: bool,
    /// `const` 限定.
    pub is_const: bool,
    /// `constexpr` / `consteval` / `constinit`.
    pub constexpr: Option<ConstexprSpec>,
}

/// declarator 名后 `[...]` 后缀的解析结果 ([`CppParser::parse_array_declarator_suffix`]).
#[derive(Debug, Clone, Default)]
pub struct ArraySuffix {
    /// 已折出的维度, **源顺序** (`a[2][3]` -> `[2, 3]`).
    pub dims: Vec<usize>,
    /// 最外层边界被省略 (`int a[][3] = ...` / `char s[] = "..."`), 需由
    /// 初始化式长度推出 ([dcl.array]/[dcl.init]).
    pub deduce_outer: bool,
    /// 存在折不成常量的维度: 调用方保持旧的降级行为 (不套数组层).
    pub unmodeled: bool,
}

/// M54: C++20 的三个"常量"说明符, 剥离语法时需要留住语义.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstexprSpec {
    /// `constexpr` —— 函数: 可编译期求值; 对象: 隐含 `const`.
    Constexpr,
    /// `consteval` —— 立即函数, 只能编译期求值 (同样可折叠).
    Consteval,
    /// `constinit` —— 只要求**静态初始化**, 对象本身可写 (不隐含 `const`).
    Constinit,
}

impl ConstexprSpec {
    /// 该说明符是否让被声明的**对象**成为常量 (可进整型常量池).
    /// `constinit` 不算: 它只约束初始化时机, 变量仍可被改写.
    fn implies_const_object(self) -> bool {
        matches!(self, Self::Constexpr | Self::Consteval)
    }
}

/// M52: 从 [`CppParser::parse_qualified_name`] 的结果里去掉表示**全局作用域**
/// 的空首段, 并剥掉末段 (它才是被访问的名字), 得到 `QualifiedAccess` 的 path.
///
/// `parse_qualified_name` 用 `parts[0] == ""` 标记 `::` 前缀 (名字/类型文本路径
/// 需要保留该信息), 但 `CppExpr::QualifiedAccess(path, name)` 的 path 存空段会让
/// sema 逐段查找时在第一段就报 `'` not found in qualified name` —— 典型入口是
/// `<cstdio>` 里 `namespace std { inline int printf(...) { return ::printf(...); } }`.
/// path 为空 = 全局作用域, 正是 sema 的语义.
fn qualified_path_without_global_marker(parts: &[String]) -> Vec<String> {
    if parts.is_empty() {
        return Vec::new();
    }
    let head = if parts[0].is_empty() { 1 } else { 0 };
    parts[head..parts.len() - 1].to_vec()
}

impl<'a> CppParser<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        let mut lex = CppLexer::new(src, file);
        let peek = lex.next_tok();
        let mut p = Self {
            lex,
            peek,
            file: Arc::from(file),
            diags: vec![],
            warn_implicit_fallthrough: true,
            lambdas: vec![],
            deferred_gt: 0,
            nttp_angle_stop: 0,
            typedef_alias: std::collections::HashMap::new(),
            alias_templates: std::collections::HashMap::new(),
            anon_counter: 0,
            src,
            type_names: std::collections::HashSet::new(),
            type_names_seeded: false,
            int_consts: vec![],
            constexpr_fns: std::collections::HashMap::new(),
            constexpr_fn_ambiguous: std::collections::HashSet::new(),
            pending_decl_spec: PendingDeclSpec::default(),
            global_obj_tys: std::collections::HashMap::new(),
            fn_body_depth: 0,
        };
        // M54: 构造时直接取的首 token 不过 `bump`, 补一次同样的指数接回.
        p.absorb_truncated_exponent();
        p
    }

    /// M54: 取走 TU 级 constexpr 函数表 (driver 交给下游折叠用).
    pub fn into_constexpr_fns(self) -> std::collections::HashMap<String, ConstexprFunction> {
        self.constexpr_fns
    }

    /// M54: 借用 constexpr 函数表 (求值器只读, 解析过程中折叠用同一张表).
    pub fn constexpr_fns(&self) -> &std::collections::HashMap<String, ConstexprFunction> {
        &self.constexpr_fns
    }

    /// 登记一个整型常量名 (重名且值不同 -> 标为歧义, 不再参与折叠).
    fn record_int_const(&mut self, name: &str, v: i128) {
        if name.is_empty() {
            return;
        }
        match self.int_consts.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => {
                if slot.1 != Some(v) {
                    slot.1 = None;
                }
            },
            None => self.int_consts.push((name.to_string(), Some(v))),
        }
    }

    /// 查一个名字是否在编译期常量表里有确定的值.
    fn int_const_value(&self, name: &str) -> Option<i128> {
        self.int_consts
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| *v)
    }

    /// 把声明符里的数组维度折叠成编译期常量.
    ///
    /// 支持常量子集的积层: 整数字面量 / 常量表里的标识符 (枚举量、
    /// `const int`) / `Enum::V` 限定名 / 一元 `+ - !` / `+ - * / % << >> & | ^`
    /// / `&& ||` / 三元 `?:` / 整形 cast。不在表里的名字一律折叠失败 (保守, 不猜值).
    ///
    /// M54: 新增 **constexpr 函数调用**分支 —— `char buf[square(12)];` 的维度、
    /// `constexpr int sq12 = square(12);` 的初值都靠它折叠。实参先各自走一遍
    /// 本函数 (因此嵌套调用 `square(square(3))` 自内向外化简), 再交给
    /// [`eval_cpp_const_full_with_calls`] 的函数体解释器求值.
    fn fold_int_const_expr(&self, e: &CppExpr) -> Option<i128> {
        match e {
            CppExpr::IntLit(v) => Some(*v as i128),
            CppExpr::Call(_, _) => self.fold_constexpr_call(e),
            CppExpr::Sizeof(arg) => self.fold_sizeof(arg),
            CppExpr::Ident(n) => self.int_const_value(n),
            CppExpr::QualifiedAccess(parts, last) => {
                let mut full = parts.join("::");
                full.push_str("::");
                full.push_str(last);
                self.int_const_value(&full)
                    .or_else(|| self.int_const_value(last))
            },
            CppExpr::Cast(_, inner)
            | CppExpr::StaticCast { expr: inner, .. }
            | CppExpr::ConstCast { expr: inner, .. }
            | CppExpr::ReinterpretCast { expr: inner, .. } => self.fold_int_const_expr(inner),
            CppExpr::UnaryOp(op, x) => {
                let v = self.fold_int_const_expr(x)?;
                match op {
                    UnaryOpKind::Neg => Some(v.checked_neg()?),
                    UnaryOpKind::Plus => Some(v),
                    UnaryOpKind::LNot => Some((v == 0) as i128),
                    UnaryOpKind::Not => Some(!v),
                    _ => None,
                }
            },
            CppExpr::BinOp(op, l, r) => {
                let a = self.fold_int_const_expr(l)?;
                let b = self.fold_int_const_expr(r)?;
                match op {
                    BinOpKind::Add => a.checked_add(b),
                    BinOpKind::Sub => a.checked_sub(b),
                    BinOpKind::Mul => a.checked_mul(b),
                    BinOpKind::Div if b != 0 => a.checked_div(b),
                    BinOpKind::Mod if b != 0 => a.checked_rem(b),
                    BinOpKind::And => Some(a & b),
                    BinOpKind::Or => Some(a | b),
                    BinOpKind::Xor => Some(a ^ b),
                    BinOpKind::Shl if (0..128).contains(&b) => Some(a << b),
                    BinOpKind::Shr if (0..128).contains(&b) => Some(a >> b),
                    BinOpKind::Lt => Some((a < b) as i128),
                    BinOpKind::Gt => Some((a > b) as i128),
                    BinOpKind::Le => Some((a <= b) as i128),
                    BinOpKind::Ge => Some((a >= b) as i128),
                    BinOpKind::EqEq => Some((a == b) as i128),
                    BinOpKind::Ne => Some((a != b) as i128),
                    // 逻辑与/或: 两侧都是无副作用的常量表达式, 短路只影响
                    // 求值顺序不影响结果, 故可先折两侧再按整值真值化。
                    BinOpKind::LAnd => Some(((a != 0) && (b != 0)) as i128),
                    BinOpKind::LOr => Some(((a != 0) || (b != 0)) as i128),
                    _ => None,
                }
            },
            CppExpr::Ternary { cond, then, els } => {
                let c = self.fold_int_const_expr(cond)?;
                self.fold_int_const_expr(if c != 0 { then } else { els })
            },
            _ => None,
        }
    }

    /// M54: 折叠 `sizeof(...)` 两种形式 ([expr.sizeof]).
    ///
    /// 尺寸**唯一真源**是 [`crate::cpp::cpp_const_eval`] 的公共求值器 (它已支持
    /// `sizeof(类型)` 的标量/指针/数组层); 本解析器不再另立一张宽度表, 只负责
    /// 把 `sizeof(表达式)` 的表达式**类型**从 [`Self::global_obj_tys`] 里查出来,
    /// 再转交回去。查不到、类型不完整、或处在可能被局部声明遮蔽的函数体里 ——
    /// 一律返回 `None` (不折叠), 绝不拿别人的类型算尺寸。
    fn fold_sizeof(&self, arg: &SizeofArg) -> Option<i128> {
        match arg {
            SizeofArg::Type(ty) => Self::sizeof_of_type(ty),
            SizeofArg::Expr(e) if self.fn_body_depth == 0 => {
                Self::sizeof_of_type(&self.sizeof_global_obj_type(e)?)
            },
            _ => None,
        }
    }

    /// M54: 一个**完整类型**的 `sizeof`; 不完整 (数组边界未知) 或求值器不认的
    /// 类型返回 `None`.
    fn sizeof_of_type(ty: &MangleType) -> Option<i128> {
        if !Self::array_bounds_known(ty) {
            return None;
        }
        crate::cpp::cpp_const_eval::eval_cpp_const(
            &CppExpr::Sizeof(SizeofArg::Type(ty.clone())),
            &std::collections::HashMap::new(),
        )
    }

    /// M54: 类型是否完整 ([basic.types]): 数组层必须有已知边界
    /// (`0` 是 parser 对**不完整数组** `extern char e[];` 的编码,
    /// 它的 `sizeof` 在标准里是非法的, 折成 `0` 比折不出更糟).
    fn array_bounds_known(ty: &MangleType) -> bool {
        match ty {
            MangleType::Array(inner, n) => *n != 0 && Self::array_bounds_known(inner),
            _ => true,
        }
    }

    /// M54: `sizeof(表达式)` 里表达式的类型 —— 只覆盖**全局/命名空间作用域对象**
    /// 可直接查表得到的形态: 名字、限定名、下标、解引用。其余 (`a.b`、函数调用
    /// 结果、依赖模板参数的表达式) 一律 `None`.
    fn sizeof_global_obj_type(&self, e: &CppExpr) -> Option<MangleType> {
        match e {
            CppExpr::Ident(n) => self.global_obj_tys.get(n).cloned(),
            // 表以**短名**为键: `ns::a` 与另一命名空间的 `b::a` 类型不同时会因
            // 歧义被整体删掉 (见 [`Self::record_global_obj_ty`]), 故此处取末段安全.
            CppExpr::QualifiedAccess(_, last) => self.global_obj_tys.get(last).cloned(),
            CppExpr::BinOp(BinOpKind::Index, base, _) => {
                Self::sizeof_element_type(self.sizeof_global_obj_type(base)?)
            },
            CppExpr::MultiIndex(base, idxs) => {
                let mut ty = self.sizeof_global_obj_type(base)?;
                for _ in 0..idxs.len() {
                    ty = Self::sizeof_element_type(ty)?;
                }
                Some(ty)
            },
            CppExpr::UnaryOp(UnaryOpKind::Deref, x) => {
                Self::sizeof_element_type(self.sizeof_global_obj_type(x)?)
            },
            _ => None,
        }
    }

    /// M54: 下标 / 解引用剥掉一层数组 (指针) 或指针间接层, 得到元素类型.
    fn sizeof_element_type(ty: MangleType) -> Option<MangleType> {
        match ty {
            MangleType::Array(inner, _) | MangleType::Pointer(inner) => Some(*inner),
            _ => None,
        }
    }

    /// M54: 登记一个全局 / 命名空间作用域对象的声明类型.
    ///
    /// 同名多条声明的规则与整型常量池一致 —— **一致就保留, 冲突就删除**:
    /// 删除后该名字的 `sizeof(表达式)` 永不折叠, 宁可退回警告也不给错尺寸。
    /// 例外是补齐: `extern char e[];` (边界未知, 记 `0`) 后跟定义
    /// `char e[4] = "abc";` 时, 数组元素层相同 => 用完整类型覆盖不完整类型。
    fn record_global_obj_ty(&mut self, name: &str, ty: &MangleType) {
        if name.is_empty() {
            return;
        }
        let old = self.global_obj_tys.get(name).cloned();
        match old {
            None => {
                self.global_obj_tys.insert(name.to_string(), ty.clone());
            },
            Some(old) if &old == ty => {},
            Some(old) => {
                let completes = matches!(
                    (&old, ty),
                    (MangleType::Array(oi, 0), MangleType::Array(ni, n)) if *n != 0 && oi == ni
                );
                if completes {
                    self.global_obj_tys.insert(name.to_string(), ty.clone());
                } else {
                    self.global_obj_tys.remove(name);
                }
            },
        }
    }

    /// M54: 折叠一个 constexpr 函数调用 (含实参里嵌套的调用).
    ///
    /// 保守策略: 被调名不在 [`CppParser::constexpr_fns`] 表里、实参个数不匹配、
    /// 解释器报错或循环/递归超限 —— 一律返回 `None` (折叠失败), 绝不给值.
    fn fold_constexpr_call(&self, e: &CppExpr) -> Option<i128> {
        let (callee, args) = match e {
            CppExpr::Call(callee, args) => (callee, args),
            _ => return None,
        };
        // 只有表里有的名字才值得继续: 避免把普通运行时函数调用当成候选.
        let callee_name = match callee.as_ref() {
            CppExpr::Ident(n) => n.clone(),
            CppExpr::TemplateIdent { name, .. } => name.clone(),
            CppExpr::QualifiedAccess(_, last) => last.clone(),
            _ => return None,
        };
        if !self.constexpr_fns.contains_key(&callee_name) {
            return None;
        }
        // 实参先递归折叠成字面量: 解释器只需处理 `f(144)` 这种已化简的形参,
        // 也让 `f(N + 1)` (N 是 const int) 能进入同一条路径.
        let mut folded_args: Vec<CppExpr> = Vec::with_capacity(args.len());
        for a in args {
            match self.fold_int_const_expr(a) {
                // 折出的 i128 必须能无损失地放进 `CppExpr::IntLit` (它存 i64):
                // 超范围就不折叠, 绝不默默截断出一个错值.
                Some(v) if i64::try_from(v).is_ok() => folded_args.push(CppExpr::IntLit(v as i64)),
                _ => return None,
            }
        }
        let call = CppExpr::Call(Box::new(CppExpr::Ident(callee_name)), folded_args);
        // 已知整型常量作为解释器的外部变量可见 (函数体里引用命名空间常量).
        let locals: std::collections::HashMap<String, i128> = self
            .int_consts
            .iter()
            .filter_map(|(n, v)| v.map(|v| (n.clone(), v)))
            .collect();
        let mut ctx = ConstexprContext::with_functions(true, &locals, &self.constexpr_fns);
        eval_cpp_const_full_with_calls(&call, &mut ctx)
    }

    /// M54: 递归把**初始化式**里可编译期求值的常量子树换成字面量.
    ///
    /// 全局对象的初值必须是静态可表示的量: native 后端只认字面量 / 字符串 /
    /// `&具名全局` / 大括号列表。一个 `Call` 混进列表 (例
    /// `int a[4] = {square(2), 1, 2, 3};`) 会让整个初值被判为不可静态表示 ——
    /// 旧行为是**静默丢初值** (对象落 .bss 读回 0)。本函数在唯一折叠真源
    /// (parser) 上先把可折叠的子树替成字面量: 除 constexpr 函数调用外还包括
    /// `-1` 这样的常量算术、`sizeof(g)`、以及枚举量 / `const int` 名字
    /// (均在其所属能力的 [`Self::fold_int_const_expr`] 范围内)。
    /// 折不动的 (运行时函数) 保持原样, 由后端显式报错而不是静默归零。
    ///
    /// **不做整体折叠**的根节点: 字面量自身、取址 / `new` 等链接期地址, 以及
    /// **带转换的表达式** —— [`Self::fold_int_const_expr`] 会剥掉 cast 只算内层
    /// 整型值, 拿它覆盖 `(double)(1 + 2)` 就把“结果类型是浮点”的信息抹掉了
    /// (后端会给浮点对象发一个整数位模式)。cast 仍向下递归, 只化简操作数。
    fn fold_static_init_constants(&self, e: &CppExpr) -> CppExpr {
        let rebuilt = self.fold_static_init_operands(e);
        if Self::init_node_folds_to_int_lit(&rebuilt) {
            if let Some(v) = self.fold_int_const_expr(&rebuilt) {
                if let Ok(iv) = i64::try_from(v) {
                    return CppExpr::IntLit(iv);
                }
            }
        }
        rebuilt
    }

    /// 该根节点形态是否允许被**整体**换成一个整型字面量.
    fn init_node_folds_to_int_lit(e: &CppExpr) -> bool {
        matches!(
            e,
            CppExpr::Call(..)
                | CppExpr::BinOp(..)
                | CppExpr::UnaryOp(..)
                | CppExpr::Sizeof(..)
                | CppExpr::Ident(..)
                | CppExpr::QualifiedAccess(..)
        )
    }

    /// 只对初始化式的**子节点**做折叠重建 (不碰根节点自身), 供
    /// [`Self::fold_static_init_constants`] 自底向上用。
    fn fold_static_init_operands(&self, e: &CppExpr) -> CppExpr {
        match e {
            CppExpr::Call(callee, args) => {
                let folded: Vec<CppExpr> = args
                    .iter()
                    .map(|a| self.fold_static_init_constants(a))
                    .collect();
                let cand = CppExpr::Call(callee.clone(), folded.clone());
                if let Some(name) = Self::constexpr_callee_name(&cand) {
                    if self.constexpr_fns.contains_key(&name) {
                        if let Some(v) = self.fold_constexpr_call(&cand) {
                            if let Ok(iv) = i64::try_from(v) {
                                return CppExpr::IntLit(iv);
                            }
                        }
                    }
                }
                CppExpr::Call(callee.clone(), folded)
            },
            CppExpr::BinOp(k, a, b) => CppExpr::BinOp(
                *k,
                Box::new(self.fold_static_init_constants(a)),
                Box::new(self.fold_static_init_constants(b)),
            ),
            CppExpr::UnaryOp(k, a) => {
                CppExpr::UnaryOp(*k, Box::new(self.fold_static_init_constants(a)))
            },
            CppExpr::InitList(v) => CppExpr::InitList(
                v.iter()
                    .map(|a| self.fold_static_init_constants(a))
                    .collect(),
            ),
            CppExpr::TypedInitList { ty, elems } => CppExpr::TypedInitList {
                ty: ty.clone(),
                elems: elems
                    .iter()
                    .map(|a| self.fold_static_init_constants(a))
                    .collect(),
            },
            CppExpr::Cast(t, a) => {
                CppExpr::Cast(t.clone(), Box::new(self.fold_static_init_constants(a)))
            },
            CppExpr::StaticCast { ty, expr } => CppExpr::StaticCast {
                ty: ty.clone(),
                expr: Box::new(self.fold_static_init_constants(expr)),
            },
            other => other.clone(),
        }
    }

    /// M54: 取 `Call` 节点的被调名 (裸名 / 限定名末段 / 模板名).
    fn constexpr_callee_name(e: &CppExpr) -> Option<String> {
        match e {
            CppExpr::Call(callee, _) => match callee.as_ref() {
                CppExpr::Ident(n) => Some(n.clone()),
                CppExpr::QualifiedAccess(_, last) => Some(last.clone()),
                CppExpr::TemplateIdent { name, .. } => Some(name.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// M54: 当前是否处于 `static_assert (...)` 声明起点.
    ///
    /// 词法器把 `static_assert` 当成**普通标识符** (它不是 CppKw), 所以只能在这里
    /// 按「声明起点」收下: 否则它会走 [`Self::parse_free_fn`] 并在 `(` 处失败,
    /// 被顶层容错降级成 [`CppDecl::Opaque`] —— sema 随即报
    /// "unsupported opaque declaration", 整份翻译单元退回 C 后端.
    fn at_static_assert_decl(&self) -> bool {
        matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) if text == "static_assert"
        ) && matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        )
    }

    /// M54: 解析 `static_assert ( cond [, "msg"] ) ;` ([assert]).
    ///
    /// 条件在**本解析点**就用 [`Self::fold_int_const_expr`] 求值 —— 这条路径已带
    /// constexpr 函数调用折叠 (`static_assert(square(12) == 144);`), 也带整型
    /// 常量池 (`static_assert(N == 5);`)。折得出来就把条件**改写**成字面量存进
    /// AST, 下游 (sema / native / lower) 只需比较一个字面量, 不必各写一遍求值;
    /// 折不出来保持原文, 由 sema 再用自己的常量上下文尝试 (见
    /// `Sema::build_decl` 的 `CppDecl::StaticAssert` 分支) —— 求不出的一律**不判
    /// 通过也不误判失败**, 而是带警告跳过, 绝不静默接受一个可证伪的断言.
    fn parse_static_assert_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        self.bump(); // static_assert
        self.expect_c_tok(TokKind::LParen, "(")?;
        let condition = self.parse_expr()?;
        let mut message = None;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Comma,
                ..
            }))
        ) {
            self.bump();
            // 消息实参按 [assert] 是字符串字面量 (C++20 起可省略, 也允许
            // 用户自定义消息类型: 那不属于本函数建模范围, 只收字面量).
            match self.bump() {
                Some(CppTok::C(Tok {
                    kind: TokKind::StringLit,
                    text,
                    ..
                })) => message = Some(text),
                Some(other) => {
                    let got = format!("{:?}", other);
                    return Err(CppParseError::Unexpected("string literal", got, other.span()));
                },
                None => {
                    return Err(CppParseError::Unexpected(
                        "string literal",
                        "EOF".into(),
                        self.span(),
                    ))
                },
            }
        }
        self.expect_c_tok(TokKind::RParen, ")")?;
        self.expect_c_tok(TokKind::Semi, ";")?;
        let condition = match self.fold_int_const_expr(&condition) {
            // i128 -> i64 超范围: 保持原文, 让 sema 侧继续判断, 绝不截断.
            Some(v) if i64::try_from(v).is_ok() => CppExpr::IntLit(v as i64),
            _ => condition,
        };
        Ok(vec![CppDecl::StaticAssert { condition, message }])
    }

    /// 消耗一个指定的 C 词法 token (按 [`TokKind`] 精确匹配), 否则报
    /// [`CppParseError::Unexpected`]. `label` 只用于诊断文本.
    fn expect_c_tok(&mut self, kind: TokKind, label: &'static str) -> Result<(), CppParseError> {
        let matched = matches!(&self.peek, Some(CppTok::C(t)) if t.kind == kind);
        if matched {
            self.bump();
            Ok(())
        } else {
            let got = match &self.peek {
                Some(tok) => format!("{:?}", tok),
                None => "EOF".to_string(),
            };
            Err(CppParseError::Unexpected(label, got, self.span()))
        }
    }

    /// M54: 一条声明里已剥掉的说明符交给终端解析函数消费 (取后即清空,
    /// 避免泄到下一条声明)。
    fn take_pending_decl_spec(&mut self) -> PendingDeclSpec {
        std::mem::take(&mut self.pending_decl_spec)
    }

    /// M54: 当前 token 是否为 `constexpr` / `consteval` / `constinit` 之一.
    /// 词法器把 `consteval auto` 折叠成单独变体 [`CppKw::ConstevalAuto`],
    /// 它同样是 constexpr 函数说明符, 一并认下.
    fn at_constexpr_spec(&self) -> Option<ConstexprSpec> {
        match &self.peek {
            Some(CppTok::Kw(k, _)) => match k {
                CppKw::Constexpr => Some(ConstexprSpec::Constexpr),
                CppKw::Consteval | CppKw::ConstevalAuto => Some(ConstexprSpec::Consteval),
                CppKw::Constinit => Some(ConstexprSpec::Constinit),
                _ => None,
            },
            _ => None,
        }
    }

    /// M54: 该类型是否是可以安全交给整型常量折叠器的**整数标量**.
    ///
    /// constexpr 函数表只收录“整数进 / 整数出”的函数: 解释器的变量模型是
    /// `HashMap<String, i128>`, 浮点 / 指针 / 类类型在其里没有合法表示,
    /// 强行登记会把它们截成整数 (例如 `1.5` → `1`) —— 那正是
    /// “宁可不可折叠, 绝不给错值”要回避的情形.
    fn is_constexpr_int_scalar(ty: &MangleType) -> bool {
        matches!(
            ty,
            MangleType::Bool
                | MangleType::Char
                | MangleType::Schar
                | MangleType::Uchar
                | MangleType::Short
                | MangleType::Ushort
                | MangleType::Int
                | MangleType::Uint
                | MangleType::Long
                | MangleType::Ulong
                | MangleType::LongLong
                | MangleType::UlongLong
        )
    }

    /// M54: 把一个 constexpr 自由函数的**函数体**转换成编译期解释器的语句序列.
    ///
    /// 两边的 `CppStmt` 是**不同的**枚举 ([`crate::cpp::cpp_const_eval`] 自己建模
    /// 了赋值 / `Adjust` / `Break` 等解释器需要的副作用), 本函数是唯一桥梁.
    /// 任何转换不了的语句 (try / goto / do-while / 结构化绑定 / 无法建模的副作用 …)
    /// 都返回 `None` —— 调用方因此**不登记**该函数, 折叠阶段会保守地拒绝求值,
    /// 而不是给一个错的常量.
    fn to_constexpr_function_body(&self, body: &CppBody) -> Option<Vec<ConstEvalStmt>> {
        // 无 return 的函数不是常量表达式 ([dcl.constexpr]); 解释器也会报错,
        // 但在这里提前拒登记能让“不可折叠”在**登记点**就是确定的.
        if !Self::stmt_seq_has_return(&body.stmts) {
            return None;
        }
        self.to_constexpr_body_stmts(body)
    }

    /// M54: 语句序列 → 解释器语句 (不做 return 检查: 嵌套块 / switch 分支
    /// 本身可以只 `break`, return 只需存在于**函数体顶层语句序列**的某处).
    fn to_constexpr_body_stmts(&self, body: &CppBody) -> Option<Vec<ConstEvalStmt>> {
        let mut out = Vec::with_capacity(body.stmts.len());
        for s in &body.stmts {
            out.extend(self.to_constexpr_stmts(s)?);
        }
        Some(out)
    }

    /// M54: 语句序列里是否至少有一条 `return` (含嵌套块/循环/switch 内).
    fn stmt_seq_has_return(stmts: &[CppStmt]) -> bool {
        stmts.iter().any(|s| match s {
            CppStmt::Return(_) => true,
            CppStmt::If { then, els, .. } | CppStmt::IfConstexpr { then, els, .. } => {
                Self::stmt_seq_has_return(&then.stmts)
                    || els
                        .as_ref()
                        .map(|b| Self::stmt_seq_has_return(&b.stmts))
                        .unwrap_or(false)
            },
            CppStmt::While { body, .. }
            | CppStmt::For { body, .. }
            | CppStmt::DoWhile { body, .. } => Self::stmt_seq_has_return(&body.stmts),
            CppStmt::Block(body) => Self::stmt_seq_has_return(&body.stmts),
            CppStmt::Switch { arms, .. } => arms
                .iter()
                .any(|(_, b)| Self::stmt_seq_has_return(&b.stmts)),
            _ => false,
        })
    }

    /// M54: 单条语句 → 解释器语句列表 (一条声明展开成多条 `Decl`).
    fn to_constexpr_stmts(&self, s: &CppStmt) -> Option<Vec<ConstEvalStmt>> {
        let boxed = |v: Vec<ConstEvalStmt>| -> Vec<Box<ConstEvalStmt>> {
            v.into_iter().map(Box::new).collect()
        };
        Some(match s {
            // 赋值类表达式语句必须翻译成解释器的**副作用**节点:
            // `CppExpr` 本身没有赋值语义, 当成 `Expr` 递下去会把写入丢掉,
            // 而循环/多语句函数恰恰靠它累积结果 (`sum = sum + a;`).
            CppStmt::Expr(e) => self.to_constexpr_expr_stmt(e)?,
            CppStmt::Return(e) => vec![ConstEvalStmt::Return(e.clone())],
            CppStmt::If { cond, then, els } => vec![ConstEvalStmt::If(
                Box::new(cond.clone()),
                boxed(self.to_constexpr_body_stmts(then)?),
                match els {
                    Some(b) => Some(boxed(self.to_constexpr_body_stmts(b)?)),
                    None => None,
                },
            )],
            // `if constexpr` 在常量上下文里等价于普通 if (条件本身会被折叠,
            // 未被选取的一支仍须能转换, 因为转换阶段不做判定).
            CppStmt::IfConstexpr { cond, then, els } => vec![ConstEvalStmt::If(
                Box::new(cond.clone()),
                boxed(self.to_constexpr_body_stmts(then)?),
                match els {
                    Some(b) => Some(boxed(self.to_constexpr_body_stmts(b)?)),
                    None => None,
                },
            )],
            CppStmt::While { cond, body } => vec![ConstEvalStmt::While(
                Some(cond.clone()),
                boxed(self.to_constexpr_body_stmts(body)?),
            )],
            // `do { body } while (cond)` 首轮**无条件**执行 body, 而解释器的 `While`
            // 先判条件。用 `While` 承载会改变累积结果 (例: `do { i = 5; } while
            // (false);` 的真值是 5, 当成 `while` 则是 0) —— 那是**给错的常量**,
            // 比不折叠更糟。故此处直接拒绝登记, 解释器侧的 do-while 支持
            // 不在本次范围 (见最终报告遗留项)。
            CppStmt::DoWhile { .. } => return None,
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => vec![ConstEvalStmt::For(
                match init {
                    Some(i) => Some(Box::new(self.to_constexpr_stmt_single(i)?)),
                    None => None,
                },
                cond.clone(),
                match step {
                    Some(st) => Some(Box::new(self.to_constexpr_stmt_single(st)?)),
                    None => None,
                },
                boxed(self.to_constexpr_body_stmts(body)?),
            )],
            CppStmt::Block(b) => vec![ConstEvalStmt::Block(boxed(
                self.to_constexpr_body_stmts(b)?,
            ))],
            CppStmt::Break => vec![ConstEvalStmt::Break],
            CppStmt::Continue => vec![ConstEvalStmt::Continue],
            CppStmt::Switch { expr, arms, .. } => vec![ConstEvalStmt::Switch(
                Some(expr.clone()),
                arms.iter()
                    .map(|(v, b)| Some((v.clone(), boxed(self.to_constexpr_body_stmts(b)?))))
                    .collect::<Option<Vec<_>>>()?,
            )],
            CppStmt::LocalVar {
                name,
                init,
                is_volatile,
                ..
            } => {
                if *is_volatile {
                    return None;
                }
                vec![ConstEvalStmt::Decl(name.clone(), init.clone())]
            },
            CppStmt::StaticLocal {
                name,
                init,
                is_volatile,
                ..
            } => {
                if *is_volatile {
                    return None;
                }
                vec![ConstEvalStmt::Decl(name.clone(), init.clone())]
            },
            CppStmt::LocalVarList {
                declarators, spec, ..
            } => {
                if spec.is_volatile {
                    return None;
                }
                declarators
                    .iter()
                    .map(|d| ConstEvalStmt::Decl(d.name.clone(), d.init.clone()))
                    .collect()
            },
            CppStmt::Decl(_) => return None,
            _ => return None,
        })
    }

    /// M54: 表达式语句 → 解释器语句 (赋值 / 增减 → 副作用节点, 其余原样).
    ///
    /// 只有**能完整建模副作用**的赋值形态才放行: 左值必须是具名变量,
    /// 否则 (`a[i] = x` / `p->m = x`) 返回 `None` 让整函数不登记 ——
    /// 解释器没有内存写入能力, 默默当成无副作用会给出错的常量.
    fn to_constexpr_expr_stmt(&self, e: &CppExpr) -> Option<Vec<ConstEvalStmt>> {
        if let CppExpr::BinOp(op, l, r) = e {
            if let CppExpr::Ident(name) = l.as_ref() {
                let combined = |kind: BinOpKind| ConstEvalStmt::Assign {
                    name: name.clone(),
                    value: CppExpr::BinOp(kind, Box::new(CppExpr::Ident(name.clone())), r.clone()),
                };
                return Some(vec![match op {
                    BinOpKind::Assign => ConstEvalStmt::Assign {
                        name: name.clone(),
                        value: r.as_ref().clone(),
                    },
                    BinOpKind::AddAssign => combined(BinOpKind::Add),
                    BinOpKind::SubAssign => combined(BinOpKind::Sub),
                    BinOpKind::MulAssign => combined(BinOpKind::Mul),
                    BinOpKind::DivAssign => combined(BinOpKind::Div),
                    BinOpKind::ModAssign => combined(BinOpKind::Mod),
                    BinOpKind::AndAssign => combined(BinOpKind::And),
                    BinOpKind::OrAssign => combined(BinOpKind::Or),
                    BinOpKind::XorAssign => combined(BinOpKind::Xor),
                    BinOpKind::ShlAssign => combined(BinOpKind::Shl),
                    BinOpKind::ShrAssign => combined(BinOpKind::Shr),
                    _ => return None,
                }]);
            }
            // 左值不是具名变量的赋值: 不登记该函数.
            if matches!(
                op,
                BinOpKind::Assign
                    | BinOpKind::AddAssign
                    | BinOpKind::SubAssign
                    | BinOpKind::MulAssign
                    | BinOpKind::DivAssign
                    | BinOpKind::ModAssign
                    | BinOpKind::AndAssign
                    | BinOpKind::OrAssign
                    | BinOpKind::XorAssign
                    | BinOpKind::ShlAssign
                    | BinOpKind::ShrAssign
            ) {
                return None;
            }
        }
        if let CppExpr::UnaryOp(op, inner) = e {
            if let CppExpr::Ident(name) = inner.as_ref() {
                return Some(vec![match op {
                    UnaryOpKind::PreInc | UnaryOpKind::PostInc => ConstEvalStmt::Adjust {
                        name: name.clone(),
                        delta: 1,
                    },
                    UnaryOpKind::PreDec | UnaryOpKind::PostDec => ConstEvalStmt::Adjust {
                        name: name.clone(),
                        delta: -1,
                    },
                    _ => ConstEvalStmt::Expr(e.clone()),
                }]);
            }
        }
        Some(vec![ConstEvalStmt::Expr(e.clone())])
    }

    /// M54: for/while 的 init / step 位置只需**一条**语句 (多声明不建模).
    fn to_constexpr_stmt_single(&self, s: &CppStmt) -> Option<ConstEvalStmt> {
        let mut v = self.to_constexpr_stmts(s)?;
        if v.len() != 1 {
            return None;
        }
        v.pop()
    }

    /// M54: 登记一个 **constexpr 自由函数**到编译期求值表.
    ///
    /// 登记门槛 (任一不合条件就**不登记**, 折叠时该调用只会得不到常量而退回
    /// 原有路径, 绝不会拿到错的值):
    /// 1. 说明符是 `constexpr` / `consteval` (`constinit` 只作用于对象);
    /// 2. 返回类型与形参类型都是整数标量 (见 [`Self::is_constexpr_int_scalar`]);
    /// 3. 每个形参都有名字 (无名形参 `f(int)` 无法在体内引用, 也不能按位绑定);
    /// 4. 函数体可完整转换成解释器语句 ([`Self::to_constexpr_function_body`]);
    /// 5. 同一名字没有已登记的**不同签名** (重载 -> 两边都拉黑)。
    fn register_constexpr_fn(
        &mut self,
        spec: Option<ConstexprSpec>,
        ret: &MangleType,
        name: &str,
        params: &[(String, MangleType)],
        body: &CppBody,
    ) {
        let is_consteval = match spec {
            Some(ConstexprSpec::Constexpr) => false,
            Some(ConstexprSpec::Consteval) => true,
            // `constinit` 不作用于函数; 无 constexpr 说明符的函数不得折叠.
            Some(ConstexprSpec::Constinit) | None => return,
        };
        if !Self::is_constexpr_int_scalar(ret) {
            return;
        }
        if params
            .iter()
            .any(|(n, t)| n.is_empty() || !Self::is_constexpr_int_scalar(t))
        {
            return;
        }
        if self.constexpr_fn_ambiguous.contains(name) {
            return;
        }
        let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
        if let Some(prev) = self.constexpr_fns.get(name) {
            if prev.params.len() != param_names.len() {
                // 同名不同参个数 = 重载: 表无法区分, 两者都废除.
                self.constexpr_fns.remove(name);
                self.constexpr_fn_ambiguous.insert(name.to_string());
                return;
            }
        }
        let Some(stmts) = self.to_constexpr_function_body(body) else {
            return;
        };
        self.constexpr_fns.insert(
            name.to_string(),
            ConstexprFunction {
                name: name.to_string(),
                params: param_names,
                return_type: format!("{:?}", ret),
                is_consteval,
                body: stmts,
            },
        );
    }

    /// M54: 把枚举量初值的 token 拼接文本折成编译期整数.
    ///
    /// 字面量走 [`Self::parse_int_literal_text`]; 不行则把文本**重新**语法分析
    /// 成表达式, 再交给 [`Self::fold_int_const_expr`] —— 这让
    /// `enum { E = square(3) };` (constexpr 函数调用) 与 `enum { F = OTHER };`
    /// (已登记常量引用) 都能拿到确定值。文本必须被**整体**消费 (末尾只能
    /// 剩 `;`), 否则视为未知; 折叠仍在 `self` 上进行, 因此常量池与函数表
    /// 与声明处完全一致 (子解析器不携带任何语义状态).
    fn fold_enum_init_text(&self, text: &str) -> Option<i128> {
        let mut sub = CppParser::new(text.as_bytes(), "<enum-init>");
        let e = sub.parse_expr().ok()?;
        if !matches!(
            sub.peek,
            None | Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            return None;
        }
        self.fold_int_const_expr(&e)
    }

    /// 把一个枚举量初始值的 token 拼接文本解析成整数.
    /// 支持 `42` / `0x1F` / `0b101` / 八进制 / `'0'` / 整型后缀 / 数字分隔符;
    /// 无法确定时返回 None (不猜).
    fn parse_int_literal_text(s: &str) -> Option<i128> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        let (neg, body) = match t.strip_prefix('-') {
            Some(r) => (-1i128, r.trim()),
            None => (1i128, t.strip_prefix('+').unwrap_or(t).trim()),
        };
        // 字符字面量 `'a'` / `'\n'` 也是整型常量表达式
        if body.len() >= 2 && body.starts_with('\'') && body.ends_with('\'') {
            let inner = &body[1..body.len() - 1];
            let v = match inner {
                "\\n" => 10,
                "\\t" => 9,
                "\\r" => 13,
                "\\0" => 0,
                "\\\\" => 92,
                "\\'" => 39,
                _ if inner.chars().count() == 1 => inner.chars().next()? as u32 as i128,
                _ => return None,
            };
            return Some(neg * v);
        }
        // 数字分隔符 `1'000` (C++14): 只在不是字符字面量时剥掉
        let body: String = body.chars().filter(|c| *c != '\'').collect();
        // 剥掉 u/U/l/L 后缀
        let digits = body.trim_end_matches(|c: char| {
            c.is_ascii_alphabetic() && !matches!(c, 'x' | 'X' | 'b' | 'B')
        });
        let (radix, rest) = if let Some(h) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            (16, h)
        } else if let Some(b) = digits
            .strip_prefix("0b")
            .or_else(|| digits.strip_prefix("0B"))
        {
            (2, b)
        } else if digits.len() > 1 && digits.starts_with('0') {
            (8, &digits[1..])
        } else {
            (10, digits)
        };
        if rest.is_empty() {
            return None;
        }
        let v = i128::from_str_radix(rest, radix).ok()?;
        Some(neg * v)
    }

    /// 登记一个已知类型名 (供 `typeid(T)` / `sizeof(T)` 消歧使用).
    /// 公开给 sema/template: 模板实例化新生成的类不在原始源文本里,
    /// 只有实例化点知道名字, 需主动登记后才能在其成员函数体内写 `typeid(T)`.
    pub fn register_type_name(&mut self, name: &str) {
        if !name.is_empty() {
            self.type_names.insert(name.to_string());
        }
    }

    /// 查询一个标识符是否是已知类型名 (会触发一次惰性扫描).
    pub fn is_known_type_name(&mut self, name: &str) -> bool {
        self.ensure_type_names();
        self.type_names.contains(name)
    }

    /// M54: `sizeof ( ... )` 括号内是否是一个**限定名值** (`ns::a` / `ns::arr`).
    ///
    /// 形状必须恰好是 `Ident (:: Ident)* )` —— 末段后面紧跟 `)` 才参与消歧,
    /// 否则 (`ns::a[2]` / `ns::x + 1`) 交给后面的类型试解析 + 回滚路径。
    /// 判定按**末段**: `type_names` 集里存的是名字本身 (不带命名空间前缀),
    /// 所以 `namespace ns { struct T {}; }` 里的 `ns::T` 仍能按类型走.
    fn sizeof_paren_is_value_name(&mut self) -> bool {
        let mut i = 0usize;
        loop {
            let seg = match self.peek_token_at(i) {
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) => text.clone(),
                _ => return false,
            };
            i += 1;
            match self.peek_token_at(i) {
                Some(CppTok::ColonColon(_)) => i += 1,
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                })) => {
                    // 括号前的那一段就是限定名的**末段**, 按它判是否类型
                    return !self.is_known_type_name(&seg);
                },
                _ => return false,
            }
        }
    }

    /// 惰性扫描整个源文本, 收集所有类型名. 只在真的需要消歧时才跑.
    fn ensure_type_names(&mut self) {
        if self.type_names_seeded {
            return;
        }
        self.type_names_seeded = true;
        let src = self.src;
        let mut lx = CppLexer::new(src, "");
        // `typedef ... NAME;` 的 NAME 是 `;` 前最后一个 Ident
        // (启发式: 多 declarator 时逗号前的名字由 `parse_typedef_decl` 从 AST 登记)
        let mut in_typedef = false;
        let mut typedef_last_ident: Option<String> = None;
        // 刚吃完 `class`/`struct`/`union`/`enum`/`typename`/`namespace` 关键字,
        // 下一个 Ident 即类型名/namespace 名
        let mut expect_tag_name = false;
        // `using NAME = ...;` 的 NAME 是 `=` 前最后一个 Ident
        let mut in_using = false;
        let mut using_last_ident: Option<String> = None;
        let mut tok = lx.next_tok();
        while let Some(t) = tok {
            match &t {
                CppTok::C(c) => {
                    if expect_tag_name {
                        expect_tag_name = false;
                        if c.kind == TokKind::Ident {
                            self.type_names.insert(c.text.to_string());
                        }
                    }
                    match c.kind {
                        TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum => {
                            expect_tag_name = true;
                        },
                        TokKind::Ident if in_using => {
                            using_last_ident = Some(c.text.to_string());
                        },
                        TokKind::Assign if in_using => {
                            if let Some(n) = using_last_ident.take() {
                                self.type_names.insert(n);
                            }
                            in_using = false;
                        },
                        TokKind::KwTypedef => {
                            in_typedef = true;
                            typedef_last_ident = None;
                        },
                        TokKind::Ident if in_typedef => {
                            typedef_last_ident = Some(c.text.to_string());
                        },
                        TokKind::Semi => {
                            in_using = false;
                            using_last_ident = None;
                            if in_typedef {
                                if let Some(n) = typedef_last_ident.take() {
                                    self.type_names.insert(n);
                                }
                                in_typedef = false;
                            }
                        },
                        _ => {},
                    }
                },
                CppTok::Kw(kw, _) => {
                    // `typename T` (模板参数 / 依赖类型) 与 `namespace N`
                    if matches!(kw, CppKw::Typename | CppKw::Namespace) {
                        expect_tag_name = true;
                    }
                    // `class X` (含 `enum class X`) 与 `template<class T>`
                    if matches!(kw, CppKw::Class) {
                        expect_tag_name = true;
                    }
                    // `using X = ...;` 的别名: `using` 后第一个 Ident, 紧跟 `=` 时登记
                    if matches!(kw, CppKw::Using) {
                        in_using = true;
                        using_last_ident = None;
                    }
                },
                _ => {},
            }
            tok = lx.next_tok();
        }
    }

    /// `typeid ( ... )` 括号内容就是 type-id 还是 expression 的判定.
    ///
    /// 旧启发式把**任意** Ident 当作类型名, 使 `typeid(a)` (a 是局部变量) 被
    /// 解析成 `TypeidType(Class("a"))`, 后面 sema 报 `unknown class type: a`.
    /// 两者词法同形, 语法上不可区分 ([basic.type.ident]: 能合法解析为
    /// type-id 的序列就是 type-id), 因此必须用已知类型名集合判定:
    ///  - 内置类型说明符 / `const` / `volatile` / `struct|union|enum` 等
    ///    只有类型位置能出现的 token → type-id;
    ///  - Ident 且该名字是已知类型 → 再看后续 token 是否构成类型 (`)` / `*` /
    ///    `&` / `::` / `<` / `[`), `typeid(f())` 这类名字后跟 `(` 的按 expression;
    ///  - 其它 (`*p` / `x.y` / 字面量 / 未知 Ident) → expression.
    fn typeid_arg_is_type_id(&mut self) -> bool {
        // 第一个 token 就是内置类型 / cv 限定 / 聚合关键字 → type-id
        let builtin_type_start = matches!(
            self.peek,
            Some(CppTok::C(Tok {
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
            }))
        );
        if builtin_type_start || self.at(CppKw::Decltype) || self.at(CppKw::Typename) {
            return true;
        }
        let name = match &self.peek {
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => text.to_string(),
            _ => return false,
        };
        if !self.is_known_type_name(&name) {
            return false;
        }
        // 已知类型名 + 后面的 token 不能把序列变成表达式 (`f(...)` / `a.b` / `a[i]`)
        matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::RParen | TokKind::Star | TokKind::Amp | TokKind::Lt,
                ..
            })) | Some(CppTok::ColonColon(_))
        )
    }

    /// 获取当前 lexer 的字节位置, 用于容错恢复时记录跳过区域起点.
    fn capture_start(&self) -> usize {
        self.lex.pos
    }

    /// 提取源文本 [start, end) 区间的原文, 用于 Opaque 声明.
    fn capture_src_text(&self, start: usize, end: usize) -> String {
        if start >= end || end > self.src.len() {
            return String::new();
        }
        std::str::from_utf8(&self.src[start..end])
            .unwrap_or("")
            .to_string()
    }

    fn bump(&mut self) -> Option<CppTok> {
        let cur = self.peek.take();
        self.peek = self.lex.next_tok();
        self.absorb_truncated_exponent();
        cur
    }

    /// M54: 把被词法器**截断的浮点指数**重新接回同一个字面量.
    ///
    /// `src/cpp/lexer.rs` 扫数字时只续接字母数字 / `.` / `'` / `p`, 指数符号
    /// `+`/`-` 处直接断词, 于是 `1.0e-05f` 被切成 `1.0e` `-` `05f` 三个 token.
    /// 后果不是报错而是**算错值**: 它变成减法表达式, 实测
    /// `float x = 1.0e-03f; (int)(x*1000)` 得 0 而不是 1, `<stdfloat>` 里
    /// `std::float16_min = 6.103515625e-05f` 一类常量全部丢符号.
    /// 词法器不在本次改动边界内, 故在解析器**唯一的取 token 入口**修复:
    /// 只接回真正不可能是别的形态的序列 —— 数字文本以 `e`/`E` 结尾 (单独
    /// 看它就是非法字面量, C++ 里没有任何合法解读), 其后紧跟 `+`/`-` 与
    /// 一个以数字开头的数字 token。`x = 1.0e-2;` 之外的 `a e-2`（标识符
    /// 相减）等形态完全不进入本分支。
    fn absorb_truncated_exponent(&mut self) {
        // 1) 当前 token 必须是"以指数标记结尾"的数字.
        let head = match &self.peek {
            Some(CppTok::C(Tok {
                kind: TokKind::Number,
                text,
                span,
            })) if matches!(text.as_bytes().last(), Some(b'e') | Some(b'E')) => {
                (text.clone(), span.clone())
            },
            _ => return,
        };
        // 2) 其后必须是 `+` / `-`, 再后是一个以数字开头的数字 token.
        let sign = match self.peek_token_at(1) {
            Some(CppTok::C(Tok {
                kind: TokKind::Minus,
                ..
            })) => '-',
            Some(CppTok::C(Tok {
                kind: TokKind::Plus,
                ..
            })) => '+',
            _ => return,
        };
        let tail = match self.peek_token_at(2) {
            Some(CppTok::C(Tok {
                kind: TokKind::Number,
                text,
                ..
            })) if text.as_bytes().first().is_some_and(|c| c.is_ascii_digit()) => text,
            _ => return,
        };
        // 3) 真正吞掉这两个 token 并合并文本 (span 沿用字面量首段).
        self.bump();
        self.bump();
        let mut merged = head.0;
        merged.push(sign);
        merged.push_str(&tail);
        self.peek = Some(CppTok::C(Tok {
            kind: TokKind::Number,
            span: head.1,
            text: merged,
        }));
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

    /// 前瞻 `self.peek` 之后的一个 token (不改变解析器状态).
    /// `self.peek` 已被 lex, 故从 `self.lex` 当前位置向前扫一个 token 即可.
    fn peek_after_peek(&self) -> Option<CppTok> {
        let mut lx = self.lex.clone();
        lx.next_tok()
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
            CppTok::RawStringLit { text, .. } => format!("R\"{}\"", text),
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
                    text == "__attribute__" || text == "__asm__" || text == "__asm" || text == "asm"
                },
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
                        },
                        CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        },
                        _ => {
                            self.bump();
                        },
                    }
                }
            }
        }
    }

    /// 解析 C++17 `[[attr1, attr2, ...]]` 属性语句（以 `;` 结尾）.
    /// 若属性为 `fallthrough`，返回 `Some(CppStmt::Fallthrough)`；
    /// 其他属性消费 token 后返回 `None`（视为空语句）.
    /// 调用前需确认 peek 是 `[` 且下一个 token 也是 `[`.
    fn parse_std_attributes_stmt(&mut self) -> Result<Option<CppStmt>, CppParseError> {
        let start_span = self.span();
        // consume `[[`
        self.bump(); // [
        self.bump(); // [
        let mut attr_text = String::new();
        let mut depth = 2i32;
        while depth > 0 {
            let cur_is_lbracket = self.at_tok(TokKind::LBracket);
            let cur_is_rbracket = self.at_tok(TokKind::RBracket);
            if cur_is_lbracket {
                if let Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                })) = self.peek_token_at(1)
                {
                    depth += 2;
                    self.bump();
                    self.bump();
                    continue;
                }
            }
            if cur_is_rbracket {
                if let Some(CppTok::C(Tok {
                    kind: TokKind::RBracket,
                    ..
                })) = self.peek_token_at(1)
                {
                    depth -= 2;
                    if depth == 0 {
                        self.bump(); // ]
                        break;
                    }
                    self.bump();
                    self.bump();
                    continue;
                }
            }
            // 收集属性文本
            if let Some(CppTok::C(Tok { text, .. })) = &self.peek {
                if !attr_text.is_empty() || *text != "," {
                    attr_text.push_str(text);
                    attr_text.push(' ');
                }
            }
            self.bump();
        }
        // consume second `]`
        if self.at_tok(TokKind::RBracket) {
            self.bump();
        }
        // consume trailing `;`
        if self.at_tok(TokKind::Semi) {
            self.bump();
        }
        let attr_trimmed = attr_text.trim();
        if attr_trimmed == "fallthrough" {
            Ok(Some(CppStmt::Fallthrough { span: start_span }))
        } else {
            Ok(None)
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
            },
            None => Err(CppParseError::Unexpected("identifier", "EOF".into(), self.span())),
        }
    }

    /// 期望一个 (可限定的) 名字: `Base` / `ns::Base` / `A::B::C`。
    ///
    /// ctor-initializer 的基类名允许带命名空间限定, 折叠成单个字符串后由
    /// sema 用最后一段与基类的简单名比对。
    fn expect_qualifiable_ident(&mut self) -> Result<String, CppParseError> {
        let mut name = self.expect_ident()?;
        while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump();
            name.push_str("::");
            name.push_str(&self.expect_ident()?);
        }
        Ok(name)
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

    /// M55: 返回类型位置的限定名解析 —— 与 [`Self::parse_qualified_name`] 的唯一
    /// 区别是**声明符边界规则**.
    ///
    /// `[decl-specifier-seq] <nested-name-specifier>? declarator` 在词法上都是
    /// `ident (:: ident)*`, 区分只能靠最后一段之后是什么:
    ///   * `A::B f(`   —— `A::B` 全属类型 (名字 `f` 后跟 `(`);
    ///   * `B::B(`     —— 无 decl-specifier, `B::` 是 nested-name-specifier, `B` 是
    ///     构造函数名; `B::~B(` 同理是析构.
    ///
    /// 旧实现在这里贪心吞掉整条链, 于是**类外构造/析构定义** (`B::B(int a) {}`) 在
    /// `expect_ident` 处撞上 `(` 失败, 整条声明被顶层容错降级成 Opaque ——
    /// 方法体连同初始化列表全部丢失.
    fn parse_qualified_type_name(&mut self) -> Result<Vec<String>, CppParseError> {
        let mut parts = vec![];
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump();
            parts.push(String::new());
        }
        parts.push(self.expect_ident()?);
        while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            if self.declaror_starts_after_coloncolon() {
                break;
            }
            self.bump();
            parts.push(self.expect_ident()?);
        }
        Ok(parts)
    }

    /// 当前 token 是 `::` 时, 判断其后是否为**声明符**起始: `:: ~ ident` (析构)
    /// 或 `:: ident (` (类外的构造函数 / 成员函数名).
    fn declaror_starts_after_coloncolon(&self) -> bool {
        if !matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            return false;
        }
        if matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::Tilde,
                ..
            }))
        ) {
            return true;
        }
        matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) && matches!(
            self.peek_token_at(2),
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        )
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

    /// 一次性收集 lexer 剩余所有 token 到 Vec (O(n) 而非 O(n²)).
    /// 用于需要多次顺序扫描的场景 (如模板实参检测).
    fn collect_remaining_tokens(&self) -> Vec<CppTok> {
        let mut clone = self.lex.clone();
        let mut tokens = Vec::new();
        while let Some(tok) = clone.next_tok() {
            tokens.push(tok);
        }
        tokens
    }

    /// 模板实参列表的闭合 `>` **之后**那个 token 是否允许。
    ///
    /// [temp.names] 的消歧规则之一: 只有当 `>` 后面的 token 能合法地接在
    /// template-id 后面时, 那个 `<` 才是模板实参的开端。
    /// 实测两个必然不合法的后续:
    ///  - **字面量**: `Foo<int> 5;` / `g(a<1, b>2)` 在任何语法里都不是声明;
    ///  - **裸标识符**: 表达式里「值后面紧跟标识符」不合法, 所以
    ///    `if (a < b && c > d)` / `g(a<b, c>d)` 的 `>` 后面是变量名 ——
    ///    那是比较而不是模板实参。但在**类型位置** (`Foo<int> x;` /
    ///    `template-id` 后的被声明对象名) 标识符是合法的, 由
    ///    `allow_ident_follow` 区分。
    fn template_args_close_is_valid(
        tokens: &[CppTok],
        idx_after_gt: usize,
        allow_ident_follow: bool,
    ) -> bool {
        let Some(next) = tokens.get(idx_after_gt) else {
            // 输入在 `>` 处结束: 没有反例证据, 按模板实参处理
            return true;
        };
        match next {
            CppTok::RawStringLit { .. } => false,
            CppTok::C(Tok {
                kind:
                    TokKind::Number
                    | TokKind::FloatLit
                    | TokKind::StringLit
                    | TokKind::WideStringLit
                    | TokKind::Char16StringLit
                    | TokKind::Char32StringLit
                    | TokKind::CharLit
                    | TokKind::WideCharLit
                    | TokKind::Char16Lit
                    | TokKind::Char32Lit,
                ..
            }) => false,
            CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }) => allow_ident_follow,
            _ => true,
        }
    }

    /// `<` 在 ident 后是否像模板实参 `T<U>` 而非比较 `a < b`.
    /// 向前扫描:
    /// - 跨过嵌套 `<`/`>` (Shr=`>>` 视为两层 `>`)
    /// - 遇到 `;` / unmatched `)` / EOF 在 depth==1 仍未闭合 `>` → 比较
    /// - 遇到合法类型 token (ident / C 类型关键字 / `*` / `&` / `,` / `const`)
    ///   不直接判否; 只有破坏 C++ 表达式的 token 才判否
    ///
    /// `allow_ident_follow`: 调用点是否处于**类型位置** (那里 `Foo<int> x;` 的
    /// 后继标识符是被声明对象的姓名); 表达式位置必须传 `false`。
    fn looks_like_template_args_after_lt(&self, allow_ident_follow: bool) -> bool {
        // 一次性收集剩余 token, 避免 peek_token_at 的 O(n²) 复杂度
        let tokens = self.collect_remaining_tokens();
        let mut depth: i32 = 1;
        // M53: 括号内的 `<` `>` `;` `,` 不参与模板实参判定: `enable_if_t<sizeof(T) == 4, T>`
        // 这类带括号的非类型实参里, `sizeof(T)` 闭合的那个 `)` 不是语句结束.
        let mut paren: i32 = 0;
        let mut i = 0usize; // tokens[0] 对应 peek_token_at(1)
        loop {
            if i >= tokens.len() {
                return false;
            }
            match &tokens[i] {
                CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                }) => {
                    depth += 1;
                    // 嵌套深度超过限制: 提前判定为模板实参,
                    // 由 parse_mangle_type_depth 报深度错误 (防 O(n^2) 扫描挂起)
                    if depth > MAX_TEMPLATE_DEPTH as i32 {
                        return true;
                    }
                    i += 1;
                },
                CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                }) => {
                    depth -= 1;
                    if depth == 0 {
                        return Self::template_args_close_is_valid(
                            &tokens,
                            i + 1,
                            allow_ident_follow,
                        );
                    }
                    i += 1;
                },
                CppTok::C(Tok {
                    kind: TokKind::Shr, ..
                }) => {
                    // `>>` 当两层 `>`
                    depth -= 1;
                    if depth == 0 {
                        return Self::template_args_close_is_valid(
                            &tokens,
                            i + 1,
                            allow_ident_follow,
                        );
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Self::template_args_close_is_valid(
                            &tokens,
                            i + 1,
                            allow_ident_follow,
                        );
                    }
                    i += 1;
                },
                // M53: 括号/方括号嵌套内的 token 一律不影响尖括号平衡判断
                CppTok::C(Tok {
                    kind: TokKind::LParen | TokKind::LBracket,
                    ..
                }) => {
                    paren += 1;
                    i += 1;
                },
                CppTok::C(Tok {
                    kind: TokKind::RParen | TokKind::RBracket,
                    ..
                }) if paren > 0 => {
                    paren -= 1;
                    i += 1;
                },
                // 仅在 depth==1 且不在括号内时遇到语句/表达式终结符才判否
                // 逗号在 depth==1 是模板实参分隔符, 合法
                CppTok::C(Tok {
                    kind: TokKind::Semi | TokKind::RParen | TokKind::LBrace | TokKind::RBrace,
                    ..
                }) if depth == 1 && paren == 0 => {
                    return false;
                },
                CppTok::C(Tok {
                    kind: TokKind::Assign | TokKind::Question | TokKind::Colon,
                    ..
                }) if depth == 1 && paren == 0 => {
                    // `a < b ? c : d` / `a < b = c` 是表达式
                    return false;
                },
                _ => {
                    i += 1;
                },
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
            // 记录 parse_decl_list 前的位置, 失败时用于捕获 Opaque 原文
            let decl_start = self.capture_start();
            match self.parse_decl_list() {
                Ok(v) => decls.extend(v),
                Err(e) => {
                    // Record error diagnostic for fallback detection
                    self.diags.push(e.to_diag(&self.file));
                    // 模板深度超限是"必须中止"的硬错误: 容错恢复会把整棵
                    // 超深类型吞成 Opaque 原文 (长度随嵌套线性膨胀), 既丢掉
                    // C0004 诊断, 又让后续阶段面对超长文本 (见 template_depth.rs).
                    if e.is_fatal() {
                        return Err(e);
                    }
                    // 容错: 跳过无法解析的顶层构造, 到下一个 ';' 或 '}'
                    // 使 C++ 前端能处理包含复杂系统头文件的 GCC 构建
                    self.skip_to_next_toplevel();
                    // 捕获跳过区域的原文, 保存为 Opaque 声明
                    let text = self.capture_src_text(decl_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        decls.push(CppDecl::Opaque(text));
                    }
                },
            }
        }
        self.check_implicit_fallthrough(&decls);
        Ok(decls)
    }

    /// 跳过 token 直到下一个顶层声明起点 (';' 或匹配所有括号后)
    fn skip_to_next_toplevel(&mut self) {
        let mut depth = 0i32;
        while let Some(tok) = &self.peek {
            match tok {
                CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }) if depth <= 0 => {
                    self.bump();
                    return;
                },
                CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                }) => {
                    depth += 1;
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                }) => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.bump();
                    if depth <= 0 {
                        return;
                    }
                },
                _ => {
                    self.bump();
                },
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
                CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }) if depth <= 0 => {
                    self.bump();
                    return;
                },
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                }) if depth <= 0 => {
                    // 不消耗 `}`, 留给外层循环
                    return;
                },
                CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                }) => {
                    depth += 1;
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                }) => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.bump();
                },
                _ => {
                    self.bump();
                },
            }
        }
    }

    /// 跳过 token 直到语句边界 (`;` 或平衡括号后).
    /// 用于 parse_body 容错恢复: 单条语句解析失败时, 跳过该语句
    /// 继续解析后续语句, 保持花括号配对完整.
    /// 与 skip_to_member_end 类似但不消耗结尾 token,
    /// 且不跳过未平衡的 `}` (留给外层 parse_body 循环).
    fn skip_to_stmt_boundary(&mut self) {
        let mut depth = 0i32;
        while let Some(tok) = &self.peek {
            match tok {
                // 分号且不在嵌套内 → 语句结束, 消耗分号
                CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }) if depth <= 0 => {
                    self.bump();
                    return;
                },
                // 花括号闭合且不在嵌套内 → 留给外层循环
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                }) if depth <= 0 => {
                    return;
                },
                // 嵌套开始
                CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                }) => {
                    depth += 1;
                    self.bump();
                },
                // 嵌套结束
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                })
                | CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                }) => {
                    if depth > 0 {
                        depth -= 1;
                        self.bump();
                    } else {
                        // 未平衡的 `}` → 不消耗, 留给外层
                        return;
                    }
                },
                _ => {
                    self.bump();
                },
            }
        }
    }

    /// 解析**恰好一条**声明的入口 (调用点必须能容纳单个声明).
    ///
    /// 带 init-declarator 列表的声明 (`int a = 0, b = 1;`) 会展开成多个
    /// `CppDecl`, 这类形态在本入口会报错 —— 需要多声明的位置
    /// (顶层 / namespace 体 / export 块 / 类体) 请用 [`Self::parse_decl_list`]。
    ///
    /// 本入口目前只服务 `extern template <declaration>`: 变量/函数的显式实例化
    /// 按 [temp.explicit] 只能带一个 **decl-id**, 所以「一条声明多个
    /// init-declarator」在该位置本就是非法代码 (g++ 对
    /// `extern template int gv<int>, gv2<int>;` 同样报 "expected ';' before ','"),
    /// 这里报错不会拒掉任何合法代码。
    fn parse_decl(&mut self) -> Result<CppDecl, CppParseError> {
        let mut decls = self.parse_decl_list()?;
        if decls.len() == 1 {
            return Ok(decls.pop().expect("长度为 1 的列表弹出后必有余元素"));
        }
        Err(CppParseError::Unsupported(
            "multiple declarators in a single-declaration context",
            self.span(),
        ))
    }

    /// 解析一条声明 = decl-specifier-seq + init-declarator-list.
    ///
    /// 文法里只有**变量声明**这一形带头带 init-declarator 列表, 故列表长度 > 1
    /// 只可能发生在 `CppDecl::GlobalVar` (及类内成员变量) 分支: 一条声明
    /// `int a = 0, b = 1;` 按声明符展开成 N 个 `CppDecl`, 与 C 前端
    /// (`src/parser.rs::parse_decl` 返回 `Vec<Decl>`) 保持同一形状。
    /// 其余形态 (函数 / class / 模板 / namespace / using / module ...) 都是单元素。
    fn parse_decl_list(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        // M54: 一条**新**声明从干净的说明符状态开始 —— 上一跳残留的
        // `constexpr` 不能泄到不相关的声明上 (否则一个标量会被当成常量池登记,
        // 给出错的数组维度 / 初值). 同一跳内的内部递归走
        // [`Self::parse_decl_list_carrying_spec`], 说明符因此得以保留.
        self.pending_decl_spec = PendingDeclSpec::default();
        self.parse_decl_list_carrying_spec()
    }

    /// M54: [`Self::parse_decl_list`] 的实现体 —— 不清除
    /// [`CppParser::pending_decl_spec`], 供需要回到**同一条**声明剩余
    /// decl-specifier (`constexpr` / `extern` 之后继续解析同一跳) 时使用.
    fn parse_decl_list_carrying_spec(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
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
        // M54: `static_assert (...)` 必须先于「Ident 开头 => 函数」的分流收掉:
        // 它在词法层只是普通标识符, 落到 parse_free_fn 会失败并降级成 Opaque.
        if self.at_static_assert_decl() {
            return self.parse_static_assert_decl();
        }
        // 模板声明 (M8: 简化为占位)
        if self.at(CppKw::Template) {
            return self.parse_template_decl(false);
        }
        // 命名空间 (M8)
        if self.at(CppKw::Namespace) {
            return Ok(vec![self.parse_namespace_decl()?]);
        }
        // using 声明 (M8)
        if self.at(CppKw::Using) {
            return Ok(vec![self.parse_using_decl()?]);
        }
        // C++20 module / export module (M9: 占位)
        if self.at(CppKw::Module) {
            return Ok(vec![self.parse_module_decl()?]);
        }
        // C++20 import (M9: 占位)
        if self.at(CppKw::Import) {
            return Ok(vec![self.parse_import_decl()?]);
        }
        // C++20 export { ... } (M9: 占位)
        if self.at(CppKw::Export) {
            return self.parse_export_decl();
        }
        // M18.0: enum (含 scoped enum class) 定义/opaque 声明。
        // 仅当 enum 后紧跟 `{` / `;` / `:` (带底层类型) 时才是定义形态；
        // `enum E foo()` / `extern enum E foo()` 里的 enum 是 **elaborated
        // type specifier** ([dcl.type.elab]) 作为返回类型，必须落到下面的
        // `is_type_kw_or_spec` 分支 (那里 KwEnum 已在合法 type-start 集里)，
        // 否则 cpplib.h 的 `extern enum cpp_token_fld_kind cpp_token_val_index
        // (const cpp_token *tok);` 会被误当 enum 定义处理失败→降级 Opaque。
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwEnum,
                ..
            }))
        ) {
            let mut k = 1usize;
            let t = self.peek_token_at(k);
            if matches!(
                t,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwStruct,
                    ..
                }))
            ) || matches!(t, Some(CppTok::Kw(CppKw::Class, _)))
            {
                k += 1;
            }
            if matches!(
                self.peek_token_at(k),
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) {
                k += 1;
            }
            if matches!(
                self.peek_token_at(k),
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace | TokKind::Semi | TokKind::Colon,
                    ..
                }))
            ) {
                return Ok(vec![self.parse_enum_decl()?]);
            }
        }
        // C++20 concept (M9: 占位, 仅顶层)
        if self.at(CppKw::Concept) {
            return Ok(vec![self.parse_concept_decl()?]);
        }
        // extern "C" 链接规范 ([lex.phases]/[basic.link]: 块形态与单声明形态)
        if self.at_linkage_spec() {
            return self.parse_linkage_spec();
        }
        // M30.2: `extern` 链接性说明 + 普通声明 (`extern void f(...);`,
        // `extern T g;`) - 无 "C" 字面量时跳过 extern, 按普通声明解析.
        // G10: 特殊处理 `extern template ...;`
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwExtern,
                ..
            }))
        ) {
            self.bump(); // consume 'extern'
                         // Check for 'template'
            if matches!(self.peek_token_at(0), Some(CppTok::Kw(CppKw::Template, ..))) {
                return Ok(vec![self.parse_extern_template_decl()?]);
            } else {
                return self.parse_decl_list_carrying_spec();
            }
        }
        // C++20: `constinit` / `consteval` / `constexpr` 说明符
        // 语法上对后续解析是噪声 (跳过后按普通声明解析), 但它们携带两条
        // **必须留住**的语义: 对象 ⇒ 常量性 (可进整型常量池), 函数 ⇒
        // 可编译期求值 (可进 constexpr 函数表). 因此记到
        // [`CppParser::pending_constexpr_spec`] 再交给终端解析函数消费.
        for (kw, spec) in &[
            (CppKw::Constinit, ConstexprSpec::Constinit),
            (CppKw::Consteval, ConstexprSpec::Consteval),
            (CppKw::Constexpr, ConstexprSpec::Constexpr),
        ] {
            if self.at(*kw) {
                self.bump();
                self.pending_decl_spec.constexpr = Some(*spec);
                return self.parse_decl_list_carrying_spec();
            }
        }
        // M54: 说明符可以任意顺序交错 (`static constexpr int N = 5;` /
        // `constexpr static int M = 6;`)。已经进入 constexpr 剥离状态时,
        // 同一条声明里的 `static` / `const` 也得在本跳吃掉 (否则会撞上
        // 不认识它们的类型解析器), 并把存储期信息一并带到终端。
        if self.pending_decl_spec.constexpr.is_some() {
            let mut consumed = false;
            while self.pending_decl_spec.constexpr.is_some() {
                if self.at(CppKw::Static) {
                    self.pending_decl_spec.is_static = true;
                } else if self.at(CppKw::Const) {
                    self.pending_decl_spec.is_const = true;
                } else if self.at(CppKw::Volatile) {
                    // volatile 无语义影响 (下游按普通限定处理), 直接吐掉
                    self.bump();
                    consumed = true;
                    continue;
                } else {
                    break;
                }
                self.bump();
                consumed = true;
            }
            if consumed {
                return self.parse_decl_list_carrying_spec();
            }
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
                return self.parse_decl_list();
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
                return Ok(vec![self.parse_class_decl()?]);
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
                    return Ok(vec![self.parse_aggr_decl(false)?]);
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
            return Ok(vec![self.parse_aggr_decl(false)?]);
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
            return Ok(vec![self.parse_aggr_decl(true)?]);
        }
        // 运算符重载: Type operator op ( ... )
        if self.is_operator_fn_start() {
            return Ok(vec![self.parse_operator_fn()?]);
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
                CppKw::Inline
                    | CppKw::Static
                    | CppKw::Const
                    | CppKw::Volatile
                    | CppKw::Class
                    // M55: `typename` 消歧义前缀开头的声明 (`typename enable_if<..>::type
                    // f(T)`) 与限定类型同形, 必须一起进入「函数 vs 变量」前瞻分流;
                    // 否则既不命中 Ident 分支也不命中类型关键字分支, 整条降级 Opaque.
                    | CppKw::Typename,
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
                        CppKw::Inline
                            | CppKw::Static
                            | CppKw::Const
                            | CppKw::Volatile
                            | CppKw::Typename
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
                    if matches!(t.kind, TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum) {
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
                // M55: nested-name-specifier 的 `::` 分隔符 —— 跳过它继续看后一段
                // (`typename A::B::type f()` / `A<int>::type f()` 等多段限定名);
                // 不跳则前瞻停在首段, 将类型误判为函数名。
                if matches!(tok, CppTok::ColonColon(_)) {
                    j += 1;
                    continue;
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
            // 名字段为 `Ident (:: [~] Ident)+ (` (析构中间带 ~)。
            let is_qualified_fn = is_name_ident && self.looks_like_out_of_class_def(name_pos);
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
            // 函数指针 declarator `ret (*name)(params)` (全局): 返回类型后紧接
            // `(` 再接 `*` —— 与 `int f(int)` (Ident 再接 `(`) 互斥, 不会误伤
            // 函数声明。`is_name_ident` 此时为 false (declarator 名被括号包围)。
            let is_fnptr_global = !is_name_ident
                && matches!(
                    name_tok,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                )
                && matches!(
                    self.peek_token_at(name_pos + 1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star,
                        ..
                    }))
                );
            if is_fnptr_global {
                return self.parse_global_fnptr_decl();
            }
            if self.is_operator_fn_start() {
                return Ok(vec![self.parse_operator_fn()?]);
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
                },
            }
        }
        Err(CppParseError::Unsupported("declaration form", self.span()))
    }

    /// G10: Parse 'extern template <...>;'
    /// Suppresses implicit instantiation of template (explicit instantiation TU has it)
    fn parse_extern_template_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // Expect 'template'
        self.expect_kw(CppKw::Template)?;

        // Parse the inner declaration (class/struct/function/etc.)
        let inner_decl = self.parse_decl()?;

        Ok(CppDecl::ExternTemplate(Box::new(inner_decl)))
    }

    /// 解析 `template <...> <decl>`; 内部声明带 init-declarator 列表时
    /// 按声明符展开成 N 个 `CppDecl::Template` (`template<class T> T a = 1, b = 2;`).
    fn parse_template_decl(&mut self, is_export: bool) -> Result<Vec<CppDecl>, CppParseError> {
        self.expect_kw(CppKw::Template)?;

        // G13: Check if this is a full specialization: `template<>` or `template<int>`
        // Key difference: no type parameters between <>
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected("<", format!("{:?}", self.peek), self.span()));
        }

        let _saved_lex = self.lex.clone();
        let _saved_peek = self.peek.clone();

        self.bump(); // consume <

        // Look ahead to determine if this is full specialization or regular/partial
        let is_full_spec = self.lookahead_is_full_specialization();

        if is_full_spec {
            // 全特化: `template<> struct X<实参> { ... }`
            return Ok(vec![self.parse_full_specialization()?]);
        }

        // Regular partial/primary template with type parameters
        // M51: 这里原本再 bump 一次并注释为 "consume <" —— 但 `<` 在上面已经吃掉了,
        // 于是参数表的**第一个 token** 被无条件丢弃:
        //   * `template <int N>`   -> `int` 被吞, N 退成一个普通类型形参 (非类型信息丢失);
        //   * `template <typename... Args>` -> `typename` 被吞, `...` 落在形参名位置
        //     上解析失败 -> 整个声明降级成 Opaque (可变参数模板整个起火).
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
            // C++11 参数包: `typename... Args` / `class... Args`
            let is_pack = has_kw
                && matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ellipsis,
                        ..
                    }))
                );
            if is_pack {
                self.bump(); // consume `...`
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
                    is_pack: false,
                    template_params: None,
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
            // Step 2 (A) 扩展: 同时支持非类型参数 pack
            // (`template <typename T, T... Ints>` 与 `template <int... N>`),
            // 这是 libstdc++ `integer_sequence<T, Ints...>` 与 `make_index_sequence<N>`
            // 的实现地基 —— pair 的 piecewise ctor 签名依赖它。
            // 尝试 <类型> [...] <名> 形态; 失败回退原路径 (保留旧容忍度)。
            let mut non_type_pack = false;
            let non_type = if has_kw {
                None
            } else {
                let saved_lex = self.lex.clone();
                let saved_peek = self.peek.clone();
                match self.parse_mangle_type() {
                    Ok(ty) => {
                        // 非类型 pack: `<type> ... <name>` —— 先吃 `...`。
                        let ellipsis = matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Ellipsis,
                                ..
                            }))
                        );
                        if ellipsis {
                            self.bump();
                        }
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Ident,
                                ..
                            }))
                        ) {
                            non_type_pack = ellipsis;
                            Some(ty)
                        } else {
                            self.lex = saved_lex;
                            self.peek = saved_peek;
                            None
                        }
                    },
                    _ => {
                        self.lex = saved_lex;
                        self.peek = saved_peek;
                        None
                    },
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
                non_type: non_type.clone(),
                is_pack: is_pack || non_type_pack,
                // G11: Simple heuristic - class keyword without typename suggests template template param
                template_params: if has_kw && !is_pack && !non_type.is_some() {
                    Some(1)
                } else {
                    None
                },
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
            return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
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
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Gt, ..
                        })) => {
                            depth -= 1;
                            if depth > 0 {
                                pattern_text.push('>');
                            }
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Shr, ..
                        })) => {
                            depth -= 1;
                            if depth > 0 {
                                pattern_text.push_str(">>");
                            }
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Colon,
                            ..
                        })) => {
                            pattern_text.push(':');
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        })) => {
                            pattern_text.push(',');
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Star,
                            ..
                        })) => {
                            pattern_text.push('*');
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident,
                            text,
                            ..
                        })) => {
                            pattern_text.push_str(&text);
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::Number,
                            text,
                            ..
                        })) => {
                            pattern_text.push_str(&text);
                        },
                        Some(CppTok::ColonColon(_)) => {
                            pattern_text.push_str("::");
                        },
                        // M30.1: true/false 字面量转回文本 (非类型偏特化
                        // pattern 如 `picker<T, false>` 需要与 pick_partial
                        // 的 NonType key 匹配; Debug 格式含逗号会破坏
                        // pattern_text 的 split(',') 约定)
                        Some(CppTok::Kw(CppKw::True, _)) => {
                            pattern_text.push_str("true");
                        },
                        Some(CppTok::Kw(CppKw::False, _)) => {
                            pattern_text.push_str("false");
                        },
                        Some(other) => {
                            // 其他 token 转为文本
                            let s = format!("{:?}", other);
                            pattern_text.push_str(&s);
                        },
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
                    members.extend(self.parse_member_decl_with_class(&_name)?);
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
                    // 偏特化声明体里没有访问说明符位置 (成员由 pattern 重建),
                    // 置空 -> sema 按 struct/class 默认访问回退
                    member_access: vec![],
                });
                return Ok(vec![CppDecl::PartialSpec {
                    params: TemplateArgs { params },
                    pattern_text: pattern_text.trim().to_string(),
                    decl: Box::new(decl),
                }]);
            } else {
                // 不是 partial spec, 恢复 lex 状态
                self.lex = saved_lex;
                self.peek = saved_peek;
            }
        }
        let decls = self.parse_decl_list()?;
        // M53: `template <...> using Name = Type;` = 别名模板 ([temp.alias]).
        // 登记后使用点在类型解析处就地展开成目标类型; AST 仍保留
        // `Template { Typedef }` 形状 (不向下游注册为类模板, 见 native::collect_templates)
        let mut out = Vec::with_capacity(decls.len());
        for decl in decls {
            self.register_alias_template(&params, &decl);
            out.push(CppDecl::Template {
                params: TemplateArgs {
                    params: params.clone(),
                },
                decl: Box::new(decl),
                requires: requires.clone(),
                is_export,
            });
        }
        Ok(out)
    }

    /// 跳过当前位置的一个 **attribute-specifier-seq** ([dcl.attr]/[attr.common]):
    /// C++11 `[[...]]`、GNU `__attribute__((...))`, 以及**未展开的属性宏**
    /// `IDENT(args)` (glibc++ 大量使用 `namespace std _GLIBCXX_VISIBILITY(default)`
    /// 与 `extern "C++" __attribute__((__noreturn__)) void f();` 形态; 真 GCC
    /// 把宏展开为 `__attribute__((...))`, 但预处理层不能保证一定展开, 因此一并
    /// 容忍 `IDENT(args)` 形态)。命名空间与 linkage-spec 两处都可能在关键字与
    /// 实际声明/块之间插入属性序列, 不容忍则整条声明被块局部容错降级成 Opaque。
    fn skip_attribute_specifier_seq(&mut self) {
        loop {
            let lb_pair = self.at_tok(TokKind::LBracket)
                && matches!(
                    self.peek_token_at(1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBracket,
                        ..
                    }))
                );
            let ident_call = matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) && matches!(
                self.peek_token_at(1),
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            );
            if lb_pair {
                self.bump(); // [
                self.bump(); // [
                let mut depth = 2i32;
                while depth > 0 {
                    match &self.peek {
                        Some(CppTok::C(Tok { kind, .. })) => {
                            match kind {
                                TokKind::LBracket => depth += 1,
                                TokKind::RBracket => depth -= 1,
                                _ => {},
                            }
                            self.bump();
                        },
                        Some(_) => {
                            self.bump();
                        },
                        None => break,
                    }
                }
            } else if ident_call {
                self.bump(); // IDENT
                self.bump(); // (
                let mut depth = 1i32;
                while depth > 0 {
                    match &self.peek {
                        Some(CppTok::C(Tok { kind, .. })) => {
                            match kind {
                                TokKind::LParen | TokKind::LBracket | TokKind::LBrace => {
                                    depth += 1
                                },
                                TokKind::RParen | TokKind::RBracket | TokKind::RBrace => {
                                    depth -= 1
                                },
                                _ => {},
                            }
                            self.bump();
                        },
                        Some(_) => {
                            self.bump();
                        },
                        None => break,
                    }
                }
            } else {
                break;
            }
        }
    }

    /// 解析 namespace foo { ... } 或 namespace { ... }
    fn parse_namespace_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // Check for 'inline' keyword
        let is_inline = self.at(CppKw::Inline);
        if is_inline {
            self.bump();
        }

        self.expect_kw(CppKw::Namespace)?;
        // 属性可出现在 `namespace` 与名字之间 (`namespace [[gnu::hidden]] X`)。
        self.skip_attribute_specifier_seq();
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
        // 属性也可出现在名字与 `{` 之间 (`namespace std _GLIBCXX_VISIBILITY(default) {`
        // / `namespace X [[attr]] {` / `namespace X __attribute__((...)) {`)。
        self.skip_attribute_specifier_seq();
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut body = vec![];
        // 逐成员解析 + 错误恢复 (与顶层 `parse_top_level` 以及类体
        // `parse_member_decl_with_class` 同构): 块内一个成员解析失败**不得**
        // 把整块降级成一个 Opaque, 否则真实 libstdc++ 头 (`namespace std { ... }`
        // 上百个成员) 只要有一处暂不支持的构造就会把整块吞掉, 首错永远停在块头
        // (如 `std {`) 而无法定位到具体成员 —— 这与 Step 1 (L) 「惰性成员体 /
        // 两阶段查找」的时序前提配套: 先把失败局部化, 后续每项能力的落地才能
        // 各自推进, 不存在"一个缺口把整块拖倒"这种缺口。
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
            let member_start = self.capture_start();
            match self.parse_decl_list() {
                Ok(ds) => body.extend(ds),
                Err(e) => {
                    // 深度超限等硬错误不参与容错恢复 (见 parse_top_level 同类处理)
                    if e.is_fatal() {
                        return Err(e);
                    }
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                    let text = self.capture_src_text(member_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        body.push(CppDecl::Opaque(text));
                    }
                },
            }
        }
        self.bump();

        if is_inline {
            Ok(CppDecl::InlineNamespace {
                name: real_name,
                body,
            })
        } else {
            Ok(CppDecl::Namespace {
                name: real_name,
                body,
            })
        }
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
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
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
        if !is_namespace
            && path.len() == 1
            && matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            )
        {
            self.bump(); // consume '='
            let target = self.parse_mangle_type()?;
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            // Record in typedef_alias for template resolution
            self.typedef_alias
                .insert(path[0].clone(), (format!("{:?}", target), vec![]));
            return Ok(CppDecl::Typedef {
                target,
                name: path[0].clone(),
            });
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

    /// G13: Look ahead to determine if we have full specialization syntax
    /// Full specializations are:
    /// 1. `template<> identifier ...` (empty parens)
    /// 2. `template<specific_type> identifier ...` (explicit types like template<void>)
    ///
    /// Not full spec: `template<typename T> identifier<T*> ...` (partial with type params + pattern)
    /// G11: Look ahead to detect template template parameter pattern
    fn lookahead_is_template_template(&self) -> bool {
        // Check pattern: `< typename/class Name > class/typename Name`
        self.peek_token_at(0).is_some_and(|t0| {
            matches!(
                t0,
                CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                })
            ) && self.peek_token_at(1).is_some_and(|t1| {
                let is_type_kw = matches!(t1, CppTok::C(Tok {
                    kind: TokKind::Ident, text, ..
                }) if text == "typename" || text == "class")
                    || self.at(CppKw::Typename)
                    || self.at(CppKw::Class);
                is_type_kw
                    && self.peek_token_at(2).is_some_and(|t2| {
                        matches!(
                            t2,
                            CppTok::C(Tok {
                                kind: TokKind::Ident,
                                ..
                            })
                        ) && self.peek_token_at(3).is_some_and(|t3| {
                            matches!(
                                t3,
                                CppTok::C(Tok {
                                    kind: TokKind::Ellipsis | TokKind::Gt,
                                    ..
                                })
                            ) && self.peek_token_at(4).is_some_and(|t4| {
                                matches!(
                                    t4,
                                    CppTok::C(Tok {
                                        kind: TokKind::Gt,
                                        ..
                                    })
                                ) && self.peek_token_at(5).is_some_and(|t5| {
                                    (matches!(t5, CppTok::C(Tok {
                                    kind: TokKind::Ident, text, ..
                                }) if text == "class")
                                        || self.at(CppKw::Class)
                                        || self.at(CppKw::Typename))
                                })
                            })
                        })
                    })
            })
        })
    }

    fn lookahead_is_full_specialization(&self) -> bool {
        // M51: 类模板/函数模板全特化唯一合法书写是 `template <>` (空参数表).
        // 调用点已吃掉开尖括号, 所以 peek 直接是 `>` 就是空表.
        //
        // G13 原实现"扫到闭合 `>` 前没看见 typename/class 就算全特化", 于是
        // `template <int N> struct Buf { ... }` (非类型形参主模板) 也被吞进
        // 全特化分支: 主模板不进入注册表, `Buf<8>` 退成"未知类型".
        matches!(
            self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Gt,
                ..
            }))
        )
    }

    /// G13: 解析类模板的显式 (全) 特化体.
    ///
    /// 形态: `template <> struct X<具体实参> [: 基类列表] { 成员 }`.
    /// 调用方已消费 `template` 与 `<`, 而 `lookahead_is_full_specialization`
    /// 保证此处 `template` 的形参表恰为空 (`>`), 因此直接吃掉它.
    ///
    /// 特化实参来自**名字之后**的 `<...>`: `template<> struct is_integral<int>`
    /// 的实参是 `[int]`. 早先的实现从 `template<...>` 里取实参, 于是
    ///   * 实参恒为空 -> `find_full_spec` 的 `specific_args.len() != args.len()`
    ///     永不匹配, 全特化形同不存在;
    ///   * 只在 `Ident` token 上累加文本 -> `X<T*>` / `X<const char*>` 的
    ///     `*` `&` `const` 被静默丢弃;
    ///   * 顶层逗号处 `break` 且不消费逗号 -> `X<int, double>` 剩下的 token 吐回
    ///     主流, 整个声明解析错位降级成 Opaque.
    ///
    /// 现在统一走 `parse_template_args_text_list` (与 M51 偏特化收集同源).
    ///
    /// 特化体是一个**完整**的类声明: 基类子句必须解析 (全特化的基类不从主模板
    /// 继承), 成员按类体规则解析 (访问说明符 / 一条声明多声明符 / 失败降级
    /// Opaque), 全部成员种类原样保留.
    fn parse_full_specialization(&mut self) -> Result<CppDecl, CppParseError> {
        self.bump(); // `template<>` 的闭合 `>`
        let is_aggr = self.at_aggr_kw();
        // `class X` 体内默认 private, `struct X` 默认 public
        let is_class_kw = matches!(&self.peek, Some(CppTok::Kw(CppKw::Class, _)));
        if is_aggr {
            self.bump(); // struct / class
        }
        let primary_name = self.expect_ident()?;
        let specific_args = self.parse_template_args_text_list()?;
        let bases = self.parse_base_list()?;

        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump(); // {

        let mut cur_access = if is_aggr && !is_class_kw {
            Access::Public
        } else {
            Access::Private
        };
        let mut members: Vec<CppDecl> = vec![];
        let mut member_access: Vec<(String, Access)> = vec![];
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
            if self.take_access_spec_or_skip(&mut cur_access) {
                continue;
            }
            let member_start = self.capture_start();
            match self.parse_member_decl_with_class(&primary_name) {
                Ok(ds) => {
                    for d in ds {
                        if let Some(key) = member_access_key(&d) {
                            member_access.push((key, cur_access));
                        }
                        members.push(d);
                    }
                },
                Err(e) => {
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                    let text = self.capture_src_text(member_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        members.push(CppDecl::Opaque(text));
                    }
                },
            }
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

        Ok(CppDecl::FullSpec {
            specific_args,
            class: ClassDecl {
                name: primary_name,
                is_struct: is_aggr && !is_class_kw,
                is_union: false,
                bases,
                members,
                member_access,
            },
        })
    }

    /// M51: 收集位于当前位置的 `<...>` (平衡尖括号, 顶层逗号分隔), 逐段解析成
    /// MangleType; 返回时 `...>` 已被消费. 不在 `<` 处则返回空表.
    ///
    /// 与 `parse_template_decl` 里偏特化的 pattern_text 收集不同, 这里必须走
    /// `tok_text` 而不是 `{:?}` 兜底: 关键字 (`const` / `char` / `unsigned` 等)
    /// 是真实文本的一部分, Debug 文本会把它变成不可解析的串 (如 `Kw(Const, ..)`),
    /// 全特化实参于是永远对不上调用点.
    fn parse_template_args_text_list(&mut self) -> Result<Vec<MangleType>, CppParseError> {
        let mut out: Vec<MangleType> = vec![];
        if !matches!(
            self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) {
            return Ok(out);
        }
        self.bump(); // `<`
        let mut depth: i32 = 1;
        let mut buf = String::new();
        while let Some(t) = self.bump() {
            match &t {
                CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                }) => {
                    depth += 1;
                    buf.push('<');
                },
                CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                }) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    buf.push('>');
                },
                CppTok::C(Tok {
                    kind: TokKind::Shr, ..
                }) => {
                    depth -= 2;
                    if depth <= 0 {
                        break;
                    }
                    buf.push_str(">>");
                },
                CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }) if depth == 1 => {
                    out.push(self.parse_mangle_type_from_str(buf.trim())?);
                    buf.clear();
                },
                other => {
                    buf.push_str(&tok_text(other));
                    buf.push(' ');
                },
            }
        }
        if !buf.trim().is_empty() {
            out.push(self.parse_mangle_type_from_str(buf.trim())?);
        }
        Ok(out)
    }

    /// Helper: parse MangleType from string representation (for full spec args)
    fn parse_mangle_type_from_str(&self, s: &str) -> Result<MangleType, CppParseError> {
        match s {
            "void" => Ok(MangleType::Void),
            "bool" => Ok(MangleType::Bool),
            "char" => Ok(MangleType::Char),
            "signed char" | "schar" => Ok(MangleType::Schar),
            "unsigned char" | "uchar" => Ok(MangleType::Uchar),
            "short" | "short int" => Ok(MangleType::Short),
            "unsigned short" | "unsigned short int" => Ok(MangleType::Ushort),
            "int" => Ok(MangleType::Int),
            "unsigned int" | "unsigned" => Ok(MangleType::Uint),
            "long" | "long int" => Ok(MangleType::Long),
            "unsigned long" | "unsigned long int" => Ok(MangleType::Ulong),
            "long long" | "long long int" => Ok(MangleType::LongLong),
            "unsigned long long" | "unsigned long long int" => Ok(MangleType::UlongLong),
            "float" => Ok(MangleType::Float),
            "double" => Ok(MangleType::Double),
            "long double" => Ok(MangleType::LongDouble),
            _ => {
                // Try parsing as template type like "std::allocator"
                if s.contains("::") || s.contains("<") {
                    // Simple heuristic: wrap in Template type
                    Ok(MangleType::Template {
                        name: s.replace("::", "_").replace("<", "").replace(">", ""),
                        args: vec![],
                    })
                } else {
                    // Regular class/type name
                    Ok(MangleType::Class(s.to_string()))
                }
            },
        }
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
    /// 返回 `Vec<CppDecl>`: `export template` 本身可能展开成多个声明.
    fn parse_export_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        self.expect_kw(CppKw::Export)?;
        // M26: `export template <...>` - export 修饰模板声明, 标记 is_export=true
        if self.at(CppKw::Template) {
            return self.parse_template_decl(true);
        }
        // M53: 声明级 export (`export int f();` / `export class S {};` /
        // `export constexpr int g = 1;`). [module.global.frag] 里
        // `export decl` 与 `export { decl }` 语义等价, 而 dcc 内部"导出"
        // 这个事实的唯一载体是 `CppDecl::ExportBlock` (modules 收集侧 /
        // sema / native / lower 均已递归下钻), 故按单条声明的块编码.
        // 旧实现 `return self.parse_decl()` 把 export 标记直接丢弃, 于是
        // `exported_interface_names` 看不见它 —— "接口里 export 声明 +
        // 私有片段里定义"的实体被误判成真私有而降为内部链接, 导入端
        // 重放出一个指向 STB_LOCAL 定义的 undefined reference.
        // `export import m;` 是重导出而非"被导出的实体", 保持透传.
        if self.at(CppKw::Import) {
            return Ok(vec![self.parse_import_decl()?]);
        }
        if !self.at(CppKw::Module)
            && !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }))
            )
        {
            let inner = self.parse_decl_list()?;
            return Ok(vec![CppDecl::ExportBlock(inner)]);
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
            return Ok(vec![CppDecl::Module {
                name,
                partition,
                is_export: true,
            }]);
        }
        // export { decls }
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut body = vec![];
        // 逐成员解析 + 错误恢复 (与顶层 `parse_top_level` 以及类体
        // `parse_member_decl_with_class` 同构): 块内一个成员解析失败**不得**
        // 把整块降级成一个 Opaque, 否则真实 libstdc++ 头 (`namespace std { ... }`
        // 上百个成员) 只要有一处暂不支持的构造就会把整块吞掉, 首错永远停在块头
        // (如 `std {`) 而无法定位到具体成员 —— 这与 Step 1 (L) 「惰性成员体 /
        // 两阶段查找」的时序前提配套: 先把失败局部化, 后续每项能力的落地才能
        // 各自推进, 不存在"一个缺口把整块拖倒"这种缺口。
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
            let member_start = self.capture_start();
            match self.parse_decl_list() {
                Ok(ds) => body.extend(ds),
                Err(e) => {
                    // 深度超限等硬错误不参与容错恢复 (见 parse_top_level 同类处理)
                    if e.is_fatal() {
                        return Err(e);
                    }
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                    let text = self.capture_src_text(member_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        body.push(CppDecl::Opaque(text));
                    }
                },
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
        Ok(vec![CppDecl::ExportBlock(body)])
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
            return Err(CppParseError::Unexpected("=", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        // 简化: 把 = 之后到 **顶层** `;` 之前的 token 收为 body_text (不解析表达式).
        // M51: 必须按括号深度找收尾分号 —— `concept C = requires(T a) { a + a; };`
        // 的分号在 `{}` 里还有一个, 早退会把 `} ;` 剩给顶层解析器,
        // 变成 Opaque 并在 native 后端报 "unsupported opaque declaration: ;".
        let mut body_text = String::new();
        let mut depth = 0i32;
        while let Some(t) = &self.peek {
            let kind = match t {
                CppTok::C(Tok { kind, .. }) => Some(*kind),
                _ => None,
            };
            match kind {
                Some(TokKind::LParen | TokKind::LBracket | TokKind::LBrace) => depth += 1,
                Some(TokKind::RParen | TokKind::RBracket | TokKind::RBrace) => depth -= 1,
                Some(TokKind::Semi) if depth <= 0 => break,
                _ => {},
            }
            body_text.push_str(&tok_text(t));
            body_text.push(' ');
            self.bump();
        }
        // 括号未闭合 (或多出闭合) 就撞上 EOF/顶层分号: 不是合法概念定义,
        // 交给上层容错 (parse_top_level 会 skip 到下一个声明起点)
        if depth != 0 {
            return Err(CppParseError::Unexpected(
                "balanced constraint expression",
                format!("depth={depth}"),
                self.span(),
            ));
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

    /// 解析模板 `requires` 子句 (M26), 返回约束表达式文本 (未命中 None).
    ///
    /// 约束文本必须**完整**, 因为实例化阶段要把它交给
    /// `feature_degrade::evaluate_trait_condition` 求值 (concept 过滤 / SFINAE).
    /// 旧实现有两个缺陷:
    ///   1. 顶层 `>` 先 break 后 push, 丢掉 `Integral<T>` 的收尾 `>`
    ///      → 约束文本残缺成 `Integral <T`, 概念永远匹配不上;
    ///   2. 结束判定只看 `>`, 无法处理 `requires sizeof(T) <= 8 T f()`
    ///      这类不以 `>` 收尾的约束;
    ///   3. `(` `)` `,` `sizeof` 等非 ident token 被直接丢弃, 文本不可求值.
    ///
    /// 现按约束表达式的语法边界判定: 维护 `expect_operand` (下一 token 是否
    /// 处于操作数位置), 在所有括号/模板尖括号闭合 (depth == 0) 且遇到
    ///   - `;` `{` `}`
    ///   - 声明符关键字 (int / void / const / static / ...) —— sizeof / noexcept /
    ///     decltype / alignof / typeid 例外, 它们本身是约束表达式的一部分
    ///   - `Ident Ident` / `Ident *` / `Ident &` / `Ident (` (返回类型 + 声明符起点)
    ///
    /// 时结束, 此前 token 已全部按原文计入文本.
    /// `<>` 仅在"刚吃完一个 primary"时视作模板实参表, 否则按比较运算符处理;
    /// `>>` 按需拆成两个 `>` (回退词法器一个字符), 保证 `C<A<B>>` 文本闭合.
    fn parse_requires_clause(&mut self) -> Option<String> {
        if !self.at(CppKw::Requires) {
            return None;
        }
        self.bump();
        let mut text = String::new();
        // () [] {} 与模板尖括号的总深度
        let mut depth = 0i32;
        // 下一个 token 是否处于操作数位置 (表达式开头 / 刚吃完运算符)
        let mut expect_operand = true;
        while let Some(tok) = self.peek.clone() {
            if depth == 0 && self.is_requires_boundary(&tok, expect_operand) {
                break;
            }
            let kind = match &tok {
                CppTok::C(Tok { kind, .. }) => Some(*kind),
                _ => None,
            };
            match kind {
                // 模板实参表起点: 只有刚吃完一个 primary 时 `<` 才是尖括号
                Some(TokKind::Lt) if !expect_operand => {
                    depth += 1;
                    self.bump();
                    text.push('<');
                },
                // 尖括号闭合: 把 `>>` 拆成两个 `>` (回退一个字符重新 lex)
                Some(TokKind::Gt | TokKind::Shr) if depth > 0 => {
                    depth -= 1;
                    let is_shr = matches!(kind, Some(TokKind::Shr));
                    self.bump();
                    text.push('>');
                    expect_operand = false;
                    if is_shr {
                        self.unget_one_char(CppTok::C(Tok {
                            kind: TokKind::Gt,
                            span: tok.span(),
                            text: ">".to_string(),
                        }));
                    }
                },
                // 顶层 `>`: 比较运算符 (如 `requires sizeof(T) > 4`)
                Some(TokKind::Gt | TokKind::Shr) => {
                    self.bump();
                    text.push_str(&tok_text(&tok));
                    text.push(' ');
                    expect_operand = true;
                },
                Some(TokKind::LParen | TokKind::LBracket | TokKind::LBrace) => {
                    depth += 1;
                    self.bump();
                    text.push_str(&tok_text(&tok));
                    expect_operand = true;
                },
                Some(TokKind::RParen | TokKind::RBracket | TokKind::RBrace) => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.bump();
                    text.push_str(&tok_text(&tok));
                    expect_operand = false;
                },
                _ => {
                    self.bump();
                    let s = tok_text(&tok);
                    let sep_needed = !(s.is_empty()
                        || s == "("
                        || s == "["
                        || s == "::"
                        || text.is_empty()
                        || text.ends_with(' ')
                        || text.ends_with('(')
                        || text.ends_with('[')
                        || text.ends_with(':'));
                    if sep_needed {
                        text.push(' ');
                    }
                    text.push_str(&s);
                    text.push(' ');
                    // 运算符/左值关键字后仍需操作数; 其余 (ident/字面量/`)`)
                    // 构成一个已完成的 primary.
                    expect_operand = matches!(
                        &tok,
                        CppTok::C(Tok {
                            kind: TokKind::AndAnd
                                | TokKind::OrOr
                                | TokKind::Not
                                | TokKind::Lt
                                | TokKind::Le
                                | TokKind::Ge
                                | TokKind::EqEq
                                | TokKind::Ne
                                | TokKind::Plus
                                | TokKind::Minus
                                | TokKind::Comma
                                | TokKind::Colon
                                | TokKind::Assign
                                | TokKind::Star
                                | TokKind::KwSizeof
                                | TokKind::KwAlignof
                                | TokKind::KwDecltype,
                            ..
                        }) | CppTok::Kw(CppKw::Noexcept, _)
                    );
                },
            }
        }
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// 回退一个字符: 把 `tok` 重新放回 `peek`, 词法器回退一格.
    /// 仅用于拆分 `>>` / `<<` 这类双字符 token (约束文本重建用).
    fn unget_one_char(&mut self, tok: CppTok) {
        if self.lex.pos > 0 {
            self.lex.pos -= 1;
            self.lex.col = self.lex.col.saturating_sub(1);
        }
        self.peek = Some(tok);
    }

    /// `requires` 约束表达式是否在此结束 (仅在顶层括号平衡时调用).
    fn is_requires_boundary(&self, tok: &CppTok, expect_operand: bool) -> bool {
        match tok {
            CppTok::C(Tok {
                kind: TokKind::Semi | TokKind::RBrace | TokKind::LBrace,
                ..
            }) => true,
            CppTok::C(Tok { kind, .. }) => {
                if is_decl_specifier_kind(*kind) {
                    return true;
                }
                // `T f(` / `T *p` / `T &r`: 约束已完整, 后面是声明的返回类型+名字
                if *kind == TokKind::Ident && !expect_operand {
                    return matches!(
                        self.peek_after_peek(),
                        Some(CppTok::C(Tok {
                            kind: TokKind::Ident | TokKind::Star | TokKind::Amp | TokKind::LParen,
                            ..
                        }))
                    );
                }
                false
            },
            CppTok::Kw(kw, _) => matches!(
                kw,
                CppKw::Class
                    | CppKw::Static
                    | CppKw::Const
                    | CppKw::Inline
                    | CppKw::Explicit
                    | CppKw::Mutable
                    | CppKw::Volatile
                    | CppKw::Friend
                    | CppKw::Using
                    | CppKw::Namespace
                    | CppKw::Template
                    | CppKw::Typename
                    | CppKw::Constexpr
                    | CppKw::Consteval
                    | CppKw::Constinit
                    | CppKw::Export
                    | CppKw::Virtual
                    | CppKw::Operator
                    | CppKw::New
                    | CppKw::Delete
                    | CppKw::Try
                    | CppKw::Catch
                    | CppKw::Throw
            ),
            CppTok::ColonColon(_) => false,
            CppTok::RawStringLit { .. } => false,
        }
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
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut members = vec![];
        // G22: `class` 体内默认 private, 访问说明符只影响其后的成员
        let mut cur_access = Access::Private;
        let mut member_access: Vec<(String, Access)> = vec![];
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
            if self.take_access_spec_or_skip(&mut cur_access) {
                continue;
            }
            // 记录成员解析前的位置, 失败时用于捕获 Opaque 原文
            let member_start = self.capture_start();
            match self.parse_member_decl_with_class(&name) {
                Ok(ds) => {
                    // 一条成员声明可能展开成多个成员 (`int a = 0, b = 1;`)
                    for d in ds {
                        if let Some(key) = member_access_key(&d) {
                            member_access.push((key, cur_access));
                        }
                        members.push(d);
                    }
                },
                Err(e) => {
                    // 深度超限等硬错误不参与容错恢复 (见 parse_top_level 同类处理)
                    if e.is_fatal() {
                        return Err(e);
                    }
                    // M30.2: 记录错误但继续解析, 跳过无法解析的成员到下一个
                    // `;` 或 `}` (与 parse_top_level 容错一致).
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                    // 捕获跳过区域的原文, 保存为 Opaque 成员
                    let text = self.capture_src_text(member_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        members.push(CppDecl::Opaque(text));
                    }
                },
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
            member_access,
        }))
    }

    /// 解析 struct/union 聚合声明 (M30.2: is_union 区分 union 关键字)
    fn parse_aggr_decl(&mut self, is_union: bool) -> Result<CppDecl, CppParseError> {
        self.parse_aggr_decl_impl(is_union, true, None).map(|(d, _)| d)
    }

    /// 聚合体定义的核心解析。返回 `(声明, 源码里的 tag 名)`。
    ///
    /// * `consume_inst` —— 是否在 `}` 之后吃掉紧跟的实例名 (`union {... } temp;`)。
    ///   `typedef struct {...} Name;` 里那个位置是 **typedef 名**而不是变量名, 必须
    ///   留给调用方按 init-declarator 列表解析。
    /// * `anon_fallback_name` —— 匿名定义 (`struct {...}`) 该用什么名字。C++ 给匿名
    ///   聚合体分配占位名, 而 typedef 场合下它的名字**就是** typedef 名
    ///   ([dcl.type.anon]/1), 由调用方预先扫出后递进来, 这样成员函数体里的类上下文
    ///   (`ctx_name`) 与最终类名一致。
    fn parse_aggr_decl_impl(
        &mut self,
        is_union: bool,
        consume_inst: bool,
        anon_fallback_name: Option<String>,
    ) -> Result<(CppDecl, Option<String>), CppParseError> {
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
                return Ok((
                    CppDecl::Typedef {
                        target: MangleType::Class(n.clone()),
                        name: n.clone(),
                    },
                    Some(n.clone()),
                ));
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
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        // M30.4: 匿名聚合体分配唯一名 (`__anon_aggr_1`, `__anon_aggr_2`, ...)
        // 避免多个匿名 union/struct 共享同名导致构造函数/typeinfo 重复定义.
        let anon_name =
            name.clone().unwrap_or_else(|| match anon_fallback_name {
                Some(given) => given,
                None => {
                    self.anon_counter += 1;
                    format!("__anon_aggr_{}", self.anon_counter)
                },
            });
        let ctx_name = anon_name.clone();
        let mut members = vec![];
        // G22: `struct`/`union` 体内默认 public
        let mut cur_access = Access::Public;
        let mut member_access: Vec<(String, Access)> = vec![];
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
            if self.take_access_spec_or_skip(&mut cur_access) {
                continue;
            }
            // 记录成员解析前的位置, 失败时用于捕获 Opaque 原文
            let member_start = self.capture_start();
            match self.parse_member_decl_with_class(&ctx_name) {
                Ok(ds) => {
                    // 一条成员声明可能展开成多个成员 (`int a = 0, b = 1;`)
                    for d in ds {
                        if let Some(key) = member_access_key(&d) {
                            member_access.push((key, cur_access));
                        }
                        members.push(d);
                    }
                },
                Err(e) => {
                    self.diags.push(e.to_diag(&self.file));
                    self.skip_to_member_end();
                    // 捕获跳过区域的原文, 保存为 Opaque 成员
                    let text = self.capture_src_text(member_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        members.push(CppDecl::Opaque(text));
                    }
                },
            }
        }
        self.bump();
        // M30.2: 定义后实例名 (`union { ... } temp;` / `struct X {} x;`)
        let inst = if consume_inst
            && matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            )
        {
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
            (None, Some(inst)) => Ok((
                CppDecl::AnonAggr { is_union, members, name: inst },
                None,
            )),
            // 带 tag 定义 (变量实例罕见, 忽略实例名)
            (Some(n), _) => Ok((
                CppDecl::Class(ClassDecl {
                    name: n.clone(),
                    is_struct: true,
                    is_union,
                    bases,
                    members,
                    member_access,
                }),
                Some(n),
            )),
            // M30.4: 匿名纯定义: 使用唯一占位名 (避免同名构造函数冲突)
            (None, None) => Ok((
                CppDecl::Class(ClassDecl {
                    name: anon_name,
                    is_struct: true,
                    is_union,
                    bases,
                    members,
                    member_access,
                }),
                None,
            )),
        }
    }

    /// 带类上下文的成员解析: 用于类内 inline 成员函数
    /// 返回展开后的**全部**成员 (一条多声明符的成员变量声明 -> N 个成员).
    fn parse_member_decl_with_class(&mut self, class: &str) -> Result<Vec<CppDecl>, CppParseError> {
        let saved = self.peek.clone();
        let r = self.parse_member_decl()?;
        // 注入类名到 MemberFn / Ctor / Dtor / OperatorFn
        Ok(r.into_iter()
            .map(|d| inject_class_name(d, class, &saved))
            .collect())
    }

    /// G22: 类体循环里的访问说明符处理.
    /// 命中 `public:` / `private:` / `protected:` 时更新 `cur` 并返回 true
    /// (调用方应 `continue`); 其它情况不消耗 token 并返回 false.
    /// 缺 `:` 的畸形输入不会被当作访问说明符 (原样交给成员解析报错).
    fn take_access_spec_or_skip(&mut self, cur: &mut Access) -> bool {
        // 必须 `access-spec` 紧跟 `:` 才算访问说明符; 否则 (如 `public` 后接
        // 别的东西的畸形输入) 原样交给成员解析, 由 parse_member_decl 报错.
        if !matches!(&self.peek, Some(t) if t.is_access_spec()) {
            return false;
        }
        if !matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            return false;
        }
        match &self.peek {
            Some(CppTok::Kw(CppKw::Public, _)) => *cur = Access::Public,
            Some(CppTok::Kw(CppKw::Protected, _)) => *cur = Access::Protected,
            Some(CppTok::Kw(CppKw::Private, _)) => *cur = Access::Private,
            _ => return false,
        }
        self.bump(); // 关键字
        self.bump(); // ':'
        true
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
            // 基类名允许限定名 (`: ns::Base`, `: A::B::C`) —— [class.derived]/2
            // 的 elaborated-type-specifier 本身就是限定-id。
            let name = self.expect_qualifiable_ident()?;
            // 解析模板实参: Base<T, U>
            let template_args = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt(true)
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

    /// 解析类内**一条**成员声明, 返回它展开出的全部成员.
    ///
    /// 文法里只有成员变量声明这一条形带头带 init-declarator 列表
    /// (`int a = 0, b = 1;` 展成 N 个 `CppDecl::MemberVar`); 其余成员形态
    /// (函数 / ctor / dtor / typedef / 嵌套类 ...) 恒为单元素.
    fn parse_member_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
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
        // M54: 类内 `static_assert (...)` ([assert]): 与顶层同一份解析器,
        // 否则它在类体里也会被当成成员变量/成员函数起点而失败.
        if self.at_static_assert_decl() {
            return self.parse_static_assert_decl();
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
                return Ok(vec![self.parse_enum_decl()?]);
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
                return Ok(vec![self.parse_aggr_decl(is_u)?]);
            }
            // 非 defines: `struct/union Tag ...` 是 **elaborated-type-specifier**
            // 类型的成员声明。不能像以前那样直接 `self.bump()` 吃掉关键字 ——
            // 那会把 tag key 丢失, 下游 convert_type 只看到裸 `Class("Tag")`,
            // 无法区分「首次引入的不完整 tag」(B4: `struct __locale_data
            // *__locales[13];`) 与「未声明的名字」。保留关键字让 parse_mangle_type
            // 把它编码成 `Class("struct Tag")` (与全局/形参/局部位置一致), 再据
            // 类型后的 token 判定变量 / 成员函数 / 函数指针。
            let t2 = self.peek_token_at(2);
            let t3 = self.peek_token_at(3);
            let at_lparen = |t: &Option<CppTok>| {
                matches!(t, Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                })))
            };
            let at_ident = |t: &Option<CppTok>| {
                matches!(t, Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })))
            };
            if at_lparen(&t2) {
                // `struct Tag (*fp)(...)` 函数指针成员
                return Ok(vec![self.parse_member_fnptr()?]);
            }
            if at_ident(&t2) && at_lparen(&t3) {
                // `struct Tag f(...)` 返回结构体的成员函数
                return Ok(vec![self.parse_member_fn_inline()?]);
            }
            // 其余 (`struct Tag *p;` / `struct Tag x;` / `struct Tag arr[N];`) 按成员变量
            return self.parse_member_var();
        }
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Tilde,
                ..
            }))
        ) {
            return Ok(vec![self.parse_dtor()?]);
        }
        // 运算符重载
        if self.is_operator_fn_start() {
            return Ok(vec![self.parse_operator_fn()?]);
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
            return Ok(vec![self.parse_ctor()?]);
        }
        // M17.0: `friend` 是自由函数/类声明标记, 跳过然后递归
        if self.at(CppKw::Friend) {
            self.bump();
            return Ok(vec![self.parse_friend_decl()?]);
        }
        // M17.0: `using Base::Base;` 继承构造 - 类内合法
        if self.at(CppKw::Using) {
            return Ok(vec![self.parse_using_decl()?]);
        }
        // M17.0: `volatile` 单独出现时可能是 volatile 成员函数限定,
        // 但通常与 const 连用 (const volatile); 这里当 cv 限定跳过
        if self.at(CppKw::Volatile) {
            self.bump();
            return self.parse_member_decl();
        }
        // [class.static]/[class.static.data]: `static` 是声明说明符, 必须进入 AST。
        // 过去它在类体里被直接吐掉: `static int f()` 变成普通成员函数 (调用点被迫
        // 凑一个 `this`), `static const int N = 7` 则整条降级成 Opaque —— traits 的
        // `integral_constant::value` 因此根本不存在。这里先记下方位, 解析完剩下的
        // 声明后把静态性回到产物上 (成员函数 / 数据成员两条路径共用一个入口)。
        if self.at(CppKw::Static) {
            self.bump();
            let mut ds = self.parse_member_decl()?;
            for d in &mut ds {
                match d {
                    CppDecl::MemberFn { is_static, .. } | CppDecl::MemberVar { is_static, .. } => {
                        *is_static = true
                    },
                    _ => {},
                }
            }
            return Ok(ds);
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
                return Ok(vec![self.parse_dtor()?]);
            }
            return Ok(vec![self.parse_member_fn_inline()?]);
        }
        // M30.2: 通用函数指针成员检测 `RetType (*name)(params);`
        // 支持关键字返回类型 (`bool (*f)(...)`, `int (*f)(...)`,
        // `void (*f)(...)`) 和多 token 返回类型 (`const char *(*f)(...)`,
        // `cpp_hashnode *(*f)(...)`).  扫描 `(*` 模式, 在 `;` 前找到
        // 则判定为函数指针成员.
        //
        // 扫描必须限于**当前声明**: 之后成员的函数指针形参
        // (`O() {} int m(int (*f)(int)) { ... }`) 里的 `(*` 不能把前面的 ctor
        // 或普通成员函数劫持成函数指针成员, 否则整条声明连同其后所有成员
        // 一起降级成 Opaque. 另外只有声明的第一个 `(` 紧跟 `*` 才是
        // `Ret (*name)(...)` 形态.
        {
            let mut k = 0;
            loop {
                match self.peek_token_at(k) {
                    Some(CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    }))
                    | None => break,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBrace | TokKind::RBrace,
                        ..
                    })) => break,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    })) => {
                        if matches!(
                            self.peek_token_at(k + 1),
                            Some(CppTok::C(Tok {
                                kind: TokKind::Star,
                                ..
                            }))
                        ) {
                            return Ok(vec![self.parse_member_fnptr()?]);
                        }
                        break;
                    },
                    _ => {},
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
                        return Ok(vec![self.parse_member_fnptr()?]);
                    }
                    return Ok(vec![self.parse_ctor()?]);
                },
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
                            return Ok(vec![self.parse_member_fn_inline()?]);
                        },
                        _ => return self.parse_member_var(),
                    }
                },
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
                            return Ok(vec![self.parse_member_fn_inline()?]);
                        }
                    }
                    return self.parse_member_var();
                },
                Some(CppTok::Kw(_, _)) => {
                    // 第二 token 是关键字: 可能是 `class operator+` (operator fn)
                    if self.is_operator_fn_start() {
                        return Ok(vec![self.parse_operator_fn()?]);
                    }
                    return Err(CppParseError::Unsupported("class member", self.span()));
                },
                Some(CppTok::ColonColon(_)) => {
                    // M52: 限定名开头的成员声明 (`std::suspend_always m() { ... }`
                    // / `Inner::value_type v_;`) —— 详见 parse_member_decl_qualified.
                    return self.parse_member_decl_qualified();
                },
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
                        return Ok(vec![self.parse_operator_fn()?]);
                    }
                    // M18.0: `T& m_;` 类内引用成员变量
                    if let Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        text,
                        ..
                    })) = &self.peek
                    {
                        // `T& m(args) ...` 是**返回引用的成员函数** (链式 `operator+=`
                        // / setter 的常见形态), 只有 `T& m;` / `T& m = ...` 才是
                        // 引用数据成员; 之前无条件当数据成员, 于是函数本体与
                        // 其后所有成员一起被降级成 Opaque (sema 报 member not found).
                        if matches!(
                            self.peek_token_at(1),
                            Some(CppTok::C(Tok {
                                kind: TokKind::LParen,
                                ..
                            }))
                        ) {
                            let ret = MangleType::Reference(Box::new(MangleType::Class(ty_text)));
                            return Ok(vec![self.parse_member_fn_after_ret(ret, false)?]);
                        }
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
                        return Ok(vec![CppDecl::MemberVar {
                            ty: MangleType::Reference(Box::new(MangleType::Class(ty_text))),
                            name,
                            is_mutable: false,
                            is_volatile: false,
                            is_const: false,
                            is_static: false,
                            init: None,
                        }]);
                    }
                    return Err(CppParseError::Unsupported("T& member", self.span()));
                },
                _ => return Err(CppParseError::Unsupported("class member", self.span())),
            }
        }
        // M52: 前导 `::` 的限定名成员声明 (`::std::true_type is_ok() { ... }`)
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            return self.parse_member_decl_qualified();
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
            Some(CppTok::Kw(CppKw::Const | CppKw::Volatile | CppKw::Static, _))
        );
        if is_type_kw_or_qual {
            // M11 stdlib: 如果是 `Type operator ...` 模式, 走 parse_operator_fn
            if self.is_operator_fn_start() {
                return Ok(vec![self.parse_operator_fn()?]);
            }
            // M18.0: 跳过 const/volatile/static 限定后再判定 fn/var
            // 否则 `static dccpp_time_point now()` 会被错判为 var.
            // 流程: 跳限定 -> 跳 typedef 类型 ident (后跟 ident/* /&)
            //      -> 跳 * 和 & -> 看 name_pos+1 是否是 `(`.
            let mut j = 1;
            // 跳过 const/volatile/static 限定
            while matches!(
                self.peek_token_at(j),
                Some(CppTok::Kw(CppKw::Const | CppKw::Volatile | CppKw::Static, _))
            ) {
                j += 1;
            }
            // 跳过额外的类型关键字 (例: `void*`, `long long`, `unsigned char` 等)
            // 或 typedef 标识符 (例: `FILE*`, `size_t f`, `MyType&`,
            // `dccpp_time_point now()`)
            while let Some(tok) = self.peek_token_at(j) {
                if let CppTok::Kw(k, _) = &tok {
                    if matches!(k, CppKw::Inline | CppKw::Static | CppKw::Const | CppKw::Volatile) {
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
                    Some(CppTok::Kw(CppKw::Const | CppKw::Volatile | CppKw::Static, _))
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
            if is_fn {
                return Ok(vec![self.parse_member_fn_inline()?]);
            }
            if is_fnptr {
                return Ok(vec![self.parse_member_fnptr()?]);
            }
            // 成员变量: 一条声明可能携多个 init-declarator
            return self.parse_member_var();
        }
        Err(CppParseError::Unsupported("class member", self.span()))
    }

    /// M52: 类内**限定名类型**成员声明的统一分发入口.
    ///
    /// 根因: `parse_member_decl` 的 ident 起始分支只枚举了第二 token 为
    /// `(` / ident / `*` / `&` / 关键字 五种形态, `Ident ::` (限定名) 直接落进
    /// `Err(Unsupported("class member"))` —— 顶层容错于是把**整条**成员声明降级
    /// 成 `CppDecl::Opaque` (sema 表现: "member 'm' not found"), 协程 stdlib 头
    /// 当时只能改写成全局 typedef 绕过.
    ///
    /// 修法: 用类型解析器自身 (`parse_mangle_type` 已支持 `a::b::c` / `<T>` /
    /// `*` / `&` / const 限定) 作为语法单一真源吞掉返回类型, 只据其**后续
    /// token** 判定函数 / 函数指针 / 变量, 然后回滚状态交由既有分发器从声明
    /// 起点重解 (结果确定, 代价可忽), 不新增任何静默降级分支.
    fn parse_member_decl_qualified(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let saved_lex = self.lex.clone();
        let saved_peek = self.peek.clone();
        let saved_deferred_gt = self.deferred_gt;
        let saved_nttp_angle_stop = self.nttp_angle_stop;
        let rollback = |s: &mut Self| {
            s.lex = saved_lex.clone();
            s.peek = saved_peek.clone();
            s.deferred_gt = saved_deferred_gt;
            s.nttp_angle_stop = saved_nttp_angle_stop;
        };
        // 先窥探类型是否能被解析; 解不了则保留原有错误路径 (不新增降级).
        if self.parse_mangle_type().is_err() {
            rollback(self);
            return Err(CppParseError::Unsupported("class member", self.span()));
        }
        let tok1 = self.peek.clone();
        let is_operator = matches!(&tok1, Some(CppTok::Kw(CppKw::Operator, _)));
        let is_fn = (matches!(
            &tok1,
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                ..
            }))
        ) && matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        )) || is_operator;
        let is_fnptr = matches!(
            &tok1,
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
        );
        rollback(self);
        if is_operator {
            return Ok(vec![self.parse_operator_fn()?]);
        }
        if is_fn {
            return Ok(vec![self.parse_member_fn_inline()?]);
        }
        if is_fnptr {
            return Ok(vec![self.parse_member_fnptr()?]);
        }
        // 变量分支: 一条声明可能带多个 init-declarator
        self.parse_member_var()
    }

    /// 判断当前位置是否是链接规范 `extern "C" ...` 的起始。
    ///
    /// 只看到 `extern` + 字符串字面量为止: 后面跟 `{` 是**块形态**
    /// (`extern "C" { ... }`), 否则是**单声明形态** (`extern "C" int f();`)。
    /// 两种形态必须一起识别, 否则单声明形态会在 `extern` 之后撞上字符串
    /// 字面量而解析失败, 整条声明被降级成 `CppDecl::Opaque` —— native 后端
    /// 随即报 "unsupported opaque declaration", 整个 TU 退回 legacy 路径。
    fn at_linkage_spec(&self) -> bool {
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwExtern,
                ..
            }))
        ) {
            return false;
        }
        // 第二个 token 必须是字符串字面量 (链接规范名, 如 "C" / "C++")
        matches!(
            self.peek_token_at(1),
            Some(CppTok::C(Tok {
                kind: TokKind::StringLit,
                ..
            }))
        )
    }

    /// 解析链接规范: `extern "C" linkage-body` 的两种形态。
    ///
    /// 块形态 ([link.spec]/3 的第二形式) 透传原文为 [`CppDecl::RawExternC`],
    /// 因为 C 头语法 (`struct xxx;` 等) 不一定被 dcc++ 完整支持;
    /// 单声明形态则剥掉 `extern "字符串"` 前缀后按同一条声明继续解析。
    fn parse_linkage_spec(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        // 记下 `extern` 关键字前的位置, 块形态截取原文时直接拿整段
        let full_start = self.lex.pos - "extern".len();
        self.bump(); // consume 'extern'
        // [link.spec]/2: 一个 linkage-specification 可以带多个链接字符串
        // (`extern "C" "ISO"`), 逐个吃掉。
        loop {
            if let Some(CppTok::C(Tok {
                kind: TokKind::StringLit,
                ..
            })) = &self.peek
            {
                self.bump();
            } else {
                break;
            }
        }
        // [dcl.attr]: linkage-spec 与内部声明/块之间可以插入属性序列 ——
        // glibc++ 里大量出现 `extern "C++" __attribute__((__noreturn__,
        // __always_inline__)) inline void __terminate() noexcept { ... }`
        // (`<exception>`/`<bits/exception.h>`) 与 `extern "C" __attribute__
        // ((...))` 形态。不容忍则整条声明被块容错降级 Opaque, libcpp 卡住。
        self.skip_attribute_specifier_seq();
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            // 单声明形态: `extern "C" int f();` / `extern "C" void g() { ... }`
            return self.parse_decl_list_carrying_spec();
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
                },
                CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                }) => {
                    self.bump();
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                },
                _ => {
                    self.bump();
                },
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
        Ok(CppDecl::RawExternC(full_text)).map(|d| vec![d])
    }

    /// 解析 typedef <decl-specifier> <init-declarator 列表>; (M11 stdlib)
    ///
    /// typedef 的声明同样遵守「一条声明 = decl-specifier + init-declarator 列表」
    /// 的文法, 故按声明符展开成 N 个 [`CppDecl::Typedef`] 返回。
    fn parse_typedef_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        // 跳过 `typedef`
        self.bump();
        // M30.2: `typedef __typeof__(expr) Name;` (glibc nullptr_t) -
        // depth 跳过 __typeof__ 的括号表达式, 与 decltype 同等处理.
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
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
                            },
                            CppTok::C(Tok {
                                kind: TokKind::RParen,
                                ..
                            }) => {
                                self.bump();
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            },
                            _ => {
                                self.bump();
                            },
                        }
                    }
                }
                // M30.2: __typeof__(expr) 即目标类型本身; 跳过后的下一
                // 个 ident 是 typedef 名 (glibc nullptr_t)。逗号之后还可以跟更多
                // declarator (`typedef __typeof__(x) A, *B;`) —— 同一条声明的
                // init-declarator 列表, 逐个展开成各自的别名。
                let typeof_spec = DeclSpec {
                    ty: MangleType::Int,
                    is_static: false,
                    is_volatile: false,
                    is_const: false,
                };
                let mut typeof_decls: Vec<CppDecl> = Vec::new();
                loop {
                    let layers = self.parse_declarator_ptr_layers()?;
                    let name = self.expect_ident()?;
                    // 类型名登记 (`sizeof(T)` / `typeid(T)` 消歧) 以 AST 为单一
                    // 真源: 逗号后的每个 declarator 名都是类型名。
                    self.register_type_name(&name);
                    let d = InitDeclarator {
                        layers,
                        array_dims: Vec::new(),
                        name,
                        init: None,
                    };
                    typeof_decls.push(CppDecl::Typedef {
                        target: d.full_ty(&typeof_spec),
                        name: d.name,
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
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Semi,
                        ..
                    }))
                ) {
                    self.bump();
                }
                return Ok(typeof_decls);
            }
        }
        // `typedef <struct|union|enum> [tag] { ... } Name;` 定义形态必须按**定义**
        // 处理 (见 [`Self::parse_typedef_aggr_decl`])。
        if self.at_aggr_definition() {
            return self.parse_typedef_aggr_decl();
        }
        // M30: 解析目标类型 (parse_mangle_type 覆盖 builtin/类名/
        // 模板实例/限定名/指针). 失败时回退 lex 状态并退化为旧跳过
        // 逻辑: 其余无法简单解析的 typedef 仍走 no-op 路径 (target 置 Int)。
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
                                },
                                CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                }) => {
                                    self.bump();
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                },
                                _ => {
                                    self.bump();
                                },
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
                            },
                            CppTok::C(Tok {
                                kind: TokKind::RBrace,
                                ..
                            }) => {
                                self.bump();
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            },
                            _ => {
                                self.bump();
                            },
                        }
                    }
                }
                t
            },
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
                                },
                                Some(CppTok::C(Tok {
                                    kind: TokKind::RBrace,
                                    ..
                                })) => {
                                    depth -= 1;
                                    self.bump();
                                    if depth == 0 {
                                        break;
                                    }
                                },
                                None => break,
                                _ => {
                                    self.bump();
                                },
                            }
                        }
                    }
                }
                // b: decltype ( ... ).
                if let Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) = &self.peek
                {
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
                                _ => {},
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
                        },
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
                        },
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
                                    _ => {},
                                }
                            }
                            if fp_name.is_some() {
                                break;
                            }
                        },
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
                                _ => {},
                            }
                        }
                    }
                }
                MangleType::Int
            },
        };
        // 一条 typedef = decl-specifier (`typedef` + 类型) + **init-declarator
        // 列表** ([dcl.typedef] / [dcl.fct]): `typedef int A, *B, C[2];` 声明 3 个
        // 别名, 其中 `B` 是 `int*`、`C` 是 `int[2]` —— 指针 / 数组层级属于各自的
        // declarator, 不随逗号共享 (`A` 才是 `int`)。首个 declarator 名前的 `*`/`&`
        // 已被类型解析器折进 `target`, 先剥回来交给它自己。
        // 函数指针形态 (`Ret (*name)(args)`) 的 `*` 在括号里、参数组已按深度整体
        // 消费, 不参与这次层级拆分。
        let (base_ty, first_layers) = if fp_name.is_some() {
            (target, Vec::new())
        } else {
            Self::split_declarator_layers(target)
        };
        let spec = DeclSpec {
            ty: base_ty,
            is_static: false,
            is_volatile: false,
            is_const: false,
        };
        let mut declarators: Vec<InitDeclarator> = Vec::new();
        let mut pending_layers = first_layers;
        loop {
            let mut layers = std::mem::take(&mut pending_layers);
            // 模板实例类型 `Name<Args> alias` 已由 parse_mangle_type 解析为
            // Template; 名字就是下一个 ident (函数指针形态则已在括号内取过)。
            let name = match fp_name.take() {
                Some(n) => n,
                None => {
                    layers.extend(self.parse_declarator_ptr_layers()?);
                    self.expect_ident()?
                },
            };
            // 类型名登记 (`sizeof(T)` / `typeid(T)` 消歧) 以 AST 为单一真源:
            // 源文本惰性扫描那份只把 `;` 前**最后一个** Ident 当 typedef 名,
            // `typedef int C, *P;` 里的 `C` 会漏掉 —— 逗号前的每个 declarator
            // 名都得在这里登记。
            self.register_type_name(&name);
            // 数组 typedef `typedef int int2[2];` —— 名后的 `[N]` 属于本 declarator.
            let mut array_dims: Vec<usize> = Vec::new();
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }))
            ) {
                self.bump(); // '['
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }))
                ) {
                    self.bump(); // ']' —— 无边界数组, 不建模
                    continue;
                }
                let size_expr = self.parse_expr()?;
                match self.fold_int_const_expr(&size_expr) {
                    Some(v) if v > 0 => array_dims.push(v as usize),
                    // 依赖型/未知维度: 已消费到下标末尾, 退化为不记录
                    _ => {},
                }
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }))
                ) {
                    self.bump(); // ']'
                }
            }
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
                        },
                        CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        },
                        _ => {
                            self.bump();
                        },
                    }
                }
            }
            // M30.2: typedef 尾部 GNU 尾缀 (`typedef int register_t
            // __attribute__ ((__mode__ (__word__)));`)
            self.skip_gnu_suffixes();
            declarators.push(InitDeclarator {
                layers,
                array_dims,
                name,
                init: None,
            });
            // `,` 之后还有下一个 declarator
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
        let mut out: Vec<CppDecl> = Vec::with_capacity(declarators.len());
        for d in &declarators {
            let ty = d.full_ty(&spec);
            // 记录别名映射, 表达式 `alias::member` 可展开为 TplStaticAccess
            // (触发实例化并引用带实例前缀的常量).
            if let MangleType::Template { name: tname, args } = &ty {
                self.typedef_alias
                    .insert(d.name.clone(), (tname.clone(), args.clone()));
            }
            out.push(CppDecl::Typedef {
                target: ty,
                name: d.name.clone(),
            });
        }
        Ok(out)
    }

    /// 当前位置是否是聚合体/枚举**定义** (`struct|union|enum [tag] { ...`)。
    /// 只有带 `{` 的才是定义; `struct X;` / `union U v;` 是前向声明或变量声明。
    fn at_aggr_definition(&self) -> bool {
        let tag_kw = matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum,
                ..
            }))
        );
        if !tag_kw {
            return false;
        }
        match self.peek_token_at(1) {
            // `struct { ...` / `enum { ...`
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            })) => true,
            // `struct Tag { ...` / `enum class Tag { ...`
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
        }
    }

    /// 只读扫描: 从当前位置 (`struct|union|enum ...`) 找到配对的 `}`, 返回其后
    /// 第一个标识符 (即 typedef 名), 然后回卷词法状态。
    ///
    /// 需要先知道这个名字, 是因为**匿名**聚合体的名字就是 typedef 名, 而成员解析
    /// 要拿它当类上下文 (`ctx_name`) —— 在成员之后才知道名字就晚了。
    fn scan_name_after_aggr_body(&mut self) -> Option<String> {
        let saved_lex = self.lex.clone();
        let saved_peek = self.peek.clone();
        let mut out: Option<String> = None;
        // 走到 `{`
        loop {
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }))
            ) {
                break;
            }
            if self.peek.is_none() {
                self.lex = saved_lex;
                self.peek = saved_peek;
                return None;
            }
            self.bump();
        }
        // 按深度吃到配对的 `}` (成员体内可能有嵌套 `{}`)
        let mut depth = 0i32;
        loop {
            match self.peek.clone() {
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                })) => {
                    depth += 1;
                    self.bump();
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::RBrace,
                    ..
                })) => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                },
                Some(_) => {
                    self.bump();
                },
                None => break,
            }
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            out = Some(text.clone());
        }
        self.lex = saved_lex;
        self.peek = saved_peek;
        out
    }

    /// `typedef <struct|union|enum> [tag] { ... } Name [, Decl2...];`
    ///
    /// [dcl.typedef]/1 不 [dcl.type.anon]/1: 匿名聚合体的**名字由 typedef 赋予**。
    /// 此前这条写法走的是「跳过成员体 + 只记一个 `Typedef{target: Int}` 占位」的
    /// 退化路径, 后果是:
    ///  - `typedef struct { int __val[2]; } _G_fpos_t;` (glibc `<stdio.h>`) 的字段
    ///    整体丢失, 使用点报 `unknown class type: _G_fpos_t` —— GCC 15.3.0
    ///    `libcpp/*.cc` 全部 16 个翻译单元因此无一能走 native 后端;
    ///  - `typedef enum { A, B } Kind;` 的枚举量不再存在, 使用点报
    ///    `undeclared identifier: A`;
    ///  - `typedef struct Tag { ... } Name;` 连 `Tag` 自身都没登记, 报
    ///    `unknown class type: Tag`。
    /// 这些写法在 C/C++ 头文件里无处不在, 所以必须按定义建成真正的类/枚举声明,
    /// 再叠加 typedef 别名 (多个声明符逐个展开)。
    fn parse_typedef_aggr_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let is_union = matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwUnion,
                ..
            }))
        );
        let is_enum = matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwEnum,
                ..
            }))
        );
        let first_name = self.scan_name_after_aggr_body();
        let (aggr, tag) = if is_enum {
            let mut d = self.parse_enum_decl()?;
            // parse_enum_decl 对匿名枚举用占位名 `_anon`; typedef 场合下把它换成
            // 真正赋予的名字, 使 `Kind::A` 这种限定访问与类型名查找都能命中。
            let t = match &mut d {
                CppDecl::Enum { name, .. } => {
                    if name == "_anon" {
                        if let Some(n) = &first_name {
                            *name = n.clone();
                        }
                        None
                    } else {
                        Some(name.clone())
                    }
                },
                _ => None,
            };
            (d, t)
        } else {
            // 匿名时把 typedef 名作为类名注入 (`anon_fallback_name`), 带 tag 那么
            // `parse_aggr_decl_impl` 会用 tag, fallback 自然被忽略。
            self.parse_aggr_decl_impl(is_union, false, first_name.clone())?
        };
        // 聚合体自身的类型: 有 tag 用 tag (别名指向 tag), 无 tag 用刚赋予的名字。
        let class_name = tag.or(first_name).unwrap_or_default();
        let spec = DeclSpec {
            ty: MangleType::Class(class_name.clone()),
            is_static: false,
            is_volatile: false,
            is_const: false,
        };
        let mut out = vec![aggr];
        // `} A, *B, C[4];` —— 同一条 typedef 的 init-declarator 列表, 逐个展开。
        loop {
            let layers = self.parse_declarator_ptr_layers()?;
            let name = self.expect_ident()?;
            self.register_type_name(&name);
            let d = InitDeclarator {
                layers,
                array_dims: Vec::new(),
                name,
                init: None,
            };
            out.push(CppDecl::Typedef {
                target: d.full_ty(&spec),
                name: d.name.clone(),
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
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(out)
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
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
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
                },
                _ => {},
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
                },
                CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                }) if cur_name.is_none() && depth == 1 => {
                    cur_name = Some(text.clone());
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }) if cur_name.is_some() && depth == 1 => {
                    cur_init = Some(Vec::new());
                    self.bump();
                },
                _ => {
                    if let Some(init) = cur_init.as_mut() {
                        init.push(self.token_text(&t));
                    }
                    self.bump();
                },
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
        // 登记枚举量的编译期值, 供 `int t[MAX_KINDS];` 这类常量维度折叠.
        // 无显式初值时递增前一个; 显式初值不是字面量时先进入常量折叠通道
        // (`square(3)` / 已登记常量), 仍未知则进入未知状态,
        // 后续的无初值枚举量也不再登记 (不猜值).
        let mut next: Option<i128> = Some(0);
        for (vname, init) in variants.iter_mut() {
            let v = match init {
                Some(text) => match Self::parse_int_literal_text(text) {
                    Some(x) => Some(x),
                    None => self
                        .fold_enum_init_text(text)
                        // 把初值文本改写成折叠后的字面量: 下游 (sema / legacy
                        // Lower) 只认整数字面量, 不改写会让枚举量在**运行期**
                        // 也是错值.
                        .inspect(|x| {
                            *text = x.to_string();
                        }),
                },
                None => next,
            };
            match v {
                Some(x) => {
                    self.record_int_const(vname, x);
                    // `enum class Color { A }` 的限定名 `Color::A` 也记下
                    if !name.is_empty() {
                        self.record_int_const(&format!("{name}::{vname}"), x);
                    }
                    next = x.checked_add(1);
                },
                None => next = None,
            }
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
            Some(CppTok::Kw(CppKw::Inline | CppKw::Static | CppKw::Virtual | CppKw::Explicit, _))
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
                // M52: 限定名返回类型 + operator (`std::string operator+(...)`).
                // 形式 2 (`Class::operator+`, 返回类型被略) 的特征是 `::` 之后
                // **直接**是 operator; 只要 `::` 后面还有名字, 首段就是命名空间
                // 限定, 整串是返回类型, 必须解析 (否则 expect_kw(operator) 撞在
                // 类型末段上, 声明降级 Opaque).
                else if matches!(self.peek_token_at(1), Some(CppTok::ColonColon(_)))
                    && !matches!(self.peek_token_at(2), Some(CppTok::Kw(CppKw::Operator, _)))
                {
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
            },
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
        // M18.0 / M30.6: `= 0` / `= delete` / `= default` (deleted / pure virtual /
        // defaulted operator) —— 词法判定与其它三条函数路径共用同一入口。
        let mut tail = SpecKinds::default();
        if self.take_deleted_or_pure_specifier(&mut tail) {
            self.consume_decl_semicolon();
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
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
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
            // G12: 参数包形参 `Args... args` / `Types&&... ts` —— 类型后的
            // `...` 是**包声明** (不是 C 风格变参, 那个是独立的 `...` token,
            // 已由循环开头的分支处理). 记成 1 元素 `VariadicPack`, 单态化时
            // 按推导出的实参个数摊平成 N 个普通形参.
            let mut is_param_pack = false;
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::AndAnd,
                    ..
                }))
            ) {
                // `Args&&... args` 万能引用形参: `&&` 属于元素类型
                self.bump();
                ty = MangleType::Reference(Box::new(ty));
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ellipsis,
                    ..
                }))
            ) {
                self.bump();
                is_param_pack = true;
            }
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
                        },
                        CppTok::C(Tok {
                            kind: TokKind::RBracket,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        },
                        _ => {
                            self.bump();
                        },
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
            // M18.0: 函数指针形参 `int (*name)(args)` 解析为
            //   `Pointer(Function { ret: int, params: [args...] })`。
            //   形式: 紧跟 `(` `*` ident `)` 然后 `(` ... `)`。
            //
            //   内层形参表**必须递归解析** (同一个 `parse_param_list` 入口,
            //   所以嵌套函数指针形参 `void (*cb)(int (*)(int))` 也能展开)。
            //   之前只按括号 depth 把 `(args)` 当原文丢掉, 类型记成
            //   `Pointer(返回类型)`: legacy C 发射器本来就把这个形参近似成
            //   `T*` 以至看不出来, 但 native sema 的重载决议拿不到真实签名,
            //   `istream& operator>>(istream& (*)(istream&))` 永远比不中,
            //   `is >> std::ws` 因此退化成内建 `>>` (对流对象做 shr) → 运行期段错。
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                // `(` `*` ident `)` `( ... )`
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
                    // 形参表: 递归同一个 parse_param_list (它自己消费左/右括号)
                    let mut fn_params: Vec<MangleType> = Vec::new();
                    if matches!(
                        &self.peek,
                        Some(CppTok::C(Tok {
                            kind: TokKind::LParen,
                            ..
                        }))
                    ) {
                        fn_params = self
                            .parse_param_list()?
                            .into_iter()
                            .map(|(_, t)| t)
                            .collect();
                    }
                    params.push((
                        name,
                        MangleType::Pointer(Box::new(MangleType::Function {
                            ret: Box::new(ty),
                            params: fn_params,
                        })),
                    ));
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
                        },
                        CppTok::C(Tok {
                            kind: TokKind::RBracket,
                            ..
                        }) => {
                            self.bump();
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        },
                        _ => {
                            self.bump();
                        },
                    }
                }
            }
            params.push((
                name.unwrap_or_else(|| "_".into()),
                if is_param_pack {
                    MangleType::VariadicPack(vec![ty])
                } else {
                    ty
                },
            ));
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
                        },
                        CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        }) => {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                            self.bump();
                        },
                        CppTok::C(Tok {
                            kind: TokKind::RBrace,
                            ..
                        }) => {
                            depth -= 1;
                            self.bump();
                        },
                        CppTok::C(Tok {
                            kind: TokKind::Comma,
                            ..
                        }) if depth == 0 => break,
                        _ => {
                            self.bump();
                        },
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
    /// 解析类内成员变量声明: `<decl-specifier-seq> init-declarator-list ;`
    ///
    /// 按 init-declarator 列表展开成 N 个 `CppDecl::MemberVar` (`int a, b;` 两个
    /// 字段); 指针/引用/数组层级属于**各自的** declarator ([dcl.name]),
    /// `int *p, q;` 里 `q` 是 `int` 而不是 `int*`。
    fn parse_member_var(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let is_mutable = if self.at(CppKw::Mutable) {
            self.bump();
            true
        } else {
            false
        };
        // 检测 volatile / const 前缀限定符 (顺序可交错: `const volatile int x;`)
        let mut is_volatile = false;
        let mut is_const = false;
        loop {
            if self.at(CppKw::Volatile) {
                is_volatile = true;
            } else if self.at(CppKw::Const) {
                is_const = true;
            } else {
                break;
            }
            self.bump();
        }
        let ty = self.parse_mangle_type()?;
        // M30.2: 无名位域 `int :32;` (结构体 padding, glibc __fd_set) -
        // 无名字, 仅跳过位宽; 成员名占位避免 expect_ident 撞 `:`.
        let first_name = if matches!(
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
        // 第一个 declarator 名前的 `*` / `&` 已被类型解析器折进 `ty`, 剥回来交给
        // 它自己 (指针层级不属于 decl-specifier, 见 split_declarator_layers).
        let (base_ty, first_layers) = Self::split_declarator_layers(ty);
        let spec = DeclSpec {
            ty: base_ty,
            is_static: false,
            is_volatile,
            is_const,
        };
        let mut declarators: Vec<InitDeclarator> = Vec::new();
        let mut head = Some((first_layers, first_name));
        loop {
            let (layers, name) = match head.take() {
                Some(h) => h,
                None => {
                    // 逗号之后的 declarator: 名前的 `*` / `&` 属于它自己
                    let layers = self.parse_declarator_ptr_layers()?;
                    (layers, self.expect_ident()?)
                },
            };
            // M52: 数组成员 `Key keys_[1024];` / `char _shortbuf[1];`.
            //
            // 之前这里只按 bracket depth 把下标**整段跳过** (注释说"数组成员退化为
            // 指针", 实际连指针都不是 —— 字段类型就是元素类型), 于是 `keys_[i]` 在
            // sema 的 `Index` 分支撞上
            // `_ => Err("subscripted value is not an array or pointer")`:
            // 任何以定长数组做存储的容器都不可用 (`<flat_map>` 的
            // flat_multimap/flat_set 的 `keys_[1024]`、`<array>` 的 `data_[N]`).
            // 现在把可求值的常量维度真正折进 `MangleType::Array`.
            //
            // 边界: 维度依赖模板非类型形参 (`T data_[N];` 里的 `N`) 或是不完整数组
            // (`int a[];`) 时, parser 阶段无从求值 —— 保持原有的跳过行为, 不新增
            // 错误 (那是"模板内依赖数组边界"这条独立空缺, 不该由本路径顺带改变).
            // 维度属于**本 declarator**: `int *a[2], b;` 里只有 `a` 是数组.
            let mut array_dims: Vec<usize> = Vec::new();
            while matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                }))
            ) {
                self.bump(); // '['
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }))
                ) {
                    self.bump(); // ']' —— 无边界数组, 不建模
                    continue;
                }
                let size_expr = self.parse_expr()?;
                match self.fold_int_const_expr(&size_expr) {
                    Some(v) if v > 0 => array_dims.push(v as usize),
                    // 依赖型/未知维度: 已消费到下标末尾, 退化为原行为
                    _ => {},
                }
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }))
                ) {
                    self.bump(); // ']'
                }
            }
            let mut declarator = InitDeclarator {
                layers,
                array_dims,
                name,
                init: None,
            };
            // M30.2: 位域 `int x : 3;` / `enum E e : 8;` - 跳过位宽表达式.
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Colon,
                    ..
                }))
            ) {
                self.bump();
                self.skip_bitfield_width();
            }
            // NSDMI `= init`: 现在**存进 AST**而不是只消费掉 ——
            // `static constexpr bool value = true;` / `int x = 3;` 的初值是常量折叠
            // 与默认构造的输入。花括号 / 不可表达式形态仍只消费掉 (折不出常量时下游
            // 按无初值处理), 但不能因为它的存在而吞掉后面的声明符 —— 旧实现的
            // "跳到 `;`" 就把 `int a = f(x), b;` 里的 `b` 静默丢过。
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                let save = self.clone_state_for_init();
                match self.parse_expr() {
                    Ok(e)
                        if matches!(
                            &self.peek,
                            Some(CppTok::C(Tok {
                                kind: TokKind::Comma | TokKind::Semi,
                                ..
                            }))
                        ) =>
                    {
                        declarator.init = Some(e);
                    },
                    _ => {
                        self.restore_state_for_init(save);
                        self.skip_init_to_declarator_end();
                    },
                }
            }
            declarators.push(declarator);
            // `,` 之后还有下一个 init-declarator
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
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        Ok(declarators
            .into_iter()
            .map(|d| CppDecl::MemberVar {
                ty: d.full_ty(&spec),
                name: d.name.clone(),
                is_mutable,
                is_volatile: spec.is_volatile,
                is_const: spec.is_const,
                is_static: spec.is_static,
                init: d.init,
            })
            .collect())
    }

    /// 类内初值解析的**状态快照**: 试解析失败时要能回到 `=` 之后继续按原文跳过。
    /// (词法机位置 + 当前 peek token; 不涉及符号表, 所以试解析本身无副作用)
    fn clone_state_for_init(&self) -> (usize, Option<CppTok>) {
        (self.lex.pos, self.peek.clone())
    }

    fn restore_state_for_init(&mut self, save: (usize, Option<CppTok>)) {
        self.lex.pos = save.0;
        self.peek = save.1;
    }

    /// 跳过位域宽度 `: expr` (停在顶层 `,` / `;` 前, 括号内的不生效).
    fn skip_bitfield_width(&mut self) {
        let mut depth = 0i32;
        while let Some(t) = &self.peek {
            match t {
                CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }) => {
                    depth += 1;
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::RParen,
                    ..
                }) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::Semi | TokKind::Comma,
                    ..
                }) if depth == 0 => break,
                _ => {
                    self.bump();
                },
            }
        }
    }

    /// 跳过初始化式直到**本声明符**结束 (顶层 `,` / `;` / `}`), 括号与花括号
    /// 内部的逗号不算 (`int a = Foo(1, 2), b;` 里的 `b` 仍是一个声明符).
    fn skip_init_to_declarator_end(&mut self) {
        let mut depth = 0i32;
        while let Some(t) = &self.peek {
            match t {
                CppTok::C(Tok {
                    kind: TokKind::LParen | TokKind::LBracket | TokKind::LBrace,
                    ..
                }) => {
                    // 只按真括号计深度: `<` 可能是关系运算符 (`int a = x < y, b;`),
                    // 不能当模板尖括号吞.
                    depth += 1;
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::RParen | TokKind::RBracket | TokKind::RBrace,
                    ..
                }) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                },
                CppTok::C(Tok {
                    kind: TokKind::Semi | TokKind::Comma,
                    ..
                }) if depth == 0 => break,
                _ => {
                    self.bump();
                },
            }
        }
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
                },
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
                    _ => {},
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
                                },
                                CppTok::C(Tok {
                                    kind: TokKind::RParen,
                                    ..
                                }) => {
                                    self.bump();
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                },
                                _ => {
                                    self.bump();
                                },
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
            is_volatile: false,
            is_const: false,
            is_static: false,
            init: None,
        })
    }

    /// 名字段是否是**嵌套名说明符 + 形参表**: `Ident (:: [~] Ident [<模板实参>])* (`。
    ///
    /// M30.2 的原实现只看**一段**限定 (`Class::method(`), 于是
    /// `ns::Class::method(` / `ns::Class<T>::method(` / `A::B::~A(` 全部掉出分类器,
    /// 整条声明降级成 Opaque (实测 `int geo::Point::sum() const { ... }` 这类
    /// 命名空间内类的类外定义在 native 路径完全不可用)。
    /// 这里按 [basic.lookup.qual] 的嵌套名语法逐段走, 只多要求一件事:
    /// 最后紧跟 `(` (否则是 `ns::x = 5` 这类静态成员定义, 归变量分支)。
    fn looks_like_out_of_class_def(&mut self, name_pos: usize) -> bool {
        let ident = |p: &mut Self, i: usize| {
            matches!(
                p.peek_token_at(i),
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            )
        };
        if !ident(self, name_pos) {
            return false;
        }
        let mut k = name_pos + 1;
        loop {
            if !matches!(self.peek_token_at(k), Some(CppTok::ColonColon(_))) {
                return false;
            }
            k += 1;
            // 析构 `~Cls`
            if matches!(
                self.peek_token_at(k),
                Some(CppTok::C(Tok {
                    kind: TokKind::Tilde,
                    ..
                }))
            ) {
                k += 1;
            }
            if !ident(self, k) {
                return false;
            }
            k += 1;
            // 模板实参 `Cls<...>` (类模板特化的类外定义)
            if matches!(
                self.peek_token_at(k),
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) {
                match self.skip_template_args_at(k) {
                    Some(after) => k = after,
                    None => return false,
                }
            }
            if matches!(
                self.peek_token_at(k),
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                return true;
            }
            if !matches!(self.peek_token_at(k), Some(CppTok::ColonColon(_))) {
                return false;
            }
        }
    }

    /// M30.2: 类外成员函数 / 构造 / 析构定义
    ///   `Ret Class::method(params) [const] [noexcept] { ... }`
    ///   `Class::Class(params) [: init, ...] { ... }`
    ///   `Class::~Class(params) { ... }`
    /// 调用点: parse_free_fn 已消费返回类型与限定名首段, peek 为 `::`.
    /// GCC 主体 (libcpp/rich_location 等) 大量使用类外定义, 此前不识别
    /// 导致整文件降级 strip 管道 (尾部 const / this 改写全部丢失, 产出
    /// 非法 C). 限定链现在由 `looks_like_out_of_class_def` 分类, 支持
    /// `ns::Cls::method(` 多段嵌套名.
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
                // 类名 = 已收集的**整条**嵌套名说明符 (`A::B::~A` 的类是 `A::B`,
                // [class.qual])。旧实现只 pop 最后一段, 于是命名空间里的类的类外析构
                // 丢掉前缀 (`geo::Point::~Point` 变成 `Point`), sema 报 unknown class。
                let cls = if parts.is_empty() { nm } else { parts.join("::") };
                let _params = self.parse_param_list()?;
                let _spec = self.skip_specifiers();
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
        // 构造 `Class::Class(params) [: inits] {...}` - 方法名与**最内层类名**相同
        // (`geo::Point::Point(int)` 的类是 `geo::Point`, 构造名是最后一段 `Point`)。
        // 旧实现拿整条限定链比较, 多段限定的类外构造因此被当成普通成员定义;
        // `parts` 的元素本身也可能已是多段限定名 (类型位就是类名的写法), 故取最后一段。
        let innermost_class = parts
            .last()
            .map(|p| p.rsplit("::").next().unwrap_or(p))
            .unwrap_or("");
        if !class.is_empty() && name == innermost_class {
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
            // 类内 `static` 由 parse_member_decl 统一回到产物上; 这里是类外定义,
            // [class.static]/2 本来就不允许重复写存储期说明符。
            is_static: false,
        })
    }

    /// 解析构造函数初始化列表 `: a(1), b{2}, Base(x, y)` (类内外定义共用)。
    ///
    /// 每个条目解析成完整的实参列表 (见 `CppCtorInit`); 无列表时返回空 vec。
    fn parse_ctor_init_list(&mut self) -> Result<Vec<CppCtorInit>, CppParseError> {
        let mut inits: Vec<CppCtorInit> = vec![];
        if !self.at_tok(TokKind::Colon) {
            return Ok(inits);
        }
        self.bump();
        loop {
            let member = self.expect_qualifiable_ident()?;
            let (args, braces) = if self.at_tok(TokKind::LParen) {
                (self.parse_delimited_expr_list(TokKind::RParen)?, false)
            } else if self.at_tok(TokKind::LBrace) {
                (self.parse_delimited_expr_list(TokKind::RBrace)?, true)
            } else {
                // 非法的裸名字 (`: x`) 按默认初始化处理: 顶层容错会把整条声明
                // 降级成 Opaque, 损失远大于在这里保留语义。
                (Vec::new(), false)
            };
            inits.push(CppCtorInit {
                name: member,
                args,
                braces,
            });
            if self.at_tok(TokKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(inits)
    }

    /// 解析 `(`…`)` / `{`…`}` 内的逗号分隔表达式列表 (允许尾随逗号)。
    fn parse_delimited_expr_list(&mut self, close: TokKind) -> Result<Vec<CppExpr>, CppParseError> {
        self.bump(); // 开括号
        let mut args: Vec<CppExpr> = Vec::new();
        loop {
            if self.at_tok(close) || self.at_tok(TokKind::Eof) {
                break;
            }
            args.push(self.parse_expr()?);
            if self.at_tok(TokKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        if !self.at_tok(close) {
            return Err(CppParseError::Unexpected(
                if close == TokKind::RParen { ")" } else { "}" },
                format!("{:?}", self.peek),
                self.span(),
            ));
        }
        self.bump();
        Ok(args)
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
        let mut spec = self.skip_specifiers();
        let is_noexcept = spec.is_noexcept;
        // 解析 ctor 初始化列表 `: re(r), im(i)` (M30.2: 逻辑抽出共用)
        let inits = self.parse_ctor_init_list()?;
        // C++11 `X(const X&) = delete;` / `X() = 0;` / `X() = default;` —— 删除、
        // 纯虚与默认化的构造函数都是**无函数体**的合法声明。之前此分支缺失:
        // 尾缀只被部分消费, 剩下的 `delete` 使下面的 `;` 前瞻不成立而落入
        // `parse_body()` 失败, 整条成员声明 (连同它后面的所有成员) 被顶层容错
        // 降级成 Opaque —— 实测 `<ranges>` 头里四个 view 类的拷贝构造/拷贝
        // 赋值全部静默消失。
        if spec.is_deleted
            || spec.is_defaulted
            || spec.is_pure_virtual
            || self.take_deleted_or_pure_specifier(&mut spec)
        {
            self.consume_decl_semicolon();
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
            return Err(CppParseError::Unexpected("~", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let class = self.expect_ident()?;
        let _params = self.parse_param_list()?;
        let mut spec = self.skip_specifiers();
        let _is_noexcept = spec.is_noexcept; // C++11: dtors always implicitly noexcept
                                             // 同 ctor: `virtual ~A() = 0;` / `~A() = delete;` 都是无体的合法声明
                                             // (纯虚析构是抽象基类的常规写法), 不能落进 parse_body 后降级成 Opaque。
        if spec.is_deleted
            || spec.is_defaulted
            || spec.is_pure_virtual
            || self.take_deleted_or_pure_specifier(&mut spec)
        {
            self.consume_decl_semicolon();
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

    /// M53: [temp.arg.nontype] 模板实参位置的**常量表达式**起点.
    /// 只收不能是类型说明符开头的形态 (`sizeof`/`alignof`/`(`/`!`/`~`),
    /// 以及字面量后还跟着运算符的情况 (`buffer<1024 * 2>`).
    /// ident 开头 (`N + 1` / `sizeof(T)` 里的 T) 不揽: 那仍是依赖型
    /// 实例名 / 类型形参的位置.
    fn at_const_expr_nttp_start(&self) -> bool {
        let kind = match &self.peek {
            Some(CppTok::C(Tok { kind, .. })) => *kind,
            _ => return false,
        };
        if matches!(
            kind,
            TokKind::KwSizeof
                | TokKind::KwAlignof
                | TokKind::LParen
                | TokKind::Not
                | TokKind::Tilde
        ) {
            return true;
        }
        // 数字后紧跟的**闭合**符号 (`>` / `>>`) 必须排除在"常量表达式续读"
        // 之外: `S<7> s;` 里的 `>` 属于模板实参列表收尾, 若按二元运算符
        // 续读会解析出 `7 > s`, 整条局部声明被降级成 OpaqueStmt (回归)。
        if matches!(kind, TokKind::Number) {
            return matches!(
                self.peek_token_at(1),
                Some(CppTok::C(Tok {
                    kind: TokKind::Star
                        | TokKind::Plus
                        | TokKind::Minus
                        | TokKind::Slash
                        | TokKind::Percent
                        | TokKind::Lt
                        | TokKind::Le
                        | TokKind::Ge
                        | TokKind::EqEq
                        | TokKind::Ne
                        | TokKind::Shl
                        | TokKind::Amp
                        | TokKind::Pipe
                        | TokKind::Caret
                        | TokKind::AndAnd
                        | TokKind::OrOr
                        | TokKind::Question,
                    ..
                }))
            );
        }
        false
    }

    /// M30.1: 解析单个模板实参 (类型或非类型字面量).
    /// 实参位置的统一入口: true/false/整数/常量表达式走 NonType, 其余走类型解析.
    /// parse_mangle_type 内部循环 / 局部变量 qualified 分支 /
    /// 基类列表 / 函数显式实参共用.
    fn parse_template_arg(&mut self, depth: u32) -> Result<MangleType, CppParseError> {
        if (self.at(CppKw::True)
            || self.at(CppKw::False)
            || matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Number | TokKind::Minus,
                    ..
                }))
            ))
            && !self.at_const_expr_nttp_start()
        {
            return self.parse_non_type_value();
        }
        // M53: [temp.arg.nontype] 非类型实参不限于字面量, 可以是常量表达式:
        // `enable_if_t<sizeof(int) == 4, int>` / `buffer<1024 * 2>` /
        // `enable_if<is_same<T,int>::value || is_same<T,long>::value, T>`.
        // 旧实现把这些一律送进类型分支: `sizeof` / `(` / `!` 不是类型起点而报错,
        // 以标识符开头的子式则在读完嵌套名后剩下 `||` 无法收尾 —— 两种都让整条
        // 声明被顶层容错降级成 `CppDecl::Opaque`.
        if self.at_const_expr_nttp_start() || self.at_nt_expr_arg() {
            return self.parse_nt_expr_template_arg();
        }
        let ty = self.parse_mangle_type_depth(depth)?;
        // G12: 模板实参位置包展开 `tuple<Types...>` —— 与形参表同一约定:
        // 1 元素 `VariadicPack` 表示"这是一个待摊平的包".
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Ellipsis,
                ..
            }))
        ) {
            self.bump();
            return Ok(MangleType::VariadicPack(vec![ty]));
        }
        Ok(ty)
    }

    /// [temp.arg.nontype]: 按**常量表达式**读一个模板实参。
    ///
    /// 当场能求出值 -> `NonType`; 含模板形参 (依赖子式) -> 保留结构 (`NtExpr` /
    /// `TplMember`), 单态化代入实参后再折 (见 `TemplateRegistry::fold_args`);
    /// 两者都做不到 (不是常量表达式的形态, 如取地址 / 调用) -> 保持解析期硬错。
    fn parse_nt_expr_template_arg(&mut self) -> Result<MangleType, CppParseError> {
        self.nttp_angle_stop += 1;
        let expr = self.parse_expr();
        self.nttp_angle_stop -= 1;
        let expr = expr?;
        if let Some(v) =
            crate::cpp::cpp_const_eval::eval_cpp_const(&expr, &std::collections::HashMap::new())
        {
            return Ok(MangleType::NonType(v as i64));
        }
        if let Some(nt) = Self::nt_arg_from_expr(&expr) {
            return Ok(nt);
        }
        Err(CppParseError::Unexpected(
            "constant-expression template argument",
            format!("{:?}", expr),
            self.span(),
        ))
    }

    /// [temp.arg.nontype]/[temp.dep.constexpr]: 当前这个模板实参是不是一个
    /// **表达式**而不是类型。
    ///
    /// `enable_if<is_same<T,int>::value || is_same<T,long>::value, T>` 的首实参以
    /// 标识符开头, 词法上与类型一模一样: 旧实现把它送进类型分支, 读完
    /// `is_same<T,int>::value` 后剩下 `||` 无法收尾, 整条声明被顶层容错降级成
    /// Opaque。这里向后扫一遍实参 (跟踪嵌套括号/尖括号深度), 只在**顶层**遇到
    /// 不可能是类型一部分的运算符时判定为表达式。
    ///
    /// 故意不把 `*` / `&` 当作判据: 它们既可是乘/与运算, 也可是指针/引用类型
    /// 的一部分 (`T*`), 而后者在实参位置常见得多 —— 误判会把类型换成表达式。
    fn at_nt_expr_arg(&self) -> bool {
        // 扫描**单遍且有界**: `peek_token_at(k)` 每次都要 clone 词法机重走 k 步,
        // 按下标探会退化成 O(k²); 深层嵌套实参 (`Wrap<Wrap<..>>` 900 层) 因此能
        // 把一个合法编译拖到百秒级 (性能回归测试钉住了这条)。超过预算仍无结论
        // 时按类型处理 —— 那等于退回改动前的行为, 不会把类型误当表达式。
        const NT_ARG_LOOKAHEAD: usize = 64;
        let mut lex = self.lex.clone();
        let mut depth = 0i32;
        for _ in 0..NT_ARG_LOOKAHEAD {
            let Some(tok) = lex.next_tok() else {
                return false;
            };
            // 只有运算符/括号/逗号/分号参与判定: `::` (`CppTok::ColonColon`) 与
            // 关键字 (`CppTok::Kw`) 都是名字的一部分, 接着往下看。
            let CppTok::C(Tok { kind, .. }) = tok else {
                continue;
            };
            match kind {
                // 嵌套开: 里面的运算符属于内层语境 (`S<sizeof(int) == 4>` 的 `==`
                // 在尖括号里), 不能拿来判外层实参
                TokKind::Lt | TokKind::LParen | TokKind::LBrace | TokKind::LBracket => {
                    depth += 1;
                },
                // `>>` 是两层尖括号一起收尾 (词法上是一个 Shr token); 当作移位用
                //  (`x >> 2`) 时它出现在深度 0, 直接停止扫描 —— 保守地退回
                // 类型分支 (与改动前行为一致), 不会把类型误当表达式。
                TokKind::Gt | TokKind::Shr => {
                    if depth == 0 {
                        // 尖括号收尾: 实参列表到此为止
                        return false;
                    }
                    depth -= if matches!(kind, TokKind::Shr) { 2 } else { 1 };
                    if depth < 0 {
                        return false;
                    }
                },
                TokKind::RParen | TokKind::RBrace | TokKind::RBracket => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                },
                TokKind::Comma if depth == 0 => return false,
                TokKind::Semi if depth == 0 => return false,
                TokKind::OrOr
                | TokKind::AndAnd
                | TokKind::EqEq
                | TokKind::Ne
                | TokKind::Le
                | TokKind::Ge
                | TokKind::Plus
                | TokKind::Minus
                | TokKind::Slash
                | TokKind::Percent
                | TokKind::Caret
                | TokKind::Question
                    if depth == 0 =>
                {
                    return true;
                },
                _ => {},
            }
        }
        false
    }

    /// [temp.dep.constexpr]: 把模板**实参位置**的常量表达式转成可延迟求值的结构。
    ///
    /// 当场能求出值的已由 `cpp_const_eval::eval_cpp_const` 折成 `NonType`; 这里只收
    /// 含模板形参、必须留到单态化之后再求值的子式。**不是常量表达式**的形态
    /// (取地址 / 函数调用 / 赋值 / 下标) 返 None -> 保持原有的解析期硬错。
    fn nt_arg_from_expr(e: &CppExpr) -> Option<MangleType> {
        use crate::cpp::mangle::NtOp;
        let conv = |x: &CppExpr| Self::nt_arg_from_expr(x);
        match e {
            CppExpr::IntLit(v) => Some(MangleType::NonType(*v)),
            // 模板形参名在类型层的既有表示就是 `Class(形参名)` (单态化时替换),
            // 非常量形参 (`template <int N>` 的 `N`) 也一样依赖名字→值的回查。
            CppExpr::Ident(n) => Some(MangleType::Class(n.clone())),
            CppExpr::TplStaticAccess { name, args, member } => Some(MangleType::TplMember {
                name: name.clone(),
                args: args.clone(),
                member: member.clone(),
            }),
            // `Cls::value` / `ns::Cls::value`: 非模板类作用域的限定名
            CppExpr::Member(obj, member) => match &**obj {
                CppExpr::Ident(cls) => Some(MangleType::TplMember {
                    name: cls.clone(),
                    args: vec![],
                    member: member.clone(),
                }),
                other => {
                    let _ = other;
                    None
                },
            },
            CppExpr::QualifiedAccess(path, name) => Some(MangleType::TplMember {
                name: path.join("::"),
                args: vec![],
                member: name.clone(),
            }),
            CppExpr::Sizeof(crate::cpp::parser::SizeofArg::Type(t)) => {
                Some(MangleType::NtExpr(NtOp::Sizeof, vec![t.clone()]))
            },
            CppExpr::UnaryOp(op, inner) => {
                let nt = match op {
                    UnaryOpKind::LNot => NtOp::LNot,
                    UnaryOpKind::Not => NtOp::Not,
                    UnaryOpKind::Neg => NtOp::Neg,
                    UnaryOpKind::Plus => return conv(inner),
                    _ => return None,
                };
                Some(MangleType::NtExpr(nt, vec![conv(inner)?]))
            },
            CppExpr::BinOp(op, l, r) => {
                let nt = match op {
                    BinOpKind::Add => NtOp::Add,
                    BinOpKind::Sub => NtOp::Sub,
                    BinOpKind::Mul => NtOp::Mul,
                    BinOpKind::Div => NtOp::Div,
                    BinOpKind::Mod => NtOp::Mod,
                    BinOpKind::Shl => NtOp::Shl,
                    BinOpKind::Shr => NtOp::Shr,
                    BinOpKind::Lt => NtOp::Lt,
                    BinOpKind::Le => NtOp::Le,
                    BinOpKind::Gt => NtOp::Gt,
                    BinOpKind::Ge => NtOp::Ge,
                    BinOpKind::EqEq => NtOp::Eq,
                    BinOpKind::Ne => NtOp::Ne,
                    BinOpKind::LAnd => NtOp::Land,
                    BinOpKind::LOr => NtOp::Lor,
                    BinOpKind::And => NtOp::BitAnd,
                    BinOpKind::Or => NtOp::BitOr,
                    BinOpKind::Xor => NtOp::BitXor,
                    // 赋值 / 下标 / 逗号等不是常量表达式
                    _ => return None,
                };
                Some(MangleType::NtExpr(nt, vec![conv(l)?, conv(r)?]))
            },
            CppExpr::Ternary { cond, then, els } => Some(MangleType::NtExpr(
                NtOp::Cond,
                vec![conv(cond)?, conv(then)?, conv(els)?],
            )),
            // 整型常量表达式里的转换保持数值 (`(int)x` / `static_cast<int>(x)`),
            // 而 `NonType` 不携带宽度/符号 -> 只透传操作数。
            CppExpr::Cast(_, inner) | CppExpr::StaticCast { expr: inner, .. } => conv(inner),
            _ => None,
        }
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
            let v: i64 =
                if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
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
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::KwVolatile,
                    ..
                })) => {
                    self.bump();
                },
                Some(CppTok::Kw(CppKw::Volatile, _)) => {
                    self.bump();
                },
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
            },
        };
        self.bump();
        // M26: 处理限定名 `std::vector` / `::std::vector` 等
        let mut full_name = name.clone();
        while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            // M55: `::` 后必须是 ident 才属于类型名的一部分; 否则 (如类外析构
            // 定义 `Plain::~Plain()` 中的 `::~`) 不能先吃掉 `::` 再断开 —— 那会把
            // 限定符踩成一半, 调用方看到的就不再接着 `::`, 整条定义降级 Opaque.
            if !matches!(
                self.peek_token_at(1),
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) {
                break;
            }
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
                // Step 2 (A) 地基: `class` 与 `struct`/`union`/`enum` 一视同仁，
                // 把 tag key 存进 Class 名 (与下面 enum/struct/union 同形式)，
                // 下游 sema `convert_type` 才能靠 `elaborated_tag_kind()` 区分
                // 「首次引入 tag」(B4 按需登记为不完整类) 与「未声明名字」。
                // 旧注里 “C 输出无 class 关键字” 的 legacy 路径已下线 (native 为唯一后端),
                // 不再需要为它提前剥除 tag。
                full_name = format!("{} {}", full_name, text);
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
        // M30: 吞组合关键字类型并按 [dcl.type.simple] 归一。
        // 仅当首个类型词是内建基类型时才组合 (避免吞类名后的类型词)。
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
        // 收集本次声明里出现的**全部**简单类型说明符词。多词基本类型的含义由
        // 「符号性 / 长度 / 基准类型」三个维度决定, 与词序无关, 所以这里保留
        // 多重集合而不是拼写串, 交给 `canonical_simple_type` 判定。
        // `full_name` 正好是这些词的空格拼接, 组合完成后拆回集合即可。
        if is_builtin_base {
            while let Some(CppTok::C(Tok { kind, .. })) = &self.peek {
                let word = match kind {
                    TokKind::KwLong => "long",
                    TokKind::KwShort => "short",
                    TokKind::KwChar => "char",
                    TokKind::KwInt => "int",
                    TokKind::KwSigned => "signed",
                    TokKind::KwUnsigned => "unsigned",
                    // M30.2: `long double` 组合 (strtold 等) - 缺失时
                    // double 被误认为下一个 ident 名.
                    TokKind::KwDouble => "double",
                    _ => break,
                };
                full_name.push(' ');
                full_name.push_str(word);
                self.bump();
            }
        }
        let spec_words: Vec<&str> =
            if is_builtin_base { full_name.split(' ').collect() } else { Vec::new() };
        // 先按简单类型说明符集合归一; 不是合法组合 (`auto` / 类名 / `struct X`
        // 等) 再退回原有的拼写表, 保持那些形态的既有行为不变。
        let mut ty = match canonical_simple_type(&spec_words) {
            Some(t) => t,
            None => match full_name.as_str() {
                "auto" => MangleType::Auto,
                other => MangleType::Class(other.to_string()),
            },
        };
        // M18: `T<U1, U2, ...>` 模板实例化语法
        // 判别: ident/类名 后紧跟 `<` 且形参合法 (递归到匹配 `>`)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) && self.looks_like_template_args_after_lt(true)
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
            // M53: [temp.alias] 别名模板使用点就地展开:
            // `enable_if_t<B, T>` -> `typename enable_if<B, T>::type`.
            ty = self.expand_alias_type(ty);
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
        // M52 根因修复: `const T` (无 `&`) 之前被包成 `Reference(T)`, 而紧跟的
        // 注释写的却是"简化: 按值传递, const 无影响" —— 代码与注释相反.
        // 后果是整个前端把 `const int* f()` 看成"指针的引用"
        // (`Reference(Pointer(Int))`), 于是 `f()[0]` 报
        // "subscripted value is not an array or pointer", `const T*` 作返回值 /
        // 形参全线走样 (std::string::c_str 一类能编译只因从不取下标).
        // cv 限定在 dcc 的 `MangleType` 里没有表示位, 也不影响整/浮/指针的
        // 表示与ABI 判定, 故这里按注释本意**原样保留** ty; `const T&` 走上面的
        // Amp 分支 (已把 is_const 清零), 仍按 Reference 处理.
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
        self.parse_member_fn_after_ret(ret, is_virtual)
    }

    /// 返回类型已解析时续解成员函数 `name(params) suffix body`.
    /// 与 `parse_member_fn_inline` 共用后缀/函数体逻辑, 供 `T& name(...)`
    /// 这种 `parse_mangle_type` 不识别的返回类型复用 (单一真源).
    fn parse_member_fn_after_ret(
        &mut self,
        ret: MangleType,
        is_virtual: bool,
    ) -> Result<CppDecl, CppParseError> {
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
        let mut spec = self.skip_specifiers();
        // M18.0 / M30.6: `= 0` / `= delete` / `= default` —— 占位无体声明。
        // `skip_specifiers` 已在单一真源处消化尾缀 (旧实现只吃 `=`、把 `delete`
        // 留在流里 → 整条声明被降级成 Opaque)。
        if spec.is_deleted
            || spec.is_defaulted
            || spec.is_pure_virtual
            || self.take_deleted_or_pure_specifier(&mut spec)
        {
            self.consume_decl_semicolon();
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
                is_static: false,
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
                is_static: false,
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
            is_static: false,
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
            ) && self.looks_like_template_args_after_lt(true)
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
                trailing_ret_type: None,
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
        // M30.5: trailing return type 一般不在 friend 声明中使用，初始化为 None
        let trailing_ret_type: Option<MangleType> = None;
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
                trailing_ret_type,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
            is_noexcept: spec.is_noexcept,
            trailing_ret_type,
        })
    }

    /// 解析顶层自由函数定义/声明, 或Ident 起始类型的全局变量声明.
    /// 变量分支可能是一条 init-declarator 列表 (`int a = 0, b = 1;`), 故返回
    /// `Vec<CppDecl>`; 函数分支永远是单个声明.
    fn parse_free_fn(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        // 跳过函数说明符 (inline, static, virtual, explicit, ...)
        // M30: 记录 static (Ident 起始的顶层变量声明需要内部链接信息)
        let mut is_static = false;
        let mut is_volatile = false;
        // M54: 本函数的 constexpr 说明符 (常量求值表的登记资格)
        let mut constexpr_spec: Option<ConstexprSpec> = None;
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
            ) || match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) => text == "__inline" || text == "__inline__" || text == "__extension__",
                _ => false,
            };
            if is_spec {
                let is_c_static = matches!(&self.peek, Some(CppTok::Kw(CppKw::Static, _)))
                    || matches!(
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
            } else if self.at(CppKw::Volatile) {
                is_volatile = true;
                self.bump();
            } else if self.at(CppKw::Typename) {
                // M55: 返回类型位置的 `typename` ([dependent.name]/2 消歧义关键字)
                // 不改变类型本身. 不在这里吞掉, `typename enable_if<B,T>::type f(T)`
                // 会落到变量解析路径并失败 -> 顶层容错降级成 Opaque.
                self.bump();
            } else if let Some(cs) = self.at_constexpr_spec() {
                // M54: `constexpr` / `consteval` 不是语法噪声: 它决定本函数能否
                // 登记进编译期求值表. 旧代码在 `parse_decl_list` 入口就把它们
                // 吐掉 (“剥离即遗忘”), 于是 `constexpr int square(int x)` 根本
                // 不会被视为可折叠函数.
                constexpr_spec = Some(cs);
                self.bump();
            } else {
                break;
            }
        }
        // M54: 合并 `parse_decl_list` 已提前剥掉的说明符 (`constexpr int f()`
        // 走的是那条跳; `static constexpr int g = 1;` 的 `static` 也存于此)。
        let pend = self.take_pending_decl_spec();
        is_static |= pend.is_static;
        if constexpr_spec.is_none() {
            constexpr_spec = pend.constexpr;
        }
        // M18.0: 解析限定名返回类型 `views::filter_view` / `std::ranges::foo`
        // 形式: [::] ident (:: ident)*  后跟 ident 名字和 (...
        // 这里的变量是**类外定义**要用的嵌套名说明符 (整条限定链), 不是首段也不是
        // 末段: 只存末段会让 `ns::Cls::~Cls()` / `ns::Cls::Cls()` 丢掉 `ns::` 前缀。
        let mut qualified_class_prefix: Option<String> = None;
        let ret = if self.is_qualified_type_start() {
            let parts = self.parse_qualified_type_name()?;
            let last = parts.last().cloned().unwrap_or_default();
            qualified_class_prefix = Some(parts.join("::"));
            // M53: 限定名后紧跟 `<` 时那是 template-id (`std::enable_if_t<B, T> h()`
            // / `std::optional<int> f()`). 旧实现只把限定名折成 `Class(last)`,
            // 于是名字前的 `<` 撞上 expect_ident, 整条全局声明被顶层容错
            // 降级成 `CppDecl::Opaque` (sema: "unsupported opaque declaration").
            let mut ty = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt(true)
            {
                self.parse_template_id_tail(last)?
            } else {
                MangleType::Class(last)
            };
            // M55: template-id 之后的 `:: member` 属于**类型** (`enable_if<B,T>::type`
            // / `underlying_type<E>::type`), 不可能是声明符名字; 与
            // `parse_mangle_type` 的 `TplMember` 分支同源.
            if matches!(&self.peek, Some(CppTok::ColonColon(_)))
                && matches!(
                    self.peek_token_at(1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        ..
                    }))
                )
                && !matches!(
                    self.peek_token_at(2),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LParen,
                        ..
                    }))
                )
            {
                if let MangleType::Template { name, args } = &ty {
                    let name = name.clone();
                    let args = args.clone();
                    self.bump(); // ::
                    let member = self.expect_ident()?;
                    ty = MangleType::TplMember { name, args, member };
                }
            }
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
                let is_ref = matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Amp,
                        ..
                    }))
                );
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    }))
                ) {
                    self.bump();
                    // 限定名开头的声明 (`const std::type_info& ti = ...`) 里 `&` 是
                    // **引用** 而不是指针: 之前 Star/Amp 共用 `Pointer` 包装, 符号
                    // 类型变成 `type_info*`。后果: 引用变量作 `T&` 形参的实参时按
                    // 「具名左值取地址」降级 —— 传的是存放指针的那个**栈槽** (双重
                    // 间接), 实测 `ti.before(ti)` 直接 SIGSEGV。`parse_mangle_type`
                    // 的同位置已区分 Star/Amp, 此处属漏改。
                    ty = if is_ref {
                        MangleType::Reference(Box::new(ty))
                    } else {
                        MangleType::Pointer(Box::new(ty))
                    };
                } else {
                    break;
                }
            }
            ty
        } else {
            self.parse_mangle_type()?
        };
        // M55: 限定类型链在**声明符边界**停住 (见 `parse_qualified_type_name`),
        // 此处剩下的就是 `:: name` —— 类外定义 (`B::B(int)` / `B::~B()`),
        // 交回 `parse_out_of_class_member` 统一绑定. 旧实现把整条链贪心吞成
        // 类型, 于是所有**类外构造/析构定义**在 expect_ident 处失败并降级成
        // Opaque (此前无任何夹具覆盖该写法).
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            // 类名首段: 限定类型链停住的那一段 (`B::B(` / `ns::Cls::~Cls(`);
            // 若走的是 `parse_mangle_type` (如 `Plain::~Plain(` —— `is_qualified_type_start`
            // 因 `::~` 不成立), 则刚解析出的「返回类型」就是类名本身——这里必须传
            // **整条**限定名: 只传最后一段会让 `ns::Cls::~Cls()` / `ns::Cls::Cls()`
            // 丢失命名空间前缀 (实测 sema 报 unknown class 'Point')。
            let first = match qualified_class_prefix {
                Some(f) => f,
                None => match &ret {
                    MangleType::Class(n) => n.clone(),
                    _ => String::new(),
                },
            };
            return Ok(vec![self.parse_out_of_class_member(first, ret)?]);
        }
        let name = self.expect_ident()?;
        // M30.2: 名字后是 `::` → 类外成员定义 `Ret Class::method(...)` /
        // `Class::Class(...)` / `Class::~Class(...)`.
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            return Ok(vec![self.parse_out_of_class_member(name, ret)?]);
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
            return self.finish_global_var(
                ret,
                name,
                GlobalVarQuals {
                    is_static,
                    is_volatile,
                    is_const: pend.is_const,
                    constexpr: constexpr_spec,
                },
            );
        }
        let params = self.parse_param_list()?;
        // M30.5: trailing return type `auto f() -> Ret`
        let mut trailing_ret_type: Option<MangleType> = None;
        if matches!(
            self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Arrow,
                ..
            }))
        ) {
            self.bump(); // consume `->`
            let ret_ty = self.parse_mangle_type()?;
            trailing_ret_type = Some(ret_ty);
        }
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
            return Ok(vec![CppDecl::FreeFn {
                ret,
                name,
                params,
                body: CppBody {
                    stmts: vec![],
                    is_decl: true,
                },
                is_noexcept: spec.is_noexcept,
                trailing_ret_type,
            }]);
        }
        let body = self.parse_body()?;
        // M54: constexpr 自由函数登记到编译期求值表 (转换失败即不登记).
        // `auto f() -> int` 的真实返回类型在 trailing 部分.
        self.register_constexpr_fn(
            constexpr_spec,
            trailing_ret_type.as_ref().unwrap_or(&ret),
            &name,
            &params,
            &body,
        );
        Ok(vec![CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
            is_noexcept: spec.is_noexcept,
            trailing_ret_type,
        }])
    }

    /// M30: 解析顶层全局变量声明 `<decl-specifier> init-declarator-list ;`
    /// 由 parse_free_fn (Ident 起始类型) 与 parse_decl_list (类型关键字起始)
    /// 共用. 按 init-declarator 列表展开成 N 个 `CppDecl::GlobalVar`.
    fn parse_global_var_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let mut quals = GlobalVarQuals::default();
        loop {
            if self.at(CppKw::Static) {
                quals.is_static = true;
                self.bump();
            } else if self.at(CppKw::Const) {
                quals.is_const = true;
                self.bump();
            } else if self.at(CppKw::Volatile) {
                quals.is_volatile = true;
                self.bump();
            } else if let Some(cs) = self.at_constexpr_spec() {
                // M54: 本循环原本只认 static/const/volatile, `constexpr` 靠
                // `parse_decl_list` 吐掉后语义就丢了 —— 此处同态收下,
                // 使 `constexpr int N = 5;` 能与 `const int N = 5;` 同样
                // 进入整型常量池 (数组维度 / static_assert 都依赖它)。
                quals.constexpr = Some(cs);
                self.bump();
            } else {
                break;
            }
        }
        // M54: 合并 `parse_decl_list` 已提前剥掉的说明符
        let pend = self.take_pending_decl_spec();
        quals.is_static |= pend.is_static;
        quals.is_const |= pend.is_const;
        if quals.constexpr.is_none() {
            quals.constexpr = pend.constexpr;
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
        // M54: 全局变量名后的 `[...]` 不再“跳过”, 而是把维度套进类型
        // (`int g[10]` => `Array(Int, 10)`), 否则目标文件里只有元素大小的存储。
        self.finish_global_var(ty, name, quals)
    }

    /// 解析**全局/文件作用域的函数指针变量声明** `ret (*name)(params)` (可带
    /// `extern`/`static`/`const`/`volatile` 前缀与外层 `**` 指针层)。产出
    /// `CppDecl::GlobalVar`，类型形状 `Pointer(Function{ret,params})`，与
    /// [`parse_param_list`] 里形参函数指针尾巴同一实现; sema `convert_type` /
    /// `require_complete_type` 已支持该形状, 无需改动。
    ///
    /// 调用前 peek 必须停在声明起点 (首个限定符/返回类型)。这是修 libcpp
    /// 「全局函数指针声明被顶层容错静默降级成 Opaque → `unsupported opaque
    /// declaration: void (*obstack_alloc_failed_handler) (`」缺陷的入口: 之前
    /// 顶层分发只认「返回类型后接 Ident 再接 `(`」为函数、「接 Ident 再接 `;`」
    /// 为变量, 对「接 `(` 再接 `*`」的函数指针 declarator 两者都不落, 于是
    /// 落到 [`parse_global_var_decl`] 的 `expect_ident` 撞 `(` 失败 → Opaque。
    fn parse_global_fnptr_decl(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let mut quals = GlobalVarQuals::default();
        loop {
            if self.at(CppKw::Static) {
                quals.is_static = true;
                self.bump();
            } else if self.at(CppKw::Const) {
                quals.is_const = true;
                self.bump();
            } else if self.at(CppKw::Volatile) {
                quals.is_volatile = true;
                self.bump();
            } else if let Some(cs) = self.at_constexpr_spec() {
                quals.constexpr = Some(cs);
                self.bump();
            } else {
                break;
            }
        }
        let pend = self.take_pending_decl_spec();
        quals.is_static |= pend.is_static;
        quals.is_const |= pend.is_const;
        if quals.constexpr.is_none() {
            quals.constexpr = pend.constexpr;
        }
        // 返回类型 (可能含 `*`/`const` 等已折进类型); 解析后 peek 应停在 `(`。
        let ret = self.parse_mangle_type()?;
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
        self.bump(); // (
        // declarator 外层指针层: `(*h)` 一层, `(**pp)` 两层。
        let mut ptr_layers = 0usize;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Star,
                ..
            }))
        ) {
            self.bump();
            ptr_layers += 1;
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
        // 被指向函数的参数表 (复用 parse_param_list, 它自己消费左右括号)。
        let mut fn_params: Vec<MangleType> = Vec::new();
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LParen,
                ..
            }))
        ) {
            fn_params = self
                .parse_param_list()?
                .into_iter()
                .map(|(_, t)| t)
                .collect();
        }
        // 组装 `Pointer(Function)`; declarator 至少一层指针, 外层 `**` 再叠加。
        let mut ty = MangleType::Pointer(Box::new(MangleType::Function {
            ret: Box::new(ret),
            params: fn_params,
        }));
        for _ in 1..ptr_layers.max(1) {
            ty = MangleType::Pointer(Box::new(ty));
        }
        // 可选简单初值 `= &fn` 与结尾 `;` (逗号 declarator 列表为后续项, libcpp
        // 首错形态均为单 declarator)。
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
        Ok(vec![CppDecl::GlobalVar {
            ty,
            name,
            init,
            is_static: quals.is_static,
            is_volatile: quals.is_volatile,
            is_const: quals.is_const,
        }])
    }


    /// M30: 完成全局变量声明尾部: 名后的 `[...]` / `= init` 与逗号之后的
    /// 其余 declarator, 整条声明按 init-declarator 列表展开成 N 个
    /// `CppDecl::GlobalVar` (顶层声明容器本来就是 `Vec<CppDecl>`).
    ///
    /// 指针/引用层级属于 declarator 而不是 decl-specifier ([dcl.name]):
    /// `int *a, b;` 里 `b` 的类型是 `int`。因此先把第一个 declarator 名前的
    /// `*`/`&` (已被类型解析器折进 `ty`) 剥回来, 与逗号之后的 declarator 同等
    /// 对待, 每个声明符自己的层级叠加到共享基础类型上。
    fn finish_global_var(
        &mut self,
        ty: MangleType,
        name: String,
        quals: GlobalVarQuals,
    ) -> Result<Vec<CppDecl>, CppParseError> {
        let (base_ty, first_layers) = Self::split_declarator_layers(ty);
        let spec = DeclSpec {
            ty: base_ty,
            is_static: quals.is_static,
            is_volatile: quals.is_volatile,
            is_const: quals.is_const,
        };
        let mut declarators: Vec<InitDeclarator> = Vec::new();
        // 调用方已经吃掉了第一个 declarator 的名前层级与名字
        let mut head = Some((first_layers, name));
        loop {
            let (layers, name) = match head.take() {
                Some(h) => h,
                None => {
                    let layers = self.parse_declarator_ptr_layers()?;
                    (layers, self.expect_ident()?)
                },
            };
            let mut declarator = InitDeclarator {
                layers,
                array_dims: Vec::new(),
                name,
                init: None,
            };
            // M54: 维度必须套进类型 (与局部路径 `parse_init_declarator` 共用
            // 同一个后缀解析器): 旧实现调 `skip_array_suffix()` 把 `[10]`
            // 整个吞掉, 于是 `int g[10];` 在目标文件里只有 4 字节存储,
            // 并且 `g[3] = 7;` 因类型不是数组而在 sema 报“不可下标”。
            // 非严格模式: 碰上折不成的维度 (依赖运行时的变量长度数组等)
            // 保持旧的降级行为 (不套数组层), 不拿错维度骗下游。
            let suffix = self.parse_array_declarator_suffix(false)?;
            if !suffix.unmodeled {
                declarator.array_dims = suffix.dims;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                declarator.init = Some(self.parse_expr()?);
            }
            // M54: 最外层边界省略 (`char s[] = "abc";`) 由初始化式长度推出
            // [dcl.init]: 字符串推 N+1 (含 `\0`), 列表推元素个数。
            if suffix.deduce_outer && !suffix.unmodeled {
                if let Some(init) = &declarator.init {
                    if let Some(n) = Self::deduce_array_bound_from_init(init) {
                        // 维度存**源顺序**, 省略的是最外层那个 => 插到索引 0
                        declarator.array_dims.insert(0, n);
                    } else {
                        // 有初值但数不出元素个数: 数组层必须**保留**,
                        // 边界当作未知 (0 = 不完整数组类型)。
                        declarator.array_dims.insert(0, 0);
                    }
                } else {
                    // `extern char ea[];` —— 不完整数组声明, 由后续定义补齐。
                    // 早期实现直接去掉整个数组层, 于是名字在 sema 眼里是个
                    // 标量 `char`, `ea[1]` 报"subscripted value is not an array
                    // or pointer"。这里按 [basic.types] 保留数组层, 边界记 0。
                    declarator.array_dims.insert(0, 0);
                }
            }
            declarators.push(declarator);
            // `,` 之后还有下一个 init-declarator (`int a = 0, *b, c;`)
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
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            // 名字后的 `(` 是函数 (由 parse_free_fn 上游分流), 这里只可能剩下
            // 不能建模的声明符形态: 保持旧降级行为 (报 Unsupported)
            return Err(CppParseError::Unsupported("global variable declarator", self.span()));
        }
        self.bump();
        // M54: 常量性对象登记整型常量池. 旧代码只认 `CppExpr::IntLit`,
        // 所以 `constexpr int sq12 = square(12);` 永远进不了池,
        // 依赖它的 `char buf[sq12];` 维度也就折不出来。
        // 现在走 [`Self::fold_int_const_expr`] (含 constexpr 函数调用),
        // 并把初值**改写**成折叠后的字面量 —— 静态初始化必须落在
        // 编译期可表示的值上 (C 后端不接受静态初始化里的函数调用)。
        let is_const_obj = quals.is_const
            || quals
                .constexpr
                .map(|c| c.implies_const_object())
                .unwrap_or(false);
        let mut out: Vec<CppDecl> = Vec::with_capacity(declarators.len());
        for d in declarators {
            let full_ty = d.full_ty(&spec);
            // M54: 登记声明类型, 供**后续**常量上下文里的 `sizeof(名字)` 折叠
            // (`static_assert(sizeof(g) == 40)` / `char b[sizeof(g)];`)。
            self.record_global_obj_ty(&d.name, &full_ty);
            let mut init = d.init;
            // 只有**整数标量**对象才参与整型常量折叠: 浮点/指针/类的初值
            // 形态不是 `IntLit` 能表达的, 强转会给错值.
            if Self::is_constexpr_int_scalar(&full_ty) {
                if let Some(e) = init.as_ref() {
                    if let Some(v) = self.fold_int_const_expr(e) {
                        // `CppExpr::IntLit` 存 i64: 超范围就不改写, 绝不默默截断.
                        if let Ok(iv) = i64::try_from(v) {
                            if is_const_obj {
                                self.record_int_const(&d.name, v);
                            }
                            init = Some(CppExpr::IntLit(iv));
                        }
                    }
                }
            }
            // M54: 上面只处理「整个初值是一个整数标量常量」; 数组/聚合初值里的
            // constexpr 调用与常量算术 (`int a[4] = {square(2), -1, 2, 3};`) 同样
            // 必须折成字面量, 否则后端判为不可静态表示.
            if let Some(e) = init.take() {
                init = Some(self.fold_static_init_constants(&e));
            }
            out.push(CppDecl::GlobalVar {
                ty: full_ty,
                name: d.name.clone(),
                init,
                is_static: spec.is_static,
                is_volatile: spec.is_volatile,
                is_const: spec.is_const,
            });
        }
        Ok(out)
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
            Some(CppTok::Kw(CppKw::Inline | CppKw::Static | CppKw::Virtual | CppKw::Explicit, _))
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
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        // M54: 函数体内的声明会**遮蔽**全局对象, 而本阶段没有作用域链,
        // 故进入体后关掉 `sizeof(表达式)` 的全局查表 (见 `fold_sizeof`)。
        self.fn_body_depth += 1;
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
            // 容错: 单条语句解析失败时, 保留原文并继续解析后续语句,
            // 而非让整个函数体因 `?` 传播而全部丢失.
            let stmt_start = self.capture_start();
            let pos_before = self.lex.pos;
            match self.parse_stmt() {
                Ok(s) => stmts.push(s),
                Err(e) => {
                    self.diags.push(e.to_diag(&self.file));
                    // 跳过到语句边界 (`;` 或平衡括号后), 保持花括号配对
                    self.skip_to_stmt_boundary();
                    // 确保前进: 若 skip_to_stmt_boundary 未消耗任何 token
                    // (例如 `)` 在 depth 0 时立即返回), 手动 bump 避免死循环.
                    if self.lex.pos == pos_before {
                        self.bump();
                    }
                    let text = self.capture_src_text(stmt_start, self.lex.pos);
                    if !text.trim().is_empty() {
                        stmts.push(CppStmt::OpaqueStmt(text));
                    }
                },
            }
        }
        self.bump();
        self.fn_body_depth -= 1;
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
            self.fn_body_depth += 1;
            let stmt = self.parse_stmt();
            self.fn_body_depth -= 1;
            let stmt = stmt?;
            Ok(CppBody {
                stmts: vec![stmt],
                is_decl: false,
            })
        }
    }

    fn parse_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        // label: stmt — check for ident followed by colon before anything else
        if matches!(
            (&self.peek, self.peek_token_at(1)),
            (
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                })),
                Some(CppTok::C(Tok {
                    kind: TokKind::Colon,
                    ..
                }))
            )
        ) {
            let label = self.expect_ident()?;
            self.bump(); // consume ':'
            let inner = self.parse_stmt()?;
            return Ok(CppStmt::Labeled {
                label,
                stmt: Box::new(inner),
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
                // `return a, 0;` 里的 `,` 是逗号运算符 ([expr.comma]),
                // 只用 parse_expr 会停在 `a` 后, 剩下的 ` 0;` 触发语句级
                // 容错降级成 OpaqueStmt (整条 return 连同其后的语句一起丢)。
                Some(self.parse_comma_expr()?)
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
        // do body while (expr);
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwDo,
                ..
            }))
        ) {
            self.bump(); // 'do'
            let body = self.parse_body_or_stmt()?;
            if !matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwWhile,
                    ..
                }))
            ) {
                return Err(CppParseError::Unexpected(
                    "while",
                    format!("{:?}", self.peek),
                    self.span(),
                ));
            }
            self.bump(); // 'while'
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
            self.bump(); // '('
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
            self.bump(); // ')'
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::DoWhile {
                body: Box::new(body),
                cond,
            });
        }
        // goto label;
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwGoto,
                ..
            }))
        ) {
            self.bump(); // 'goto'
            let label = self.expect_ident()?;
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
            return Ok(CppStmt::Goto(label));
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
        let e = self.parse_comma_expr()?;
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

    /// 解析逗号表达式列表 ([expr.comma]): `e1, e2, ...`
    ///
    /// 只用于「表达式语句」与 `for` 的 step 这两个位置 —— 那里的 `,` 一定是逗号
    /// 运算符。不能把它接进 `parse_expr`: 实参列表 / 模板实参 / 初始化列表里的
    /// `,` 是分隔符, `f(a, b)` 之后多吃 `, b` 会全线解析错乱。
    /// 只有一个操作数时直接返回它本身, 不包 `Comma`。
    fn parse_comma_expr(&mut self) -> Result<CppExpr, CppParseError> {
        let mut elems = vec![self.parse_expr()?];
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Comma,
                ..
            }))
        ) {
            self.bump();
            elems.push(self.parse_expr()?);
        }
        if elems.len() == 1 {
            Ok(elems.into_iter().next().expect("至少有一个操作数"))
        } else {
            Ok(CppExpr::Comma(elems))
        }
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
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
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
            // step 可以是逗号表达式: `for (int i = 0, j = n; i < j; i++, j--)`
            Some(Box::new(CppStmt::Expr(self.parse_comma_expr()?)))
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
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        // for 的语句体同样允许无 `{}` 的单条语句 (与 if/while/do-while 一致,
        // [stmt.for]); 之前只收 `parse_body()` (强制 `{`), 于是
        // `for (int i = 0; i < n; ++i) s += i;` 整条语句被降级成 OpaqueStmt.
        let body = self.parse_body_or_stmt()?;
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
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        // Parse body: collect case/default arms
        let mut arms: Vec<(Option<CppExpr>, CppBody)> = Vec::new();
        let mut label_spans: Vec<Span> = Vec::new();
        let mut cur_expr: Option<CppExpr> = None;
        let mut cur_stmts: Vec<CppStmt> = Vec::new();
        let mut cur_label: Span = self.span();
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
                    label_spans.push(cur_label.clone());
                }
                cur_label = self.span(); // `case` 关键字位置
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
                // M54: `case square(2):` / `case E:` 的标签按 [stmt.switch] 必须是
                // 整型常量表达式, 但 sema 的折叠器 (`eval_cpp_const`) 只认字面量
                // 运算树 —— 既不查枚举常量表也不接 constexpr 函数表。常量名与
                // constexpr 调用的**唯一真源**在 parser (`int_consts` +
                // `constexpr_fns`), 因此折叠在本解析点完成: 标签换成整型字面量,
                // 下游 (sema / native / legacy Lower) 拿到的就是常量。
                // 折不成 (依赖运行时变量等) 则保留原表达式, 由 sema 同样地报错。
                let case_expr = match self.fold_int_const_expr(&case_expr) {
                    Some(v) => match i64::try_from(v) {
                        Ok(x) => CppExpr::IntLit(x),
                        // IntLit 存 i64: 超出则不改写, 绝不默默截断。
                        Err(_) => case_expr,
                    },
                    None => case_expr,
                };
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
                    label_spans.push(cur_label.clone());
                }
                cur_label = self.span(); // `default` 关键字位置
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
            // Check for `[[fallthrough]];` (C++17 standard attribute)
            if self.at_tok(TokKind::LBracket) {
                if let Some(CppTok::C(Tok {
                    kind: TokKind::LBracket,
                    ..
                })) = self.peek_token_at(1)
                {
                    if let Some(stmt) = self.parse_std_attributes_stmt()? {
                        cur_stmts.push(stmt);
                    }
                    continue;
                }
            }
            // Check for `__attribute__((fallthrough));` (GNU extension)
            let is_attr_fallthrough = matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    text,
                    ..
                })) if text == "__attribute__" || text == "__attribute"
            );
            if is_attr_fallthrough {
                let attr_span = self.span();
                self.bump(); // __attribute__
                             // expect `((`
                if self.at_tok(TokKind::LParen) {
                    self.bump();
                    if self.at_tok(TokKind::LParen) {
                        self.bump();
                        // collect attribute name
                        let mut attr_name = String::new();
                        while !self.at_tok(TokKind::RParen) {
                            if let Some(CppTok::C(Tok { text, .. })) = &self.peek {
                                attr_name.push_str(text);
                            }
                            self.bump();
                        }
                        // consume `))`
                        if self.at_tok(TokKind::RParen) {
                            self.bump();
                        }
                        if self.at_tok(TokKind::RParen) {
                            self.bump();
                        }
                        // consume `;`
                        if self.at_tok(TokKind::Semi) {
                            self.bump();
                        }
                        if attr_name.trim() == "fallthrough" {
                            cur_stmts.push(CppStmt::Fallthrough { span: attr_span });
                        }
                        continue;
                    }
                }
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
            label_spans.push(cur_label);
        }
        // bump past `}`
        self.bump();
        Ok(CppStmt::Switch {
            expr,
            arms,
            label_spans,
        })
    }

    // === fallthrough 诊断辅助函数 ===

    /// 从 case body 末尾向前查找最后一条"有效语句"（跳过空语句、空块）.
    /// 返回找到的语句的引用，空 body 返回 None.
    fn last_effective_stmt(body: &CppBody) -> Option<&CppStmt> {
        for stmt in body.stmts.iter().rev() {
            match stmt {
                // 空块跳过
                CppStmt::Block(b) if b.stmts.is_empty() => continue,
                // Block：递归进入找最后一条
                CppStmt::Block(b) => {
                    if let Some(s) = Self::last_effective_stmt(b) {
                        return Some(s);
                    }
                    continue;
                },
                _ => return Some(stmt),
            }
        }
        None
    }

    /// 判断 switch case 体最后一条语句是否会终止控制流 (break/return/continue/goto).
    fn stmt_terminates(s: &CppStmt) -> bool {
        match s {
            CppStmt::Break
            | CppStmt::Continue
            | CppStmt::Return(_)
            | CppStmt::Throw(_)
            | CppStmt::Goto(_)
            | CppStmt::Fallthrough { .. }
            | CppStmt::StructuredBinding { .. } => true,
            CppStmt::If { then, els, .. } => {
                let then_term = Self::last_effective_stmt(then).is_some_and(Self::stmt_terminates);
                let else_term = els.as_ref().is_some_and(|e| {
                    Self::last_effective_stmt(e).is_some_and(Self::stmt_terminates)
                });
                then_term && else_term
            },
            CppStmt::IfConstexpr { then, els, .. } => {
                let then_term = Self::last_effective_stmt(then).is_some_and(Self::stmt_terminates);
                let else_term = els.as_ref().is_some_and(|e| {
                    Self::last_effective_stmt(e).is_some_and(Self::stmt_terminates)
                });
                then_term && else_term
            },
            CppStmt::Block(b) => Self::last_effective_stmt(b).is_some_and(Self::stmt_terminates),
            CppStmt::Labeled { stmt, .. } => Self::stmt_terminates(stmt),
            CppStmt::Switch { .. } => true,
            CppStmt::While { .. }
            | CppStmt::DoWhile { .. }
            | CppStmt::For { .. }
            | CppStmt::CoAwait(_)
            | CppStmt::CoYield(_)
            | CppStmt::CoReturn(_) => false,
            CppStmt::Expr(_)
            | CppStmt::LocalVar { .. }
            | CppStmt::StaticLocal { .. }
            | CppStmt::LocalVarList { .. }
            | CppStmt::Decl(_)
            | CppStmt::ThisMember(_, _)
            | CppStmt::Try { .. }
            | CppStmt::OpaqueStmt(_) => false,
        }
    }

    /// 从 case body 中查找第一个 Fallthrough 语句的 span（用于 misplaced 警告定位）.
    fn find_fallthrough_span(body: &CppBody) -> Option<Span> {
        for stmt in &body.stmts {
            match stmt {
                CppStmt::Fallthrough { span } => return Some(span.clone()),
                CppStmt::Block(b) => {
                    if let Some(s) = Self::find_fallthrough_span(b) {
                        return Some(s);
                    }
                },
                _ => {},
            }
        }
        None
    }

    /// 递归扫描所有声明中的 switch 案例, 检查未标注的隐式贯穿.
    fn check_implicit_fallthrough(&mut self, decls: &[CppDecl]) {
        if !self.warn_implicit_fallthrough {
            return;
        }
        for d in decls {
            self.check_fallthrough_decl(d);
        }
    }

    fn check_fallthrough_decl(&mut self, d: &CppDecl) {
        match d {
            CppDecl::FreeFn { body, .. }
            | CppDecl::MemberFn { body, .. }
            | CppDecl::Ctor { body, .. }
            | CppDecl::Dtor { body, .. }
            | CppDecl::OperatorFn { body, .. } => {
                for stmt in &body.stmts {
                    self.check_fallthrough_stmt(stmt);
                }
            },
            CppDecl::Class(cd) => {
                for m in &cd.members {
                    self.check_fallthrough_decl(m);
                }
            },
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                // Regular and inline namespaces
                for x in body {
                    self.check_fallthrough_decl(x);
                }
            },
            CppDecl::ExternTemplate(_) => {
                // Extern templates: no fallthrough check needed (declaration only)
            },
            CppDecl::Template { decl, .. } => {
                self.check_fallthrough_decl(decl);
            },
            CppDecl::PartialSpec { decl, .. } => {
                self.check_fallthrough_decl(decl);
            },
            CppDecl::FullSpec { class, .. } => {
                for d in &class.members {
                    self.check_fallthrough_decl(d);
                }
            },
            CppDecl::ExportBlock(decls) => {
                for d in decls {
                    self.check_fallthrough_decl(d);
                }
            },
            CppDecl::AnonAggr { members, .. } => {
                for m in members {
                    self.check_fallthrough_decl(m);
                }
            },
            CppDecl::MemberVar { .. }
            | CppDecl::Using { .. }
            | CppDecl::UsingEnum { .. }
            | CppDecl::InheritingCtor { .. }
            | CppDecl::Concept { .. }
            | CppDecl::Module { .. }
            | CppDecl::Import { .. }
            | CppDecl::PrivateFragment
            | CppDecl::RawExternC(_)
            | CppDecl::GlobalVar { .. }
            | CppDecl::Typedef { .. }
            | CppDecl::Empty
            | CppDecl::Enum { .. }
            | CppDecl::Opaque(_)
            | CppDecl::StaticAssert { .. } => {},
        }
    }

    fn check_fallthrough_stmt(&mut self, s: &CppStmt) {
        match s {
            CppStmt::Switch {
                arms, label_spans, ..
            } => {
                let n = arms.len();
                for (i, (_expr, body)) in arms.iter().enumerate() {
                    // case 标签 span (与 arms 一一对应), 缺省时退化为 point(0,0)
                    let label_span = label_spans
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| Span::point(&self.file, 0, 0));
                    // 最后一个 case 之后即退出 switch, 不存在贯穿.
                    if i + 1 == n {
                        continue;
                    }
                    // 空 case 体是合法的 (允许 fallthrough 而无需标注).
                    if body.stmts.is_empty() {
                        continue;
                    }
                    // 找到 case 体最后一条有效语句.
                    let last = Self::last_effective_stmt(body);
                    match last {
                        // 最后一条有效语句是 fallthrough：合法标注
                        Some(CppStmt::Fallthrough { .. }) => {},
                        // 最后一条有效语句终止控制流：不会贯穿
                        Some(s) if Self::stmt_terminates(s) => {},
                        // 其他情况：未标注的隐式贯穿
                        _ => {
                            // 如果 body 中有 fallthrough 但不在最后，额外发 misplaced 警告
                            if let Some(mis_span) = Self::find_fallthrough_span(body) {
                                self.diags.push(Diagnostic::warn(
                                    "W0502",
                                    "fallthrough attribute is not at the end of a case label",
                                    mis_span,
                                ).with_hint("move `[[fallthrough]];` to the last statement before the next case label"));
                            }
                            // W0501: 隐式贯穿警告, 定位到 case 标签 (对齐 C 端)
                            let diag_span = label_span.clone();
                            self.diags.push(Diagnostic::warn(
                                "W0501",
                                "implicit fallthrough between switch cases (use [[fallthrough]] to mark intentional fallthrough)",
                                diag_span,
                            ).with_hint("add `[[fallthrough]];` at the end of the case body to silence this warning"));
                        },
                    }
                }
            },
            CppStmt::If { then, els, .. } => {
                for stmt in &then.stmts {
                    self.check_fallthrough_stmt(stmt);
                }
                if let Some(e) = els {
                    for stmt in &e.stmts {
                        self.check_fallthrough_stmt(stmt);
                    }
                }
            },
            CppStmt::IfConstexpr { then, els, .. } => {
                for stmt in &then.stmts {
                    self.check_fallthrough_stmt(stmt);
                }
                if let Some(e) = els {
                    for stmt in &e.stmts {
                        self.check_fallthrough_stmt(stmt);
                    }
                }
            },
            CppStmt::While { body, .. }
            | CppStmt::DoWhile { body, .. }
            | CppStmt::For { body, .. } => {
                for stmt in &body.stmts {
                    self.check_fallthrough_stmt(stmt);
                }
            },
            CppStmt::Block(b) => {
                for stmt in &b.stmts {
                    self.check_fallthrough_stmt(stmt);
                }
            },
            CppStmt::Labeled { stmt, .. } => {
                self.check_fallthrough_stmt(stmt);
            },
            CppStmt::Try { body, catches, .. } => {
                for stmt in &body.stmts {
                    self.check_fallthrough_stmt(stmt);
                }
                for c in catches {
                    for stmt in &c.body.stmts {
                        self.check_fallthrough_stmt(stmt);
                    }
                }
            },
            CppStmt::Return(_)
            | CppStmt::Break
            | CppStmt::Continue
            | CppStmt::Goto(_)
            | CppStmt::Throw(_)
            | CppStmt::Expr(_)
            | CppStmt::CoAwait(_)
            | CppStmt::CoYield(_)
            | CppStmt::CoReturn(_)
            | CppStmt::LocalVar { .. }
            | CppStmt::StaticLocal { .. }
            | CppStmt::LocalVarList { .. }
            | CppStmt::Decl(_)
            | CppStmt::ThisMember(_, _)
            | CppStmt::Fallthrough { .. }
            | CppStmt::OpaqueStmt(_)
            | CppStmt::StructuredBinding { .. }
            | CppStmt::IfConstexpr { .. } => {},
        }
    }

    /// 获取语句的 span（用于诊断定位）.
    fn stmt_span(s: &CppStmt) -> Option<Span> {
        match s {
            CppStmt::Fallthrough { span } => Some(span.clone()),
            _ => None,
        }
    }

    fn parse_expr(&mut self) -> Result<CppExpr, CppParseError> {
        // [expr]/[expr.cond]: 表达式的语法头是 assignment-expression, 而三元在
        // conditional-expression 这一层 —— `?:` 的优先级**高于**赋值。
        self.parse_assignment()
    }

    /// [expr.cond]: `logical-OR ? expression : conditional-expression`。
    ///
    /// 为什么必须单独一层: 早前 `parse_expr` 先做 `parse_assignment` 再看 `?`,
    /// 于是 `t = a > 3 ? 100 : 200` 被解析成 `(t = (a > 3)) ? 100 : 200`,
    /// `t` 拿到的是条件的布尔值 1 而不是 100/200 (实测与 g++ 不一致)。按标准
    /// 文法, 三元的**条件**是 logical-OR 表达式 (不含赋值), 赋值表达式的**右操作数**
    /// 是 conditional-expression —— 因此三元必须在赋值内层、在 `||` 外层。
    fn parse_conditional(&mut self) -> Result<CppExpr, CppParseError> {
        let cond = self.parse_or()?;
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Question,
                ..
            }))
        ) {
            return Ok(cond);
        }
        self.bump();
        // 中间操作数是完整的 expression (允许赋值与三元嵌套)
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
        // 末尾操作数是 conditional-expression (不含逗号运算符)
        let els = self.parse_conditional()?;
        Ok(CppExpr::Ternary {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        })
    }

    fn parse_assignment(&mut self) -> Result<CppExpr, CppParseError> {
        // 左操作数同样是 conditional-expression: 它后面的 `?` 只能属于内层
        // (赋值左侧要求是 cast-expression, 带三元的左值本身不合法)。
        let l = self.parse_conditional()?;
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
                })) if self.nttp_angle_stop == 0 => BinOpKind::Gt,
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
                })) if self.nttp_angle_stop == 0 => BinOpKind::Shr,
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
            // G12: `sizeof...(Pack)` —— 参数包元素个数. 括号里既可能是模板
            // 形参名 (`sizeof...(Args)`) 也可能是包变量名 (`sizeof...(args)`),
            // 统一存成 `SizeofArg::Pack(name)`, 单态化时按包长度替成字面量;
            // 未实例化的模板体里保留该节点 (不参与求值, 不静默归 0).
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ellipsis,
                    ..
                }))
            ) {
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
                let pack_name = self.expect_ident()?;
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
                return Ok(CppExpr::Sizeof(SizeofArg::Pack(pack_name)));
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            ) {
                self.bump();
                // M31: `sizeof(e)` 与 `sizeof(T)` 词法同形. 只有"括号里是一个
                // 非类型的裸标识符"时才按表达式解析 (保持未知类型名原有行为不变).
                // M54: 同一个启发式必须覆盖**限定名** (`sizeof(ns::a)`):
                // 旧写法只看一个裸 Ident, 于是 `ns::a` 落到类型路径被解析成
                // `Class("ns::a")` —— 命名空间里的变量/数组在 sizeof 里报
                // "unknown class type". 末段不是已知类型名即为值.
                let bare_expr = match &self.peek {
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        text,
                        ..
                    })) => Some(text.clone()),
                    _ => None,
                };
                let bare_expr = match bare_expr {
                    Some(name) => {
                        let followed_by_rparen = matches!(
                            self.peek_token_at(1),
                            Some(CppTok::C(Tok {
                                kind: TokKind::RParen,
                                ..
                            }))
                        );
                        (followed_by_rparen && !self.is_known_type_name(&name))
                            || self.sizeof_paren_is_value_name()
                    },
                    None => false,
                };
                if bare_expr {
                    // `sizeof(a)` / `sizeof x` 同一形态: 走表达式路径 (消耗收尾 `)`)
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
                    return Ok(CppExpr::Sizeof(SizeofArg::Expr(Box::new(e))));
                }
                // M54: 先按**类型**试, 只有类型解析成功且**正好**吃到 `)` 才是
                // `sizeof(T)`; 否则回滚词法状态改按表达式解析。旧实现只留类型
                // 一条路, 于是 `sizeof(g[0])` / `sizeof(*g)` / `sizeof(ns::a)`
                // 这类下标/解引用/限定名实参直接报错, 整条语句被顶层容错降级成
                // `OpaqueStmt` (多维数组的逐维 sizeof 因此完全不可用)。
                let saved_lex = self.lex.clone();
                let saved_peek = self.peek.clone();
                let ty_try = self.parse_mangle_type();
                let ends_at_rparen = matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::RParen,
                        ..
                    }))
                );
                if let (Ok(ty), true) = (ty_try, ends_at_rparen) {
                    self.bump();
                    return Ok(CppExpr::Sizeof(SizeofArg::Type(ty)));
                }
                self.lex = saved_lex;
                self.peek = saved_peek;
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
                return Ok(CppExpr::Sizeof(SizeofArg::Expr(Box::new(e))));
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
            // M17.0/M31: typeid(T) 静态类型 RTTI vs typeid(e) 表达式.
            // 见 typeid_arg_is_type_id: 必须查已知类型名集合, 不能把任意 Ident
            // 当类型名 (否则 `typeid(a)` 会被误判成 `typeid(class a)`).
            if self.typeid_arg_is_type_id() {
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
                // typeid 的结果是 glvalue ([expr.type.id]/3), `typeid(T).name()`
                // 这类后置选择完全合法 —— 不能在此提前 return 而跳过后缀循环.
                return self.parse_postfix_tail(CppExpr::TypeidType(ty));
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
            return self.parse_postfix_tail(CppExpr::Typeid(Box::new(e)));
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
        let e = self.parse_primary()?;
        self.parse_postfix_tail(e)
    }

    /// `parse_postfix` 的后缀循环部分, 以 `e` 为起点拆出.
    ///
    /// 拆分理由: `typeid(...)` 在 `parse_unary` 里作为一个提前返回的
    /// 一元构造, 若不经过后缀循环就丢失了 `.name()` / `[0]` / `(...)` 等
    /// 后置运算能力。循环体本身行为不变。
    fn parse_postfix_tail(&mut self, mut e: CppExpr) -> Result<CppExpr, CppParseError> {
        loop {
            // G12: 包展开后缀 `args...` (只能在实参 / 初始化列表等列表语境
            // 出现, 由单态化摊平; 出现在其它位置时展平阶段会硬报错).
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ellipsis,
                    ..
                }))
            ) {
                self.bump();
                e = CppExpr::PackExpansion(Box::new(e));
                continue;
            }
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
            let lit_text = text.clone();
            self.bump();
            return Ok(int_literal_expr(&lit_text, v));
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
                    },
                    Err(_) => {
                        // 解析类型失败, 恢复 lexer 状态
                        self.lex = saved;
                        self.peek = lparen.clone();
                    },
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
            // [expr.comma]/1: 括号里的 `,` 是**逗号运算符**而不是分隔符
            // (括号自己就构成了一个序列点边界), 所以括号表达式必须按
            // comma-expression 解析: `x = (a, b);` / `n += (a < b, c > d);`
            // / `return (a, b) ? 1 : 0;` 都是合法 C++, 早前只走 `parse_expr`
            // 会让整个语句解析失败并降级成 OpaqueStmt。
            let e = self.parse_comma_expr()?;
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
                ) && self.looks_like_template_args_after_lt(false)
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
                    let path = qualified_path_without_global_marker(&parts);
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
            ) && self.looks_like_template_args_after_lt(false)
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
                let path = qualified_path_without_global_marker(&parts);
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
            return Err(CppParseError::Unexpected("[", format!("{:?}", self.peek), self.span()));
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
            return Err(CppParseError::Unexpected("]", format!("{:?}", self.peek), self.span()));
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
            },
            Some(other) => {
                let got = format!("{:?}", other);
                Err(CppParseError::Unexpected("type-name", got, other.span()))
            },
            None => Err(CppParseError::Unexpected("type-name", "EOF".into(), self.span())),
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
            return Err(CppParseError::Unexpected("}", format!("{:?}", self.peek), self.span()));
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
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::KwConst,
                    ..
                })) => {
                    is_const = true;
                    true
                },
                Some(CppTok::Kw(CppKw::Const, _)) => {
                    is_const = true;
                    true
                },
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
            // `= 0` / `= delete` / `= default` 尾缀: 交单一真源入口消化
            if self.take_deleted_or_pure_specifier(&mut kinds) {
                continue;
            }
            // [dcl.fct]: 属性序列可以插在任何后置修饰之后、也之间 ——
            // glibc++ 大量写 `void terminate() noexcept __attribute__((__noreturn__));`
            // 与 `[[...]]` 后置形式; 本单一真源把尾缀属性一并循环消化,
            // 避免每个调用方 (free fn / member fn / ctor / dtor / 局部函数声明)
            // 各自记得补 skip_gnu_suffixes 导致不一致。
            let before = self.lex.pos;
            self.skip_gnu_suffixes();
            // C++11 `[[...]]` 后置形式 (不包含未展开宏 `IDENT(args)`，那
            // 会在当前函数尾缀位置与下一个声明的起点同形, 只在 namespace /
            // linkage-spec 前缀位置才安全)。
            if self.at_tok(TokKind::LBracket)
                && matches!(
                    self.peek_token_at(1),
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBracket,
                        ..
                    }))
                )
            {
                self.bump(); // [
                self.bump(); // [
                let mut depth = 2i32;
                while depth > 0 {
                    match &self.peek {
                        Some(CppTok::C(Tok { kind, .. })) => {
                            match kind {
                                TokKind::LBracket => depth += 1,
                                TokKind::RBracket => depth -= 1,
                                _ => {}
                            }
                            self.bump();
                        },
                        Some(_) => {
                            self.bump();
                        },
                        None => break,
                    }
                }
            }
            if self.lex.pos != before {
                continue;
            }
            break;
        }
        kinds
    }

    /// 识别并消费函数声明尾缀 `= 0` (纯虚) / `= delete` (删除) /
    /// `= default` (默认化), 命中时把对应标记写进 `kinds`。
    ///
    /// 未命中时**不消耗任何 token** (先前瞻后 bump)。
    ///
    /// 旧实现只吃 `=`、把 `delete` / `default` 留在 token 流里, 于是
    /// `X(const X&) = delete;` 的结尾 token 撞上后面的 `parse_body()` 而失败,
    /// 整条成员声明 (连同它之后的所有成员) 被顶层容错降级成 `CppDecl::Opaque`
    /// —— 实测 `<ranges>` 头里四个 view 类的拷贝构造/拷贝赋值全部静默消失。
    fn take_deleted_or_pure_specifier(&mut self, kinds: &mut SpecKinds) -> bool {
        if !matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            return false;
        }
        // `=` 后面那个 token 必须是 `0` / `delete` / `default`; 否则不是本形态
        // (例如变量初始化 `int arr[4] = {0};` 的那个 `=`)。
        let (pure, deleted, defaulted) = match self.peek_token_at(1) {
            Some(CppTok::C(Tok {
                kind: TokKind::Number,
                text,
                ..
            })) => (text == "0", false, false),
            Some(CppTok::C(Tok {
                kind: TokKind::KwDefault,
                ..
            })) => (false, false, true),
            Some(CppTok::Kw(CppKw::Delete, _)) => (false, true, false),
            // lexer 不把 delete/default 收成关键字时走 Ident 分支
            Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) => (false, text == "delete", text == "default"),
            _ => return false,
        };
        if !(pure || deleted || defaulted) {
            return false;
        }
        self.bump(); // =
        self.bump(); // 0 / delete / default
        kinds.is_pure_virtual |= pure;
        kinds.is_deleted |= deleted;
        kinds.is_defaulted |= defaulted;
        true
    }

    /// 声明形态 (无函数体) 的收尾: 后面有 `;` 就吃掉。
    fn consume_decl_semicolon(&mut self) {
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
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
    /// M26: 从 token 索引 i 起跳过 `ident<...>` 模板实参.
    /// 返回模板实参后的索引; 该处不是模板实参时返回 None.
    ///
    /// M53: 「`<` 是模板实参还是小于号」的判定**单一真源** = 已知类型名集合
    /// (`type_names`, 与 `typeid` / `sizeof` 的消歧共用一条渠道, [basic.type.ident]):
    /// `wrap<int> x{5};` 里 `wrap` 已在集合内 → 模板实参; 而 `a < b` 里的 `a`
    /// 只是变量. 名字**不**在集合内时不能反向判否 —— 类型名可能来自 `import`
    /// 的模块片段或未展开进 `self.src` 的头 (如 `std::coroutine_handle<P> h;`),
    /// 因此退回尖括号平衡扫描.
    fn skip_template_args_at(&mut self, i: usize) -> Option<usize> {
        if !matches!(
            self.peek_token_at(i),
            Some(CppTok::C(Tok {
                kind: TokKind::Lt,
                ..
            }))
        ) {
            return Some(i);
        }
        let before_lt = i.checked_sub(1).and_then(|j| self.peek_token_at(j));
        let mut known_type = false;
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &before_lt
        {
            let name = text.clone();
            known_type = self.is_known_type_name(&name);
        }
        let end = self.balanced_template_args_end(i)?;
        if known_type {
            return Some(end);
        }
        // 未知名字 + 不平衡 → 是小于号; 未知名字 + 平衡但括号内不是
        // 「像类型的 token」也按表达式处理 (`a < b + c > d;` 不是声明).
        if self.template_args_content_is_type_like(i, end) {
            Some(end)
        } else {
            None
        }
    }

    /// `i`..`end` (不含) 之间的模板实参内容是否全由类型样 token 构成
    /// (标识符 / `::` / 内置类型关键字 / `*` `&` `,` / 字面量 / 嵌套括号).
    fn template_args_content_is_type_like(&self, i: usize, end: usize) -> bool {
        let mut j = i + 1;
        while j < end.saturating_sub(1) {
            match &self.peek_token_at(j) {
                Some(CppTok::C(Tok {
                    kind:
                        TokKind::Ident
                        | TokKind::Number
                        | TokKind::CharLit
                        | TokKind::Star
                        | TokKind::Amp
                        | TokKind::Comma
                        | TokKind::Lt
                        | TokKind::Gt
                        | TokKind::Shr,
                    ..
                }))
                | Some(CppTok::ColonColon(_)) => {},
                Some(CppTok::C(t))
                    if matches!(
                        t.kind,
                        TokKind::KwInt
                            | TokKind::KwChar
                            | TokKind::KwShort
                            | TokKind::KwLong
                            | TokKind::KwFloat
                            | TokKind::KwDouble
                            | TokKind::KwSigned
                            | TokKind::KwUnsigned
                            | TokKind::KwBool
                            | TokKind::KwVoid
                            | TokKind::KwConst
                            | TokKind::KwVolatile
                            | TokKind::KwStruct
                            | TokKind::KwUnion
                            | TokKind::KwEnum
                    ) => {},
                Some(CppTok::Kw(CppKw::Const | CppKw::Volatile | CppKw::Typename, _)) => {},
                _ => return false,
            }
            j += 1;
        }
        true
    }

    /// 尖括号平衡扫描: `i` 处是 `<`, 返回闭合 `>` 之后的索引; 不平衡 → None.
    ///
    /// M53: 括号/方括号**嵌套内部**的 `)` / `;` 不是语句终结符: 非类型实参
    /// 可以是一个括起来的常量表达式 (`S<(2 + 5)> s;`), 那个 `)` 属于
    /// `(` 的配对. 与 [`CppParser::looks_like_template_args_after_lt`] 同一规则,
    /// 两处都是 [basic.type.ident] 的 `<` 消歧真源.
    fn balanced_template_args_end(&self, mut i: usize) -> Option<usize> {
        let mut depth: i32 = 1;
        let mut paren: i32 = 0;
        i += 1;
        loop {
            match self.peek_token_at(i) {
                None => return None,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt, ..
                })) if paren == 0 => {
                    depth += 1;
                    i += 1;
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::Gt, ..
                })) if paren == 0 => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return Some(i);
                    }
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::Shr, ..
                })) if paren == 0 => {
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
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen | TokKind::LBracket,
                    ..
                })) => {
                    paren += 1;
                    i += 1;
                },
                Some(CppTok::C(Tok {
                    kind: TokKind::RParen | TokKind::RBracket,
                    ..
                })) if paren > 0 => {
                    paren -= 1;
                    i += 1;
                },
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
                })) if depth == 1 && paren == 0 => return None,
                _ => {
                    i += 1;
                },
            }
        }
    }

    /// 局部变量声明探测: `Type name;` / `Type name = expr;` / `Type name{...};` 等
    fn is_local_var_decl_start(&mut self) -> bool {
        let mut i = 0;
        // 第一个类型说明符是否是内置类型关键字 (决定能否跟数组声明符)
        let mut first_is_builtin = false;
        // M53: 第一个类型说明符是否命中已知类型名集合 (决定能否跟花括号初始化符:
        // `Foo bar{1};` 只在 `Foo` 确为类型时才是声明, 否则是 `Foo{1}` 函数式转换)
        let mut first_is_known_type = false;
        let mut first_is_deduced = false;
        // 第一个类型说明符是否是 tag 关键字 (`struct`/`union`/`enum`)。tag 关键字开头
        // 的词法上**不可能**是表达式, 因此它与内置类型同样可以无歧义地
        // 允许数组声明符 / 花括号初始化符 (`struct S arr[3];` / `union U u{1};`)。
        let mut first_is_tag_spec = false;
        // 跳过 static
        let is_static = self.at(CppKw::Static);
        if is_static {
            i += 1;
        }
        // C++20: `constexpr` / `constinit` / `consteval` 是声明说明符而非类型的一部分,
        // 与全局路径 parse_decl 的处理保持一致 (降级时剥离), 否则局部
        // `constexpr int x = 1;` 会被误判为非声明语句.
        while matches!(
            self.peek_token_at(i),
            Some(CppTok::Kw(CppKw::Constexpr | CppKw::Constinit | CppKw::Consteval, _))
        ) {
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
            // M53: 用户类型名 (`wrap<int> x{5};`) 要认出 `wrap` 是已知类型名,
            // 与 `typeid` / `sizeof` 共用 `type_names` 单一真源.
            if let Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text,
                ..
            })) = first.as_ref()
            {
                let name = text.clone();
                first_is_known_type = self.is_known_type_name(&name);
            }
            // `auto` / `decltype(...)` 后的 `{` 也是初始化符 (`auto x{5};`)
            first_is_deduced = matches!(
                first,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwAuto,
                    ..
                }))
            ) || matches!(first, Some(CppTok::Kw(CppKw::Decltype, _)));
            // 内置类型关键字 (int/char/...) 开头的声明, 后面允许出现数组
            // 声明符 `[N]`. 用户类型 (Ident) 开头时不放行: `a[0] = x;`
            // 这种下标赋值语句与 `T a[N];` 在只看前几个 token 时无法区分,
            // 因此只对不可能与表达式混淆的内置类型开绿灯.
            first_is_builtin = matches!(
                first.as_ref(),
                Some(CppTok::C(Tok {
                    kind: TokKind::KwInt
                        | TokKind::KwChar
                        | TokKind::KwShort
                        | TokKind::KwLong
                        | TokKind::KwFloat
                        | TokKind::KwDouble
                        | TokKind::KwSigned
                        | TokKind::KwUnsigned
                        | TokKind::KwBool
                        | TokKind::KwVoid,
                    ..
                }))
            );
            i += 1;
            // [dcl.type.elab]: **elaborated type specifier** 是「tag 关键字 + 类型名」
            // 两个 token (`struct S x;` / `union U u;` / `enum E e;` /
            // `struct ::ns::T *p;`)。tag 关键字本身不是类型, 必须把紧跟的 tag 名
            // 一并计入类型部分, 否则 tag 名会被当成变量名, 真正的变量名 (`x`) 落在
            // 「名字后必须是 `;`/`=`/`(`」的检查上直接失败 —— 局部 `struct S x;`
            // 因此整条语句降级成 OpaqueStmt (C 代码编成 C++ 时极其常见:
            // `struct stat st;` / `union {…} u;` / `enum kind k;`)。
            if matches!(
                first.as_ref(),
                Some(CppTok::C(Tok {
                    kind: TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum,
                    ..
                }))
            ) {
                first_is_tag_spec = true;
                // `enum class X` / `enum struct X`: scoped enum 的 `class` 也是类型一部分
                if matches!(self.peek_token_at(i), Some(CppTok::Kw(CppKw::Class, _))) {
                    i += 1;
                }
                if matches!(
                    self.peek_token_at(i),
                    Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        ..
                    }))
                ) {
                    i += 1;
                }
            }
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
                        },
                        Some(CppTok::C(Tok {
                            kind: TokKind::RParen,
                            ..
                        })) => {
                            depth -= 1;
                            i += 1;
                        },
                        None => return false,
                        _ => {
                            i += 1;
                        },
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
        // 第 i+1 个 token 必须是 `;` `=` `(` `{` 之一; 内置类型开头时还允许
        // `[` (数组声明 `int name[N];`, 由 parse_local_var_decl 处理).
        // M53: `{` (列表初始化 `T name{v};`) 只对「类型说明符来源明确」的声明
        // 放行 —— 内置关键字 / 已知类型名 / 限定名 / `auto`, 因为 `Foo{1,2};`
        // 这种函数式转换表达式与 `Foo f{1,2};` 只差一个名字 token.
        let after_name = self.peek_token_at(i + 1);
        let is_lparen_or_semi_or_assign = matches!(
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
        );
        if is_lparen_or_semi_or_assign {
            return true;
        }
        let type_is_identifiable = first_is_builtin
            || first_is_tag_spec
            || first_is_known_type
            || first_is_deduced
            || starts_with_qualified_name;
        // `,` (多 init-declarator 列表 `int a = 0, b = 1;`) 同样只对「类型说明符
        // 来源明确」的声明放行: `x, y;` 这种逗号表达式语句开头是普通 Ident,
        // 不能当成声明, 否则会把整条语句误吞掉.
        let allows_declarator_list = type_is_identifiable
            && matches!(
                after_name,
                Some(CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }))
            );
        if allows_declarator_list {
            return true;
        }
        let allows_brace_init = type_is_identifiable
            && matches!(
                after_name,
                Some(CppTok::C(Tok {
                    kind: TokKind::LBrace,
                    ..
                }))
            );
        if allows_brace_init {
            return true;
        }
        first_is_builtin
            || (first_is_tag_spec
                && matches!(
                    after_name,
                    Some(CppTok::C(Tok {
                        kind: TokKind::LBracket,
                        ..
                    }))
                ))
    }

    /// 列表初始化 (`T n{a, b}`) / 直接初始化 (`T n(a, b)`) 的初始化式表达式.
    /// 两者只在「能否声明函数」上不同, 初始化式的构造规则共用这一处:
    /// - 单实参: 类类型 → ctor 调用 `Call(Ident(T), [e])` (`Widget w(7)` /
    ///   `Widget w{7}`); 标量 / 引用 / 类模板 (`wrap<int> x{5}`) → 直接取该实参,
    ///   与既有 `()` 行为一致;
    /// - 多实参 / 空 `{}`: 保留 `InitList`, 由 lower 按成员序聚合展开 —— 类类型
    ///   多实参若也走 ctor 调用, 会依赖一个不一定存在的 `T_ctor_complete` 符号
    ///   (聚合体 `P p{1, 2};` 没有用户 ctor, 发射端不会生成构造函数).
    fn list_init_expr(&self, ty: &MangleType, elems: Vec<CppExpr>) -> Option<CppExpr> {
        match ty {
            MangleType::Class(cls) if elems.len() == 1 => {
                Some(CppExpr::Call(Box::new(CppExpr::Ident(cls.clone())), elems))
            },
            _ if elems.len() == 1 => Some(elems.into_iter().next().unwrap()),
            _ => Some(CppExpr::InitList(elems)),
        }
    }

    /// [temp.alias]: 若 `ty` 是**别名模板**的使用 (`enable_if_t<B, T>`),
    /// 把形参代入目标类型并返回; 目标是另一个别名时使用 (别名可以套别名),
    /// 循环别名 (`template <class T> using X = X<T>;`) 由深度上限兜住 ——
    /// 标准不允许, 但不能让它在编译器里死循环.
    fn expand_alias_type(&self, ty: MangleType) -> MangleType {
        let mut cur = ty;
        for _ in 0..16 {
            let next = match &cur {
                MangleType::Template { name, args } => {
                    match self.alias_templates.get(name.as_str()) {
                        Some((params, defaults, target)) => {
                            // 省略的尾部默认实参先补齐 (`enable_if_t<true>` 的 `T = void`)
                            let mut full: Vec<MangleType> = Vec::with_capacity(params.len());
                            for (k, p) in params.iter().enumerate() {
                                _ = p;
                                full.push(match args.get(k) {
                                    Some(a) => a.clone(),
                                    None => match &defaults.get(k) {
                                        Some(Some(d)) => d.clone(),
                                        _ => return cur.clone(),
                                    },
                                });
                            }
                            crate::cpp::template::substitute_type(
                                target,
                                params.as_slice(),
                                full.as_slice(),
                            )
                        },
                        None => break,
                    }
                },
                _ => break,
            };
            cur = next;
        }
        cur
    }

    /// 解析 `template <...> using Name = Type;` 的别名模板本体 (在
    /// `parse_template_decl` 里参数表已解析完, 这里只登记).
    fn register_alias_template(&mut self, params: &[TemplateParam], td: &CppDecl) {
        if let CppDecl::Typedef { name, target } = td {
            let names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let defaults: Vec<Option<MangleType>> =
                params.iter().map(|p| p.default.clone()).collect();
            self.alias_templates
                .insert(name.clone(), (names, defaults, target.clone()));
        }
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
        matches!(self.peek_token_at(i), Some(CppTok::Kw(CppKw::Const | CppKw::Volatile, _)))
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

    /// M53: 类型名之后 `< Args > [:: member]` 尾巴的**唯一**解析入口。
    /// 调用点保证当前 token 是 `<` 且 `looks_like_template_args_after_lt()`
    /// 已判真 (`name` = 已消费的限定名最后一段)。
    /// 收敛 [basic.type.ident] 的 `<` 消歧与 [temp.alias] 别名模板展开:
    /// 此前 parse_mangle_type / parse_local_var_decl 各有一份手写平衡扫描,
    /// 全局声明位置 (parse_free_fn 的限定返回类型分支) 则完全没有 `<` 分支。
    fn parse_template_id_tail(&mut self, name: String) -> Result<MangleType, CppParseError> {
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
            // 内层模板 `>>` 拆出的第二个 `>`: 闭合当前层
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
            return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
        }
        let mut ty = MangleType::Template {
            name: name.clone(),
            args: args.clone(),
        };
        // M30: `Tpl<Args>::member` 拖尾 (模板类内 typedef 成员)
        if matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump(); // ::
            let member = self.expect_ident()?;
            ty = MangleType::TplMember { name, args, member };
        }
        // M53: [temp.alias] 别名模板使用点就地展开 (`std::enable_if_t<B, T>`
        // -> `typename enable_if<B, T>::type`)
        Ok(self.expand_alias_type(ty))
    }

    /// 把 decl-specifier 里被 `parse_mangle_type` 顺带折进来的 `*` / `&` 剥下来.
    ///
    /// C++ 文法上指针/引用属于 **declarator** ([dcl.ptr]/[dcl.ref]), 但
    /// `parse_mangle_type` 按「类型 + 紧跟的指针符」整体解析, 于是 `int *p, q;`
    /// 里第一个 declarator 的 `*` 已经进了 `ty` (`Pointer(Int)`), 而它并不能被同一条
    /// 声明里的其它 declarator 共享 (`q` 的类型是 `int`)。本函数把最外层的
    /// `Pointer` / `Reference` 逐层剥掉, 返回 `(基础类型, 源码书写顺序的层级)`,
    /// 让「共享的 decl-specifier」与「每个 declarator 自己的层级」各归其位。
    /// 遇到第一个非 `*`/`&` 层级即停止 (数组 `[N]` 写在名后, 由声明符解析处理)。
    fn split_declarator_layers(ty: MangleType) -> (MangleType, Vec<DeclaratorLayer>) {
        // 由外向内逐层剥离: `int *&p` 的 ty 是 Reference(Pointer(Int)),
        // 剥出来是 [Reference, Pointer]; 而 declarator 的层级按「从内向外应用」
        // 的源码顺序保存 ([Pointer, Reference]), 所以最后反转一次。
        let mut outer_to_inner: Vec<DeclaratorLayer> = Vec::new();
        let mut base = ty;
        loop {
            let peeled = match base {
                MangleType::Pointer(inner) => (DeclaratorLayer::Pointer, *inner),
                MangleType::Reference(inner) => (DeclaratorLayer::Reference, *inner),
                _ => break,
            };
            outer_to_inner.push(peeled.0);
            base = peeled.1;
        }
        outer_to_inner.reverse();
        (base, outer_to_inner)
    }

    /// M9: 解析局部变量声明
    ///
    /// 一条声明 = decl-specifier + **init-declarator 列表** ([dcl.dcls]):
    /// `int a = 0, *b, c[2];` 整体是**一条**语句 (见 [`CppStmt::LocalVarList`]),
    /// 单 declarator 的常见情形退化成 [`CppStmt::LocalVar`] / [`CppStmt::StaticLocal`]。
    fn parse_local_var_decl(&mut self) -> Result<CppStmt, CppParseError> {
        // M10+: 检测 static 关键字
        let is_static = self.at(CppKw::Static);
        if is_static {
            self.bump();
        }
        // C++20: constexpr/constinit/consteval 声明说明符 - 同 parse_decl 剥离
        let mut was_const = false;
        let mut is_const = false;
        while self.at(CppKw::Constexpr) || self.at(CppKw::Constinit) || self.at(CppKw::Consteval) {
            was_const = true;
            is_const = true;
            self.bump();
        }
        // M30.2: 先跳过 const/volatile 前缀，否则 `const bidi::context &x`
        // `const` 既进 `was_const` (整型常量折叠表) 也进 `is_const` (对象的顶层
        // cv 限定, 重载决议要看它: `const M c; c.f()` 必须选 `f() const`)。
        let mut is_volatile = false;
        while self.at(CppKw::Const) || self.at(CppKw::Volatile) {
            if self.at(CppKw::Volatile) {
                is_volatile = true;
            } else {
                was_const = true;
                is_const = true;
            }
            self.bump();
        }
        // M30.4: 结构化绑定 `auto [a, b] = expr;`
        // 检查：类型后紧跟 `[` 而不是标识符
        let ty = if self.is_qualified_type_start() {
            // 解析限定名为 class 类型，存为 (last segment, qualified path)
            let parts = self.parse_qualified_name()?;
            let last = parts.last().cloned().unwrap_or_default();
            // 用 last segment 当 Class 名 (lower 时根据 namespace ctx 还原)
            let mut ty = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Lt,
                    ..
                }))
            ) && self.looks_like_template_args_after_lt(true)
            {
                // M26: 限定名后的模板实参 `std::vector<int>` (M53: 与全局
                // 声明位置共用 parse_template_id_tail)
                self.parse_template_id_tail(last)?
            } else {
                MangleType::Class(last)
            };
            // 跳过 const/volatile 后缀
            if self.at(CppKw::Const) {
                self.bump();
            }
            // 跳过多 token 类型后缀 (例：long long)
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
                let is_ref = matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Amp,
                        ..
                    }))
                );
                if matches!(
                    &self.peek,
                    Some(CppTok::C(Tok {
                        kind: TokKind::Star | TokKind::Amp,
                        ..
                    }))
                ) {
                    self.bump();
                    // 限定名开头的声明 (`const std::type_info& ti = ...`) 里 `&` 是
                    // **引用** 而不是指针: 之前 Star/Amp 共用 `Pointer` 包装, 符号
                    // 类型变成 `type_info*`。后果: 引用变量作 `T&` 形参的实参时按
                    // 「具名左值取地址」降级 —— 传的是存放指针的那个**栈槽** (双重
                    // 间接), 实测 `ti.before(ti)` 直接 SIGSEGV。`parse_mangle_type`
                    // 的同位置已区分 Star/Amp, 此处属漏改。
                    ty = if is_ref {
                        MangleType::Reference(Box::new(ty))
                    } else {
                        MangleType::Pointer(Box::new(ty))
                    };
                } else {
                    break;
                }
            }
            ty
        } else {
            self.parse_mangle_type()?
        };

        // M30.4: 检测结构化绑定 - 下一个 token 是 `[` 而非 ident
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            return self.parse_structured_binding(&ty, is_static, is_volatile);
        }

        // 普通变量声明：一条声明 = decl-specifier + init-declarator 列表.
        // 第一个 declarator 名前的 `*` / `&` 已经被类型解析器折进 `ty`, 先剥回来
        // 交给它自己 (指针层级不属于 decl-specifier, 见 split_declarator_layers).
        let (base_ty, first_layers) = Self::split_declarator_layers(ty);
        let spec = DeclSpec {
            ty: base_ty,
            is_static,
            is_volatile,
            is_const,
        };
        let mut declarators: Vec<InitDeclarator> = Vec::new();
        let mut pending_layers = first_layers;
        loop {
            let d =
                self.parse_init_declarator(&spec, std::mem::take(&mut pending_layers), was_const)?;
            declarators.push(d);
            // `,` 之后还有下一个 init-declarator (`int a = 0, *b, c[2];`)
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
        // 声明结尾的 `;` 统一在这里吞掉 (旧实现是每个 declarator 出口各自吞).
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }
        // 单 declarator 退化成旧变体, 让下游那批按单变量匹配的 pass 保持原样;
        // 2 个以上必须是**一条**语句 —— for-init 只能容纳单个
        // declaration-statement ([stmt.init]).
        if declarators.len() == 1 {
            let d = declarators
                .into_iter()
                .next()
                .expect("declarator 列表至少有一个元素");
            return Ok(d.to_stmt(&spec));
        }
        Ok(CppStmt::LocalVarList { spec, declarators })
    }

    /// 解析 init-declarator 名前的 `*` / `&` 层级 (允许交错 cv / `__restrict`).
    ///
    /// 局部声明 / 全局声明 / 类成员三条路径共用: 指针与引用层级属于 **declarator**,
    /// 逗号之后的每个 declarator 都要各自解析这一段 (`int *p, q;` 的 `q` 是 `int`).
    fn parse_declarator_ptr_layers(&mut self) -> Result<Vec<DeclaratorLayer>, CppParseError> {
        let mut layers: Vec<DeclaratorLayer> = Vec::new();
        loop {
            while self.at(CppKw::Const) || self.at(CppKw::Volatile) {
                self.bump();
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::KwRestrict,
                    ..
                }))
            ) {
                self.bump();
            }
            let star = matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
                    ..
                }))
            );
            let amp = matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Amp,
                    ..
                }))
            );
            if !star && !amp {
                break;
            }
            self.bump();
            layers.push(if amp {
                DeclaratorLayer::Reference
            } else {
                DeclaratorLayer::Pointer
            });
        }
        Ok(layers)
    }

    /// 吞掉 declarator 名后的 `[...]` 数组下标 (按括号深度整体吞掉).
    ///
    /// 仅用于 [`Self::parse_array_declarator_suffix`] 的**非严格**分支: 某个维度
    /// 折不成常量时, 剩下的下标仍需消费以保证整条声明可继续解析
    /// (系统头里 `extern T arr[EXPR];` 不应当变成硬错误).
    fn skip_array_suffix(&mut self) {
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
                    },
                    CppTok::C(Tok {
                        kind: TokKind::RBracket,
                        ..
                    }) => {
                        self.bump();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    },
                    _ => {
                        self.bump();
                    },
                }
            }
        }
    }

    /// 解析 declarator 名后的**一整串** `[...]` 数组后缀 —— 局部与全局声明的
    /// **单一真源** (旧的全局路径只有 `skip_array_suffix`, 把维度直接吐掉,
    /// 于是 `int g[10];` 的类型是 `int`, 目标文件里只有 4 字节存储).
    ///
    /// 返回的 `dims` 是**源顺序** (`a[2][3]` -> `[2, 3]`), 由
    /// [`InitDeclarator::full_ty`] 从内向外套成 `Array(Array(T, 3), 2)`.
    ///
    /// `strict = true` (局部路径既有行为): 维度折不出正常常量直接报错.
    /// `strict = false` (全局路径): 折不动时保持旧的降级语义 —— 吞掉剩余下标
    /// 并标记 `unmodeled`, 调用方不套数组层 (宁可退回标量也不能给出错的值).
    fn parse_array_declarator_suffix(
        &mut self,
        strict: bool,
    ) -> Result<ArraySuffix, CppParseError> {
        let mut out = ArraySuffix::default();
        let mut outermost = true;
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            self.bump(); // '['
                         // 省略边界 `[]`: 只有**最外层** (声明符的第一个 `[`) 合法
                         // ([dcl.array]/[d.init.list]), 边界由初始化式长度推出.
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RBracket,
                    ..
                }))
            ) {
                self.bump(); // ']'
                if outermost {
                    out.deduce_outer = true;
                } else if strict {
                    return Err(CppParseError::Unsupported(
                        "only the outermost array bound may be omitted",
                        self.span(),
                    ));
                } else {
                    out.unmodeled = true;
                    self.skip_array_suffix();
                    return Ok(out);
                }
                outermost = false;
                continue;
            }
            let size_expr = self.parse_expr()?;
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
            self.bump(); // ']'
            match self.fold_int_const_expr(&size_expr) {
                Some(v) if v > 0 => out.dims.push(v as usize),
                _ => {
                    if strict {
                        return Err(CppParseError::Unsupported(
                            "array size must be a positive integer constant expression",
                            self.span(),
                        ));
                    }
                    // 非严格路径: 该维度未知 -> 整串后缀不建模, 剩下的仍须吞干净
                    out.unmodeled = true;
                    self.skip_array_suffix();
                    return Ok(out);
                },
            }
            outermost = false;
        }
        Ok(out)
    }

    /// 由初始化式推出省略边界的数组长度 ([dcl.init]/[array.to.pointer]):
    /// `char s[] = "abc";` -> 4 (含结尾 `\0`), `int a[] = {1, 2};` -> 2。
    /// 推不出 (初值形态未建模) 时返回 `None`, 调用方按未建模降级。
    fn deduce_array_bound_from_init(init: &CppExpr) -> Option<usize> {
        match init {
            // 字符串字面量: `char s[] = "abc";` 的长度是字节数 + 1 (`\0`)。
            // `StrLit` 存的是**未解转义**的源文本 (引号已由词法层去掉,
            // 转义则要到 sema/rodata 落点才由 `decode_string_escapes` 解码),
            // 所以长度必须走解码后的字节数, 不能 `chars().count()`
            // (`"a\nb"` 文本 4 字符, 实际存 3 字节 + `\0` = 4)。
            CppExpr::StrLit(s) => Some(crate::cpp::const_text::decode_string_escapes(s).len() + 1),
            // 相邻字面量拼接 (`char t[] = "xy" "z";`) : 长度是各段解码后的
            // 字节数之和 + 1 (`\0`), 不是段数。
            CppExpr::StrConcat(parts) => Some(
                parts
                    .iter()
                    .map(|p| crate::cpp::const_text::decode_string_escapes(p).len())
                    .sum::<usize>()
                    + 1,
            ),
            CppExpr::InitList(v) | CppExpr::TypedInitList { elems: v, .. } => Some(v.len()),
            _ => None,
        }
    }

    /// 解析一条声明里的**单个** init-declarator: `[*&] name [[N]...] [init]`.
    ///
    /// `layers` 传入名前已经确定的 `*` / `&` 层级 —— 只有整条声明的第一个
    /// declarator 有 (从 decl-specifier 里剥出来的), 逗号之后的 declarator 传空
    /// 列表, 它们的指针/引用层级在本函数内按源码顺序解析。这正是 `int *p, q;`
    /// 中 `q` 类型为 `int` 的保证。结尾的 `;` 由调用方统一吞掉。
    fn parse_init_declarator(
        &mut self,
        spec: &DeclSpec,
        mut layers: Vec<DeclaratorLayer>,
        was_const: bool,
    ) -> Result<InitDeclarator, CppParseError> {
        // 名前的 `*` / `&` (允许交错 cv / __restrict). 第一个 declarator 的这一段
        // 已被类型解析器消化, 循环只会命中逗号之后的 declarator (`*q` / `&r`).
        layers.extend(self.parse_declarator_ptr_layers()?);
        let name = self.expect_ident()?;
        // 数组声明符：`T name[N];` — 收集所有 `[N]` 后缀 (多维数组),
        // 从最内层向外包装: `int a[2][3]` => Array(Array(Int, 3), 2)
        let mut array_dims: Vec<usize> = Vec::new();
        while matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBracket,
                ..
            }))
        ) {
            self.bump(); // '['
            let size_expr = self.parse_expr()?;
            let n = match self.fold_int_const_expr(&size_expr) {
                Some(v) if v > 0 => v as usize,
                _ => {
                    return Err(CppParseError::Unsupported(
                        "array size must be a positive integer constant expression",
                        self.span(),
                    ))
                },
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
            self.bump(); // ']'
            array_dims.push(n);
        }
        let mut declarator = InitDeclarator {
            layers,
            array_dims,
            name,
            init: None,
        };
        let is_array = !declarator.array_dims.is_empty();
        // 本 declarator 的完整类型 = 共享基础类型 + 自己的指针/数组层级.
        let ty = declarator.full_ty(spec);
        // 数组声明可以带初值 `int a[2] = {1, 2};` / `int a[2] = {};`
        if is_array
            && matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            )
        {
            self.bump();
            self.parse_expr()?; // 初值暂不建模, 仅消费以避免误解析
            return Ok(declarator);
        }
        // 初始化式: `{...}` (列表) / `(args)` (直接) / `= expr` (拷贝) ([dcl.init]).
        // M53: 花括号初始化 `T name{a, b};` / `T name{};` 该形态过去只被
        // is_local_var_decl_start 认成表达式 (`(T) name < a , b ...`),
        // 现在按声明解析并构造初始化式.
        let init = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::LBrace,
                ..
            }))
        ) {
            let elems = match self.parse_init_list()? {
                CppExpr::InitList(v) => v,
                other => vec![other],
            };
            self.list_init_expr(&ty, elems)
        } else if matches!(
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
            // M26.4: 类类型直接初始化 `Widget w(7)` 包成 ctor 调用
            // Call(Ident("Widget"), [7]), 让 lower 走 ctor 路径而非普通赋值.
            match &ty {
                MangleType::Class(cls) if !args.is_empty() => {
                    Some(CppExpr::Call(Box::new(CppExpr::Ident(cls.clone())), args))
                },
                _ if args.len() == 1 => Some(args.into_iter().next().unwrap()),
                _ => None,
            }
        } else if matches!(
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
        // `const int N = 42;` / `constexpr int N = 1 << 4;` 这类局部常量同样
        // 要进常量表, 否则下一行的 `int t[N];` 无法折叠. 多 declarator 时逐个登记.
        if was_const {
            if let Some(e) = &init {
                if let Some(v) = self.fold_int_const_expr(e) {
                    let n = declarator.name.clone();
                    self.record_int_const(&n, v);
                }
            }
        }
        declarator.init = init;
        Ok(declarator)
    }

    /// M30.4: 解析结构化绑定 `auto [a, b, c] = init;`
    fn parse_structured_binding(
        &mut self,
        ty: &MangleType,
        _is_static: bool,
        is_volatile: bool,
    ) -> Result<CppStmt, CppParseError> {
        self.bump(); // consume '['

        // 解析绑定的成员名列表：`a`, `a, b`, `a, b, c`
        let mut names = Vec::new();
        loop {
            // 跳过空白并允许空绑定 `_`
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RBracket,
                    ..
                }))
            ) {
                break;
            }
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Comma,
                    ..
                }))
            ) {
                self.bump(); // skip leading comma
                continue;
            }
            // 支持忽略符 `_`
            let name = if matches!(&self.peek, Some(CppTok::C(Tok {
                kind: TokKind::Ident,
                text: ref t,
                ..
            })) if t == "_")
            {
                self.bump();
                "_".to_string()
            } else {
                self.expect_ident()?
            };
            names.push(name);
            // 检查是否还有更多成员
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::RBracket,
                    ..
                }))
            ) {
                break;
            }
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
            return Err(CppParseError::Unexpected(
                "]",
                "expected comma or ]".to_string(),
                self.span(),
            ));
        }
        self.bump(); // consume ']'

        // 解析初始化表达式：`= init`
        let init_expr = if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Assign,
                ..
            }))
        ) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            return Err(CppParseError::Unexpected("=", "expected init".to_string(), self.span()));
        };

        // 期待分号
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Semi,
                ..
            }))
        ) {
            self.bump();
        }

        Ok(CppStmt::StructuredBinding {
            names,
            ty: ty.clone(),
            init: init_expr.unwrap_or(CppExpr::IntLit(0)),
            is_volatile,
        })
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
            is_static,
            class: _,
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
                is_static,
            }
        },
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

/// token 的规范拼写 (含 C++ 关键字 / `::`), 用于从 token 流重建源码文本.
fn tok_text(t: &CppTok) -> String {
    match t {
        CppTok::C(Tok { text, kind, .. }) => {
            if text.is_empty() {
                // 少数 token (digraph 等) text 为空, 按 kind 回退到规范拼写
                match kind {
                    TokKind::LParen => "(",
                    TokKind::RParen => ")",
                    TokKind::LBrace => "{",
                    TokKind::RBrace => "}",
                    TokKind::LBracket => "[",
                    TokKind::RBracket => "]",
                    TokKind::Comma => ",",
                    TokKind::AndAnd => "&&",
                    TokKind::OrOr => "||",
                    _ => "",
                }
                .to_string()
            } else {
                text.clone()
            }
        },
        CppTok::Kw(kw, _) => kw.as_str().to_string(),
        CppTok::ColonColon(_) => "::".to_string(),
        CppTok::RawStringLit { text, .. } => text.clone(),
    }
}

/// 是否为"只能是声明符/类型说明符"的 C 关键字 —— 约束表达式里不会出现,
/// 因此顶层遇到它就意味着 requires 子句结束. 注意 sizeof / alignof /
/// decltype / noexcept / typeid 是约束表达式的合法组成部分, 不列入.
fn is_decl_specifier_kind(kind: TokKind) -> bool {
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
            | TokKind::KwBool
            | TokKind::KwAuto
            | TokKind::KwStruct
            | TokKind::KwUnion
            | TokKind::KwEnum
            | TokKind::KwTypedef
            | TokKind::KwConst
            | TokKind::KwStatic
            | TokKind::KwInline
            | TokKind::KwExtern
            | TokKind::KwRegister
            | TokKind::KwVolatile
            | TokKind::KwChar16
            | TokKind::KwChar32
            | TokKind::KwInt128
            | TokKind::KwUInt128
    )
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

/// [dcl.type.simple]/1: 把「简单类型说明符」的**多重集合**折成唯一的基本类型。
///
/// 多词基本类型的含义由三个维度决定 —— 符号性 (`signed`/`unsigned`)、
/// 长度 (`short` / `long`×N)、基准类型 (`char`/`int`/`float`/`double`/`void`) ——
/// 并且**与词序无关**: `signed short int`、`short int`、`int signed short` 是
/// 同一个类型。因此不能按拼写字符串枚举组合: 枚举必然漏 (实测漏了
/// `signed short int` / `signed long long int`, 未命中时类型名降级为
/// `MangleType::Class("signed short int")`, sema 报 `unknown class type`,
/// 整份含 `<stdio.h>` 的 TU 全部退回 legacy 文本降级路径 —— GCC 15.3.0
/// `libcpp/*.cc` 16 个翻译单元因此无一能通过 native 后端)。
///
/// 入参是本次声明中出现的全部简单类型说明符词 (按出现顺序, 允许重复的
/// `long`)。空集合、含非简单类型说明符的集合、以及标准里不合法的组合都返回
/// `None`, 由调用方按「不是基本类型」处理。
fn canonical_simple_type(words: &[&str]) -> Option<MangleType> {
    if words.is_empty() {
        return None;
    }
    let mut unsigned: Option<bool> = None;
    let (mut long, mut short, mut char_, mut int_) = (0u8, false, false, false);
    let (mut double_, mut float_, mut void_, mut bool_) = (false, false, false, false);
    for w in words {
        match *w {
            "signed" => {
                if unsigned == Some(true) {
                    return None;
                }
                unsigned = Some(false);
            },
            "unsigned" => {
                if unsigned == Some(false) {
                    return None;
                }
                unsigned = Some(true);
            },
            "long" => long += 1,
            "short" => short = true,
            "char" => char_ = true,
            "int" => int_ = true,
            "double" => double_ = true,
            "float" => float_ = true,
            "void" => void_ = true,
            "bool" | "_Bool" => bool_ = true,
            // 不是简单类型说明符 (类名 / `auto` / tag 名等)
            _ => return None,
        }
    }
    // `void` / `bool` 只能单独出现
    if void_ || bool_ {
        return (words.len() == 1).then(|| if void_ { MangleType::Void } else { MangleType::Bool });
    }
    if float_ {
        // `float` 不与其他说明符组合 (`long float` 在 GNU C 里是警告, C++ 里非法)
        if long != 0 || double_ || short || char_ || int_ {
            return None;
        }
        return Some(MangleType::Float);
    }
    if double_ {
        // `int double` / `char double` 等不是合法组合
        if short || char_ || int_ || long > 1 {
            return None;
        }
        return Some(if long == 1 { MangleType::LongDouble } else { MangleType::Double });
    }
    // `char` 是三态的: 裸 `char` 是独立类型 (`MangleType::Char`), 与
    // `signed char` / `unsigned char` 在 [basic.types] 里是三个不同类型。
    // 其余整数类型的裸书写总是有符号的, 所以用 bool 即可。
    if char_ {
        // `char` 与长度/整型说明符不能组合
        if short || long != 0 || int_ {
            return None;
        }
        return Some(match unsigned {
            Some(true) => MangleType::Uchar,
            Some(false) => MangleType::Schar,
            None => MangleType::Char,
        });
    }
    let unsigned = unsigned == Some(true);
    if short {
        if long != 0 {
            return None;
        }
        return Some(if unsigned { MangleType::Ushort } else { MangleType::Short });
    }
    match long {
        0 => Some(if unsigned { MangleType::Uint } else { MangleType::Int }),
        1 => Some(if unsigned { MangleType::Ulong } else { MangleType::Long }),
        _ => Some(if unsigned {
            MangleType::UlongLong
        } else {
            MangleType::LongLong
        }),
    }
}

#[cfg(test)]
mod canonical_simple_type_tests {
    use super::*;

    fn t(words: &[&str]) -> Option<MangleType> {
        canonical_simple_type(words)
    }

    /// 词序无关: 同一个三个维度的集合, 无论怎么写都得同一类型。
    #[test]
    fn order_does_not_matter() {
        let expect = Some(MangleType::Short);
        for w in [
            &["short"][..],
            &["short", "int"][..],
            &["int", "short"][..],
            &["signed", "short", "int"][..],
            &["short", "int", "signed"][..],
            &["signed", "short"][..],
        ] {
            assert_eq!(t(w), expect, "组合 {:?}", w);
        }
    }

    /// 此前被拼写表漏掉、导致整个 GCC libcpp 退到 legacy 路径的写法。
    #[test]
    fn previously_missing_signed_combinations() {
        assert_eq!(t(&["signed", "short", "int"]), Some(MangleType::Short));
        assert_eq!(t(&["signed", "long", "int"]), Some(MangleType::Long));
        assert_eq!(
            t(&["signed", "long", "long", "int"]),
            Some(MangleType::LongLong)
        );
        assert_eq!(
            t(&["unsigned", "long", "long"]),
            Some(MangleType::UlongLong)
        );
    }

    #[test]
    fn single_and_multi_word_builtins() {
        assert_eq!(t(&["void"]), Some(MangleType::Void));
        assert_eq!(t(&["bool"]), Some(MangleType::Bool));
        assert_eq!(t(&["char"]), Some(MangleType::Char));
        assert_eq!(t(&["signed", "char"]), Some(MangleType::Schar));
        assert_eq!(t(&["unsigned", "char"]), Some(MangleType::Uchar));
        assert_eq!(t(&["int"]), Some(MangleType::Int));
        assert_eq!(t(&["signed"]), Some(MangleType::Int));
        assert_eq!(t(&["unsigned"]), Some(MangleType::Uint));
        assert_eq!(t(&["long"]), Some(MangleType::Long));
        assert_eq!(t(&["long", "long"]), Some(MangleType::LongLong));
        assert_eq!(t(&["float"]), Some(MangleType::Float));
        assert_eq!(t(&["double"]), Some(MangleType::Double));
        assert_eq!(t(&["long", "double"]), Some(MangleType::LongDouble));
    }

    /// 非法组合与非简单类型说明符必须返回 None (交给调用方按类名处理)。
    #[test]
    fn rejects_illegal_and_non_builtin_sets() {
        assert_eq!(t(&[]), None);
        assert_eq!(t(&["auto"]), None);
        assert_eq!(t(&["MyClass"]), None);
        assert_eq!(t(&["signed", "unsigned"]), None);
        assert_eq!(t(&["void", "int"]), None);
        assert_eq!(t(&["long", "long", "double"]), None);
        assert_eq!(t(&["short", "long"]), None);
        assert_eq!(t(&["char", "int"]), None);
    }
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
        },
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

/// 把整数字面量文本变成 AST 节点, 并**保留 [lex.icon] 推出来的类型**。
///
/// `CppExpr::IntLit(i64)` 只带值 —— 后缀 (`1L`/`0xFFu`) 与基数在词法/语法边界
/// 就被剥掉了, 语义层因此永远只能把字面量当 `int`:
///   * `long sh = 1L << 40;` 在 32 位里移位得 0 (g++ 按 `long` 得 1099511627776);
///   * `0xFFFFFFFF * 2` 本该是 `unsigned int` 的 32 位绕回, 却按有符号算。
///
/// 类型真源在 [`crate::cpp::const_text::int_literal_type`]。推出来不是 `int` 时,
/// 用已有的 `Cast` 节点把类型交给语义层 —— 不改动 `IntLit` 的形状 (它有 ~30 处
/// 匹配点, 包括并行开发中的 sema/lower/coroutine_lower), 而 `Cast` 在所有常量
/// 语境里已经是透明的 (`fold_int_const_expr` / `cpp_const_eval::eval_cpp_const`
/// 都会直接递归内层), 所以 case 标签、数组维度、模板非类型实参等不会因它失配。
fn int_literal_expr(text: &str, v: i64) -> CppExpr {
    let lit = CppExpr::IntLit(v);
    let Some((bits, signed, long_long)) = crate::cpp::const_text::int_literal_type(text) else {
        return lit;
    };
    // 秩与符号性和默认 `int` 完全一致时不包壳, 保持原有 AST 形态
    if bits == 32 && signed {
        return lit;
    }
    let ty = match (bits, signed, long_long) {
        (32, false, _) => MangleType::Uint,
        (64, true, false) => MangleType::Long,
        (64, true, true) => MangleType::LongLong,
        (64, false, false) => MangleType::Ulong,
        _ => MangleType::UlongLong,
    };
    CppExpr::Cast(ty, Box::new(lit))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 核心正确性要求 ([dcl.name]): 指针 / 引用 / 数组层级属于 **declarator**,
    /// 不属于 decl-specifier —— `int *p, q, *r[2];` 里 `q` 的类型是 `int`,
    /// 只有 `p` 是指针、`r` 是数组。一条声明必须输出成**一个**
    /// `CppStmt::LocalVarList` (for-init 位置只容得下一个 declaration-statement).
    #[test]
    fn multi_init_declarator_owns_its_pointer_layers() {
        let src = b"int f() { int *p, q, *r[2]; return 0; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let body = match &decls[0] {
            CppDecl::FreeFn { body, .. } => body,
            other => panic!("预期自由函数, 实得 {:?}", other),
        };
        let (spec, declarators) = match &body.stmts[0] {
            CppStmt::LocalVarList { spec, declarators } => (spec, declarators),
            other => panic!("预期 LocalVarList, 实得 {:?}", other),
        };
        // 共享的 decl-specifier 只带剥掉所有层级后的基础类型.
        assert_eq!(spec.ty, MangleType::Int);
        assert_eq!(declarators.len(), 3);
        assert_eq!(
            declarators
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["p", "q", "r"]
        );
        assert_eq!(declarators[0].layers, vec![DeclaratorLayer::Pointer]);
        assert!(declarators[1].layers.is_empty());
        assert_eq!(declarators[1].full_ty(spec), MangleType::Int);
        assert_eq!(declarators[0].full_ty(spec), MangleType::Pointer(Box::new(MangleType::Int)));
        // `int *r[2]` = 2 个 int* 的数组
        assert_eq!(declarators[2].array_dims, vec![2]);
        assert_eq!(
            declarators[2].full_ty(spec),
            MangleType::Array(Box::new(MangleType::Pointer(Box::new(MangleType::Int))), 2)
        );
    }

    /// 全局路径: 一条声明按 declarator 展开成 N 个 `CppDecl::GlobalVar`,
    /// 每个自带完整类型 (不会共享第一个的 `*`).
    #[test]
    fn global_multi_init_declarator_expands_to_n_decls() {
        let src = b"int a = 0, *bp = 0, bv = 1;";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let vars: Vec<(String, MangleType)> = decls
            .iter()
            .filter_map(|d| match d {
                CppDecl::GlobalVar { name, ty, .. } => Some((name.clone(), ty.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(vars.len(), 3, "实得 {:?}", decls);
        assert_eq!(vars[0], ("a".to_string(), MangleType::Int));
        assert_eq!(vars[1], ("bp".to_string(), MangleType::Pointer(Box::new(MangleType::Int))));
        assert_eq!(vars[2], ("bv".to_string(), MangleType::Int));
    }

    /// 缺陷: 全局函数指针变量声明 `ret (*name)(params)` 之前被顶层容错静默
    /// 降级成 Opaque (libcpp 统一首错 `unsupported opaque declaration:
    /// void (*obstack_alloc_failed_handler) (`)。修复后必须解析成
    /// `CppDecl::GlobalVar{ ty: Pointer(Function{...}) }`, 且不得把普通函数
    /// 声明 `int f(int);` 误抢。
    #[test]
    fn global_fnptr_decl_parses_as_global_var_not_opaque() {
        let src = b"extern void (*h)(void);\n\
                    int (*cmp)(const void *, const void *);\n\
                    void (*h2)(int, char);\n\
                    int (**pp)(void);\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        // 护栏 1: 一条都不许是 Opaque。
        assert!(
            decls.iter().all(|d| !matches!(d, CppDecl::Opaque(_))),
            "全局函数指针被静默降级成 Opaque: {:?}",
            decls
        );
        // 护栏 2: 每条都是 GlobalVar 且类型为 Pointer(Function) (不能退化成 void*)。
        let tys: Vec<&MangleType> = decls
            .iter()
            .filter_map(|d| match d {
                CppDecl::GlobalVar { ty, .. } => Some(ty),
                _ => None,
            })
            .collect();
        assert_eq!(tys.len(), 4, "实得 {:?}", decls);
        for ty in &tys {
            let MangleType::Pointer(inner) = ty else {
                panic!("函数指针类型顶层应为 Pointer, 实得 {:?}", ty);
            };
            // 最内层必须是 Function (允许 `**pp` 多一层 Pointer)。
            let mut cur = inner.as_ref();
            while let MangleType::Pointer(n) = cur {
                cur = n.as_ref();
            }
            assert!(
                matches!(cur, MangleType::Function { .. }),
                "函数指针所指类型必须是 Function, 实得 {:?}",
                cur
            );
        }
        // 反护栏: `int f(int);` 仍必须是 FreeFn, 不得被新分支抢走。
        let mut q = CppParser::new(b"int f(int);", "t.cpp");
        let d = q.parse_top_level().unwrap();
        assert!(
            matches!(&d[0], CppDecl::FreeFn { .. }),
            "误把函数声明当函数指针: {:?}",
            d
        );
    }

    /// M54: 全局数组声明符的**维度必须折进类型** —— 旧实现把 `[...]` 整段
    /// `skip_array_suffix()` 吞掉, `int g[10];` 的类型退化成 `int`, 目标文件里
    /// 只分配 4 字节存储 (真实内存踩踏), 且 `g[3]=7;` 在 sema 报“不可下标”。
    /// 同时卡多维从内向外的套层与边界推导 ([dcl.init]/[dcl.array])。
    #[test]
    fn global_array_dims_fold_into_mangle_type() {
        let src = b"int g[10];\nchar s[] = \"abc\";\nint two[2][3];\nextern char ea[];\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let tys: Vec<MangleType> = decls
            .iter()
            .filter_map(|d| match d {
                CppDecl::GlobalVar { ty, .. } => Some(ty.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(tys.len(), 4, "实得 {:?}", decls);
        assert_eq!(tys[0], MangleType::Array(Box::new(MangleType::Int), 10));
        // `char s[] = "abc"` 的边界由字符串长度推出 (含结尾 `\0` => 4)
        assert_eq!(tys[1], MangleType::Array(Box::new(MangleType::Char), 4));
        // 多维从内向外套: `int two[2][3]` => Array(Array(Int, 3), 2)
        assert_eq!(
            tys[2],
            MangleType::Array(Box::new(MangleType::Array(Box::new(MangleType::Int), 3)), 2)
        );
        // 不完整数组仍保留数组层, 边界记 0 (由定义处补齐, 不能退化成标量)
        assert_eq!(tys[3], MangleType::Array(Box::new(MangleType::Char), 0));
    }

    /// M54: constexpr 函数调用必须在**解析期**就折叠 —— `constexpr int sq12 =
    /// square(12);` 既要进整型常量池 (供后续数组维度/switch 标签/enum 用),
    /// 初值本身也要**回写成字面量** (静态初始化后端不接受函数调用)。
    #[test]
    fn constexpr_call_folds_into_bound_and_init() {
        let src = b"constexpr int square(int x) { return x * x; }\n\
                    constexpr int sq12 = square(12);\n\
                    char buf[sq12];\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        assert_eq!(p.int_const_value("sq12"), Some(144));
        let buf = decls
            .iter()
            .find_map(|d| match d {
                CppDecl::GlobalVar { name, ty, .. } if name == "buf" => Some(ty.clone()),
                _ => None,
            })
            .expect("应有 buf 声明");
        assert_eq!(buf, MangleType::Array(Box::new(MangleType::Char), 144));
        // 初值已被改写成 `IntLit`, 不再是 `Call` 节点
        let sq_init = decls.iter().find_map(|d| match d {
            CppDecl::GlobalVar { name, init, .. } if name == "sq12" => init.clone(),
            _ => None,
        });
        assert!(
            matches!(sq_init, Some(CppExpr::IntLit(144))),
            "初值应折成字面量, 实得 {:?}",
            sq_init
        );
    }

    /// M54: `&&` / `||` / 三元也必须参与常量折叠 —— 否则
    /// `static_assert(A && B)` 只会报“不可求值”警告而放行, 常量上下文里的
    /// 逻辑表达式形同未实现。
    #[test]
    fn fold_int_const_expr_handles_logic_and_ternary() {
        let src = b"enum { A = 1 && 2, B = 0 || 3, C = 1 ? 7 : 9, D = (2 > 1) && (3 > 4) };";
        let mut p = CppParser::new(src, "test.cpp");
        p.parse_top_level().unwrap();
        assert_eq!(p.int_const_value("A"), Some(1));
        assert_eq!(p.int_const_value("B"), Some(1));
        assert_eq!(p.int_const_value("C"), Some(7));
        assert_eq!(p.int_const_value("D"), Some(0));
    }

    /// M54: “转换失败即不登记”的两条边界 —— 同名不同参的重载两边一起拉黑,
    /// 体内有解释器无法建模的语句 (`do {} while ()`) 则整函数不登记。两者都
    /// 只能**不折叠** (退回运行期路径), 绝不拿近似值当常量。
    #[test]
    fn constexpr_fn_registration_refuses_ambiguous_and_unsupported() {
        let overloads = b"constexpr int f(int x) { return x + 1; }\n\
                          constexpr int f(int x, int y) { return x * y; }\n";
        let mut p = CppParser::new(overloads, "test.cpp");
        p.parse_top_level().unwrap();
        assert!(
            !p.constexpr_fns().contains_key("f"),
            "同名不同参的 constexpr 重载必须两边都废除"
        );

        let unsupported = b"constexpr int g(int x) { do { x = x + 1; } while (x < 10); return x; }";
        let mut p = CppParser::new(unsupported, "test.cpp");
        p.parse_top_level().unwrap();
        assert!(!p.constexpr_fns().contains_key("g"), "体内有不可转换语句时不得登记");

        // 正向对照: 可建模的循环/递归函数正常登记
        let ok = b"constexpr int tri(int n) { int s = 0; for (int i = 1; i <= n; i = i + 1) { s = s + i; } return s; }";
        let mut p = CppParser::new(ok, "test.cpp");
        p.parse_top_level().unwrap();
        assert!(p.constexpr_fns().contains_key("tri"));
    }

    /// 成员路径: `int m_a, *m_p, m_b;` 必须展开成 3 个字段 (旧实现只留
    /// 第一个名字, 剩下的静默丢失 -> 下游 "member not found").
    #[test]
    fn member_multi_init_declarator_expands_to_n_fields() {
        let src = b"struct S { int m_a, *m_p, m_b; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let members = match decls
            .iter()
            .find(|d| matches!(d, CppDecl::Class(_)))
            .expect("应有类声明")
        {
            CppDecl::Class(c) => &c.members,
            other => panic!("预期 class, 实得 {:?}", other),
        };
        let fields: Vec<(String, MangleType)> = members
            .iter()
            .filter_map(|m| match m {
                CppDecl::MemberVar { name, ty, .. } => Some((name.clone(), ty.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(fields.len(), 3, "实得 {:?}", members);
        assert_eq!(fields[0], ("m_a".to_string(), MangleType::Int));
        assert_eq!(fields[1], ("m_p".to_string(), MangleType::Pointer(Box::new(MangleType::Int))));
        assert_eq!(fields[2], ("m_b".to_string(), MangleType::Int));
    }

    /// typedef 也是「decl-specifier + init-declarator 列表」: `typedef int C, *P,
    /// Arr[4];` 必须展开成 3 个别名, 且各自持有自己的指针 / 数组层级
    /// (`C` 是 `int`, 不跟着第一个 declarator 变 `int*`)。
    #[test]
    fn typedef_multi_init_declarator_expands_to_n_aliases() {
        let src = b"typedef int C, *P, Arr[4];\nint main() { return 0; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let aliases: Vec<(String, MangleType)> = decls
            .iter()
            .filter_map(|d| match d {
                CppDecl::Typedef { name, target } => Some((name.clone(), target.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(aliases.len(), 3, "实得 {:?}", decls);
        assert_eq!(aliases[0], ("C".to_string(), MangleType::Int));
        assert_eq!(aliases[1], ("P".to_string(), MangleType::Pointer(Box::new(MangleType::Int))));
        assert_eq!(
            aliases[2],
            ("Arr".to_string(), MangleType::Array(Box::new(MangleType::Int), 4))
        );
        // 类型名登记不依赖「`;` 前最后一个 Ident」那份启发式扫描:
        // 逗号之前的 `C` 也必须能被 `sizeof(C)` 认成类型.
        assert!(p.is_known_type_name("C"));
        assert!(p.is_known_type_name("P"));
        assert!(p.is_known_type_name("Arr"));
    }

    #[test]
    fn parse_class_with_ctor_and_dtor() {
        let src = b"class Foo { public: Foo(); ~Foo(); int bar(); };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        match &d[0] {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "Foo");
                assert_eq!(c.members.len(), 3);
            },
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
            },
            _ => panic!("expected namespace, got {:?}", d[0]),
        }
    }

    /// 缺陷: glibc++ 头写 `namespace std _GLIBCXX_VISIBILITY(default) { ... }`,
    /// 名字与 `{` 之间夹了一个属性说明符 (展开后为 `__attribute__(...)`, 未展开
    /// 时为 `IDENT(args)`)。之前顶层分发不容忍 → 整条 namespace 降级 Opaque,
    /// native 硬错。修复后三种属性形态都必须解析成 Namespace 而非 Opaque。
    #[test]
    fn parse_namespace_with_attributes() {
        let cases = [
            b"namespace std __attribute__((__visibility__(\"default\"))) { struct A {}; }".as_slice(),
            b"namespace [[gnu::hidden]] bar { struct B {}; }".as_slice(),
            b"namespace VISIBLE(default) { struct C {}; }".as_slice(),
        ];
        for src in cases {
            let mut p = CppParser::new(src, "test.cpp");
            let d = p.parse_top_level().unwrap();
            assert!(
                matches!(&d[0], CppDecl::Namespace { .. }),
                "带属性的 namespace 不得降级 Opaque: {:?}",
                d[0]
            );
        }
    }

    /// Step 1 (L) 架构地基: 命名空间体里一个成员解析失败**不得**把整块
    /// 降级成一条 Opaque, 而应像顶层 [`parse_top_level`] 与类体
    /// [`parse_member_decl_with_class`] 同构地逐成员局部恢复。真实 libstdc++ 头
    /// 的 `namespace std { ... }` 上百个成员, 若整体吞成一个 Opaque, 首错永远
    /// 停在 `std {`, 无法定位到具体成员 —— 后续每项能力 (A~K) 的增量都失去
    /// 立足点。测试中 `class ;` 强制让一个成员失败, 好成员 `struct Good`
    /// 仍必须作为独立 `CppDecl::Class` 存在在命名空间体内。
    #[test]
    fn parse_namespace_body_recovers_per_member() {
        let src = b"namespace N { class ; struct Good { int g; }; } int afterNs() { return 0; }";
        let mut p = CppParser::new(src, "t.cpp");
        let d = p.parse_top_level().unwrap_or_default();
        // 关键不变量: 一个成员失败不得让整块 namespace 变成顶层的一条
        // Opaque; namespace 之后的顶层声明也必须被继续解析。两者同时验证了
        // Step 1 (L) 把失败局部化、后续每项能力能各自推进的架构前提。
        assert!(
            d.iter().any(|x| matches!(x, CppDecl::Namespace { .. })),
            "整块 namespace 被降级成顶层 Opaque, 逐成员恢复未生效: {:?}",
            d
        );
        assert!(
            d.iter().any(|x| matches!(x, CppDecl::FreeFn { .. })),
            "namespace 之后的顶层声明被拖没: {:?}",
            d
        );
    }

    /// 修复: linkage-spec 与内部声明/块之间可插入属性序列 ([dcl.attr]):
    /// glibc++ `<exception>` 大量写
    /// `extern "C++" __attribute__((__noreturn__, __always_inline__))
    ///  inline void __terminate() noexcept { ... }`。之前 `parse_linkage_spec`
    /// 只吃字符串字面量后直接要求 `{` 或落到 decl, 属性留在流里→整条降级 Opaque。
    /// 现在复用 [`skip_attribute_specifier_seq`] (与 namespace 同一真源),
    /// `[[...]]` / `__attribute__((...))` / 未展开属性宏 `IDENT(args)` 三种形态都容错。
    #[test]
    fn extern_linkage_spec_with_attributes() {
        let cases: &[&[u8]] = &[
            b"extern \"C++\" __attribute__((__noreturn__)) void f();",
            b"extern \"C\" __attribute__((__noreturn__, __always_inline__)) void g();",
            b"extern \"C++\" [[gnu::noreturn]] void h();",
            b"extern \"C\" __attribute__((__visibility__(\"default\"))) { void k(); }",
            b"extern \"C++\" VISIBLE_MACRO(args) inline void m() noexcept { }",
        ];
        for src in cases {
            let mut p = CppParser::new(src, "t.cpp");
            let d = p.parse_top_level().unwrap();
            assert!(
                !matches!(&d[0], CppDecl::Opaque(_)),
                "带属性的 linkage-spec 不得降级 Opaque: {:?}",
                d[0]
            );
        }
    }

    /// Step 2 (A) 首个增量: 非类型**参数 pack** (`typename T, T... Ints` 与
    /// `int... N`) 必须能解析。libstdc++ `integer_sequence<T, Ints...>` / `make_index_sequence<N>`
    /// 与 pair 的 piecewise ctor 签名 (`tuple<_Args1...>&`) 都依赖它 ——
    /// 修前 `--component=libcpp` 首错直接卡在 `<typename T, T... Ints>`。
    #[test]
    fn non_type_parameter_pack_parses() {
        let cases: &[&[u8]] = &[
            b"template<typename T, T... Ints> struct seq { };",
            b"template<int... N> struct iseq { };",
            b"template<typename T, int... N> struct mix { };",
        ];
        for src in cases {
            let mut p = CppParser::new(src, "t.cpp");
            let d = p.parse_top_level().unwrap();
            assert!(
                !matches!(&d[0], CppDecl::Opaque(_)),
                "非类型参数 pack 不得降级 Opaque: {:?}",
                d[0]
            );
        }
    }

    /// Step 8 (F) 地基: `skip_specifiers` 单一真源现在同时消化
    /// `noexcept` / `override` / `final` 与其后的 GNU 属性 / C++11 `[[...]]`。
    /// libcpp 卡点形态 `void terminate() noexcept __attribute__((__noreturn__));`
    /// 直接依赖。修前仅能吃 “属性在 noexcept 之前” 一种顺序。
    #[test]
    fn function_declarator_trailing_attributes_parses() {
        let cases: &[&[u8]] = &[
            b"void f() noexcept __attribute__((__noreturn__));",
            b"void g() noexcept [[gnu::noreturn]];",
            b"struct B { virtual void m() noexcept __attribute__((__pure__)); };",
            b"void h() override __attribute__((__always_inline__));",
        ];
        for src in cases {
            let mut p = CppParser::new(src, "t.cpp");
            let d = p.parse_top_level().unwrap();
            assert!(
                d.iter().all(|x| !matches!(x, CppDecl::Opaque(_))),
                "后置属性形态不得降级 Opaque: {:?}",
                d
            );
        }
    }

    /// cpplib.h 写 `extern enum cpp_token_fld_kind cpp_token_val_index
    /// (const cpp_token *tok);` —— `enum <Tag>` 作返回类型。修前顶层分发
    /// 将 `enum ...` 无条件送到 parse_enum_decl → 遇到 `E foo()` 失败→
    /// 整条降级 Opaque。现在只有定义/opaque 声明形态 (`{`/`;`/`:`) 走定义分支，
    /// elaborated 用法落到 is_type_kw_or_spec 当类型开头处理。
    #[test]
    fn elaborated_enum_return_type_parses() {
        let cases: &[&[u8]] = &[
            b"enum E { A }; extern enum E foo();",
            b"enum E { A }; enum E foo() { return A; }",
            b"enum E { A }; struct S { enum E member; };",
        ];
        for src in cases {
            let mut p = CppParser::new(src, "t.cpp");
            let d = p.parse_top_level().unwrap();
            assert!(
                d.iter().all(|x| !matches!(x, CppDecl::Opaque(_))),
                "elaborated enum 类型不得降级 Opaque: {:?}",
                d
            );
        }
    }

    /// libcpp `internal.h:556` 写 `class mkdeps *deps;` 作类字段, 又写
    /// `extern class mkdeps *deps_init (void);` 作顶层函数返回类型。
    /// 修前 `parse_mangle_type` 专意剥除 `class` tag key (legacy C 输出旧
    /// 需求: 否则拼出 `struct class mkdeps`), 但 legacy 路径已下线且 tag
    /// key 丢失使 sema `elaborated_tag_kind()` 无法区分「首次引入 tag」(B4
    /// 按需登记为不完整类) 与「未声明名字」。现在 `class` 与 `struct`/
    /// `union`/`enum` 同形式保留 tag key，下面三种形态都不再报 `unknown class type: mkdeps`。
    #[test]
    fn elaborated_class_tag_preserved_end_to_end() {
        let cases: &[&[u8]] = &[
            b"class mkdeps *get(void);",
            b"extern class mkdeps *get(void);",
            b"void h(class mkdeps *d);",
            b"struct S { class mkdeps *deps; };",
        ];
        for src in cases {
            let mut p = CppParser::new(src, "t.cpp");
            let d = p.parse_top_level().unwrap();
            assert!(
                d.iter().all(|x| !matches!(x, CppDecl::Opaque(_))),
                "elaborated class tag 形态不得降级 Opaque: {:?}",
                d
            );
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
            },
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
    fn parse_concept_with_requires_expression_body() {
        // M51: 概念体里带 requires-expression 时, 真正的收尾分号在 `{}` 之外.
        // 旧实现撞到 `a + a;` 里那个分号就停, 残留 `} ;` 被降级成 Opaque 声明,
        // 概念没登记进注册表 (表现为 "unsupported opaque declaration: ;").
        let src = b"template <typename T> concept Addable = requires (T a) { a + a; };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert_eq!(d.len(), 1, "不应残留容错产生的额外声明: {d:?}");
        let CppDecl::Template { decl, .. } = &d[0] else {
            panic!("expected Template, got {:?}", d[0])
        };
        let CppDecl::Concept { name, body_text } = decl.as_ref() else {
            panic!("expected Concept, got {decl:?}")
        };
        assert_eq!(name, "Addable");
        assert!(body_text.starts_with("requires"), "{body_text}");
        assert!(body_text.contains('+'), "{body_text}");
        assert!(body_text.ends_with('}'), "约束体被截断: {body_text}");
    }

    #[test]
    fn parse_requires_clause_captured_in_full() {
        // M51: `requires` 子句必须完整捕获并交到 TemplateDecl.requires,
        // 否则实例化阶段无从求值约束 (concept 过滤直接失效).
        let src = b"template <typename T> requires Integral<T> T identity(T x) { return x; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        let CppDecl::Template { requires, decl, .. } = &d[0] else {
            panic!("expected Template, got {:?}", d[0])
        };
        let cond = requires.clone().expect("requires 子句必须被捕获");
        assert!(cond.contains("Integral"), "{cond}");
        assert!(cond.contains('<') && cond.contains('>'), "实参表丢失: {cond}");
        assert!(
            matches!(decl.as_ref(), CppDecl::FreeFn { .. }),
            "被约束的函数声明未保留: {decl:?}"
        );
    }

    #[test]
    fn deep_template_nesting_is_a_fatal_parse_error() {
        // M51: 超深嵌套必须是硬错误 (is_fatal), 不参与"跳到分隔符后继续"的
        // 容错恢复 —— 否则整棵超深类型会被降级成超长 Opaque 原文, 既丢了
        // C0004 诊断码, 又拖慢后续阶段 (表现为挂起).
        let depth = MAX_TEMPLATE_DEPTH as usize + 1;
        let mut src = String::new();
        src.push_str("int take(");
        src.push_str(&"Wrap<".repeat(depth));
        src.push_str("int");
        src.push_str(&">".repeat(depth));
        src.push_str("* p);");
        // 深度上限 900 意味着报错前递归链就有 901 帧, `cargo test` 默认的
        // 2MiB 线程栈不够 (真实的 dcc 二进制跑在 8MiB 主线程栈上),
        // 故显式给足栈空间复现真实运行条件.
        let err = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut p = CppParser::new(src.as_bytes(), "test.cpp");
                p.parse_top_level()
                    .expect_err("超 MAX_TEMPLATE_DEPTH 应报错")
            })
            .expect("spawn big-stack thread")
            .join()
            .expect("parser thread must not panic");
        assert!(
            matches!(err, CppParseError::TemplateDepthExceeded { .. }),
            "应报 TemplateDepthExceeded, 实际 {err:?}"
        );
        assert!(err.is_fatal(), "深度超限必须是硬错误: {err:?}");
    }

    #[test]
    fn parse_class_template_full_specialization() {
        // M51: `template <> struct X<int> { ... }` 是 `<stdfloat>` / `<print>` 等
        // 标准头的惯用法. 旧实现 (G13) 以为 `<>` 之后直接是名字, expect_ident()
        // 撞在 struct 关键字上报错 -> 整个 namespace std 降级成 Opaque,
        // native 后端只能硬拒 (一个未解析声明拖垮一整个头).
        //
        // 本用例同时钉住全特化建模的三个要点:
        //   * 特化实参来自名字之后的 `<int>` (不是 `template<>` 里);
        //   * 特化体自己的基类子句必须保留 (全特化不从主模板继承基类);
        //   * 成员种类必须原样保留 (成员函数 + `enum { value = 7 }` trait 惯用法).
        let src = b"template <typename T> struct formatter { int f(T v) const { return 0; } }; \
                    struct base_formatter { int pad; }; \
                    template <> struct formatter<int> : base_formatter { \
                        int f(int v) const { return 1; } \
                        enum { value = 7 }; \
                    };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert_eq!(d.len(), 3, "不应残留容错 Opaque: {:?}", d);
        assert!(matches!(&d[0], CppDecl::Template { .. }), "{:?}", d[0]);
        let CppDecl::FullSpec {
            specific_args,
            class,
        } = &d[2]
        else {
            panic!("expected FullSpec, got {:?}", d[2])
        };
        assert_eq!(class.name, "formatter");
        // 实参写在名字之后, 必须被收集 (否则 find_full_spec 永不匹配)
        assert_eq!(specific_args.as_slice(), &[MangleType::Int], "{specific_args:?}");
        assert_eq!(class.bases.len(), 1, "特化体基类子句丢失: {:?}", class.bases);
        assert_eq!(class.bases[0].name, "base_formatter");
        assert_eq!(class.members.len(), 2, "特化体成员丢失: {:?}", class.members);
        assert!(matches!(&class.members[0], CppDecl::MemberFn { .. }), "{:?}", class.members[0]);
        assert!(matches!(&class.members[1], CppDecl::Enum { .. }), "{:?}", class.members[1]);
    }

    #[test]
    fn parse_out_of_line_ctor_and_dtor_definitions() {
        // M55: 类外的构造/析构**定义** —— `B::B(int a) {}` / `B::~B() {}` /
        // `ns::Cls::Cls(int)`。这类写法没有 decl-specifier: `B::` 是
        // nested-name-specifier, 紧跟的 `B` 是声明符名字。旧实现把整条限定链
        // 贪心吞成返回类型, 于是在取名字处撞上 `(` 失败, 整条定义降级成 Opaque
        // (方法体与初始化列表全丢), 而旧夹具只写过**类内**定义, 从未覆盖该形式。
        let src = b"struct B { int x; B(int a); ~B(); }; \
                    B::B(int a) : x(a) {} \
                    B::~B() {} \
                    namespace ns { struct C { int v; C(int a); }; } \
                    ns::C::C(int a) : v(a) {}";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert_eq!(d.len(), 5, "应得到 5 条声明且无容错 Opaque: {:?}", d);
        assert!(matches!(&d[0], CppDecl::Class(_)));
        let CppDecl::Ctor {
            class, inits, body, ..
        } = &d[1]
        else {
            panic!("expected out-of-line Ctor, got {:?}", d[1])
        };
        assert_eq!(class, "B");
        assert_eq!(inits.len(), 1, "初始化列表丢失: {:?}", inits);
        assert_eq!(inits[0].name, "x");
        assert!(!body.is_decl, "构造体应为定义而非声明");
        let CppDecl::Dtor {
            class: dcls, body, ..
        } = &d[2]
        else {
            panic!("expected out-of-line Dtor, got {:?}", d[2])
        };
        assert_eq!(dcls, "B");
        assert!(!body.is_decl, "析构体应为定义而非声明");
        assert!(matches!(&d[3], CppDecl::Namespace { .. }));
        let CppDecl::Ctor {
            class: ccls, inits, ..
        } = &d[4]
        else {
            panic!("expected namespaced out-of-line Ctor, got {:?}", d[4])
        };
        assert_eq!(
            ccls, "ns::C",
            "类外定义的类名必须保留嵌套名说明符: 丢前缀会让 sema 解析不到类, 同名类也会撞在一起"
        );
        assert_eq!(inits.len(), 1);
    }

    #[test]
    fn parse_multi_segment_qualified_definitions() {
        // 多段限定名的类外定义: `ns::Cls::method()` / `ns::Cls::~Cls()` /
        // `a::b::Cls::method()`。旧实现的「函数 vs 变量」前瞻只认**一段**限定
        // (`Class::method(`), 多段限定名整条降级成 Opaque; 析构分支又只 pop 最后一段
        // 当类名, 丢掉命名空间前缀 (sema: unknown class 'Point')。
        let src = b"namespace geo { struct Point { int x; Point(); int sum() const; ~Point(); }; \
                    int total(const Point& p); } \
                    int geo::Point::sum() const { return x; } \
                    geo::Point::~Point() {} \
                    int geo::total(const geo::Point& p) { return p.x; }";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        assert!(
            !d.iter().any(|x| matches!(x, CppDecl::Opaque(_))),
            "不应有容错 Opaque: {:?}",
            d.iter()
                .filter_map(|x| match x {
                    CppDecl::Opaque(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
        let CppDecl::MemberFn {
            class, name, body, ..
        } = &d[1]
        else {
            panic!("expected namespaced out-of-line MemberFn, got {:?}", d[1])
        };
        assert_eq!(class, "geo::Point");
        assert_eq!(name, "sum");
        assert!(!body.is_decl);
        let CppDecl::Dtor { class: dcls, .. } = &d[2] else {
            panic!("expected namespaced out-of-line Dtor, got {:?}", d[2])
        };
        assert_eq!(dcls, "geo::Point", "命名空间里的类外析构必须保留限定前缀");
        // `int geo::total(...)` 的前缀是命名空间而非类: parser 只看词形, 发成
        // MemberFn{class:"geo"} 交由持有符号表的 sema 定夺 ([basic.def.outside])。
        let CppDecl::MemberFn {
            class: fcls,
            name: fname,
            ..
        } = &d[3]
        else {
            panic!("expected namespaced free-function definition, got {:?}", d[3])
        };
        assert_eq!(fcls, "geo");
        assert_eq!(fname, "total");
    }

    #[test]
    fn full_specialization_probe_keeps_primary_templates() {
        // M51: 只有 `template <>` (空参数表) 才是全特化. 旧实现"扫到闭合 > 前没看见
        // typename/class 就算全特化", 于是 `template <int N> struct Buf` (非类型
        // 形参主模板) 被吞进全特化分支, 主模板根本不进注册表.
        let src = b"template <int N> struct Buf { int cap() const { return N; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let d = p.parse_top_level().unwrap();
        let CppDecl::Template { params, decl, .. } = &d[0] else {
            panic!("expected 主模板 Template, got {:?}", d[0])
        };
        assert_eq!(params.params.len(), 1, "非类型形参丢失: {:?}", params);
        assert!(params.params[0].non_type.is_some(), "N 应记录为非类型形参");
        assert!(matches!(decl.as_ref(), CppDecl::Class { .. }), "{decl:?}");
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
                if variants.is_empty() {
                    println!("DEBUG: variants is EMPTY!");
                } else {
                    println!("DEBUG: variants: {:?}", variants);
                }
            },
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
                // M54: 枚举量初值文本在 parser 就折成字面量 (与前值引用
                // `A + 2` 等价, 但下游 sema 文本求值器 / legacy C 发射只需
                // 认整数字面量, 不再重复一套求值规则).
                assert_eq!(variants[1], ("B".to_string(), Some("3".to_string())));
            },
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
                assert_eq!(variants, &vec![("ANON_X".to_string(), Some("42".to_string()))]);
            },
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
                assert_eq!(variants[0], ("A".to_string(), Some("f ( 1 , 2 )".to_string())));
                assert_eq!(variants[1], ("B".to_string(), Some("3".to_string())));
            },
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    // M26.1: 数字 / 字符字面量解析 (hex/oct/bin/float/转义)
    /// 取整字面量的值, 必要时剥掉 [`int_literal_expr`] 为携带 [lex.icon] 类型而包的
    /// `Cast` 壳 (`42ULL` → `Cast(UlongLong, IntLit(42))`) —— 常量语境里 Cast 透明。
    fn int_lit_value(e: Option<&CppExpr>) -> Option<i64> {
        let mut cur = e?;
        loop {
            match cur {
                CppExpr::IntLit(v) => return Some(*v),
                CppExpr::Cast(_, inner) => cur = inner,
                _ => return None,
            }
        }
    }

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
                        CppStmt::LocalVar { init, .. } => {
                            if let Some(v) = int_lit_value(init.as_ref()) {
                                vals.push(v);
                            }
                        },
                        CppStmt::Return(e) => {
                            if let Some(v) = int_lit_value(e.as_ref()) {
                                vals.push(v);
                            }
                        },
                        _ => {},
                    }
                }
                assert!(vals.contains(&65), "hex 0x41: {:?}", vals);
                assert!(vals.contains(&493), "oct 0755: {:?}", vals);
                assert!(vals.contains(&10), "bin 0b1010: {:?}", vals);
                assert!(vals.contains(&42), "suffix 42ULL: {:?}", vals);
                assert!(vals.contains(&10), "\\n: {:?}", vals);
                assert!(vals.contains(&65), "\\x41: {:?}", vals);
            },
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
                },
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
        assert!(d.is_ok(), "parse failed: {:?}", d.as_ref().map(|v| v[0].clone()));
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
            },
            _ => panic!("expected Typedef, got {:?}", d[0]),
        }
    }

    #[test]
    fn parse_top_level_produces_opaque_on_failure() {
        // 混合可解析/不可解析的顶层声明:
        // 不可解析的声明应产生 CppDecl::Opaque, 后续声明正常解析.
        let src = b"int good_fn() { return 0; }\n\
                     @@@ invalid tokens here ;;;\n\
                     int another_good_fn() { return 1; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        // 应有至少 3 个声明: good_fn, Opaque, another_good_fn
        assert!(
            decls.len() >= 3,
            "expected >= 3 decls, got {}: {:?}",
            decls.len(),
            decls
                .iter()
                .map(|d| match d {
                    CppDecl::Opaque(_) => "Opaque",
                    CppDecl::FreeFn { name, .. } => name.as_str(),
                    _ => "other",
                })
                .collect::<Vec<_>>()
        );
        // 第一个应为 FreeFn
        assert!(matches!(&decls[0], CppDecl::FreeFn { name, .. } if name == "good_fn"));
        // 中间应有 Opaque
        let has_opaque = decls.iter().any(|d| matches!(d, CppDecl::Opaque(_)));
        assert!(has_opaque, "expected at least one Opaque decl");
        // 最后一个应为 FreeFn
        assert!(
            matches!(decls.last(), Some(CppDecl::FreeFn { name, .. }) if name == "another_good_fn")
        );
    }

    #[test]
    fn parse_body_resilient_preserves_subsequent_stmts() {
        // 函数体内含不可解析的语句时, 后续语句应保留而非整个函数体丢失.
        // parse_body 容错后应继续解析后续语句.
        let src = b"class Foo {\n\
        public:\n\
            int method() {\n\
                int x = 1;\n\
                return x;\n\
            }\n\
        };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        // 应解析出 class Foo
        assert!(!decls.is_empty());
        match &decls[0] {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "Foo");
                // method 应有函数体
                let has_method = c
                    .members
                    .iter()
                    .any(|m| matches!(m, CppDecl::MemberFn { name, .. } if name == "method"));
                assert!(has_method, "expected method member fn");
            },
            _ => panic!("expected Class, got {:?}", decls[0]),
        }
        // parser 不应有错误 (这个源文件完全可解析)
        let errors = p
            .diags
            .iter()
            .filter(|d| matches!(d.sev, crate::diagnostics::Severity::Error))
            .count();
        assert_eq!(
            errors,
            0,
            "expected no parse errors, got {}: {:?}",
            errors,
            p.diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
        );
    }

    // ===== M53 任务 3/4/6: 别名模板 template-id / 非类型实参 / 声明级 export =====

    /// 顶层声明里是否混进了容错降级的 Opaque (静默吞掉解析失败的信号).
    fn opaque_decls(decls: &[CppDecl]) -> Vec<String> {
        decls
            .iter()
            .filter_map(|d| match d {
                CppDecl::Opaque(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn qualified_alias_template_return_type_is_not_opaque() {
        // M53 任务 3 [temp.alias]: 限定名别名模板用在**全局函数返回类型**位置.
        // 旧实现 parse_free_fn 的限定名分支只把 `ns::type_of` 折成 Class(最后一段),
        // 名字前的 `<` 撞上 expect_ident -> 整条声明被顶层容错降级成 Opaque
        // (sema 侧表现为 "unsupported opaque declaration").
        let src = b"\
namespace ns {
    template <typename T> struct holder { typedef T type; };
    template <typename T> using type_of = typename holder<T>::type;
}
ns::type_of<int> h(int v) { return v; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        let fn_decl = decls
            .iter()
            .find(|d| matches!(d, CppDecl::FreeFn { name, .. } if name == "h"))
            .unwrap_or_else(|| panic!("`ns::type_of<int> h` 应解析成 FreeFn, got {:?}", decls));
        match fn_decl {
            CppDecl::FreeFn { ret, .. } => {
                // 展开了就不会残留 `Template{type_of, ..}` 形态 (别名使用点即真身)
                assert!(
                    !matches!(ret, MangleType::Template { name, .. } if name == "type_of"),
                    "限定名别名模板未展开: {:?}",
                    ret
                );
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn alias_template_expands_to_target_in_return_position() {
        // 目标类型就是形参本身的别名模板 (`using raw_t = T`): 展开后返回类型
        // 必须直接是实参 `int`, 不再带 template-id 外壳.
        let src = b"\
template <typename T> using raw_t = T;\nraw_t<int> one(int v) { return v; }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        match &decls[decls.len() - 1] {
            CppDecl::FreeFn { name, ret, .. } => {
                assert_eq!(name, "one");
                assert!(matches!(ret, MangleType::Int), "别名应展开成 int, got {:?}", ret);
            },
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn parenthesized_const_expr_nttp_is_a_value_not_opaque() {
        // M53 任务 3 [temp.arg.nontype]: 非类型实参可以是常量表达式,
        // 不限于字面量 (`S<(2 + 5)>`). 旧实现把它送进类型分支 -> 报错 -> Opaque.
        let src = b"\
template <int N> struct S { int n() const { return N; } };\n\
int f() { S<(2 + 5)> s; return s.n(); }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        assert_eq!(
            p.diags
                .iter()
                .filter(|d| matches!(d.sev, crate::diagnostics::Severity::Error))
                .count(),
            0,
            "不该有解析错误: {:?}",
            p.diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
        );
        match &decls[1] {
            CppDecl::FreeFn { body, .. } => {
                assert!(
                    matches!(&body.stmts[0], CppStmt::LocalVar { name, .. } if name == "s"),
                    "`S<(2 + 5)> s;` 应是局部变量声明, got {:?}",
                    body.stmts[0]
                );
            },
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn nttp_literal_closing_gt_is_not_a_comparison() {
        // 回归护栏: 数字实参后紧跟的 `>` 是模板实参列表收尾, 不能被当成
        // 二元比较 (`S<7> s;` 里 `7 > s`), 否则整条局部声明降级成 OpaqueStmt.
        let src = b"\
template <int N> struct S { int n() const { return N; } };\nint f() { S<7> s; return s.n(); }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        match &decls[1] {
            CppDecl::FreeFn { body, .. } => {
                assert!(
                    matches!(&body.stmts[0], CppStmt::LocalVar { ty, .. } if matches!(ty, MangleType::Template { name, args } if name == "S" && matches!(args.as_slice(), [MangleType::NonType(7)]))),
                    "`S<7> s;` 的类型应是 S<7>, got {:?}",
                    body.stmts[0]
                );
            },
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn class_template_braced_init_is_local_var_decl() {
        // M53 任务 4: `wrap<int> x{5};` 在语句位置必须是"类型跟随的花括号初始化"
        // 局部变量声明, 而不是 `x < 5` 比较表达式.
        let src = b"\
template <typename T> struct wrap { T v; wrap(T x) : v(x) {} };\nint f() { wrap<int> x{5}; return x.v; }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        match &decls[1] {
            CppDecl::FreeFn { body, .. } => match &body.stmts[0] {
                CppStmt::LocalVar { ty, name, init, .. } => {
                    assert_eq!(name, "x");
                    assert!(
                        matches!(ty, MangleType::Template { name, .. } if name == "wrap"),
                        "局部变量类型应是 wrap<int>, got {:?}",
                        ty
                    );
                    assert!(init.is_some(), "花括号初始化式不得丢失");
                },
                other => panic!("expected LocalVar, got {:?}", other),
            },
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn single_declaration_export_becomes_export_block() {
        // M53 任务 6 [module.global.frag]: `export decl` 与 `export { decl }` 等价.
        // 旧实现把 `export` 关键字透传丢弃, 模块收集侧 (exported_interface_names)
        // 看不见导出标记 -> "接口里声明 + 私有片段里定义"被误降为内部链接.
        let src = b"\
export module m;\
export int f();\
export class S {};\
export int g = 1;\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        let exported: Vec<&CppDecl> = decls
            .iter()
            .filter_map(|d| match d {
                CppDecl::ExportBlock(inner) => inner.first(),
                _ => None,
            })
            .collect();
        assert_eq!(exported.len(), 3, "三条单声明 export 都该包成 ExportBlock: {:?}", decls);
        assert!(matches!(exported[0], CppDecl::FreeFn { name, .. } if name == "f"));
        assert!(matches!(exported[1], CppDecl::Class(c) if c.name == "S"));
        assert!(matches!(exported[2], CppDecl::GlobalVar { name, .. } if name == "g"));
    }

    #[test]
    fn export_import_and_export_block_paths_keep_shape() {
        // 任务 6 共存护栏: `export import m;` 是重导出 (不是被导出的实体),
        // `export { ... }` 块与 `export template` 保持 D 已有路径的编码.
        let src = b"\
export module m;\
export import other;\
export { int e(); }\
export template <class T> struct B {};\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(opaque_decls(&decls).is_empty(), "不得降级: {:?}", opaque_decls(&decls));
        assert!(
            decls
                .iter()
                .any(|d| matches!(d, CppDecl::Import { path } if path == &[String::from("other")])),
            "`export import` 应仍是 Import: {:?}",
            decls
        );
        assert!(
            decls.iter().any(|d| matches!(d, CppDecl::ExportBlock(inner) if matches!(inner.as_slice(), [CppDecl::FreeFn { name, .. }] if name == "e"))),
            "`export {{}}` 块路径不变: {:?}",
            decls
        );
        assert!(
            decls.iter().any(|d| matches!(
                d,
                CppDecl::Template {
                    is_export: true,
                    ..
                }
            )),
            "`export template` 的 is_export 不得回退: {:?}",
            decls
        );
    }

    #[test]
    fn parse_class_member_produces_opaque_on_failure() {
        // 类成员中含不可解析构造时，应产生 Opaque 成员，
        // 后续成员正常解析。
        let src = b"struct Bar {\n\
            int x;\n\
            @@@ invalid member ;\n\
            int y;\n\
        };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        assert!(!decls.is_empty());
        match &decls[0] {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "Bar");
                // 应有 Opaque 成员
                let has_opaque = c.members.iter().any(|m| matches!(m, CppDecl::Opaque(_)));
                assert!(has_opaque, "expected Opaque member in struct Bar");
                // x 和 y 应正常解析
                let has_x = c
                    .members
                    .iter()
                    .any(|m| matches!(m, CppDecl::MemberVar { name, .. } if name == "x"));
                let has_y = c
                    .members
                    .iter()
                    .any(|m| matches!(m, CppDecl::MemberVar { name, .. } if name == "y"));
                assert!(has_x, "expected member x");
                assert!(has_y, "expected member y");
            },
            _ => panic!("expected Class, got {:?}", decls[0]),
        }
    }
}

// ===== G14: Dependent Name Analysis (Two-Phase Lookup Support) =====

use std::collections::HashSet;

/// 检查一个标识符是否是模板参数
pub fn is_template_parameter(name: &str, params: &[TemplateParam]) -> bool {
    params.iter().any(|p| p.name == name)
}

/// 检查表达式是否依赖于模板参数
pub fn is_dependent_expression(expr: &CppExpr, params: &[TemplateParam]) -> bool {
    match expr {
        // 标识符如果是模板参数名，则是依赖的
        CppExpr::Ident(name) => is_template_parameter(name, params),
        // 成员访问 obj.member - 如果 obj 是依赖的，则整个表达式是依赖的
        CppExpr::Member(obj, _member) => is_dependent_expression(obj, params),
        CppExpr::PtrMember(obj, _member) => is_dependent_expression(obj, params),
        // 调用 - 检查callee
        CppExpr::Call(callee, args) => {
            is_dependent_expression(callee, params)
                || args.iter().any(|arg| is_dependent_expression(arg, params))
        },
        // 二元/一元运算 - 操作数检查
        CppExpr::BinOp(_op, lhs, rhs) => {
            is_dependent_expression(lhs, params) || is_dependent_expression(rhs, params)
        },
        CppExpr::UnaryOp(_op, operand) => is_dependent_expression(operand, params),
        // 其他表达式类型默认非依赖
        _ => false,
    }
}

/// 分析声明中的依赖名称
pub fn analyze_dependent_names_for_decl(
    decl: &CppDecl,
    params: &[TemplateParam],
) -> DependentAnalysisResult {
    let mut dependent = HashSet::new();
    let mut non_dependent = HashSet::new();

    fn collect_identifiers<'a>(expr: &'a CppExpr, ids: &mut Vec<&'a str>) {
        match expr {
            CppExpr::Ident(name) => ids.push(name),
            CppExpr::Member(obj, member) => {
                collect_identifiers(obj, ids);
                ids.push(member);
            },
            CppExpr::PtrMember(obj, member) => {
                collect_identifiers(obj, ids);
                ids.push(member);
            },
            CppExpr::Call(f, args) => {
                collect_identifiers(f, ids);
                for arg in args {
                    collect_identifiers(arg, ids);
                }
            },
            CppExpr::BinOp(_op, lhs, rhs) => {
                collect_identifiers(lhs, ids);
                collect_identifiers(rhs, ids);
            },
            CppExpr::UnaryOp(_op, operand) => {
                collect_identifiers(operand, ids);
            },
            _ => {},
        }
    }

    // 遍历声明中的所有表达式
    match decl {
        CppDecl::FreeFn {
            ret,
            params: fn_params,
            body,
            ..
        } => {
            // 返回类型
            collect_types(ret, &mut non_dependent); // 暂时都视为非依赖
                                                    // 参数类型
            for (_name, ty) in fn_params {
                collect_types(ty, &mut non_dependent);
            }
            // 函数体
            for stmt in &body.stmts {
                collect_from_stmt(stmt, params, &mut dependent, &mut non_dependent);
            }
        },
        CppDecl::MemberFn {
            ret,
            params: fn_params,
            body,
            class,
            ..
        } => {
            collect_types(ret, &mut non_dependent);
            for (_name, ty) in fn_params {
                collect_types(ty, &mut non_dependent);
            }
            // 类名也参与依赖检查
            let all_params = {
                let mut extended = params.to_vec();
                extended.push(TemplateParam {
                    name: class.clone(),
                    default: None,
                    non_type: None,
                    is_pack: false,
                    template_params: None,
                });
                extended
            };
            for stmt in &body.stmts {
                collect_from_stmt(stmt, &all_params, &mut dependent, &mut non_dependent);
            }
        },
        CppDecl::Class(c) => {
            // 基类
            for base in &c.bases {
                collect_types_from_spec(&base.name, &mut non_dependent);
            }
            // 成员
            for member in &c.members {
                match member {
                    CppDecl::MemberVar { ty, .. } => {
                        collect_types(ty, &mut non_dependent);
                    },
                    CppDecl::Typedef { target, .. } => {
                        collect_types(target, &mut non_dependent);
                    },
                    _ => {},
                }
            }
        },
        _ => {},
    }

    DependentAnalysisResult {
        dependent_names: dependent,
        non_dependent_names: non_dependent,
    }
}

/// 从语句中收集依赖信息
fn collect_from_stmt(
    stmt: &CppStmt,
    params: &[TemplateParam],
    dependent: &mut HashSet<String>,
    non_dependent: &mut HashSet<String>,
) {
    match stmt {
        CppStmt::Expr(expr) => {
            classify_expr(expr, params, dependent, non_dependent);
        },
        CppStmt::Return(Some(e)) => {
            classify_expr(e, params, dependent, non_dependent);
        },
        CppStmt::If { cond, then, els } => {
            classify_expr(cond, params, dependent, non_dependent);
            for s in &then.stmts {
                collect_from_stmt(s, params, dependent, non_dependent);
            }
            if let Some(else_body) = els {
                for s in &else_body.stmts {
                    collect_from_stmt(s, params, dependent, non_dependent);
                }
            }
        },
        CppStmt::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(_i) = init {
                // _i is Box<CppStmt>, recursively collect from this single statement
                // This is typically a variable declaration or empty
                // For now, skip collecting (it's usually simple)
            }
            if let Some(c) = cond {
                classify_expr(c, params, dependent, non_dependent);
            }
            if let Some(_s) = step {
                // _s is Box<CppStmt>, similarly just skip for now
            }
            for stmt in &body.stmts {
                collect_from_stmt(stmt, params, dependent, non_dependent);
            }
        },
        CppStmt::While { cond, body } => {
            classify_expr(cond, params, dependent, non_dependent);
            for stmt in &body.stmts {
                collect_from_stmt(stmt, params, dependent, non_dependent);
            }
        },
        CppStmt::LocalVar { ty, init, .. } => {
            collect_types(ty, non_dependent);
            if let Some(i) = init {
                classify_expr(i, params, dependent, non_dependent);
            }
        },
        CppStmt::StaticLocal { ty, init, .. } => {
            collect_types(ty, non_dependent);
            if let Some(i) = init {
                classify_expr(i, params, dependent, non_dependent);
            }
        },
        // 多 init-declarator: 共享 decl-specifier 与每个 declarator 的初始化式
        // 都要参与依赖名分类, 否则模板体里 `Pack... a = x, b = y;` 的类型/表达式
        // 会被漏判为非依赖。
        CppStmt::LocalVarList { spec, declarators } => {
            collect_types(&spec.ty, non_dependent);
            for d in declarators {
                if let Some(i) = &d.init {
                    classify_expr(i, params, dependent, non_dependent);
                }
            }
        },
        _ => {},
    }
}

/// 分类表达式：区分依赖和非依赖名称
fn classify_expr(
    expr: &CppExpr,
    params: &[TemplateParam],
    dependent: &mut HashSet<String>,
    non_dependent: &mut HashSet<String>,
) {
    match expr {
        CppExpr::Ident(name) => {
            if is_template_parameter(name, params) {
                dependent.insert(name.clone());
            } else {
                non_dependent.insert(name.clone());
            }
        },
        CppExpr::Member(obj, member) => {
            let obj_str = match obj.as_ref() {
                CppExpr::Ident(n) => n.clone(),
                _ => "expr".to_string(),
            };
            if is_dependent_expression(expr, params) {
                dependent.insert(format!("{}.{}", obj_str, member));
            } else {
                let obj_str = match obj.as_ref() {
                    CppExpr::Ident(n) => n.clone(),
                    _ => "expr".to_string(),
                };
                non_dependent.insert(format!("{}.{}", obj_str, member));
            }
        },
        CppExpr::PtrMember(obj, member) => {
            let obj_str = match obj.as_ref() {
                CppExpr::Ident(n) => n.clone(),
                _ => "expr".to_string(),
            };
            if is_dependent_expression(expr, params) {
                dependent.insert(format!("{}.{}", obj_str, member));
            } else {
                let obj_str = match obj.as_ref() {
                    CppExpr::Ident(n) => n.clone(),
                    _ => "expr".to_string(),
                };
                non_dependent.insert(format!("{}.{}", obj_str, member));
            }
        },
        _ => {
            // 递归处理子表达式 - 收集所有标识符
            let mut ids = Vec::new();
            collect_identifiers_for_classification(expr, &mut ids);
            for id in ids {
                if is_template_parameter(&id, params) {
                    dependent.insert(id);
                } else {
                    non_dependent.insert(id);
                }
            }
        },
    }
}

/// 收集表达式中的标识符用于分类（使用 String 以避免生命周期问题）
fn collect_identifiers_for_classification(expr: &CppExpr, ids: &mut Vec<String>) {
    match expr {
        CppExpr::Ident(name) => ids.push(name.clone()),
        CppExpr::Member(obj, member) => {
            collect_identifiers_for_classification(obj, ids);
            ids.push(member.clone());
        },
        CppExpr::PtrMember(obj, member) => {
            collect_identifiers_for_classification(obj, ids);
            ids.push(member.clone());
        },
        CppExpr::Call(f, args) => {
            collect_identifiers_for_classification(f, ids);
            for arg in args {
                collect_identifiers_for_classification(arg, ids);
            }
        },
        CppExpr::BinOp(_op, lhs, rhs) => {
            collect_identifiers_for_classification(lhs, ids);
            collect_identifiers_for_classification(rhs, ids);
        },
        CppExpr::UnaryOp(_op, operand) => {
            collect_identifiers_for_classification(operand, ids);
        },
        _ => {},
    }
}

/// 从类型中提取标识符
fn collect_types(ty: &MangleType, non_dependent: &mut HashSet<String>) {
    match ty {
        MangleType::Class(name) => {
            non_dependent.insert(name.clone());
        },
        MangleType::Pointer(inner) | MangleType::Reference(inner) => {
            collect_types(inner, non_dependent);
        },
        MangleType::Template { name, args } => {
            non_dependent.insert(name.clone());
            for arg in args {
                collect_types(arg, non_dependent);
            }
        },
        MangleType::TplMember { name, member, args } => {
            non_dependent.insert(format!("{}.{}", name, member));
            for arg in args {
                collect_types(arg, non_dependent);
            }
        },
        _ => {},
    }
}

/// 从类型声明中提取标识符
fn collect_types_from_spec(spec: &str, non_dependent: &mut HashSet<String>) {
    // 简化：将完整类型声明作为非依赖标识符
    if !spec.is_empty() {
        non_dependent.insert(spec.to_string());
    }
}

/// 依赖分析结果
#[derive(Debug, Clone, Default)]
pub struct DependentAnalysisResult {
    pub dependent_names: HashSet<String>,
    pub non_dependent_names: HashSet<String>,
}
