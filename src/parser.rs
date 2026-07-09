// src/parser.rs - 语法分析
// 第一性原理: 解析器把 token 流构造成 AST;不计算值,不检查类型(那是语义层)。
// suckless 准则: 用递归下降,每种语法一个 fn,出错时 panic-free 收集诊断后继续。
//
// M0 范围: 仅支持 int main() { return <int>; }
//         已实现:基础算术、变量声明(无初始化)、if/while、函数调用
//         推迟到 M1: for/do/switch/字符串/字符/结构体等

use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Lexer, Tok, TokKind};
use crate::ast::*;

pub struct Parser<'a> {
    lex:    Lexer<'a>,
    cur:    Tok,
    peek:   Tok,
    diags:  Vec<Diagnostic>,
    finished: bool,
}

pub fn parse(src: &[u8], file: &str) -> (Program, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    p.program()
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        let mut lex = Lexer::new(src, file);
        let cur  = lex.next();
        let peek = lex.next();
        Self { lex, cur, peek, diags: vec![], finished: false }
    }

    fn advance(&mut self) {
        self.cur = std::mem::replace(&mut self.peek, self.lex.next());
    }

    fn err_at(&self, span: Span, code: &'static str, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::err(code, msg, span)
    }

    fn err_here(&mut self, code: &'static str, msg: impl Into<String>) {
        self.diags.push(Diagnostic::err(code, msg, self.cur.span.clone()));
    }

    fn expect(&mut self, k: TokKind, what: &str) -> bool {
        if self.cur.kind == k { self.advance(); return true; }
        self.err_here("E0001", format!("expected {}, got {:?}", what, self.cur.kind));
        false
    }

    // 解析顶层: program -> decl*
    pub fn program(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut decls = vec![];
        while self.cur.kind != TokKind::Eof {
            // 跳过前导 ; (如 dcc 自身的某些空语句)
            if self.cur.kind == TokKind::Semi { self.advance(); continue; }
            match self.parse_decl() {
                Some(d) => decls.push(d),
                None    => {
                    if !self.finished {
                        // 恢复:吃一个 token
                        self.advance();
                    } else { break; }
                }
            }
        }
        (Program { decls }, std::mem::take(&mut self.diags))
    }

    // decl: int|char|long|void [static|extern] IDENT ('(' params ')') ( ';' | '{' block '}' )
    // M0: 仅 int/void 函数, int 变量 (无初始化), static/extern 略
    fn parse_decl(&mut self) -> Option<Decl> {
        let ty = self.parse_type()?;
        let (is_static, is_extern) = (false, false);
        let name = self.parse_ident()?;
        let name_span = name.span.clone();
        let name_for_decl = name.clone();
        // 区分变量 vs 函数
        if self.cur.kind == TokKind::LParen {
            self.advance();
            let params = self.parse_params()?;
            if !self.expect(TokKind::RParen, "')'") { return None; }
            let body = if self.cur.kind == TokKind::LBrace {
                self.advance();
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(b)
            } else {
                if !self.expect(TokKind::Semi, "';' or '{'") { return None; }
                None
            };
            Some(Decl::Func(FuncDecl {
                ret_ty: ty, name, params, body,
                span: name_span, // M0 简化:用名字的 span 当函数 span
                is_static, is_extern,
            }))
        } else if self.cur.kind == TokKind::Semi {
            self.advance();
            Some(Decl::Var(VarDecl {
                ty, name: name_for_decl, init: None,
                span: name_span,
                is_static, is_extern,
            }))
        } else {
            self.err_here("E0002", format!("expected ';' or '(', got {:?}", self.cur.kind));
            None
        }
    }

    fn parse_type(&mut self) -> Option<TypeRef> {
        // 极简:int / void / char / long
        let k = self.cur.kind;
        let t = match k {
            TokKind::KwInt    => int_t(),
            TokKind::KwVoid   => void_t(),
            TokKind::KwChar   => char_t(),
            TokKind::KwLong   => long_t(),
            TokKind::KwShort  => int_t(),
            TokKind::KwUnsigned => int_t(),
            _ => {
                self.err_here("E0003", format!("expected type, got {:?}", k));
                return None;
            }
        };
        self.advance();
        Some(t)
    }

    fn parse_ident(&mut self) -> Option<Ident> {
        if self.cur.kind != TokKind::Ident {
            self.err_here("E0004", format!("expected identifier, got {:?}", self.cur.kind));
            return None;
        }
        let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
        self.advance();
        Some(id)
    }

    fn parse_params(&mut self) -> Option<Vec<Param>> {
        let mut ps = vec![];
        if self.cur.kind == TokKind::RParen { return Some(ps); }
        loop {
            let ty = self.parse_type()?;
            let name = if self.cur.kind == TokKind::Ident {
                let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
                self.advance();
                Some(id)
            } else { None };
            ps.push(Param { ty, name, span: self.cur.span.clone() });
            if self.cur.kind == TokKind::Comma { self.advance(); continue; }
            break;
        }
        Some(ps)
    }

    // block: { (decl | stmt)* }
    fn parse_block(&mut self) -> Block {
        let _start = self.cur.span.line;
        let mut items = vec![];
        loop {
            if self.cur.kind == TokKind::RBrace || self.cur.kind == TokKind::Eof { break; }
            // decl: 永远是 type 开头
            if self.is_type_start() {
                if let Some(d) = self.parse_decl() { items.push(BlockItem::Decl(d)); }
                else { self.advance(); }
            } else {
                if let Some(s) = self.parse_stmt() { items.push(BlockItem::Stmt(s)); }
                else { self.advance(); }
            }
        }
        Block { items, span: self.cur.span.clone() }
    }

    fn is_type_start(&self) -> bool {
        matches!(self.cur.kind,
            TokKind::KwInt | TokKind::KwChar | TokKind::KwLong |
            TokKind::KwVoid | TokKind::KwShort | TokKind::KwUnsigned)
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let span = self.cur.span.clone();
        match self.cur.kind {
            TokKind::KwReturn => {
                self.advance();
                let val = if self.cur.kind != TokKind::Semi { Some(self.parse_expr()?) } else { None };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Return { val, span })
            }
            TokKind::Semi => { self.advance(); Some(Stmt::Expr { e: Expr::Int { v: 0, span: span.clone(), ty: int_t() }, span }) }
            TokKind::LBrace => {
                self.advance();
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(Stmt::Block { b, span })
            }
            TokKind::KwIf => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let then_ = Box::new(self.parse_stmt()?);
                let else_ = if self.cur.kind == TokKind::KwElse {
                    self.advance();
                    Some(Box::new(self.parse_stmt()?))
                } else { None };
                Some(Stmt::If { cond, then_, else_, span })
            }
            TokKind::KwWhile => {
                self.advance();
                if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let body = Box::new(self.parse_stmt()?);
                Some(Stmt::While { cond, body, span })
            }
            TokKind::KwBreak => { self.advance();
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Break { span })
            }
            TokKind::KwContinue => { self.advance();
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Continue { span })
            }
            _ => {
                let e = self.parse_expr()?;
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Expr { e, span })
            }
        }
    }

    // 表达式 - 优先级爬升 (Pratt parser)
    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Option<Expr> {
        // 前缀
        let mut lhs = self.parse_unary()?;
        loop {
            // 赋值是右结合, 优先级最低
            if self.cur.kind == TokKind::Assign {
                if min_bp > 1 { break; }
                self.advance();
                let r = self.parse_expr_bp(1)?;  // 右结合
                lhs = Expr::Assign { op: BinOp::Add, l: Box::new(lhs), r: Box::new(r),
                                      span: self.cur.span.clone(), ty: int_t() };
                continue;
            }
            let (op, lbp, rbp) = match self.cur.kind {
                TokKind::Plus    => (BinOp::Add, 1u8, 2u8),
                TokKind::Minus   => (BinOp::Sub, 1, 2),
                TokKind::Star    => (BinOp::Mul, 3, 4),
                TokKind::Slash   => (BinOp::Div, 3, 4),
                TokKind::Percent => (BinOp::Mod, 3, 4),
                TokKind::Lt      => (BinOp::Lt,  5, 6),
                TokKind::Gt      => (BinOp::Gt,  5, 6),
                TokKind::Le      => (BinOp::Le,  5, 6),
                TokKind::Ge      => (BinOp::Ge,  5, 6),
                TokKind::EqEq    => (BinOp::EqEq,5, 6),
                TokKind::Ne      => (BinOp::Ne,  5, 6),
                TokKind::Amp     => (BinOp::And, 7, 8),
                TokKind::Caret   => (BinOp::Xor, 9, 10),
                TokKind::Pipe    => (BinOp::Or,  11, 12),
                TokKind::AndAnd  => (BinOp::AndAnd, 13, 14),
                TokKind::OrOr    => (BinOp::OrOr,   13, 14),
                TokKind::Shl     => (BinOp::Shl, 5, 6),
                TokKind::Shr     => (BinOp::Shr, 5, 6),
                _ => break,
            };
            if lbp < min_bp { break; }
            self.advance();
            let r = self.parse_expr_bp(rbp)?;
            lhs = Expr::Binop { op, l: Box::new(lhs), r: Box::new(r),
                                span: self.cur.span.clone(), ty: int_t() };
        }
        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        let span = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Plus => { self.advance(); let e = self.parse_unary()?;
                Some(Expr::Unary { op: UnOp::Plus, e: Box::new(e), span, ty: int_t() }) }
            TokKind::Minus => { self.advance(); let e = self.parse_unary()?;
                Some(Expr::Unary { op: UnOp::Neg, e: Box::new(e), span, ty: int_t() }) }
            TokKind::Not  => { self.advance(); let e = self.parse_unary()?;
                Some(Expr::Unary { op: UnOp::Not, e: Box::new(e), span, ty: int_t() }) }
            TokKind::Tilde => { self.advance(); let e = self.parse_unary()?;
                Some(Expr::Unary { op: UnOp::BitNot, e: Box::new(e), span, ty: int_t() }) }
            _ => self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Option<Expr> {
        let span = self.cur.span.clone();
        let mut e = self.parse_primary()?;
        while self.cur.kind == TokKind::LParen {
            // 函数调用
            if let Expr::Ident { name, .. } = &e {
                self.advance();
                let mut args = vec![];
                if self.cur.kind != TokKind::RParen {
                    loop {
                        args.push(self.parse_expr()?);
                        if self.cur.kind == TokKind::Comma { self.advance(); continue; }
                        break;
                    }
                }
                if !self.expect(TokKind::RParen, "')'") { return None; }
                e = Expr::Call { callee: name.clone(), args, span: span.clone(), ty: int_t() };
            } else {
                self.err_here("E0005", "only identifier() can be called");
                return None;
            }
        }
        Some(e)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let span = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Number => {
                let s = &self.cur.text;
                let v = parse_int_lit(s);
                self.advance();
                Some(Expr::Int { v, span, ty: int_t() })
            }
            TokKind::Ident => {
                let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
                self.advance();
                Some(Expr::Ident { name: id, span, ty: int_t() })
            }
            TokKind::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(e)
            }
            _ => {
                self.err_here("E0006", format!("expected expression, got {:?}", self.cur.kind));
                None
            }
        }
    }
}

fn parse_int_lit(s: &str) -> i128 {
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(rest, 16).unwrap_or(0)
    } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        i128::from_str_radix(rest, 2).unwrap_or(0)
    } else if let Some(rest) = s.strip_prefix("0").filter(|r| r.len() > 0 && r.chars().all(|c| c.is_ascii_digit())) {
        i128::from_str_radix(rest, 8).unwrap_or(0)
    } else {
        s.parse::<i128>().unwrap_or(0)
    }
}
