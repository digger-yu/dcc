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
    /// 基类名 (M8: 不支持模板基类)
    pub name: String,
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
    Template {
        params: TemplateArgs,
        decl: Box<CppDecl>,
        requires: Option<String>,
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
    /// C++20 export 块声明 (M9: 占位, 立即展开为 no-op)
    ExportBlock(Vec<CppDecl>),
    /// M11: extern "C" { ... } 块 (M18: 透传原文到 lower 输出)
    /// 原始文本作为字符串保存, lower 时整体 re-emit 为 extern "C" 块
    RawExternC(String),
    /// M11: typedef 声明 (解析为 no-op, lower 时不产生 C 输出)
    Typedef,
    /// M18.0: 空占位声明 (enum / 修饰后无实际声明时使用)
    Empty,
    /// M18.0: enum 占位声明, name 为枚举名 (lower 输出 `typedef int <name>;`)
    EnumPlaceholder(String),
}

#[derive(Debug, Clone, Default)]
pub struct CppBody {
    pub stmts: Vec<CppStmt>,
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

    #[allow(dead_code)]
    fn at_c_kw(&self, _kind: TokKind) -> bool {
        matches!(&self.peek, Some(CppTok::C(Tok { .. })))
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
            decls.push(self.parse_decl()?);
        }
        Ok(decls)
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
            return self.parse_template_decl();
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
        // class / struct Name (M8: 可继承)
        if self.at(CppKw::Class) {
            return self.parse_class_decl();
        }
        if let Some(CppTok::C(Tok {
            kind: TokKind::Ident,
            text,
            ..
        })) = &self.peek
        {
            if text == "struct" {
                return self.parse_struct_decl();
            }
        }
        // `struct` 作为 C 关键字直接出现 (dcc C lexer 把 struct 标为 KwStruct)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::KwStruct,
                ..
            }))
        ) {
            return self.parse_struct_decl();
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
                CppKw::Inline | CppKw::Static | CppKw::Const | CppKw::Volatile,
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
                // C++ 关键字: inline, static, const, volatile
                if let CppTok::Kw(k, _) = &tok {
                    if matches!(
                        k,
                        CppKw::Inline | CppKw::Static | CppKw::Const | CppKw::Volatile
                    ) {
                        j += 1;
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
            let is_fn = matches!(
                name_tok,
                Some(CppTok::C(Tok {
                    kind: TokKind::Ident,
                    ..
                }))
            ) && matches!(
                self.peek_token_at(name_pos + 1),
                Some(CppTok::C(Tok {
                    kind: TokKind::LParen,
                    ..
                }))
            );
            if is_fn {
                return self.parse_free_fn();
            }
            if self.is_operator_fn_start() {
                return self.parse_operator_fn();
            }
        }
        Err(CppParseError::Unsupported("declaration form", self.span()))
    }

    /// 解析 template <typename T> ...
    fn parse_template_decl(&mut self) -> Result<CppDecl, CppParseError> {
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
            let name = self.expect_ident()?;
            let default = if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Assign,
                    ..
                }))
            ) {
                self.bump();
                Some(self.parse_mangle_type()?)
            } else {
                None
            };
            params.push(TemplateParam { name, default });
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
        if self.at(CppKw::Class) {
            // 窥探: class Name <
            let saved_lex = self.lex.clone();
            let saved_peek = self.peek.clone();
            self.bump(); // class
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
                    members.push(self.parse_member_decl_with_class(&_name)?);
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
                let decl = CppDecl::Class(ClassDecl {
                    name: _name,
                    is_struct: false,
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
    fn parse_using_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Using)?;
        let is_namespace = if self.at(CppKw::Namespace) {
            self.bump();
            true
        } else {
            false
        };
        let path = self.parse_qualified_name()?;
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

    /// 解析 `module foo;` / `module foo:part1;` / `module;` / `export module foo;` (M9 占位)
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

    /// 解析 `import foo;` / `import <iostream>;` / `import "header.h";` (M9 占位)
    fn parse_import_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Import)?;
        let path = if matches!(
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

    /// 解析 `export { ... }` / `export module foo;` (M9 占位)
    fn parse_export_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Export)?;
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
            members.push(self.parse_member_decl_with_class(&name)?);
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
            bases,
            members,
        }))
    }

    fn parse_struct_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // 'struct' already peeked
        self.bump();
        let name = self.expect_ident()?;
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
            members.push(self.parse_member_decl_with_class(&name)?);
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
            is_struct: true,
            bases,
            members,
        }))
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
            bases.push(BaseSpec {
                access,
                is_virtual,
                name,
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
            return self.parse_template_decl();
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
                    self.bump(); // 跳过 T
                    self.bump(); // 跳过 &
                    if matches!(&self.peek, Some(CppTok::Kw(CppKw::Operator, _))) {
                        return self.parse_operator_fn();
                    }
                    // M18.0: `T& m_;` 类内引用成员变量
                    // 第一个 ident 是 type, 第二个是 name
                    let ty = MangleType::Class("_".into());
                    // 这里 type 信息已经丢失, 用 `_` 占位; lower 时按 var 解析.
                    let _ = ty;
                    if let Some(CppTok::C(Tok {
                        kind: TokKind::Ident,
                        text,
                        ..
                    })) = &self.peek
                    {
                        let name = text.clone();
                        self.bump();
                        // 默认初始化 0; lower 时用 placeholder
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
                            ty: MangleType::Reference(Box::new(MangleType::Class(
                                "__ref__".into(),
                            ))),
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
            return if is_fn {
                self.parse_member_fn_inline()
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
        self.bump(); // 跳过 `typedef`
                     // 跳过 <type> 部分 (这里简化: 只跳过 ident / 关键字 + 可能的 * / [N])
                     // 注意: 最后一个 ident 是 typedef 名称, 要留给 expect_ident.
                     // 所以 loop 中遇到 ident 时不能直接 bump, 需要 break.
                     // 简化策略: 跳过所有 * / 关键字, ident 留给 expect_ident
        loop {
            match &self.peek {
                Some(CppTok::C(Tok {
                    kind: TokKind::Star,
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
                _ => break,
            }
        }
        // 期望 ident (typedef 名)
        let _name = self.expect_ident()?;
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
        // M11 stdlib: typedef 不在 lower 阶段产生任何输出 (lower 后的 C
        // 代码会用 stddef.h 等系统头处理 size_t, NULL 等).
        Ok(CppDecl::Typedef)
    }

    /// M18.0: 解析 `enum [class] Name { ... };` 占位声明.
    /// 简化: 消耗整段 enum 声明, 记录 enum 名, lower 输出
    /// `typedef int <name>;` 以保证后续引用该 enum 类型时类型存在.
    fn parse_enum_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // 跳过 `enum`
        self.bump();
        // 跳过 `class` (scoped enum)
        if self.at(CppKw::Class) {
            self.bump();
        }
        // 收集 enum 名 (ident)
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
        // 跳过 `: type` (enum base)
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
            self.bump();
            while let Some(t) = &self.peek.clone() {
                if matches!(
                    t,
                    CppTok::C(Tok {
                        kind: TokKind::LBrace,
                        ..
                    })
                ) {
                    break;
                }
                self.bump();
            }
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
        // 消耗整个 body
        self.bump(); // {
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
        Ok(CppDecl::EnumPlaceholder(name))
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
        // 形式 B: type-kw operator...  (例: int operator==(...))
        if matches!(
            self.peek_token_at(j),
            Some(CppTok::C(Tok {
                kind: TokKind::KwConst | TokKind::KwVolatile,
                ..
            }))
        ) {
            if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(j + 2) {
                return true;
            }
        }
        if matches!(
            self.peek_token_at(j),
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
                    | TokKind::KwInline
                    | TokKind::KwBool
                    | TokKind::KwAuto
                    | TokKind::KwAtomic
                    | TokKind::KwRestrict,
                ..
            }))
        ) {
            if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(j + 1) {
                return true;
            }
        }
        false
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
                body: CppBody::default(),
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
                body: CppBody::default(),
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
                return Ok(op);
            }
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
            // 跳过 const 后缀限定
            if self.at(CppKw::Const) {
                self.bump();
            }
            // 跳过 & 和 * 后缀 (C++ 中可放在标识符前)
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Amp,
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
            params.push((name.unwrap_or_else(|| "_".into()), ty));
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
        let name = self.expect_ident()?;
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
        // 解析 ctor 初始化列表 `: re(r), im(i)`
        let mut inits: Vec<(String, CppExpr)> = vec![];
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Colon,
                ..
            }))
        ) {
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
                body: CppBody::default(),
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
        let is_noexcept = spec.is_noexcept;
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
                body: CppBody::default(),
                is_virtual,
                is_noexcept,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Dtor {
            class,
            body,
            is_virtual,
            is_noexcept,
        })
    }

    fn parse_mangle_type(&mut self) -> Result<MangleType, CppParseError> {
        self.parse_mangle_type_depth(0)
    }

    /// 带模板嵌套深度的 parse_mangle_type.
    /// depth 随递归递增; 超过 MAX_TEMPLATE_DEPTH 时报错,
    /// 避免超深嵌套模板 (T<T<...>>) 挂起解析器 (见 template_depth.rs).
    fn parse_mangle_type_depth(&mut self, depth: u32) -> Result<MangleType, CppParseError> {
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
        let mut ty = match name.as_str() {
            "void" => MangleType::Void,
            "bool" => MangleType::Bool,
            "char" => MangleType::Char,
            "short" => MangleType::Short,
            "int" => MangleType::Int,
            "long" => MangleType::Long,
            "float" => MangleType::Float,
            "double" => MangleType::Double,
            "unsigned" => MangleType::Uint,
            "signed" => MangleType::Int,
            // M17.0: `auto` 改为 Auto 占位, 由 lower 阶段基于 init 推导
            "auto" => MangleType::Auto,
            other => MangleType::Class(other.to_string()),
        };
        // 跳过 const 限定后缀 (`int const*` 等)
        if self.at(CppKw::Const) {
            self.bump();
        }
        // 吞 unsigned long long / long long / unsigned char / signed int 等组合
        // M11 stdlib: signed/unsigned 后可跟 char/short/int/long/long long
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
                let arg = self.parse_mangle_type_depth(depth + 1)?;
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
            ty = MangleType::Template {
                name: name.clone(),
                args,
            };
        }
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
        if matches!(
            &self.peek,
            Some(CppTok::C(Tok {
                kind: TokKind::Amp,
                ..
            }))
        ) {
            self.bump();
            ty = MangleType::Reference(Box::new(ty));
        }
        if is_const {
            ty = MangleType::Reference(Box::new(ty));
        } // 简化: const T& -> T*
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
                body: CppBody::default(),
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
                body: CppBody::default(),
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
        // `friend class Foo;` 简化: 接受, 跳过; 不生成任何代码
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
            // 读取类名 (如有) 然后 ;
            let _ = self.expect_ident();
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
                body: CppBody::default(),
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
        let _spec = self.skip_specifiers();
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
                body: CppBody::default(),
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
        })
    }

    fn parse_free_fn(&mut self) -> Result<CppDecl, CppParseError> {
        // 跳过函数说明符 (inline, static, virtual, explicit, ...)
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
        let params = self.parse_param_list()?;
        self.skip_cv_qual();
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
                body: CppBody::default(),
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
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
        Ok(CppBody { stmts })
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
            Ok(CppBody { stmts: vec![stmt] })
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
            return Ok(CppStmt::Block(CppBody { stmts: vec![] }));
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
            let v: i64 = text.parse().unwrap_or(0);
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
            let v = s.chars().next().unwrap_or('\0') as i64;
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
                let parts = self.parse_qualified_name()?;
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
                    let arg = self.parse_mangle_type_depth(1)?;
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
        } // end else (M18.0 qualified-name branch)
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
        let is_qualified_type = self.is_qualified_type_start();
        let ty = if is_qualified_type {
            // 解析限定名为 class 类型, 存为 (last segment, qualified path)
            let parts = self.parse_qualified_name()?;
            let last = parts.last().cloned().unwrap_or_default();
            // 用 last segment 当 Class 名 (lower 时根据 namespace ctx 还原)
            let mut ty = MangleType::Class(last);
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
            // 简化: 仅支持空参 `int x();` 或单参 `int x(0);`
            self.bump();
            let init = if !matches!(
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
            if matches!(
                &self.peek,
                Some(CppTok::C(Tok {
                    kind: TokKind::Semi,
                    ..
                }))
            ) {
                self.bump();
            }
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
}
