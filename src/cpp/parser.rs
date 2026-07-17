//! C++ 解析器 (M7 最小).
//!
//! M7 范围: class 声明 / 成员函数 / 成员变量 / this / :: / 引用 /
//! 构造 / 析构.  解析结果用 CppDecl 枚举表示, 供 lower.rs 降级
//! 到 dcc C IR.
//!
//! 限制: 不支持模板, 异常, 虚函数, 多继承, 命名空间嵌套. 解析器
//! 输出尽量保守: 遇到不支持特性时返回 LoweredAs 降级 hint 或报错.

use crate::cpp::lexer::{CppLexer, CppTok, CppKw};
use crate::cpp::mangle::{MangleType, NestedName};
use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Tok, TokKind};

/// 解析错误
#[derive(Debug, Clone)]
pub enum CppParseError {
    /// 期待 X, 但遇到 Y
    Unexpected(&'static str, String, Span),
    /// 不支持的特性, 给出 hint
    Unsupported(&'static str, Span),
    /// 嵌套过深
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
                "M7 minimum C++ subset; templates/exception/virtual not yet implemented"
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

/// 解析后的 C++ 类
#[derive(Debug, Clone)]
pub struct CppClass {
    pub name: String,
    pub members: Vec<CppDecl>,
}

/// 解析后的顶层 / 类内声明
#[derive(Debug, Clone)]
pub enum CppDecl {
    /// 自由函数 (M7: 仅全局, 无成员)
    FreeFn {
        ret: MangleType,
        name: String,
        params: Vec<(String, MangleType)>,
        body: CppBody,
    },
    /// 成员函数定义 (类外)
    MemberFn {
        class: String,
        ret: MangleType,
        name: String,
        params: Vec<(String, MangleType)>,
        body: CppBody,
    },
    /// 构造函数 (类外)
    Ctor {
        class: String,
        params: Vec<(String, MangleType)>,
        body: CppBody,
    },
    /// 析构函数 (类外)
    Dtor {
        class: String,
        body: CppBody,
    },
    /// 成员变量 (M7: 仅记录类型+名, 降级为 struct 字段)
    MemberVar {
        ty: MangleType,
        name: String,
    },
    /// 类声明
    Class {
        name: String,
        members: Vec<CppDecl>,
    },
}

/// 简化的函数体 (M7: 仅保留 stmt 列表; lower 阶段降级到 C IR)
#[derive(Debug, Clone, Default)]
pub struct CppBody {
    pub stmts: Vec<CppStmt>,
}

/// M7 语句 (最小子集: 表达式, 返回, this->member 调用, new/delete)
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
}

/// M7 表达式 (最小)
#[derive(Debug, Clone)]
pub enum CppExpr {
    IntLit(i64),
    StrLit(String),
    Ident(String),
    This,
    /// this->member
    ThisMember(String),
    /// 类作用域访问: Class::member
    QualifiedAccess(Vec<String>, String),
    /// 成员访问: obj.member
    Member(Box<CppExpr>, String),
    Call(Box<CppExpr>, Vec<CppExpr>),
    New(String, Vec<CppExpr>),
    Delete(Box<CppExpr>),
    BinOp(BinOpKind, Box<CppExpr>, Box<CppExpr>),
    UnaryOp(UnaryOpKind, Box<CppExpr>),
    Ref(Box<CppExpr>),  // T&: 降级为指针
    AddrOf(Box<CppExpr>),
    Cast(MangleType, Box<CppExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOpKind { Add, Sub, Mul, Div, Mod, EqEq, Lt, Assign, Shl, Shr }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOpKind { Neg, Not, Plus }

/// C++ 解析器
pub struct CppParser<'a> {
    pub lex: CppLexer<'a>,
    pub peek: Option<CppTok>,
    pub file: String,
    pub diags: Vec<Diagnostic>,
}

impl<'a> CppParser<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        let mut lex = CppLexer::new(src, file);
        let peek = lex.next_tok();
        Self { lex, peek, file: file.to_string(), diags: vec![] }
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

    fn at_c_kw(&self, kind: TokKind) -> bool {
        if let Some(CppTok::C(Tok { kind, .. })) = &self.peek { true } else { false }
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

    /// 顶层解析: 返回声明列表
    pub fn parse_top_level(&mut self) -> Result<Vec<CppDecl>, CppParseError> {
        let mut decls = vec![];
        while self.peek.is_some() {
            // 顶层空语句 `;` 跳过
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
        // class Name { ... };
        if self.at(CppKw::Class) {
            return self.parse_class_decl();
        }
        // 形如 `Type Class::Name(...)` 的类外成员函数定义
        // M7 阶段不支持类外成员函数定义, 留待后续
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
            // 形如 `int main() { ... }` 的全局函数
            return self.parse_free_fn();
        }
        Err(CppParseError::Unsupported("declaration form", self.span()))
    }

    fn peek_with_ident(&self) -> Option<&CppTok> {
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Ident, .. }))) {
            self.peek.as_ref()
        } else { None }
    }

    fn parse_class_decl(&mut self) -> Result<CppDecl, CppParseError> {
        self.expect_kw(CppKw::Class)?;
        let name = self.expect_ident()?;
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LBrace, .. }))) {
            return Err(CppParseError::Unexpected("{", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut members = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RBrace, .. })) { break; }
            members.push(self.parse_member_decl()?);
        }
        self.bump(); // consume RBrace
        // 类声明后通常有 `;`
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppDecl::Class { name, members })
    }

    fn parse_member_decl(&mut self) -> Result<CppDecl, CppParseError> {
        // Foo(int x) {...}         -- 构造函数
        // ~Foo() {...}              -- 析构函数
        // int bar() {...}           -- 成员函数
        // int bar();                -- 成员函数声明 (M7: 跳过)
        // int x;                    -- 成员变量
        // public: / private: / protected:
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
        // ~Name()   -> 析构
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Tilde, .. }))) {
            return self.parse_dtor();
        }
        // 简单判断: 窥探 lex 当前 pos 后的 token 流
        //   ident (                  -> ctor (M7: 假定 ident == class 名)
        //   ident ident (            -> 成员函数 (返回类型 + 名 + 参数)
        //   ident ident ; / ident ident, -> 成员变量 (返回类型 + 名, 跳过到 ;)
        //   ident ( [int] ... {       -> 函数声明
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) = &self.peek {
            // peek 第二个 token
            let second = self.peek_token_at(1);
            match second {
                Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) => {
                    // 形如 Foo(...) — 构造函数
                    return self.parse_ctor();
                }
                Some(CppTok::C(Tok { kind: TokKind::Ident, .. })) => {
                    // 形如 int bar / int x — 成员函数或成员变量
                    let third = self.peek_token_at(2);
                    match third {
                        Some(CppTok::C(Tok { kind: TokKind::LParen, .. })) => {
                            return self.parse_member_fn_inline();
                        }
                        _ => {
                            // 成员变量: 解析类型+名, 然后跳到 ; 或 { ... }
                            return self.parse_member_var();
                        }
                    }
                }
                _ => {
                    // 其它: 不支持
                    return Err(CppParseError::Unsupported("class member", self.span()));
                }
            }
        }
        Err(CppParseError::Unsupported("class member", self.span()))
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

    /// 解析成员变量: `int x;` / `int x = 0;`
    fn parse_member_var(&mut self) -> Result<CppDecl, CppParseError> {
        let ty = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        // 跳过可选的 = 初始值 (M7: 忽略表达式, 简单跳过到 ; 或 ,)
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Assign, .. }))) {
            self.bump();
            // 跳过到 ;
            while let Some(t) = &self.peek {
                if matches!(t, CppTok::C(Tok { kind: TokKind::Semi, .. })) { break; }
                self.bump();
            }
        }
        // 跳过 ;
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppDecl::MemberVar { ty, name })
    }

    fn parse_ctor(&mut self) -> Result<CppDecl, CppParseError> {
        let class = self.expect_ident()?;
        let params = self.parse_param_list()?;
        // 可选 const / override
        self.skip_cv_qual();
        // 形如 `Foo();` 是声明 (M7: 空体); `{` 是定义
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppDecl::Ctor { class, params, body: CppBody::default() });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Ctor { class, params, body })
    }

    fn parse_dtor(&mut self) -> Result<CppDecl, CppParseError> {
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Tilde, .. }))) {
            return Err(CppParseError::Unexpected("~", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let class = self.expect_ident()?;
        let _params = self.parse_param_list()?;
        self.skip_cv_qual();
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppDecl::Dtor { class, body: CppBody::default() });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::Dtor { class, body })
    }

    fn parse_param_list(&mut self) -> Result<Vec<(String, MangleType)>, CppParseError> {
        if !matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::LParen, .. }))) {
            return Err(CppParseError::Unexpected("(", format!("{:?}", self.peek), self.span()));
        }
        self.bump();
        let mut params = vec![];
        while let Some(t) = &self.peek {
            if matches!(t, CppTok::C(Tok { kind: TokKind::RParen, .. })) { break; }
            // 简单解析: type name
            let ty = self.parse_mangle_type()?;
            let name = self.expect_ident().unwrap_or_else(|_| "_".into());
            params.push((name, ty));
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Comma, .. }))) {
                self.bump();
            }
        }
        self.bump(); // )
        Ok(params)
    }

    fn parse_mangle_type(&mut self) -> Result<MangleType, CppParseError> {
        // M7: 支持 void, int, long, char, float, double, T* (前缀 *), T& (前缀 &), ClassName
        // 简化: 不解析复杂限定符
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            let name = text.clone();
            self.bump();
            // 后续 * 链
            let mut ty = match name.as_str() {
                "void" => MangleType::Void,
                "bool" => MangleType::Bool,
                "char" => MangleType::Char,
                "short" => MangleType::Short,
                "int" => MangleType::Int,
                "long" => MangleType::Long, // 简化: 不区分 long/long long
                "float" => MangleType::Float,
                "double" => MangleType::Double,
                _ => MangleType::Class(name),
            };
            while matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Star, .. }))) {
                self.bump();
                ty = MangleType::Pointer(Box::new(ty));
            }
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Amp, .. }))) {
                self.bump();
                ty = MangleType::Reference(Box::new(ty));
            }
            Ok(ty)
        } else {
            Err(CppParseError::Unexpected("type", format!("{:?}", self.peek), self.span()))
        }
    }

    fn parse_member_fn(&mut self) -> Result<CppDecl, CppParseError> {
        // Type Class::Name(...)
        // 当前用 peek_with_ident + parse_param_list 模拟
        // 简化: 直接要求 "Ident :: Ident ( ... )" 形式
        let _ret = self.parse_mangle_type()?;
        let mut parts = self.parse_qualified_name()?;
        if parts.len() < 2 {
            return Err(CppParseError::Unexpected("Class::Name", format!("{:?}", parts), self.span()));
        }
        let class = parts.remove(0);
        let name = parts.remove(0);
        let _params = self.parse_param_list()?;
        self.skip_cv_qual();
        let body = self.parse_body()?;
        Ok(CppDecl::MemberFn { class, ret: MangleType::Void, name, params: vec![], body })
    }

    fn parse_member_fn_inline(&mut self) -> Result<CppDecl, CppParseError> {
        // class 内部的 int Name(...) { ... }  (M7: 不区分 ctor / 成员函数, 都当 ctor 解析)
        // 因为 ctor 走 parse_ctor 优先路径, 这里是普通成员函数
        let _ret = self.parse_mangle_type()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        self.skip_cv_qual();
        // 声明 (no body) 用 `;` 终止
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
            return Ok(CppDecl::MemberFn {
                class: String::new(),
                ret: MangleType::Void,
                name,
                params,
                body: CppBody::default(),
            });
        }
        let body = self.parse_body()?;
        Ok(CppDecl::MemberFn {
            class: String::new(),
            ret: MangleType::Void,
            name,
            params,
            body,
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
        // return 是 C 关键字, 走 TokKind::KwReturn
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
        let e = self.parse_expr()?;
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Semi, .. }))) {
            self.bump();
        }
        Ok(CppStmt::Expr(e))
    }

    fn parse_expr(&mut self) -> Result<CppExpr, CppParseError> {
        // 简化: 支持 int lit, ident, this, this->x, new T(), delete p, call, assign
        // 详细实现可在 M8 阶段扩展
        let mut e = self.parse_primary()?;
        // 可选赋值: ident = expr
        if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Assign, .. }))) {
            self.bump();
            let r = self.parse_primary()?;
            e = CppExpr::BinOp(BinOpKind::Assign, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<CppExpr, CppParseError> {
        if self.at(CppKw::This) {
            self.bump();
            if matches!(&self.peek, Some(CppTok::C(Tok { kind: TokKind::Arrow, .. }))) {
                self.bump();
                let m = self.expect_ident()?;
                return Ok(CppExpr::ThisMember(m));
            }
            return Ok(CppExpr::This);
        }
        if self.at(CppKw::New) {
            self.bump();
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
            return Ok(CppExpr::New(ty, args));
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
        if let Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) = &self.peek {
            let name = text.clone();
            self.bump();
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
                return Ok(CppExpr::Call(Box::new(CppExpr::Ident(name)), a));
            }
            return Ok(CppExpr::Ident(name));
        }
        Err(CppParseError::Unsupported("expression", self.span()))
    }

    fn skip_cv_qual(&mut self) {
        // M7: 跳过 const / override (无意义)
        loop {
            let is = match &self.peek {
                Some(CppTok::C(Tok { kind: TokKind::Ident, text, .. })) if text == "const" => true,
                _ => false,
            };
            if is { self.bump(); } else { break; }
        }
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
            CppDecl::Class { name, members } => {
                assert_eq!(name, "Foo");
                assert_eq!(members.len(), 3);
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
}
