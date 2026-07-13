// src/parser.rs - 语法分析
// 第一性原理: 解析器把 token 流构造成 AST。
// suckless: 递归下降 + Pratt 表达式解析。

use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Lexer, Tok, TokKind};
use crate::ast::*;
use std::collections::HashMap;

pub struct Parser<'a> {
    lex: Lexer<'a>, cur: Tok, peek: Tok,
    diags: Vec<Diagnostic>, finished: bool,
    typedefs: HashMap<String, Type>,
    struct_defs: HashMap<String, (Vec<Field>, bool)>,
    enum_defs: HashMap<String, Vec<(String, i64)>>,
    enum_consts: HashMap<String, i64>,
    locals: HashMap<String, Type>, // 变量名 → 类型 (函数内会清除)
    globals: HashMap<String, Type>, // 全局变量名 → 类型 (永不清除)
    in_function: bool,              // 是否在函数体内
}

pub fn parse(src: &[u8], file: &str) -> (Program, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    p.program()
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8], file: &str) -> Self {
        let mut lex = Lexer::new(src, file);
        let cur = lex.next(); let peek = lex.next();
        Self { lex, cur, peek, diags: vec![], finished: false,
               typedefs: HashMap::new(), struct_defs: HashMap::new(),
               enum_defs: HashMap::new(), enum_consts: HashMap::new(),
               locals: HashMap::new(), globals: HashMap::new(), in_function: false }
    }
    fn advance(&mut self) { self.cur = std::mem::replace(&mut self.peek, self.lex.next()); }
    fn err_here(&mut self, code: &'static str, msg: impl Into<String>) {
        let mut d = Diagnostic::err(code, msg, self.cur.span.clone());
        // 自动添加修复建议
        d.hint = match code {
            "E0001" => Some(format!("check syntax near '{:?}'", self.cur.kind)),
            "E0002" => Some("expected ';', '=', or '(' after declaration".into()),
            "E0003" => Some("did you mean 'int'? or use a typedef/struct/enum".into()),
            "E0004" => Some("identifiers must start with a letter or '_'".into()),
            "E0005" => Some("only function names can be called directly".into()),
            "E0006" => Some("expected a value, variable, or expression here".into()),
            _ => None,
        };
        self.diags.push(d);
    }
    fn expect(&mut self, k: TokKind, what: &str) -> bool {
        if self.cur.kind == k { self.advance(); return true; }
        self.err_here("E0001", format!("expected {}, got {:?}", what, self.cur.kind)); false
    }
    /// 静默剥离 __attribute__((...)): GNU 扩展属性不影响代码生成
    fn skip_attributes(&mut self) {
        while self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
            self.advance(); // __attribute__
            if self.cur.kind == TokKind::LParen { self.advance(); } // (
            if self.cur.kind == TokKind::LParen { self.advance(); } // (
            let mut depth = 2i32;
            while depth > 0 && self.cur.kind != TokKind::Eof {
                if self.cur.kind == TokKind::LParen { depth += 1; }
                if self.cur.kind == TokKind::RParen { depth -= 1; }
                self.advance();
            }
        }
    }
    /// 剥离 __asm__("label"): GNU asm 标签, 不影响代码生成
    fn skip_asm_label(&mut self) {
        if self.cur.kind == TokKind::Ident && (self.cur.text == "__asm__" || self.cur.text == "__asm") {
            if self.peek.kind == TokKind::LParen {
                let saved_cur = self.cur.clone();
                self.advance(); // __asm__
                self.advance(); // (
                if self.cur.kind == TokKind::StringLit {
                    self.advance(); // "label"
                    if self.cur.kind == TokKind::RParen { self.advance(); } // )
                } else {
                    // 不是 asm 标签, 恢复
                    self.cur = saved_cur;
                }
            }
        }
    }

    pub fn program(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut decls = vec![];
        while self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::Semi { self.advance(); continue; }
            match self.parse_decl() {
                Some(d) => decls.extend(d),
                None => { if !self.finished { self.advance(); } else { break; } }
            }
        }
        (Program { decls }, std::mem::take(&mut self.diags))
    }

    fn parse_decl(&mut self) -> Option<Vec<Decl>> {
        // __extension__: GNU 扩展前缀，直接跳过
        if self.cur.kind == TokKind::Ident && self.cur.text == "__extension__" {
            self.advance();
        }
        // 跳过声明开头的 __attribute__: GCC 扩展, 如 __attribute__((unused)) static int foo()
        self.skip_attributes();
        // 跳过声明开头的 __asm__("label")
        self.skip_asm_label();
        // typedef
        if self.cur.kind == TokKind::KwTypedef {
            self.advance();
            let ty = self.parse_type()?;
            // 函数指针 typedef: typedef ret_type (*name)(params)
            if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                self.advance(); // (
                self.advance(); // *
                while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) { self.advance(); }
                let name = self.parse_ident()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    let params = self.parse_params()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let func_ty = Type::Func(Box::new(ty), params.iter().map(|p| p.ty.clone()).collect());
                    let ptr_ty = Type::Ptr(Box::new(func_ty));
                    let sp = name.span.clone();
                    if !self.expect(TokKind::Semi, "';'") { return None; }
                    self.typedefs.insert(name.name.clone(), ptr_ty.clone());
                    return Some(vec![Decl::Typedef { name, ty: ptr_ty, span: sp }]);
                }
            }
            let name = self.parse_ident()?;
            let sp = name.span.clone();
            let ty = self.try_parse_vector_attr(ty);
            if !self.expect(TokKind::Semi, "';'") { return None; }
            self.typedefs.insert(name.name.clone(), ty.clone());
            return Some(vec![Decl::Typedef { name, ty, span: sp }]);
        }
        // struct/union/enum 定义或声明
        if matches!(self.cur.kind, TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum) {
            let ty = self.parse_type()?;
            if self.cur.kind == TokKind::Semi { self.advance(); return Some(vec![]); }
            return self.parse_var_or_func(ty);
        }
        let ty = self.parse_type()?;
        self.parse_var_or_func(ty)
    }

    fn parse_var_or_func(&mut self, ty: Type) -> Option<Vec<Decl>> {
        // 函数指针声明/定义: type (*name)(params)
        // 第一性原理: (* 表示这不是普通变量名, 而是函数指针声明
        if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
            self.advance(); // (
            self.advance(); // *
            // 可选的 const/volatile/restrict 限定符
            while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) {
                self.advance();
            }
            let name = self.parse_ident()?;
            if !self.expect(TokKind::RParen, "')'") { return None; }
            // 函数指针的参数列表
            if self.cur.kind == TokKind::LParen {
                self.advance();
                let params = self.parse_params()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let func_ty = Type::Func(Box::new(ty.clone()), params.iter().map(|p| p.ty.clone()).collect());
                let ptr_ty = Type::Ptr(Box::new(func_ty));
                self.skip_attributes();
                self.skip_asm_label();
                // 注册到变量表
                self.locals.insert(name.name.clone(), ptr_ty.clone());
                if !self.in_function { self.globals.insert(name.name.clone(), ptr_ty.clone()); }
                // 可选初始化器
                let init = if self.cur.kind == TokKind::Assign { self.advance(); Some(self.parse_expr()?) } else { None };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                let sp = name.span.clone();
                return Some(vec![Decl::Var(VarDecl { ty: ptr_ty, name, init, span: sp, is_extern: false, is_static: false })]);
            }
            // 不是函数指针, 只是带括号的指针变量 (罕见)
            self.skip_attributes();
            self.skip_asm_label();
            let ptr_ty = Type::Ptr(Box::new(ty));
            let init = if self.cur.kind == TokKind::Assign { self.advance(); Some(self.parse_expr()?) } else { None };
            if !self.expect(TokKind::Semi, "';'") { return None; }
            let sp = name.span.clone();
            return Some(vec![Decl::Var(VarDecl { ty: ptr_ty, name, init, span: sp, is_extern: false, is_static: false })]);
        }
        // 跳过变量/函数名前的 __attribute__: 如 int __attribute__((unused)) x;
        self.skip_attributes();
        self.skip_asm_label();
        let name = self.parse_ident()?;
        let sp = name.span.clone();
        let name_for_decl = name.clone();
        if self.cur.kind == TokKind::LParen {
            self.advance();
            let params = self.parse_params()?;
            if !self.expect(TokKind::RParen, "')'") { return None; }
            // 函数声明后: 跳过 __attribute__ 和 __asm__ 标签
            self.skip_attributes();
            self.skip_asm_label();
            // 进入函数: 从全局变量表恢复, 清除局部变量
            self.locals = self.globals.clone();
            self.in_function = true;
            for p in &params {
                if let Some(nm) = &p.name { self.locals.insert(nm.name.clone(), p.ty.clone()); }
            }
            let body = if self.cur.kind == TokKind::LBrace {
                self.advance();
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(b)
            } else { if !self.expect(TokKind::Semi, "';' or '{'") { return None; } None };
            // 函数声明后可能还有额外的 __attribute__
            self.skip_attributes();
            Some(vec![Decl::Func(FuncDecl { ret_ty: ty, name: name_for_decl, params, body, span: sp, is_extern: false, is_static: false })])
        } else if self.cur.kind == TokKind::LBracket {
            self.advance();
            let arr_size = if self.cur.kind != TokKind::RBracket {
                let e = self.parse_expr()?;
                match &e {
                    Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                    _ => ArraySize::Var(Box::new(e)),
                }
            } else { ArraySize::Const(0) };
            if !self.expect(TokKind::RBracket, "']'") { return None; }
            let arr_ty = Type::Array(Box::new(ty), arr_size);
            self.locals.insert(name_for_decl.name.clone(), arr_ty.clone());
            if !self.in_function { self.globals.insert(name_for_decl.name.clone(), arr_ty.clone()); }
            self.skip_attributes(); // 变量属性
            let init = if self.cur.kind == TokKind::Assign { self.advance(); Some(self.parse_expr()?) } else { None };
            if !self.expect(TokKind::Semi, "';'") { return None; }
            Some(vec![Decl::Var(VarDecl { ty: arr_ty, name: name_for_decl, init, span: sp, is_extern: false, is_static: false })])
        } else {
            // 变量名后可能有 __attribute__((unused)) 等
            self.skip_attributes();
            if self.cur.kind == TokKind::Assign {
                self.locals.insert(name_for_decl.name.clone(), ty.clone());
                if !self.in_function { self.globals.insert(name_for_decl.name.clone(), ty.clone()); }
                self.advance();
                let init = self.parse_expr()?;
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(vec![Decl::Var(VarDecl { ty, name: name_for_decl, init: Some(init), span: sp, is_extern: false, is_static: false })])
            } else if self.cur.kind == TokKind::Semi {
                self.locals.insert(name_for_decl.name.clone(), ty.clone());
                if !self.in_function { self.globals.insert(name_for_decl.name.clone(), ty.clone()); }
                self.advance();
                Some(vec![Decl::Var(VarDecl { ty, name: name_for_decl, init: None, span: sp, is_extern: false, is_static: false })])
            } else {
                self.err_here("E0002", format!("expected ';', '=', '(', got {:?}", self.cur.kind));
                None
            }
        }
    }

    fn parse_type(&mut self) -> Option<Type> {
        let mut is_unsigned = false; let mut is_long = 0u8; let mut is_short = false;
        let mut base: Option<Type> = None;
        // 跳过存储类/const/volatile/_Atomic/__extension__/__attribute__
        loop {
            match self.cur.kind {
                TokKind::KwConst | TokKind::KwVolatile | TokKind::KwStatic | TokKind::KwExtern
                | TokKind::KwInline | TokKind::KwRestrict | TokKind::KwRegister | TokKind::KwAuto => { self.advance(); }
                TokKind::KwAtomic => {
                    self.advance();
                    // _Atomic(type) 形式: 解析内部类型
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let inner = self.parse_type();
                        if self.cur.kind == TokKind::RParen { self.advance(); }
                        return Some(self.parse_ptr_suffix(inner?));
                    }
                    // 否则作为限定符跳过
                }
                // __extension__: GNU 扩展, 抑制警告, 直接跳过
                TokKind::Ident if self.cur.text == "__extension__" => { self.advance(); }
                // __attribute__: 在类型限定符之间出现, 跳过
                TokKind::Ident if self.cur.text == "__attribute__" => { self.skip_attributes(); }
                // __asm__("label"): 在类型限定符之间出现, 跳过
                TokKind::Ident if self.cur.text == "__asm__" || self.cur.text == "__asm" => { self.skip_asm_label(); }
                // typeof(expr/type): GNU 扩展, 推导类型
                TokKind::Ident if self.cur.text == "typeof" || self.cur.text == "__typeof__" || self.cur.text == "__typeof" => {
                    self.advance();
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        if self.is_peek_type_start() {
                            let ty = self.parse_type()?;
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                            return Some(self.parse_ptr_suffix(ty));
                        } else {
                            let e = self.parse_expr()?;
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                            return Some(self.parse_ptr_suffix(e.ty()));
                        }
                    }
                }
                _ => break,
            }
        }
        // struct/union/enum
        if self.cur.kind == TokKind::KwStruct || self.cur.kind == TokKind::KwUnion {
            let is_u = self.cur.kind == TokKind::KwUnion; self.advance();
            let nm = if self.cur.kind == TokKind::Ident { let n = self.cur.text.clone(); self.advance(); n } else { String::new() };
            if self.cur.kind == TokKind::LBrace {
                self.advance(); let fields = self.parse_fields();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                let mut fields = fields;
                // 检查 struct 后的 __attribute__((packed/aligned(N)))
                let mut pack = false; let mut align_n: Option<usize> = None;
                if self.cur.kind == TokKind::Ident && self.cur.text == "__attribute__" {
                    self.advance(); // __attribute__
                    if self.cur.kind == TokKind::LParen { self.advance(); } // (
                    if self.cur.kind == TokKind::LParen { self.advance(); } // (
                    loop {
                        if self.cur.kind == TokKind::Ident && self.cur.text == "packed" {
                            pack = true; self.advance();
                        } else if self.cur.kind == TokKind::Ident && self.cur.text == "aligned" {
                            self.advance(); // aligned
                            if self.cur.kind == TokKind::LParen { self.advance(); }
                            if self.cur.kind == TokKind::Number {
                                align_n = self.cur.text.parse().ok();
                                self.advance();
                            }
                            if self.cur.kind == TokKind::RParen { self.advance(); }
                        } else if self.cur.kind == TokKind::Ident || self.cur.kind == TokKind::Comma {
                            self.advance(); // 跳过未知属性
                        } else { break; }
                    }
                    if self.cur.kind == TokKind::RParen { self.advance(); } // )
                    if self.cur.kind == TokKind::RParen { self.advance(); } // )
                }
                if pack { Type::layout_struct_packed(&mut fields, is_u); }
                else { Type::layout_struct(&mut fields, is_u); }
                if let Some(a) = align_n { for f in fields.iter_mut() { f.offset = (f.offset + a - 1) & !(a - 1); } }
                self.struct_defs.insert(nm.clone(), (fields.clone(), is_u));
                let ty = if is_u { Type::Union(nm, fields) } else { Type::Struct(nm, fields) };
                return Some(self.parse_ptr_suffix(ty));
            } else if !nm.is_empty() {
                if let Some((f, _)) = self.struct_defs.get(&nm) {
                    let ty = if is_u { Type::Union(nm, f.clone()) } else { Type::Struct(nm, f.clone()) };
                    return Some(self.parse_ptr_suffix(ty));
                }
                return Some(self.parse_ptr_suffix(Type::Struct(nm, vec![])));
            }
            self.err_here("E0003", "expected struct name or '{'"); return None;
        }
        if self.cur.kind == TokKind::KwEnum {
            self.advance();
            let nm = if self.cur.kind == TokKind::Ident { let n = self.cur.text.clone(); self.advance(); n } else { String::new() };
            if self.cur.kind == TokKind::LBrace {
                self.advance(); let vs = self.parse_enum_body();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                let vals: Vec<(String, i64)> = vs.iter().map(|(id, v)| (id.name.clone(), v.unwrap_or(0))).collect();
                self.enum_defs.insert(nm.clone(), vals.clone());
                let mut nv = 0i64;
                for (id, v) in &vs { let val = v.unwrap_or(nv); self.enum_consts.insert(id.name.clone(), val); nv = val + 1; }
                return Some(self.parse_ptr_suffix(Type::Enum(nm, vals)));
            } else if !nm.is_empty() {
                if let Some(v) = self.enum_defs.get(&nm) { return Some(self.parse_ptr_suffix(Type::Enum(nm, v.clone()))); }
                return Some(self.parse_ptr_suffix(Type::Enum(nm, vec![])));
            }
        }
        // typedef 引用
        if self.cur.kind == TokKind::Ident {
            if let Some(ty) = self.typedefs.get(&self.cur.text).cloned() {
                self.advance();
                // 跳过 typedef 名后的 __attribute__: 如 asmlinkage = __attribute__((aligned(4)))
                self.skip_attributes();
                return Some(self.parse_ptr_suffix(ty));
            }
        }
        // 基本类型
        let mut is_complex = false;
        loop {
            match self.cur.kind {
                TokKind::KwComplex => { is_complex = true; self.advance(); }
                TokKind::KwInt => { base = Some(Type::Int); self.advance(); }
                TokKind::KwChar => { base = Some(Type::Char); self.advance(); }
                TokKind::KwShort => { is_short = true; self.advance(); }
                TokKind::KwLong => { is_long += 1; self.advance(); }
                TokKind::KwVoid => { base = Some(Type::Void); self.advance(); }
                TokKind::KwFloat => { base = Some(Type::Float); self.advance(); }
                TokKind::KwDouble => { base = Some(Type::Double); self.advance(); }
                TokKind::KwDecimal32 => { base = Some(Type::Float); self.advance(); }      // _Decimal32 → float 近似
                TokKind::KwDecimal64 => { base = Some(Type::Double); self.advance(); }     // _Decimal64 → double 近似
                TokKind::KwDecimal128 => { base = Some(Type::LongDouble); self.advance(); } // _Decimal128 → long double 近似
                TokKind::KwFract => { base = Some(Type::Fract); self.advance(); }
                TokKind::KwAccum => { base = Some(Type::Accum); self.advance(); }
                TokKind::KwSigned => { self.advance(); }
                TokKind::KwUnsigned => { is_unsigned = true; self.advance(); }
                _ => break,
            }
        }
        if base.is_some() || is_unsigned || is_long > 0 || is_short || is_complex {
            let ty = self.resolve_base(base, is_unsigned, is_long, is_short);
            let ty = if is_complex { Type::Complex(Box::new(ty)) } else { ty };
            let ty = self.try_parse_vector_attr(ty);
            Some(self.parse_ptr_suffix(ty))
        } else {
            self.err_here("E0003", format!("expected type, got {:?}", self.cur.kind)); None
        }
    }

    fn resolve_base(&self, base: Option<Type>, uns: bool, long: u8, short: bool) -> Type {
        match (base, uns, long, short) {
            (Some(Type::Void), ..) => Type::Void,
            (Some(Type::Float), ..) => Type::Float,
            (Some(Type::Double), _, 0, _) => Type::Double,
            (Some(Type::Double), _, _, _) => Type::LongDouble,
            (Some(Type::LongDouble), ..) => Type::LongDouble,  // _Decimal128 直接映射
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

    fn parse_ptr_suffix(&mut self, mut ty: Type) -> Type {
        while self.cur.kind == TokKind::Star {
            self.advance();
            while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwAtomic) { self.advance(); }
            ty = Type::Ptr(Box::new(ty));
        }
        ty
    }

    /// 解析 __attribute__((vector_size(N))) 等属性, 返回可能包装为 Vector 的类型
    fn try_parse_vector_attr(&mut self, ty: Type) -> Type {
        if self.cur.kind != TokKind::Ident || self.cur.text != "__attribute__" { return ty; }
        self.advance(); // __attribute__
        if self.cur.kind != TokKind::LParen { return ty; }
        self.advance(); // (
        if self.cur.kind != TokKind::LParen { return ty; }
        self.advance(); // (
        // 解析 vector_size(N)
        if self.cur.kind == TokKind::Ident && self.cur.text == "vector_size" {
            self.advance(); // vector_size
            if self.cur.kind == TokKind::LParen {
                self.advance(); // (
                let size = if self.cur.kind == TokKind::Number {
                    let v: usize = self.cur.text.parse().unwrap_or(16);
                    self.advance();
                    v
                } else { 16 };
                if self.cur.kind == TokKind::RParen { self.advance(); } // )
                let elem_size = ty.size().max(1);
                let count = size / elem_size;
                // 消耗 ))
                if self.cur.kind == TokKind::RParen { self.advance(); }
                if self.cur.kind == TokKind::RParen { self.advance(); }
                return Type::Vector(Box::new(ty), count);
            }
        }
        // 其他属性: 跳过直到 ))
        let mut depth = 2i32; // 已在两层括号内
        while depth > 0 && self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::LParen { depth += 1; }
            if self.cur.kind == TokKind::RParen { depth -= 1; }
            self.advance();
        }
        ty
    }

    fn parse_fields(&mut self) -> Vec<Field> {
        let mut fs = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            let ty = match self.parse_type() { Some(t) => t, None => { self.advance(); continue; } };
            let nm = if self.cur.kind == TokKind::Ident { let n = self.cur.text.clone(); self.advance(); n } else { String::new() };
            let bw = if self.cur.kind == TokKind::Colon {
                self.advance();
                if let Expr::Int { v, .. } = self.parse_expr().unwrap_or(Expr::Int { v: 0, span: Span::point("", 0, 0), ty: Type::Int }) { Some(v as u32) } else { None }
            } else { None };
            let ty = if self.cur.kind == TokKind::LBracket {
                self.advance();
                let arr_size = if self.cur.kind != TokKind::RBracket {
                    let e = self.parse_expr().unwrap_or(Expr::Int { v: 0, span: Span::point("", 0, 0), ty: Type::Int });
                    match &e {
                        Expr::Int { v, .. } => ArraySize::Const(*v as usize),
                        _ => ArraySize::Var(Box::new(e)),
                    }
                } else { ArraySize::Const(0) };
                let _ = self.expect(TokKind::RBracket, "']'");
                Type::Array(Box::new(ty), arr_size)
            } else { ty };
            fs.push(Field { name: nm, ty, offset: 0, bit_width: bw, bit_offset: 0 });
            if self.cur.kind == TokKind::Semi { self.advance(); } else if self.cur.kind == TokKind::Comma { self.advance(); }
        }
        fs
    }

    fn parse_enum_body(&mut self) -> Vec<(Ident, Option<i64>)> {
        let mut vs = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            if self.cur.kind == TokKind::Comma { self.advance(); continue; }
            let nm = match self.parse_ident() { Some(id) => id, None => { self.advance(); continue; } };
            let val = if self.cur.kind == TokKind::Assign {
                self.advance();
                if let Expr::Int { v, .. } = self.parse_expr().unwrap_or(Expr::Int { v: 0, span: Span::point("", 0, 0), ty: Type::Int }) { Some(v as i64) } else { None }
            } else { None };
            vs.push((nm, val));
            if self.cur.kind == TokKind::Comma { self.advance(); }
        }
        vs
    }

    fn parse_ident(&mut self) -> Option<Ident> {
        if self.cur.kind != TokKind::Ident { self.err_here("E0004", format!("expected identifier, got {:?}", self.cur.kind)); return None; }
        let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() }; self.advance(); Some(id)
    }

    fn parse_params(&mut self) -> Option<Vec<Param>> {
        let mut ps = vec![];
        if self.cur.kind == TokKind::KwVoid && self.peek.kind == TokKind::RParen { self.advance(); return Some(ps); }
        if self.cur.kind == TokKind::RParen { return Some(ps); }
        if self.cur.kind == TokKind::Ellipsis { self.advance(); return Some(ps); }
        loop {
            // 函数指针参数: ret_type (*name)(params)
            // 第一性原理: 当类型后跟 (* 时, 这是函数指针参数而非普通参数
            if self.is_type_start() && self.peek.kind == TokKind::LParen {
                // 检查是否是函数指针参数模式: type (* ...)
                let saved_cur = self.cur.clone();
                let saved_peek = self.peek.clone();
                if let Some(ret_ty) = self.parse_type() {
                    if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                        self.advance(); // (
                        self.advance(); // *
                        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) { self.advance(); }
                        let nm = if self.cur.kind == TokKind::Ident {
                            let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() };
                            self.advance(); Some(id)
                        } else { None };
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                        if self.cur.kind == TokKind::LParen {
                            self.advance();
                            let inner_params = self.parse_params()?;
                            if !self.expect(TokKind::RParen, "')'") { return None; }
                            let func_ty = Type::Func(Box::new(ret_ty), inner_params.iter().map(|p| p.ty.clone()).collect());
                            let ptr_ty = Type::Ptr(Box::new(func_ty));
                            ps.push(Param { ty: ptr_ty, name: nm, span: saved_cur.span.clone() });
                            if self.cur.kind == TokKind::Comma { self.advance();
                                if self.cur.kind == TokKind::Ellipsis { self.advance(); break; }
                                continue;
                            }
                            if self.cur.kind == TokKind::Ellipsis { self.advance(); }
                            break;
                        }
                    }
                    // 不是函数指针参数, 恢复并走普通路径
                    self.cur = saved_cur;
                    self.peek = saved_peek;
                } else {
                    self.cur = saved_cur;
                    self.peek = saved_peek;
                }
            }
            let ty = self.parse_type()?;
            let nm = if self.cur.kind == TokKind::Ident { let id = Ident { name: self.cur.text.clone(), span: self.cur.span.clone() }; self.advance(); Some(id) } else { None };
            let ty = match ty { Type::Array(e, _) => Type::Ptr(e), o => o };
            ps.push(Param { ty, name: nm, span: self.cur.span.clone() });
            if self.cur.kind == TokKind::Comma { self.advance();
                if self.cur.kind == TokKind::Ellipsis { self.advance(); break; }
                continue;
            }
            if self.cur.kind == TokKind::Ellipsis { self.advance(); }
            break;
        }
        Some(ps)
    }

    fn parse_block(&mut self) -> Block {
        let mut items = vec![];
        loop {
            if self.cur.kind == TokKind::RBrace || self.cur.kind == TokKind::Eof { break; }
            if self.is_type_start() {
                match self.parse_decl() { Some(ds) => { for d in ds { items.push(BlockItem::Decl(d)); } } None => { self.advance(); } }
            } else {
                match self.parse_stmt() { Some(s) => items.push(BlockItem::Stmt(s)), None => { self.advance(); } }
            }
        }
        Block { items, span: self.cur.span.clone() }
    }

    fn is_type_start(&self) -> bool {
        matches!(self.cur.kind, TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
            | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned | TokKind::KwFloat
            | TokKind::KwDouble | TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum
            | TokKind::KwConst | TokKind::KwVolatile | TokKind::KwStatic | TokKind::KwExtern
            | TokKind::KwInline | TokKind::KwRegister | TokKind::KwAuto | TokKind::KwTypedef
            | TokKind::KwAtomic | TokKind::KwFract | TokKind::KwAccum | TokKind::KwComplex
            | TokKind::KwDecimal32 | TokKind::KwDecimal64 | TokKind::KwDecimal128)
            || (self.cur.kind == TokKind::Ident
                && (self.typedefs.contains_key(&self.cur.text)
                    || self.cur.text == "typeof" || self.cur.text == "__typeof__" || self.cur.text == "__typeof"
                    || self.cur.text == "__extension__"))
    }

    fn is_peek_type_start(&self) -> bool {
        matches!(self.peek.kind, TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
            | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned | TokKind::KwFloat
            | TokKind::KwDouble | TokKind::KwStruct | TokKind::KwUnion | TokKind::KwEnum
            | TokKind::KwConst | TokKind::KwVolatile | TokKind::KwStatic | TokKind::KwExtern
            | TokKind::KwInline | TokKind::KwRegister | TokKind::KwAuto | TokKind::KwTypedef
            | TokKind::KwAtomic | TokKind::KwFract | TokKind::KwAccum | TokKind::KwComplex
            | TokKind::KwDecimal32 | TokKind::KwDecimal64 | TokKind::KwDecimal128)
            || (self.peek.kind == TokKind::Ident
                && (self.typedefs.contains_key(&self.peek.text)
                    || self.peek.text == "typeof" || self.peek.text == "__typeof__" || self.peek.text == "__typeof"
                    || self.peek.text == "__extension__"))
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::KwReturn => {
                self.advance();
                let val = if self.cur.kind != TokKind::Semi { Some(self.parse_expr()?) } else { None };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Return { val, span: sp })
            }
            TokKind::Semi => { self.advance(); Some(Stmt::Expr { e: Expr::Int { v: 0, span: sp.clone(), ty: int_t() }, span: sp }) }
            TokKind::LBrace => { self.advance(); let b = self.parse_block(); if !self.expect(TokKind::RBrace, "'}'") { return None; } Some(Stmt::Block { b, span: sp }) }
            TokKind::KwIf => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                let then_ = Box::new(self.parse_stmt()?);
                let else_ = if self.cur.kind == TokKind::KwElse { self.advance(); Some(Box::new(self.parse_stmt()?)) } else { None };
                Some(Stmt::If { cond, then_, else_, span: sp })
            }
            TokKind::KwWhile => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(Stmt::While { cond, body: Box::new(self.parse_stmt()?), span: sp })
            }
            TokKind::KwDo => {
                self.advance(); let body = Box::new(self.parse_stmt()?);
                if !self.expect(TokKind::KwWhile, "'while'") { return None; }
                if !self.expect(TokKind::LParen, "'('") { return None; }
                let cond = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                if !self.expect(TokKind::Semi, "';'") { return None; }
                Some(Stmt::Do { body, cond, span: sp })
            }
            TokKind::KwFor => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let init = if self.cur.kind == TokKind::Semi { self.advance(); None } else { Some(Box::new(self.parse_stmt()?)) };
                let cond = if self.cur.kind == TokKind::Semi { None } else { Some(self.parse_expr()?) };
                if !self.expect(TokKind::Semi, "';'") { return None; }
                let step = if self.cur.kind == TokKind::RParen { None } else { Some(self.parse_expr()?) };
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(Stmt::For { init, cond, step, body: Box::new(self.parse_stmt()?), span: sp })
            }
            TokKind::KwBreak => { self.advance(); if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Break { span: sp }) }
            TokKind::KwContinue => { self.advance(); if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Continue { span: sp }) }
            TokKind::KwSwitch => {
                self.advance(); if !self.expect(TokKind::LParen, "'('") { return None; }
                let expr = self.parse_expr()?; if !self.expect(TokKind::RParen, "')'") { return None; }
                if !self.expect(TokKind::LBrace, "'{'") { return None; }
                let cases = self.parse_cases();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(Stmt::Switch { expr, cases, span: sp })
            }
            TokKind::KwGoto => { self.advance(); let t = self.parse_ident()?; if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Goto { target: t, span: sp }) }
            TokKind::Ident if self.peek.kind == TokKind::Colon => {
                let nm = self.parse_ident()?; self.advance();
                Some(Stmt::Label { name: nm, stmt: Box::new(self.parse_stmt()?), span: sp })
            }
            _ => { let e = self.parse_expr()?; if !self.expect(TokKind::Semi, "';'") { return None; } Some(Stmt::Expr { e, span: sp }) }
        }
    }

    fn parse_cases(&mut self) -> Vec<Case> {
        let mut cases = vec![];
        while self.cur.kind != TokKind::RBrace && self.cur.kind != TokKind::Eof {
            let sp = self.cur.span.clone();
            let val = if self.cur.kind == TokKind::KwCase { self.advance(); let e = self.parse_expr(); let _ = self.expect(TokKind::Colon, "':'"); e }
                else if self.cur.kind == TokKind::KwDefault { self.advance(); let _ = self.expect(TokKind::Colon, "':'"); None }
                else { self.advance(); continue; };
            let mut items = vec![];
            while !matches!(self.cur.kind, TokKind::KwCase | TokKind::KwDefault | TokKind::RBrace | TokKind::Eof) {
                if self.is_type_start() { match self.parse_decl() { Some(ds) => { for d in ds { items.push(BlockItem::Decl(d)); } } None => { self.advance(); } } }
                else { match self.parse_stmt() { Some(s) => items.push(BlockItem::Stmt(s)), None => { self.advance(); } } }
            }
            cases.push(Case { val, body: Block { items, span: sp.clone() }, span: sp });
        }
        cases
    }

    // ===== Pratt 表达式 =====
    fn parse_expr(&mut self) -> Option<Expr> { self.parse_expr_bp(0) }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            // 逗号运算符: bp=1, 最低优先级
            if self.cur.kind == TokKind::Comma && min_bp <= 1 {
                let sp = self.cur.span.clone();
                self.advance();
                let r = self.parse_expr_bp(1)?;
                let ty = r.ty();
                lhs = Expr::Comma { l: Box::new(lhs), r: Box::new(r), span: sp, ty };
                continue;
            }
            if self.cur.kind == TokKind::Assign && min_bp <= 2 {
                self.advance(); let r = self.parse_expr_bp(2)?;
                lhs = Expr::Assign { op: BinOp::Add, l: Box::new(lhs), r: Box::new(r), span: self.cur.span.clone(), ty: int_t() }; continue;
            }
            if let Some(op) = self.compound_op() {
                if min_bp <= 2 { self.advance(); let r = self.parse_expr_bp(2)?;
                    // 展开 lhs += e → target = lhs + e
                    // 提取赋值目标 (如果 lhs 已是 Assign, 提取其目标)
                    let target = Self::assign_target(&lhs);
                    let read = Box::new(lhs.clone()); // 读取当前值
                    let binop_ty = if matches!(target.ty(), Type::Ptr(_)) && matches!(op, BinOp::Add | BinOp::Sub) {
                        target.ty()
                    } else if matches!(target.ty(), Type::Float | Type::Double | Type::LongDouble) {
                        target.ty()
                    } else { int_t() };
                    let expanded = Expr::Binop { op, l: read, r: Box::new(r), span: self.cur.span.clone(), ty: binop_ty };
                    let assign_ty = target.ty();
                    lhs = Expr::Assign { op: BinOp::Add, l: Box::new(target), r: Box::new(expanded), span: self.cur.span.clone(), ty: assign_ty }; continue; }
            }
            if self.cur.kind == TokKind::Question {
                self.advance(); let t = self.parse_expr()?;
                if !self.expect(TokKind::Colon, "':'") { return None; }
                let e = self.parse_expr_bp(0)?;
                lhs = Expr::Cond { cond: Box::new(lhs), then_: Box::new(t), else_: Box::new(e), span: self.cur.span.clone(), ty: int_t() }; continue;
            }
            let (op, lbp, rbp) = match self.cur.kind {
                TokKind::Plus => (BinOp::Add, 3u8, 4), TokKind::Minus => (BinOp::Sub, 3, 4),
                TokKind::Star => (BinOp::Mul, 5, 6), TokKind::Slash => (BinOp::Div, 5, 6),
                TokKind::Percent => (BinOp::Mod, 5, 6),
                TokKind::Lt => (BinOp::Lt, 7, 8), TokKind::Gt => (BinOp::Gt, 7, 8),
                TokKind::Le => (BinOp::Le, 7, 8), TokKind::Ge => (BinOp::Ge, 7, 8),
                TokKind::EqEq => (BinOp::EqEq, 7, 8), TokKind::Ne => (BinOp::Ne, 7, 8),
                TokKind::Amp => (BinOp::And, 9, 10), TokKind::Caret => (BinOp::Xor, 11, 12),
                TokKind::Pipe => (BinOp::Or, 13, 14),
                TokKind::AndAnd => (BinOp::AndAnd, 15, 16), TokKind::OrOr => (BinOp::OrOr, 15, 16),
                TokKind::Shl => (BinOp::Shl, 7, 8), TokKind::Shr => (BinOp::Shr, 7, 8),
                _ => break,
            };
            if lbp < min_bp { break; }
            self.advance(); let r = self.parse_expr_bp(rbp)?;
            // 类型推断: 浮点运算→浮点结果, 指针运算→指针, 其余→int
            let binop_ty = {
                let lt = lhs.ty(); let rt = r.ty();
                let is_fa = matches!(&lt, Type::Float | Type::Double | Type::LongDouble);
                let is_fb = matches!(&rt, Type::Float | Type::Double | Type::LongDouble);
                if is_fa || is_fb {
                    if matches!(&lt, Type::LongDouble) || matches!(&rt, Type::LongDouble) { Type::LongDouble }
                    else if matches!(&lt, Type::Double) || matches!(&rt, Type::Double) { Type::Double }
                    else { Type::Float }
                } else if matches!(op, BinOp::Add | BinOp::Sub) {
                    if matches!(&lt, Type::Ptr(_)) { lt }
                    else if matches!(op, BinOp::Add) && matches!(&rt, Type::Ptr(_)) { rt }
                    else { int_t() }
                } else { int_t() }
            };
            lhs = Expr::Binop { op, l: Box::new(lhs), r: Box::new(r), span: self.cur.span.clone(), ty: binop_ty };
        }
        Some(lhs)
    }

    fn compound_op(&self) -> Option<BinOp> {
        match self.cur.kind {
            TokKind::PlusEq => Some(BinOp::Add), TokKind::MinusEq => Some(BinOp::Sub),
            TokKind::StarEq => Some(BinOp::Mul), TokKind::SlashEq => Some(BinOp::Div),
            TokKind::PercentEq => Some(BinOp::Mod), TokKind::AmpEq => Some(BinOp::And),
            TokKind::PipeEq => Some(BinOp::Or), TokKind::CaretEq => Some(BinOp::Xor),
            TokKind::ShlEq => Some(BinOp::Shl), TokKind::ShrEq => Some(BinOp::Shr),
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

    fn parse_unary(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Plus => { self.advance(); let e = self.parse_unary()?; Some(Expr::Unary { op: UnOp::Plus, e: Box::new(e), span: sp, ty: int_t() }) }
            TokKind::Minus => { self.advance(); let e = self.parse_unary()?; Some(Expr::Unary { op: UnOp::Neg, e: Box::new(e), span: sp, ty: int_t() }) }
            TokKind::Not => { self.advance(); let e = self.parse_unary()?; Some(Expr::Unary { op: UnOp::Not, e: Box::new(e), span: sp, ty: int_t() }) }
            TokKind::Tilde => { self.advance(); let e = self.parse_unary()?; Some(Expr::Unary { op: UnOp::BitNot, e: Box::new(e), span: sp, ty: int_t() }) }
            TokKind::Star => { self.advance(); let e = self.parse_unary()?;
                let ty = if let Type::Ptr(inner) = &e.ty() { *inner.clone() } else { int_t() };
                Some(Expr::Deref { e: Box::new(e), span: sp, ty }) }
            TokKind::Amp => { self.advance(); let e = self.parse_unary()?;
                let ty = Type::Ptr(Box::new(e.ty()));
                Some(Expr::AddrOf { e: Box::new(e), span: sp, ty }) }
            TokKind::PlusPlus => { self.advance(); let e = self.parse_unary()?; let ty = e.ty(); Some(Expr::PreInc { e: Box::new(e), span: sp, ty }) }
            TokKind::MinusMinus => { self.advance(); let e = self.parse_unary()?; let ty = e.ty(); Some(Expr::PreDec { e: Box::new(e), span: sp, ty }) }
            TokKind::KwSizeof => {
                self.advance();
                if self.cur.kind == TokKind::LParen && self.is_peek_type_start() {
                    self.advance(); let ty = self.parse_type()?;
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    Some(Expr::SizeofTy { ty, span: sp })
                } else { let e = self.parse_unary()?; Some(Expr::SizeofExpr { e: Box::new(e), span: sp }) }
            }
            // _Alignof(type) 或 _Alignof(expr)
            TokKind::KwAlignof => {
                self.advance();
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    if self.is_type_start() {
                        let ty = self.parse_type()?;
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                        Some(Expr::AlignofTy { ty, span: sp })
                    } else {
                        let e = self.parse_expr()?;
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                        Some(Expr::AlignofExpr { e: Box::new(e), span: sp })
                    }
                } else { self.err_here("E0001", "expected '(' after _Alignof"); None }
            }
            // __extension__: 跳过, 继续解析下一个一元表达式
            TokKind::Ident if self.cur.text == "__extension__" => {
                self.advance();
                self.parse_unary()
            }
            // __asm__("..."): 内联汇编, 解析为 no-op 表达式
            TokKind::Ident if self.cur.text == "__asm__" || self.cur.text == "__asm" => {
                self.advance();
                // 可选的 volatile/const/__volatile__
                while matches!(self.cur.kind, TokKind::KwVolatile | TokKind::KwConst)
                    || (self.cur.kind == TokKind::Ident && (self.cur.text == "__volatile__" || self.cur.text == "__const__")) {
                    self.advance();
                }
                if self.cur.kind == TokKind::LParen {
                    self.advance();
                    // 收集所有字符串
                    let mut asm_str = String::new();
                    while self.cur.kind == TokKind::StringLit {
                        let s = self.cur.text.clone();
                        // 去掉引号
                        let inner = if s.len() >= 2 { &s[1..s.len()-1] } else { &s };
                        asm_str.push_str(inner);
                        self.advance();
                    }
                    // 跳过冒号分隔的输出/输入/clobber列表
                    let mut depth = 1i32;
                    while depth > 0 && self.cur.kind != TokKind::Eof {
                        if self.cur.kind == TokKind::LParen { depth += 1; }
                        if self.cur.kind == TokKind::RParen { depth -= 1; if depth == 0 { break; } }
                        self.advance();
                    }
                    if self.cur.kind == TokKind::RParen { self.advance(); }
                    Some(Expr::AsmExpr { asm_str, span: sp, ty: Type::Void })
                } else {
                    self.err_here("E0001", "expected '(' after __asm__"); None
                }
            }
            // 语句表达式: ({...})
            TokKind::LParen if self.peek.kind == TokKind::LBrace => {
                self.advance(); // (
                self.advance(); // {
                let b = self.parse_block();
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                if !self.expect(TokKind::RParen, "')'") { return None; }
                // 类型取最后一个表达式语句的类型
                let ty = b.items.last().map_or(Type::Void, |item| {
                    if let BlockItem::Stmt(Stmt::Expr { e, .. }) = item { e.ty() }
                    else if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = item { e.ty() }
                    else { Type::Void }
                });
                Some(Expr::StmtExpr { block: Box::new(b), span: sp, ty })
            }
            TokKind::LParen if self.is_cast() => {
                self.advance(); let ty = self.parse_type()?;
                // 函数指针强制转换: (type (*)(params))expr
                // 解析完类型后, 检查是否是 (* 模式
                if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                    self.advance(); // (
                    self.advance(); // *
                    while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) { self.advance(); }
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    if self.cur.kind == TokKind::LParen {
                        self.advance();
                        let _params = self.parse_params()?;
                        if !self.expect(TokKind::RParen, "')'") { return None; }
                    }
                    let ty = Type::Ptr(Box::new(ty));
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let e = self.parse_unary()?;
                    return Some(Expr::Cast { ty, e: Box::new(e), span: sp });
                }
                if !self.expect(TokKind::RParen, "')'") { return None; }
                let e = self.parse_unary()?;
                Some(Expr::Cast { ty, e: Box::new(e), span: sp })
            }
            // 函数指针强制转换: 当 is_cast() 未检测到时, 尝试解析 (type (*...))expr
            TokKind::LParen if matches!(self.peek.kind,
                TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
                | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned
                | TokKind::KwFloat | TokKind::KwDouble | TokKind::KwStruct
                | TokKind::KwUnion | TokKind::KwEnum) => {
                // 推测性解析: 保存状态, 尝试解析为函数指针cast
                let saved_cur = self.cur.clone();
                let saved_peek = self.peek.clone();
                self.advance(); // (
                if let Some(ty) = self.parse_type() {
                    if self.cur.kind == TokKind::LParen && self.peek.kind == TokKind::Star {
                        self.advance(); // (
                        self.advance(); // *
                        while matches!(self.cur.kind, TokKind::KwConst | TokKind::KwVolatile | TokKind::KwRestrict) { self.advance(); }
                        if self.cur.kind == TokKind::RParen {
                            self.advance(); // )
                            if self.cur.kind == TokKind::LParen {
                                self.advance();
                                let _params = self.parse_params();
                                if self.cur.kind == TokKind::RParen {
                                    self.advance();
                                    if self.cur.kind == TokKind::RParen {
                                        self.advance();
                                        let ty = Type::Ptr(Box::new(ty));
                                        let e = self.parse_unary()?;
                                        return Some(Expr::Cast { ty, e: Box::new(e), span: sp });
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
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(e)
            }
            _ => self.parse_postfix(),
        }
    }

    fn is_cast(&self) -> bool {
        self.cur.kind == TokKind::LParen && matches!(self.peek.kind,
            TokKind::KwInt | TokKind::KwChar | TokKind::KwLong | TokKind::KwVoid
            | TokKind::KwShort | TokKind::KwUnsigned | TokKind::KwSigned
            | TokKind::KwFloat | TokKind::KwDouble | TokKind::KwStruct
            | TokKind::KwUnion | TokKind::KwEnum | TokKind::KwConst | TokKind::KwVolatile
            | TokKind::KwAtomic | TokKind::KwFract | TokKind::KwAccum)
            || (self.peek.kind == TokKind::Ident && self.typedefs.contains_key(&self.peek.text))
    }

    fn parse_postfix(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        let mut e = self.parse_primary()?;
        loop {
            match self.cur.kind {
                TokKind::LParen => {
                    // 函数调用: 支持标识符和函数指针表达式
                    let callee_expr = e.clone();
                    self.advance();
                    let mut args = vec![];
                    if self.cur.kind != TokKind::RParen { loop { args.push(self.parse_expr_bp(2)?); if self.cur.kind == TokKind::Comma { self.advance(); continue; } break; } }
                    if !self.expect(TokKind::RParen, "')'") { return None; }
                    let call_ty = int_t(); // 默认 int, 实际类型由函数签名决定
                    e = Expr::Call { callee: Box::new(callee_expr), args, span: sp.clone(), ty: call_ty };
                }
                TokKind::LBracket => {
                    self.advance(); let idx = self.parse_expr()?;
                    if !self.expect(TokKind::RBracket, "']'") { return None; }
                    let ty = match &e.ty() { Type::Ptr(inner) | Type::Array(inner, _) => *inner.clone(), _ => int_t() };
                    e = Expr::Index { arr: Box::new(e), idx: Box::new(idx), span: sp.clone(), ty };
                }
                TokKind::Dot => { self.advance(); let f = self.parse_ident()?; e = Expr::Member { expr: Box::new(e), field: f, span: sp.clone(), ty: int_t(), is_arrow: false }; }
                TokKind::Arrow => { self.advance(); let f = self.parse_ident()?; e = Expr::Member { expr: Box::new(e), field: f, span: sp.clone(), ty: int_t(), is_arrow: true }; }
                TokKind::PlusPlus => { self.advance(); let ty = e.ty(); e = Expr::PostInc { e: Box::new(e), span: sp.clone(), ty }; }
                TokKind::MinusMinus => { self.advance(); let ty = e.ty(); e = Expr::PostDec { e: Box::new(e), span: sp.clone(), ty }; }
                _ => break,
            }
        }
        Some(e)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let sp = self.cur.span.clone();
        match self.cur.kind {
            TokKind::Number => {
                let s = &self.cur.text; let v = parse_int_lit(s);
                let ty = if s.ends_with('L') || s.ends_with('l') { Type::Long } else { Type::Int };
                self.advance(); Some(Expr::Int { v, span: sp, ty })
            }
            TokKind::FloatLit => {
                let s = &self.cur.text;
                let s_clean = s.trim_end_matches(|c| c == 'f' || c == 'F' || c == 'l' || c == 'L');
                let v: f64 = s_clean.parse().unwrap_or(0.0);
                let ty = if s.ends_with('f') || s.ends_with('F') { Type::Float }
                    else if s.ends_with('l') || s.ends_with('L') { Type::LongDouble }
                    else { Type::Double };
                self.advance(); Some(Expr::Float { v, span: sp, ty })
            }
            TokKind::StringLit => {
                let mut data = parse_str(&self.cur.text); self.advance();
                while self.cur.kind == TokKind::StringLit { data.extend(parse_str(&self.cur.text)); self.advance(); }
                Some(Expr::String { data, span: sp, ty: Type::Ptr(Box::new(Type::Char)) })
            }
            TokKind::CharLit => { let v = parse_chr(&self.cur.text); self.advance(); Some(Expr::Char { v, span: sp, ty: Type::Int }) }
            TokKind::Ident => {
                let t = self.cur.text.clone();
                if let Some(&val) = self.enum_consts.get(&t) { self.advance(); return Some(Expr::Int { v: val as i128, span: sp, ty: Type::Int }); }
                let id = Ident { name: t, span: sp.clone() }; self.advance();
                let id_ty = self.locals.get(&id.name).cloned().unwrap_or_else(int_t);
                Some(Expr::Ident { name: id, span: sp, ty: id_ty })
            }
            TokKind::LParen => {
                self.advance();
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
                                let item = if self.cur.kind == TokKind::Dot {
                                    self.advance();
                                    let field = self.parse_ident()?;
                                    if !self.expect(TokKind::Assign, "'='") { return None; }
                                    let value = self.parse_expr_bp(2)?;
                                    InitItem::Designated { designator: Designator::Field(field), value }
                                } else {
                                    let e = self.parse_expr_bp(2)?;
                                    InitItem::Expr(e)
                                };
                                inits.push(item);
                                if self.cur.kind == TokKind::Comma { self.advance(); } else { break; }
                            }
                            if !self.expect(TokKind::RBrace, "'}'") { return None; }
                            return Some(Expr::CompoundLit { ty, inits, span: sp });
                        }
                    }
                    // 不是复合字面量，恢复状态
                    self.cur = saved_pos;
                    self.peek = saved_peek;
                }
                let e = self.parse_expr()?;
                if !self.expect(TokKind::RParen, "')'") { return None; }
                Some(e)
            }
            TokKind::LBrace => {
                // 初始化列表: {1, 2, 3} 或 {.x = 1, .y = 2}
                self.advance();
                let mut inits = Vec::new();
                while self.cur.kind != TokKind::RBrace {
                    let item = if self.cur.kind == TokKind::Dot {
                        // 指定初始化器: .field = value
                        self.advance();
                        let field = self.parse_ident()?;
                        if !self.expect(TokKind::Assign, "'='") { return None; }
                        let value = self.parse_expr_bp(2)?;
                        InitItem::Designated { designator: Designator::Field(field), value }
                    } else if self.cur.kind == TokKind::LBracket {
                        // 数组指定初始化器: [idx] = value
                        self.advance();
                        let idx = self.parse_expr()?;
                        if !self.expect(TokKind::RBracket, "']'") { return None; }
                        if !self.expect(TokKind::Assign, "'='") { return None; }
                        let value = self.parse_expr_bp(2)?;
                        InitItem::Designated { designator: Designator::Index(idx), value }
                    } else {
                        // 普通初始化值
                        let e = self.parse_expr_bp(2)?;
                        InitItem::Expr(e)
                    };
                    inits.push(item);
                    if self.cur.kind == TokKind::Comma { self.advance(); } else { break; }
                }
                if !self.expect(TokKind::RBrace, "'}'") { return None; }
                Some(Expr::InitList { inits, span: sp, ty: Type::Int }) // 类型稍后由上下文确定
            }
            _ => { self.err_here("E0006", format!("expected expression, got {:?}", self.cur.kind)); None }
        }
    }
}

fn parse_int_lit(s: &str) -> i128 {
    let s = s.trim_end_matches(|c: char| "lLuU".contains(c));
    if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) { i128::from_str_radix(r, 16).unwrap_or(0) }
    else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) { i128::from_str_radix(r, 2).unwrap_or(0) }
    else if let Some(r) = s.strip_prefix("0").filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())) { i128::from_str_radix(r, 8).unwrap_or(0) }
    else { s.parse::<i128>().unwrap_or(0) }
}

fn parse_str(raw: &str) -> Vec<u8> {
    let inner = &raw[1..raw.len()-1]; let mut d = vec![]; let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' { match it.next() { Some('n') => d.push(b'\n'), Some('t') => d.push(b'\t'), Some('r') => d.push(b'\r'), Some('0') => d.push(0), Some('\\') => d.push(b'\\'), Some('"') => d.push(b'"'), Some('\'') => d.push(b'\''), Some(c) => { d.push(b'\\'); d.push(c as u8); } None => d.push(b'\\'), } }
        else { d.extend_from_slice(c.to_string().as_bytes()); }
    }
    d.push(0); d
}

fn parse_chr(raw: &str) -> u8 {
    let inner = &raw[1..raw.len()-1];
    if inner.starts_with('\\') { match inner.chars().nth(1) { Some('n') => b'\n', Some('t') => b'\t', Some('r') => b'\r', Some('0') => 0, Some('\\') => b'\\', Some('\'') => b'\'', _ => inner.as_bytes().get(1).copied().unwrap_or(0) } }
    else { inner.as_bytes().first().copied().unwrap_or(0) }
}
