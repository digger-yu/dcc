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

use crate::cpp::lexer::{CppLexer, CppTok, CppKw};
use crate::cpp::mangle::{MangleType, NestedName, Operator};
use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Tok, TokKind};

/// 解析错误
#[derive(Debug, Clone)]
pub enum CppParseError {
    Unexpected(&'static str, String, Span),
    Unsupported(&'static str, Span),
    TooDeep(Span),
}

impl CppParseError {
    pub fn to_diag(&self, file: &str) -> Diagnostic {
        let mut span = self.span();
        span.file = file.to_string();
        match self {
            Self::Unexpected(want, got, _) => Diagnostic::err(
                "C0001",
                format!("expected {}, got '{}'", want, got),
                span,
            ),
            Self::Unsupported(what, _) => Diagnostic::err(
                "C0002",
                format!("C++ feature not yet supported: {}", what),
                span,
            ).with_hint(
                "M8 implements most C++ features; some advanced patterns may still fail"
            ),
            Self::TooDeep(_) => Diagnostic::err(
                "C0003",
                "nesting too deep",
                span,
            ),
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Unexpected(_, _, s) | Self::Unsupported(_, s) | Self::TooDeep(s) => s.clone(),
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
pub enum Access { Public, Private, Protected }

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
        /// 是否有 `override` / `final` 修饰
        is_override: bool,
        /// 是否 const 方法
        is_const: bool,
    },
    /// 构造函数 (类外)
    Ctor {
        class: String,
        params: Vec<(String, MangleType)>,
        body: CppBody,
        is_virtual: bool,
    },
    /// 析构函数 (类外)
    Dtor {
        class: String,
        body: CppBody,
        is_virtual: bool,
    },
    /// 成员变量
    MemberVar {
        ty: MangleType,
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
    Template {
        params: TemplateArgs,
        decl: Box<CppDecl>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct CppBody {
    pub stmts: Vec<CppStmt>,
}

#[derive(Debug, Clone)]
pub enum CppStmt {
    Expr(CppExpr),
    Return(Option<CppExpr>),
    If { cond: CppExpr, then: Box<CppBody>, els: Option<Box<CppBody>> },
    While { cond: CppExpr, body: Box<CppBody> },
    For { init: Option<Box<CppStmt>>, cond: Option<CppExpr>, step: Option<Box<CppStmt>>, body: Box<CppBody> },
    Block(CppBody),
    /// `this->member = expr` 或 `this->member(args)`
    ThisMember(String, Box<CppStmt>),
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
    /// `obj.*ptr_to_member` (M8 简化: 不支持)
    Call(Box<CppExpr>, Vec<CppExpr>),
    New(String, Vec<CppExpr>),
    Delete(Box<CppExpr>),
    BinOp(BinOpKind, Box<CppExpr>, Box<CppExpr>),
    UnaryOp(UnaryOpKind, Box<CppExpr>),
    Ref(Box<CppExpr>),
    AddrOf(Box<CppExpr>),
    Cast(MangleType, Box<CppExpr>),
    /// `sizeof(T)` 或 `sizeof expr` (M8: 仅作常量返回)
    Sizeof(SizeofArg),
    /// `typeid(e)` (M8)
    Typeid(Box<CppExpr>),
    /// `dynamic_cast<T*>(e)` (M8)
    DynamicCast { ty: MangleType, expr: Box<CppExpr> },
    /// `static_cast<T>(e)` (M8)
    StaticCast { ty: MangleType, expr: Box<CppExpr> },
    /// `const_cast<T>(e)` (M8)
    ConstCast { ty: MangleType, expr: Box<CppExpr> },
    /// `reinterpret_cast<T>(e)` (M8)
    ReinterpretCast { ty: MangleType, expr: Box<CppExpr> },
    /// Lambda 表达式 (M8: 降级为闭包类型)
    Lambda(LambdaExpr),
    /// 运算符调用 (M8: 解析时合成, lower 时按运算符分派)
    OpCall {
        op: crate::cpp::mangle::Operator,
        /// 显式形参 (含左操作数); 一元/转型时只 1 个
        args: Vec<CppExpr>,
    },
    /// 三元 `cond ? a : b` (M8: 解析器支持, 留作降级)
    Ternary { cond: Box<CppExpr>, then: Box<CppExpr>, els: Box<CppExpr> },
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
    Add, Sub, Mul, Div, Mod, EqEq, Lt, Assign, Shl, Shr,
    Gt, Le, Ge, Ne, And, Or, Xor, LAnd, LOr,
    AddAssign, SubAssign, MulAssign, DivAssign, ModAssign,
    ShlAssign, ShrAssign, AndAssign, OrAssign, XorAssign,
    /// 索引下标 `a[b]` (M8: 解析为 BinOp)
    Index,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOpKind {
    Neg, Not, Plus, LNot, PreInc, PreDec, PostInc, PostDec, Deref,
}

/// C++ 解析器
pub struct CppParser<'a> {
    pub lex: CppLexer<'a>,
    pub peek: Option<CppTok>,
    pub file: String,
    pub diags: Vec<Diagnostic>,
    /// 解析期间记下的 lambda (M8: lower 时分配闭包类型名)
    pub lambdas: Vec<LambdaExpr>,
}

impl<'a> CppParser<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        let mut lex = CppLexer::new(src, file);
        let peek = lex.next_tok();
        Self { lex, peek, file: file.to_string(), diags: vec![], lambdas: vec![] }
    }

    fn bump(&mut self) -> Option<CppTok> {
        let cur = self.peek.take();
        self.peek = self.lex.next_tok();
        cur
    }

    fn span(&self) -> Span {
        match &self.peek {
            Some(t) => t.span(),
            None => Span { file: self.file.clone(), line: 0, col: 0, len: 1 },
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
            Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) => Ok(text),
            Some(other) => {
                let got = format!("{:?}", other);
                Err(CppParseError::Unexpected("identifier", got, other.span()))
            }
            None => Err(CppParseError::Unexpected("identifier", "EOF".into(), self.span())),
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
        let first = self.expect_ident()?;
        let mut parts = vec![first];
        while matches!(&self.peek, Some(CppTok::ColonColon(_))) {
            self.bump();
            parts.push(self.expect_ident()?);
        }
        Ok(parts)
    }

    /// 窥探 lexer 中第 n 个 token (n=1 表示当前 token 之后下一个)
    fn peek_token_at(&self, n: usize) -> Option<CppTok> {
        let mut clone = self.lex.clone();
        let mut result = None;
        for _ in 0..n {
            result = clone.next_tok();
        }
        result
    }

    /// 顶层解析
    pub fn parse_top_level(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let mut decls = vec![];
        while self.peek.is_some() {
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                self.bump();
                continue;
            }
            decls.push(self.parse_decl()?);
        }
        Ok(decls)
    }

    /// 解析一条声明
    fn parse_decl(&mut self) -> Result<CppDecl, CppParseError> {
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
        // class / struct Name (M8: 可继承)
        if self.at(CppKw::Class) {
            return self.parse_class_decl();
        }
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            if text == "struct" {
                return self.parse_struct_decl();
            }
        }
        // 运算符重载: Type operator op ( ... )
        if self.is_operator_fn_start() {
            return self.parse_operator_fn();
        }
        // 顶层: 全局函数 (返回类型 + 名 + 形参)
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
            return self.parse_free_fn();
        }
        Err(CppParseError::Unsupported("declaration form", self.span()))
    }

    /// 解析 template <typename T> ...
    fn parse_template_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Template)?;
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Lt, .. }))) {
            return Err(CppParseError::Unexpected("<", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut params = vec![];
        loop {
            // class / typename Name (= default)?
            let _is_ty = if self.at(CppKw::Typename) {
                self.bump(); true
            } else if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
                if text == "class" { self.bump(); true } else { false }
            } else { false };
            let name = self.expect_ident()?;
            let default = if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Assign, .. }))) {
                self.bump();
                Some(self.parse_mangle_type()?)
            } else { None };
            params.push(TemplateParam { name, default });
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                self.bump();
            } else { break; }
        }
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Gt, .. }))) {
            return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let decl = self.parse_decl()?;
        Ok(CppDecl::Template { params: TemplateArgs { params }, decl: Box::new(decl) })
    }

    /// 解析 namespace foo { ... } 或 namespace { ... }
    fn parse_namespace_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Namespace)?;
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            // inline 关键字 (C++17) 跳过
            if text == "inline" { self.bump(); }
        }
        let real_name: Option<String> = if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            if text != "{" {
                Some(self.expect_ident()?)
            } else { None }
        } else { None };
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBrace, .. }))) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut body = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RBrace, .. })) { break; }
            body.push(self.parse_decl()?);
        }
        self.bump();
        Ok(CppDecl::Namespace { name: real_name, body })
    }

    /// 解析 using namespace std; 或 using std::cout;
    fn parse_using_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Using)?;
        let is_namespace = if self.at(CppKw::Namespace) {
            self.bump(); true
        } else { false };
        let path = self.parse_qualified_name()?;
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppDecl::Using { is_namespace, path })
    }

    fn parse_class_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Class)?;
        let name = self.expect_ident()?;
        let bases = self.parse_base_list()?;
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBrace, .. }))) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut members = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RBrace, .. })) { break; }
            members.push(self.parse_member_decl_with_class(&name)?);
        }
        self.bump();
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppDecl::Class(ClassDecl {
            name, is_struct: false, bases, members,
        }))
    }

    fn parse_struct_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // 'struct' already peeked
        self.bump();
        let name = self.expect_ident()?;
        let bases = self.parse_base_list()?;
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBrace, .. }))) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut members = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RBrace, .. })) { break; }
            members.push(self.parse_member_decl_with_class(&name)?);
        }
        self.bump();
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppDecl::Class(ClassDecl {
            name, is_struct: true, bases, members,
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
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Colon, .. }))) {
            return Ok(vec![]);
        }
        self.bump();
        let mut bases = vec![];
        loop {
            let mut access = Access::Private; // struct default
            let mut is_virtual = false;
            // access spec
            if self.at(CppKw::Public) { self.bump(); access = Access::Public; }
            else if self.at(CppKw::Private) { self.bump(); access = Access::Private; }
            else if self.at(CppKw::Protected) { self.bump(); access = Access::Protected; }
            // virtual keyword
            if self.at(CppKw::Virtual) {
                self.bump();
                is_virtual = true;
            }
            let name = self.expect_ident()?;
            bases.push(BaseSpec { access, is_virtual, name });
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                self.bump();
            } else { break; }
        }
        Ok(bases)
    }

    fn parse_member_decl(&mut self) -> Result<CppDecl, CppParseError> {
        if let Some(t) = &self.peek {
            if t.is_access_spec() {
                self.bump();
                if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Colon, .. }))) {
                    return Err(CppParseError::Unexpected(":", format!("{:?}", self.peek), self.span()));
                }
                self.bump();
                return self.parse_member_decl();
            }
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Tilde, .. }))) {
            return self.parse_dtor();
        }
        // 运算符重载
        if self.is_operator_fn_start() {
            return self.parse_operator_fn();
        }
        // 跳过 `typedef` 简单形式
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            if text == "typedef" {
                // 简单 typedef 跳过
                while let Some(t) = &self.peek {
                    if matches!(t, CppTok::C(Tok { kind: TokKind::Semi, .. })) { break; }
                    self.bump();
                }
                self.bump();
                return self.parse_member_decl();
            }
        }
        // `virtual` -> 跳到下个非 virtual 修饰, 由 parse_member_fn_inline / parse_dtor 决定
        if self.at(CppKw::Virtual) {
            // 跳过后看是 ~ (dtor) 还是 type (fn)
            let next_is_tilde = if let Some(t2) = self.peek_token_at(1) {
                matches!(t2, CppTok::C(Tok { kind: TokKind::Tilde, .. }))
            } else { false };
            if next_is_tilde {
                return self.parse_dtor();
            }
            return self.parse_member_fn_inline();
        }
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
            let second = self.peek_token_at(1);
            match second {
                Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) => {
                    return self.parse_ctor();
                }
                Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) => {
                    let third = self.peek_token_at(2);
                    match third {
                        Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) => {
                            return self.parse_member_fn_inline();
                        }
                        _ => return self.parse_member_var(),
                    }
                }
                Some(CppTok::Kw(_, _)) => {
                    // 第二 token 是关键字: 可能是 `class operator+` (operator fn)
                    if self.is_operator_fn_start() {
                        return self.parse_operator_fn();
                    }
                    return Err(CppParseError::Unsupported("class member", self.span()));
                }
                _ => return Err(CppParseError::Unsupported("class member", self.span())),
            }
        }
        Err(CppParseError::Unsupported("class member", self.span()))
    }

    /// 判断当前位置是否是 `Type operator ... (...)`
    fn is_operator_fn_start(&self) -> bool {
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
            if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(1) {
                return true;
            }
        }
        false
    }

    /// 解析运算符重载函数
    fn parse_operator_fn(&mut self) -> Result<CppDecl, CppParseError> {
        // 形式 1: Type Class::operator+(...)  (类外定义)
        // 形式 2: Class::operator+(...)        (返回类型被略)
        // 形式 3: Type operator+(...)          (全局)
        // 形式 4: Class::operator()(...)        (类内 inline)
        // 形式 5: operator()(...)               (类内 inline, 无返回类型)
        let mut ret = MangleType::Void;
        let _consumed_ret = false;
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
            if !is_reserved_word(text_of(&self.peek)) {
                if let Some(CppTok::Kw(CppKw::Operator, _)) = self.peek_token_at(1) {
                    ret = self.parse_mangle_type()?;
                }
            }
        }
        let mut class: Option<String> = None;
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
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
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            // 仅声明, 无函数体
            self.bump();
            return Ok(CppDecl::OperatorFn { class, op, params, body: CppBody::default(), ret, is_unary });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::OperatorFn { class, op, params, body, ret, is_unary })
    }

    /// 解析 operator 后的运算符名 (如 + - * / << >> < > == != () [] -> etc.)
    fn parse_operator_name(&mut self) -> Result<Operator, CppParseError> {
        // 优先解析多元 (>=2 char)
        let saved = self.peek.clone();
        let t1 = self.bump();
        if let Some(t1) = t1 {
            let s1 = match &t1 {
                CppTok::C(Tok { kind: TokKind::Plus, .. }) => "+",
                CppTok::C(Tok { kind: TokKind::Minus, .. }) => "-",
                CppTok::C(Tok { kind: TokKind::Star, .. }) => "*",
                CppTok::C(Tok { kind: TokKind::Slash, .. }) => "/",
                CppTok::C(Tok { kind: TokKind::Percent, .. }) => "%",
                CppTok::C(Tok { kind: TokKind::Caret, .. }) => "^",
                CppTok::C(Tok { kind: TokKind::Amp, .. }) => "&",
                CppTok::C(Tok { kind: TokKind::Pipe, .. }) => "|",
                CppTok::C(Tok { kind: TokKind::Tilde, .. }) => "~",
                CppTok::C(Tok { kind: TokKind::Not, .. }) => "!",
                CppTok::C(Tok { kind: TokKind::Lt, .. }) => "<",
                CppTok::C(Tok { kind: TokKind::Gt, .. }) => ">",
                CppTok::C(Tok { kind: TokKind::Assign, .. }) => "=",
                CppTok::C(Tok { kind: TokKind::Comma, .. }) => ",",
                CppTok::C(Tok { kind: TokKind::LParen, .. }) => "()",
                CppTok::C(Tok { kind: TokKind::LBracket, .. }) => "[]",
                _ => "",
            };
            // 尝试 2-char 组合
            let two = format!("{}{}", s1, peek_two_str(&self.peek));
            if let Some(op) = Operator::from_token(&two) {
                // 吃掉第二个字符
                if two.len() == 2 { self.bump(); }
                return Ok(op);
            }
            if let Some(op) = Operator::from_token(s1) {
                return Ok(op);
            }
        }
        let _ = saved;
        Err(CppParseError::Unsupported("operator name", self.span()))
    }

    fn parse_param_list(&mut self) -> Result<Vec<(String, MangleType)>, CppParseError> {
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut params = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RParen, .. })) { break; }
            // 跳过 const 限定 (例如 `T const&` 表达成 const T& )
            if self.at(CppKw::Const) { self.bump(); continue; }
            let mut ty = self.parse_mangle_type()?;
            // 跳过 const 后缀限定
            if self.at(CppKw::Const) { self.bump(); }
            // 跳过 & 和 * 后缀 (C++ 中可放在标识符前)
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Amp, .. }))) {
                self.bump();
                ty = MangleType::Reference(Box::new(ty));
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Star, .. }))) {
                self.bump();
                ty = MangleType::Pointer(Box::new(ty));
            }
            let name = if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
                Some(self.expect_ident()?)
            } else { None };
            params.push((name.unwrap_or_else(|| "_".into()), ty));
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                self.bump();
            }
        }
        self.bump();
        Ok(params)
    }

    /// 解析成员变量 (支持 `int a, b, c;`)
    fn parse_member_var(&mut self) -> Result<CppDecl, CppParseError> {
        let ty = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        // 简单: 只取第一个名字, 后续 `, name` 直接吃掉
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
            self.bump();
            if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
                self.bump(); // 消耗 ident
            }
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Assign, .. }))) {
            // 简单值 (单字面量)
            self.bump();
            while let Some(t) = &self.peek {
                if matches!(t, CppTok::C(Tok { kind: TokKind::Semi, .. })) { break; }
                self.bump();
            }
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppDecl::MemberVar { ty, name })
    }

    fn parse_ctor(&mut self) -> Result<CppDecl, CppParseError> {
        let class = self.expect_ident()?;
        let params = self.parse_param_list()?;
        let is_virtual = self.skip_cv_qual() || false;
        // 可能有 `noexcept` / `override` (跳过)
        self.skip_specifiers();
        // 跳过 ctor 初始化列表 `: re(r), im(i)`
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Colon, .. }))) {
            self.bump();
            // 简单跳过到匹配的 `{` 或 `;` (平衡括号)
            let mut paren_depth: i32 = 0;
            let mut brace_depth: i32 = 0;
            loop {
                match self.peek.clone() {
                    Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) => { paren_depth += 1; self.bump(); }
                    Some(CppTok::C(Tok { kind: TokKind::RParen, .. })) => { paren_depth -= 1; self.bump(); }
                    Some(CppTok::C(Tok { kind: TokKind::LBrace, .. })) if brace_depth == 0 && paren_depth == 0 => break,
                    Some(CppTok::C(Tok { kind: TokKind::LBrace, .. })) => { brace_depth += 1; self.bump(); }
                    Some(CppTok::C(Tok { kind: TokKind::RBrace, .. })) => { brace_depth -= 1; self.bump(); }
                    Some(CppTok::C(Tok { kind: TokKind::Semi, .. })) if brace_depth == 0 && paren_depth == 0 => break,
                    Some(_) => { self.bump(); }
                    None => return Err(CppParseError::Unexpected(";", "EOF".into(), self.span())),
                }
            }
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppDecl::Ctor { class, params, body: CppBody::default(), is_virtual });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Ctor { class, params, body, is_virtual })
    }

    fn parse_dtor(&mut self) -> Result<CppDecl, CppParseError> {
        let is_virtual = if self.at(CppKw::Virtual) { self.bump(); true } else { false };
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Tilde, .. }))) {
            return Err(CppParseError::Unexpected("~", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let class = self.expect_ident()?;
        let _params = self.parse_param_list()?;
        self.skip_specifiers();
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppDecl::Dtor { class, body: CppBody::default(), is_virtual });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Dtor { class, body, is_virtual })
    }

    fn parse_mangle_type(&mut self) -> Result<MangleType, CppParseError> {
        // const 是限定符, 不进类型
        let mut is_const = false;
        if self.at(CppKw::Const) {
            self.bump();
            is_const = true;
        }
        // 接受普通 ident (含 'auto')
        let name = match self.peek.clone() {
            Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) => text,
            Some(CppTok::C(Tok { kind: TokKind::KwAuto, .. })) => "auto".to_string(),
            _ => return Err(CppParseError::Unexpected("type", format!("{:?}", self.peek), self.span())),
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
            "auto" => MangleType::Int, // M8 简化
            other => MangleType::Class(other.to_string()),
        };
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Star, .. }))) {
            self.bump();
            ty = MangleType::Pointer(Box::new(ty));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Amp, .. }))) {
            self.bump();
            ty = MangleType::Reference(Box::new(ty));
        }
        if is_const { ty = MangleType::Reference(Box::new(ty)); } // 简化: const T& -> T*
        let _ = is_const;
        Ok(ty)
    }

    fn parse_member_fn_inline(&mut self) -> Result<CppDecl, CppParseError> {
        let mut is_virtual = false;
        if self.at(CppKw::Virtual) { self.bump(); is_virtual = true; }
        let _ret = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        let is_const = self.skip_cv_qual();
        let is_override = self.skip_specifiers();
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppDecl::MemberFn {
                class: String::new(), ret: MangleType::Void, name, params,
                body: CppBody::default(),
                is_virtual, is_override, is_const,
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::MemberFn {
            class: String::new(), ret: MangleType::Void, name, params, body,
            is_virtual, is_override, is_const,
        })
    }

    fn parse_free_fn(&mut self) -> Result<CppDecl, CppParseError> {
        let _ret = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        self.skip_cv_qual();
        let body = self.parse_body()?;
        Ok(CppDecl::FreeFn { ret: MangleType::Void, name, params, body })
    }

    fn parse_body(&mut self) -> Result<CppBody, CppParseError> {
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBrace, .. }))) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut stmts = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RBrace, .. })) { break; }
            stmts.push(self.parse_stmt()?);
        }
        self.bump();
        Ok(CppBody { stmts })
    }

    fn parse_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppStmt::Block(CppBody { stmts: vec![] }));
        }
        if self.at(CppKw::Try) {
            return self.parse_try_stmt();
        }
        if self.at(CppKw::Throw) {
            return self.parse_throw_stmt();
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwIf, .. }))) {
            return self.parse_if_stmt();
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwWhile, .. }))) {
            return self.parse_while_stmt();
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwFor, .. }))) {
            return self.parse_for_stmt();
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwReturn, .. }))) {
            self.bump();
            let val = if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                Some(self.parse_expr()?)
            } else { None };
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                self.bump();
            }
            return Ok(CppStmt::Return(val));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwBreak, .. }))) {
            self.bump();
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                self.bump();
            }
            return Ok(CppStmt::Break);
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwContinue, .. }))) {
            self.bump();
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
                self.bump();
            }
            return Ok(CppStmt::Continue);
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBrace, .. }))) {
            let body = self.parse_body()?;
            return Ok(CppStmt::Block(body));
        }
        let e = self.parse_expr()?;
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
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
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            // catch (...) 或 catch (T& e)
            let (ty, name) = if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Ellipsis, .. }))) {
                self.bump();
                (None, None)
            } else {
                let ty = self.parse_mangle_type()?;
                let name = if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
                    Some(self.expect_ident()?)
                } else { None };
                (Some(ty), name)
            };
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let b = self.parse_body()?;
            catches.push(CatchClause { ty, name, body: b });
        }
        Ok(CppStmt::Try { body: Box::new(body), catches })
    }

    fn parse_throw_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.expect_kw(CppKw::Throw)?;
        let val = if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            Some(self.parse_expr()?)
        } else { None };
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppStmt::Throw(val))
    }

    fn parse_if_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        // if 已在 peek 中
        self.bump();
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let cond = self.parse_expr()?;
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let then = self.parse_body()?;
        let els = if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwElse, .. }))) {
            self.bump();
            Some(Box::new(self.parse_body()?))
        } else { None };
        Ok(CppStmt::If { cond, then: Box::new(then), els })
    }

    fn parse_while_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.bump();
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let cond = self.parse_expr()?;
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let body = self.parse_body()?;
        Ok(CppStmt::While { cond, body: Box::new(body) })
    }

    fn parse_for_stmt(&mut self) -> Result<CppStmt, CppParseError> {
        self.bump();
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let init = if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            None
        } else {
            let s = self.parse_stmt()?;
            Some(Box::new(s))
        };
        let cond = if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            Some(self.parse_expr()?)
        } else { None };
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        let step = if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
            Some(Box::new(CppStmt::Expr(self.parse_expr()?)))
        } else { None };
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
            return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let body = self.parse_body()?;
        Ok(CppStmt::For { init, cond, step, body: Box::new(body) })
    }

    fn parse_expr(&mut self) -> Result<CppExpr, CppParseError> {
        // 三元: a ? b : c
        let e = self.parse_assignment()?;
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Question, .. }))) {
            self.bump();
            let then = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Colon, .. }))) {
                return Err(CppParseError::Unexpected(":", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let els = self.parse_expr()?;
            return Ok(CppExpr::Ternary {
                cond: Box::new(e), then: Box::new(then), els: Box::new(els)
            });
        }
        Ok(e)
    }

    fn parse_assignment(&mut self) -> Result<CppExpr, CppParseError> {
        let l = self.parse_or()?;
        let asop = match &self.peek {
            Some(CppTok::C(Tok { kind: TokKind::Assign, .. })) => Some(BinOpKind::Assign),
            Some(CppTok::C(Tok { kind: TokKind::PlusEq, .. })) => Some(BinOpKind::AddAssign),
            Some(CppTok::C(Tok { kind: TokKind::MinusEq, .. })) => Some(BinOpKind::SubAssign),
            Some(CppTok::C(Tok { kind: TokKind::StarEq, .. })) => Some(BinOpKind::MulAssign),
            Some(CppTok::C(Tok { kind: TokKind::SlashEq, .. })) => Some(BinOpKind::DivAssign),
            Some(CppTok::C(Tok { kind: TokKind::PercentEq, .. })) => Some(BinOpKind::ModAssign),
            Some(CppTok::C(Tok { kind: TokKind::ShlEq, .. })) => Some(BinOpKind::ShlAssign),
            Some(CppTok::C(Tok { kind: TokKind::ShrEq, .. })) => Some(BinOpKind::ShrAssign),
            Some(CppTok::C(Tok { kind: TokKind::AmpEq, .. })) => Some(BinOpKind::AndAssign),
            Some(CppTok::C(Tok { kind: TokKind::PipeEq, .. })) => Some(BinOpKind::OrAssign),
            Some(CppTok::C(Tok { kind: TokKind::CaretEq, .. })) => Some(BinOpKind::XorAssign),
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
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::OrOr, .. }))) {
            self.bump();
            let r = self.parse_and()?;
            l = CppExpr::BinOp(BinOpKind::LOr, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_bitor()?;
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::AndAnd, .. }))) {
            self.bump();
            let r = self.parse_bitor()?;
            l = CppExpr::BinOp(BinOpKind::LAnd, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitor(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_bitxor()?;
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Pipe, .. }))) {
            self.bump();
            let r = self.parse_bitxor()?;
            l = CppExpr::BinOp(BinOpKind::Or, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitxor(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_bitand()?;
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Caret, .. }))) {
            self.bump();
            let r = self.parse_bitand()?;
            l = CppExpr::BinOp(BinOpKind::Xor, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitand(&mut self) -> Result<CppExpr, CppParseError> {
        let mut l = self.parse_equality()?;
        while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Amp, .. }))) {
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
                Some(CppTok::C(Tok { kind: TokKind::EqEq, .. })) => BinOpKind::EqEq,
                Some(CppTok::C(Tok { kind: TokKind::Ne, .. })) => BinOpKind::Ne,
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
                Some(CppTok::C(Tok { kind: TokKind::Lt, .. })) => BinOpKind::Lt,
                Some(CppTok::C(Tok { kind: TokKind::Gt, .. })) => BinOpKind::Gt,
                Some(CppTok::C(Tok { kind: TokKind::Le, .. })) => BinOpKind::Le,
                Some(CppTok::C(Tok { kind: TokKind::Ge, .. })) => BinOpKind::Ge,
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
                Some(CppTok::C(Tok { kind: TokKind::Shl, .. })) => BinOpKind::Shl,
                Some(CppTok::C(Tok { kind: TokKind::Shr, .. })) => BinOpKind::Shr,
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
                Some(CppTok::C(Tok { kind: TokKind::Plus, .. })) => BinOpKind::Add,
                Some(CppTok::C(Tok { kind: TokKind::Minus, .. })) => BinOpKind::Sub,
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
                Some(CppTok::C(Tok { kind: TokKind::Star, .. })) => BinOpKind::Mul,
                Some(CppTok::C(Tok { kind: TokKind::Slash, .. })) => BinOpKind::Div,
                Some(CppTok::C(Tok { kind: TokKind::Percent, .. })) => BinOpKind::Mod,
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
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::PlusPlus, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::PreInc, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::MinusMinus, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::PreDec, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Plus, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Plus, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Minus, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Neg, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Not, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::LNot, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Tilde, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Not, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Star, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::UnaryOp(UnaryOpKind::Deref, Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Amp, .. }))) {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(CppExpr::AddrOf(Box::new(e)));
        }
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::KwSizeof, .. }))) {
            self.bump();
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                self.bump();
                let ty = self.parse_mangle_type()?;
                if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                    return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
                }
                self.bump();
                return Ok(CppExpr::Sizeof(SizeofArg::Type(ty)));
            }
            let e = self.parse_unary()?;
            return Ok(CppExpr::Sizeof(SizeofArg::Expr(Box::new(e))));
        }
        if self.at(CppKw::Typeid) {
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            return Ok(CppExpr::Typeid(Box::new(e)));
        }
        if self.at(CppKw::DynamicCast) {
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Lt, .. }))) {
                return Err(CppParseError::Unexpected("<", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Gt, .. }))) {
                return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            return Ok(CppExpr::DynamicCast { ty, expr: Box::new(e) });
        }
        if self.at(CppKw::StaticCast) {
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Lt, .. }))) {
                return Err(CppParseError::Unexpected("<", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Gt, .. }))) {
                return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            return Ok(CppExpr::StaticCast { ty, expr: Box::new(e) });
        }
        if self.at(CppKw::ConstCast) {
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Lt, .. }))) {
                return Err(CppParseError::Unexpected("<", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Gt, .. }))) {
                return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            return Ok(CppExpr::ConstCast { ty, expr: Box::new(e) });
        }
        if self.at(CppKw::ReinterpretCast) {
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Lt, .. }))) {
                return Err(CppParseError::Unexpected("<", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let ty = self.parse_mangle_type()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Gt, .. }))) {
                return Err(CppParseError::Unexpected(">", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            return Ok(CppExpr::ReinterpretCast { ty, expr: Box::new(e) });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<CppExpr, CppParseError> {
        let mut e = self.parse_primary()?;
        loop {
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
                self.bump();
                let mut a = vec![];
                while !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                    a.push(self.parse_expr()?);
                    if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                        self.bump();
                    }
                }
                self.bump();
                e = CppExpr::Call(Box::new(e), a);
                continue;
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBracket, .. }))) {
                self.bump();
                let idx = self.parse_expr()?;
                if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RBracket, .. }))) {
                    return Err(CppParseError::Unexpected("]", format!("{:?}", self.peek), self.span()));
                }
                self.bump();
                e = CppExpr::BinOp(BinOpKind::Index, Box::new(e), Box::new(idx));
                continue;
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Dot, .. }))) {
                self.bump();
                let m = self.expect_ident()?;
                e = CppExpr::Member(Box::new(e), m);
                continue;
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Arrow, .. }))) {
                self.bump();
                let m = self.expect_ident()?;
                e = CppExpr::PtrMember(Box::new(e), m);
                continue;
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::PlusPlus, .. }))) {
                self.bump();
                e = CppExpr::UnaryOp(UnaryOpKind::PostInc, Box::new(e));
                continue;
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::MinusMinus, .. }))) {
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
        if self.at(CppKw::True) { self.bump(); return Ok(CppExpr::IntLit(1)); }
        if self.at(CppKw::False) { self.bump(); return Ok(CppExpr::IntLit(0)); }
        if self.at(CppKw::Nullptr) { self.bump(); return Ok(CppExpr::IntLit(0)); }
        if self.at(CppKw::New) {
            return self.parse_new();
        }
        if self.at(CppKw::Delete) {
            self.bump();
            let p = self.parse_expr()?;
            return Ok(CppExpr::Delete(Box::new(p)));
        }
        if let Some(CppTok::C(Tok { kind: TokKind::Number, text, .. })) = &self.peek {
            let v: i64 = text.parse().unwrap_or(0);
            self.bump();
            return Ok(CppExpr::IntLit(v));
        }
        if let Some(CppTok::C(Tok { kind: TokKind::StringLit, text, .. })) = &self.peek {
            // 字符串字面量 (拼接支持)
            let mut parts = vec![strip_quotes(text)];
            self.bump();
            while let Some(CppTok::C(Tok { kind: TokKind::StringLit, text, .. })) = &self.peek {
                parts.push(strip_quotes(text));
                self.bump();
            }
            if parts.len() == 1 {
                return Ok(CppExpr::StrLit(parts.remove(0)));
            }
            return Ok(CppExpr::StrConcat(parts));
        }
        if let Some(CppTok::C(Tok { kind: TokKind::CharLit, text, .. })) = &self.peek {
            let s = strip_quotes(text);
            self.bump();
            let v = s.chars().next().unwrap_or('\0') as i64;
            return Ok(CppExpr::IntLit(v));
        }
        if let Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) = &self.peek {
            self.bump();
            let e = self.parse_expr()?;
            if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                return Err(CppParseError::Unexpected(")", format!("{:?}", self.peek), self.span()));
            }
            self.bump();
            return Ok(e);
        }
        if let Some(CppTok::C(Tok { kind: TokKind::LBracket, .. })) = &self.peek {
            // Lambda
            return self.parse_lambda();
        }
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek.clone() {
            // qualified name?
            if matches!(&self.peek_token_at(1), Some(CppTok::ColonColon(_))) {
                let parts = self.parse_qualified_name()?;
                if let Some(last) = parts.last().cloned() {
                    let path = parts[..parts.len()-1].to_vec();
                    return Ok(CppExpr::QualifiedAccess(path, last));
                }
            }
            let name = text.clone();
            self.bump();
            // 模板实例化: Name<...>
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Lt, .. }))) {
                // 仅识别并跳过类型实参 (lower 阶段处理实际实例化)
                self.bump();
                let mut depth: i32 = 1;
                while depth > 0 {
                    match self.bump() {
                        Some(CppTok::C(Tok { kind: TokKind::Lt, .. })) => depth += 1,
                        Some(CppTok::C(Tok { kind: TokKind::Gt, .. })) => depth -= 1,
                        Some(CppTok::C(Tok { kind: TokKind::Shr, .. })) => depth -= 1,
                        _ => {}
                    }
                }
            }
            return Ok(CppExpr::Ident(name));
        }
        // 把 `auto` 当作类型占位 (M8: 不做实际类型推断)
        if let Some(CppTok::C(Tok { kind: TokKind::KwAuto, .. })) = &self.peek {
            self.bump();
            return Ok(CppExpr::Ident("auto".into()));
        }
        Err(CppParseError::Unsupported("expression", self.span()))
    }

    /// 解析 lambda: [captures](params){ body }
    fn parse_lambda(&mut self) -> Result<CppExpr, CppParseError> {
        // 已经在 [
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBracket, .. }))) {
            return Err(CppParseError::Unexpected("[", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut captures = vec![];
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RBracket, .. }))) {
            // 默认捕获 [&] / [=]
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Amp, .. }))) {
                self.bump();
                captures.push(Capture::ByRefAll);
            } else if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Assign, .. }))) {
                self.bump();
                captures.push(Capture::ByValAll);
            } else {
                loop {
                    if self.at(CppKw::This) {
                        self.bump();
                        captures.push(Capture::This);
                    } else if let Some(CppTok::C(Tok { kind: TokKind::Amp, .. })) = &self.peek {
                        self.bump();
                        let n = self.expect_ident()?;
                        captures.push(Capture::ByRef(n));
                    } else if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek.clone() {
                        let n = text.clone();
                        self.bump();
                        captures.push(Capture::ByVal(n));
                    }
                    if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                        self.bump();
                    } else { break; }
                }
            }
        }
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RBracket, .. }))) {
            return Err(CppParseError::Unexpected("]", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        // 形参列表 (可选)
        let params = if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            self.parse_param_list()?
        } else { vec![] };
        // const 等限定
        self.skip_cv_qual();
        // body
        let body = self.parse_body()?;
        let closure_name = format!("__lambda_{}", self.lambdas.len());
        let lam = LambdaExpr { captures, params, body, closure_name: closure_name.clone() };
        self.lambdas.push(lam);
        Ok(CppExpr::Lambda(LambdaExpr {
            captures: self.lambdas.last().unwrap().captures.clone(),
            params: self.lambdas.last().unwrap().params.clone(),
            body: self.lambdas.last().unwrap().body.clone(),
            closure_name,
        }))
    }

    /// 解析 new T(args) 或 new T[](args)
    fn parse_new(&mut self) -> Result<CppExpr, CppParseError> {
        self.bump(); // new
        let ty = self.expect_ident()?;
        let args = if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            self.bump();
            let mut a = vec![];
            while !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::RParen, .. }))) {
                a.push(self.parse_expr()?);
                if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                    self.bump();
                }
            }
            self.bump();
            a
        } else { vec![] };
        Ok(CppExpr::New(ty, args))
    }

    /// 跳过 const / override / final / noexcept / 0
    fn skip_cv_qual(&mut self) -> bool {
        let mut is_const = false;
        loop {
            let is = match &self.peek {
                Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) if text == "const" => { is_const = true; true }
                _ => false,
            };
            if is { self.bump(); } else { break; }
        }
        is_const
    }

    /// 跳过 override / final / noexcept / = 0 / 纯虚
    fn skip_specifiers(&mut self) -> bool {
        let mut is_override = false;
        loop {
            if self.at(CppKw::Override) { self.bump(); is_override = true; continue; }
            if self.at(CppKw::Final) { self.bump(); continue; }
            if self.at(CppKw::Noexcept) { self.bump(); continue; }
            break;
        }
        is_override
    }
}

/// 给定声明 d, 注入类名 (用于类内 inline 函数)
fn inject_class_name(d: CppDecl, class: &str, _saved: &Option<CppTok>) -> CppDecl {
    match d {
        CppDecl::MemberFn { name, ret, params, body, is_virtual, is_override, is_const, .. } => {
            // 已经是定义形式: 不再注入
            let is_extern_def = name.contains("::");
            if is_extern_def {
                CppDecl::MemberFn { class: class.to_string(), ret, name, params, body,
                    is_virtual, is_override, is_const }
            } else {
                CppDecl::MemberFn { class: class.to_string(), ret, name, params, body,
                    is_virtual, is_override, is_const }
            }
        }
        CppDecl::Ctor { params, body, is_virtual, .. } => {
            CppDecl::Ctor { class: class.to_string(), params, body, is_virtual }
        }
        CppDecl::Dtor { body, is_virtual, .. } => {
            CppDecl::Dtor { class: class.to_string(), body, is_virtual }
        }
        CppDecl::OperatorFn { op, params, body, ret, is_unary, .. } => {
            CppDecl::OperatorFn { class: Some(class.to_string()), op, params, body, ret, is_unary }
        }
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
    matches!(s, "class" | "struct" | "enum" | "union" | "template" | "namespace" | "using" |
             "if" | "else" | "while" | "for" | "do" | "return" | "break" | "continue" |
             "try" | "catch" | "throw" | "const" | "static" | "extern" | "typedef" |
             "virtual" | "explicit" | "friend" | "inline" | "operator" | "sizeof" |
             "alignof" | "typeid" | "constexpr" | "consteval" | "noexcept" | "mutable" |
             "volatile" | "register" | "auto" | "decltype" | "switch" | "case" | "default")
}

fn peek_two_str(t: &Option<CppTok>) -> &str {
    match t {
        Some(CppTok::C(Tok { kind: TokKind::Plus, .. })) => "+",
        Some(CppTok::C(Tok { kind: TokKind::Minus, .. })) => "-",
        Some(CppTok::C(Tok { kind: TokKind::Star, .. })) => "*",
        Some(CppTok::C(Tok { kind: TokKind::Slash, .. })) => "/",
        Some(CppTok::C(Tok { kind: TokKind::Percent, .. })) => "%",
        Some(CppTok::C(Tok { kind: TokKind::Caret, .. })) => "^",
        Some(CppTok::C(Tok { kind: TokKind::Amp, .. })) => "&",
        Some(CppTok::C(Tok { kind: TokKind::Pipe, .. })) => "|",
        Some(CppTok::C(Tok { kind: TokKind::Lt, .. })) => "<",
        Some(CppTok::C(Tok { kind: TokKind::Gt, .. })) => ">",
        Some(CppTok::C(Tok { kind: TokKind::Assign, .. })) => "=",
        _ => "",
    }
}

fn strip_quotes(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len()-1].to_string()
    } else if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        s[1..s.len()-1].to_string()
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
}
