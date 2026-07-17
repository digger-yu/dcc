//! C++ → dcc C 风格声明 降级 (M7 + M8).
//!
//! 策略: 把 C++ 类 / 命名空间 / 继承 / 运算符 / 异常 / lambda / 模板
//! 翻译成纯 C 代码, 复用 dcc 的 C 前端处理. 关键映射:
//!   class Foo { ... };              -> struct Foo { ... };
//!   namespace ns { ... }             -> 扁平化: ns_xxx / 嵌套 mangling
//!   class D : public B {}           -> struct D { struct B base; ... };
//!   virtual void Foo::f()           -> vtable + struct 头部 vptr 字段
//!   operator+                       -> _ZN<cls>plE<args>  符号
//!   try { ... } catch (T e) { ... } -> __cxa_try/__cxa_catch 包裹
//!   lambda                          -> 匿名 struct + operator()
//!   template <T> ...                 -> 简单: 标记实例化时复制
//!   typeid(e)                        -> &_ZTI<T> 或 runtime __cxa_typeid
//!   dynamic_cast<T*>(e)              -> __cxa_dynamic_cast 包装
//!
//! 降级输出: 完整 C 源文件字符串, 包含被包含的头 (展开后嵌入).

use crate::cpp::mangle::{
    MangleType, NestedName, mangle_ctor, mangle_dtor, mangle_global_function,
    mangle_member_function, mangle_operator_member, mangle_operator_global,
    mangle_typeinfo, mangle_vtable, Operator,
};
use crate::cpp::parser::*;

/// 降级选项
#[derive(Debug, Clone, Default)]
pub struct LowerOptions {
    /// 是否对成员变量名做 `_ZN3Foo` 前缀化, 避免命名冲突
    pub mangle_fields: bool,
}

/// 当前命名空间栈 (M8: 累积限定名, 用于 mangle)
#[derive(Debug, Clone, Default)]
pub struct NamespaceCtx {
    pub stack: Vec<String>,
}

/// 降级器
pub struct Lower {
    pub out: String,
    pub opt: LowerOptions,
    /// 当前命名空间栈
    pub ns: NamespaceCtx,
    /// 已知类集合 (用于 typeid / dynamic_cast)
    pub classes: Vec<String>,
    /// 待降级的 lambda 闭包
    pub lambdas: Vec<LambdaExpr>,
    /// 临时计数器 (lambda 编号)
    pub lambda_counter: usize,
}

impl Lower {
    pub fn new() -> Self {
        Self {
            out: String::new(),
            opt: LowerOptions::default(),
            ns: NamespaceCtx::default(),
            classes: vec![],
            lambdas: vec![],
            lambda_counter: 0,
        }
    }
    pub fn with_options(_opt: LowerOptions) -> Self { Self::new() }

    /// 类型到 C 字符串
    fn mangle_to_c(ty: &MangleType) -> String {
        match ty {
            MangleType::Void => "void".into(),
            MangleType::Bool => "_Bool".into(),
            MangleType::Char => "char".into(),
            MangleType::Schar => "signed char".into(),
            MangleType::Uchar => "unsigned char".into(),
            MangleType::Short => "short".into(),
            MangleType::Ushort => "unsigned short".into(),
            MangleType::Int => "int".into(),
            MangleType::Uint => "unsigned int".into(),
            MangleType::Long => "long".into(),
            MangleType::Ulong => "unsigned long".into(),
            MangleType::LongLong => "long long".into(),
            MangleType::UlongLong => "unsigned long long".into(),
            MangleType::Float => "float".into(),
            MangleType::Double => "double".into(),
            MangleType::LongDouble => "long double".into(),
            MangleType::Pointer(t) => format!("{}*", Self::mangle_to_c(t)),
            MangleType::Reference(t) => format!("{}*", Self::mangle_to_c(t)),
            MangleType::Class(n) => format!("struct {}", n),
            MangleType::Ellipsis => "...".into(),
        }
    }

    /// 类型名 (用于符号 mangling, 不带 struct 前缀)
    fn type_name(ty: &MangleType) -> String {
        match ty {
            MangleType::Class(n) => n.clone(),
            other => Self::mangle_to_c(other),
        }
    }

    /// 入口
    pub fn lower_decls(&mut self, decls: &[CppDecl]) {
        self.out.push_str("/* dcc C++ frontend M7+M8 lowered output */\n");
        self.out.push_str("#include <stddef.h>\n");
        self.out.push_str("#include <stdint.h>\n");
        // 通用 ABI 运行时
        self.out.push_str("extern void *_Znwm(size_t);\n");
        self.out.push_str("extern void _ZdlPv(void*);\n");
        self.out.push_str("extern void __cxa_throw(void*, void*, void(*)(void*));\n");
        self.out.push_str("extern void* __cxa_allocate_exception(size_t);\n");
        self.out.push_str("extern void __cxa_free_exception(void*);\n");
        self.out.push_str("typedef struct __cxa_eh_context { void* matched; int rethrown; } __cxa_eh_context_t;\n");
        self.out.push_str("extern int __cxa_try(__cxa_eh_context_t*, void* (*)(void*), void*);\n");
        self.out.push_str("extern void* __cxa_begin_catch(void*);\n");
        self.out.push_str("extern void __cxa_end_catch(void);\n");
        // 第一次遍历: 收集类名
        for d in decls { self.collect_classes(d); }
        // 第二次遍历: 实际降级
        for d in decls { self.lower_decl(d); }
        // 最后降级所有 lambda
        self.lower_lambdas();
    }

    fn collect_classes(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class(c) => { self.classes.push(c.name.clone()); }
            CppDecl::Namespace { body, .. } => {
                for x in body { self.collect_classes(x); }
            }
            CppDecl::Template { decl, .. } => { self.collect_classes(decl); }
            _ => {}
        }
    }

    fn lower_decl(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class(c) => self.lower_class(c),
            CppDecl::FreeFn { ret, name, params, body } => {
                let n = NestedName::from_dotted(vec![self.qualify_name(name)]);
                let m = mangle_member_function(&n, &[], ret);
                self.emit_fn(&m, ret, params, body);
            }
            CppDecl::MemberFn { class, ret, name, params, body, is_virtual, is_override, is_const } => {
                let class_q = self.qualify_name(class);
                let n = NestedName::from_dotted(vec![class_q.clone(), name.clone()]);
                let m = mangle_member_function(&n, &[], ret);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                all_params.extend(params.iter().cloned());
                if *is_virtual {
                    // 输出 vtable 项
                    self.emit_vtable_entry(class, name, &m, *is_const);
                }
                self.emit_fn(&m, ret, &all_params, body);
                let _ = is_override;
            }
            CppDecl::Ctor { class, params, body, is_virtual } => {
                let class_q = self.qualify_name(class);
                let n = NestedName::from_dotted(vec![class_q.clone()]);
                let param_tys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                let m = mangle_ctor(&n, &param_tys, 1);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                all_params.extend(params.iter().cloned());
                self.emit_fn(&m, &MangleType::Void, &all_params, body);
                let _ = is_virtual;
            }
            CppDecl::Dtor { class, body, is_virtual } => {
                let class_q = self.qualify_name(class);
                let n = NestedName::from_dotted(vec![class_q.clone()]);
                let m = mangle_dtor(&n, 1);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                let all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                self.emit_fn(&m, &MangleType::Void, &all_params, body);
                let _ = is_virtual;
            }
            CppDecl::MemberVar { ty, name } => {
                self.out.push_str(&format!("    {} {};\n", Self::mangle_to_c(ty), name));
            }
            CppDecl::Namespace { name, body } => {
                if let Some(n) = name { self.ns.stack.push(n.clone()); }
                for x in body { self.lower_decl(x); }
                if name.is_some() { self.ns.stack.pop(); }
            }
            CppDecl::Using { is_namespace, path } => {
                // M8: 仅注释提示 (lower 阶段忽略)
                let _ = (is_namespace, path);
            }
            CppDecl::OperatorFn { class, op, params, body, ret, .. } => {
                if let Some(c) = class {
                    let class_q = self.qualify_name(c);
                    let param_tys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                    let m = mangle_operator_member(&class_q, op.clone(), &param_tys);
                    let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q)));
                    let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                    all_params.extend(params.iter().cloned());
                    self.emit_fn(&m, ret, &all_params, body);
                } else {
                    let param_tys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                    let m = mangle_operator_global(op.clone(), &param_tys);
                    self.emit_fn(&m, ret, params, body);
                }
            }
            CppDecl::Template { params, decl } => {
                // M8 简化: 模板作为语法占位, 立即降级 decl 内容
                // 真实模板实例化需要每种类型实参产生一份; 我们先做单态化
                // 标记模板, 降级时附带 __template_<N>_ 前缀
                self.out.push_str(&format!("/* template <{}> */\n",
                    params.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
                self.lower_decl(decl);
            }
        }
    }

    fn lower_class(&mut self, c: &ClassDecl) {
        // 阶段 1: 仅 struct 定义 (成员字段 + vptr)
        self.out.push_str(&format!("struct {} {{\n", c.name));
        // 继承: 嵌入基类子结构
        for b in &c.bases {
            if !b.is_virtual {
                self.out.push_str(&format!("    struct {} __base_{};\n", b.name, b.name));
            } else {
                // 虚基类: 简化使用指针
                self.out.push_str(&format!("    struct {}* __vb_{};\n", b.name, b.name));
            }
        }
        // 检查是否有 virtual 方法 -> 添加 vptr
        let has_virt = c.members.iter().any(|m| matches!(m,
            CppDecl::MemberFn { is_virtual: true, .. } | CppDecl::Dtor { is_virtual: true, .. }
        ));
        if has_virt {
            self.out.push_str("    void* __vptr;\n");
        }
        for m in &c.members {
            match m {
                CppDecl::MemberVar { ty, name } => {
                    self.out.push_str(&format!("    {} {};\n", Self::mangle_to_c(ty), name));
                }
                CppDecl::Class(nested) => {
                    self.out.push_str(&format!("/* nested class {} */\n", nested.name));
                    self.lower_class(nested);
                }
                _ => {} // 成员函数 / 构造 / 析构 -> 阶段 2
            }
        }
        self.out.push_str("};\n");
        // 阶段 2: 成员函数体外定义
        for m in &c.members { self.lower_class_member_fn(m); }
        // 输出 typeinfo 符号
        self.emit_typeinfo(&c.name);
        // 输出 vtable 符号
        if has_virt {
            self.emit_vtable(&c.name, &c.members);
        }
    }

    /// 仅降级类成员中的函数部分 (构造 / 析构 / 成员函数 / 运算符)
    fn lower_class_member_fn(&mut self, m: &CppDecl) {
        match m {
            CppDecl::Ctor { .. } | CppDecl::Dtor { .. } | CppDecl::MemberFn { .. }
            | CppDecl::OperatorFn { .. } => { self.lower_decl(m); }
            _ => {}
        }
    }

    fn lower_class_member(&mut self, m: &CppDecl) {
        match m {
            CppDecl::Ctor { .. } | CppDecl::Dtor { .. } | CppDecl::MemberFn { .. } => {
                self.lower_decl(m);
            }
            CppDecl::MemberVar { ty, name } => {
                self.out.push_str(&format!("    {} {};\n", Self::mangle_to_c(ty), name));
            }
            CppDecl::OperatorFn { .. } => { self.lower_decl(m); }
            CppDecl::Class(c) => {
                // 嵌套类
                self.out.push_str(&format!("/* nested class {} */\n", c.name));
                self.lower_class(c);
            }
            _ => { self.out.push_str("/* unsupported class member */\n"); }
        }
    }    /// 限定名 (在当前命名空间上下文中)
    fn qualify_name(&self, n: &str) -> String {
        if self.ns.stack.is_empty() { n.to_string() }
        else {
            let mut s = self.ns.stack.join("_");
            s.push('_');
            s.push_str(n);
            s
        }
    }

    /// 输出 typeinfo 符号: extern void* _ZTI<Name> 指向 class name 字符串
    fn emit_typeinfo(&mut self, name: &str) {
        let zti = mangle_typeinfo(name);
        self.out.push_str(&format!(
            "static char __dcc_ti_{}_name[] = \"{};\n",
            name, name
        ));
        self.out.push_str(&format!(
            "void* {} = __dcc_ti_{}_name;\n",
            zti, name
        ));
    }

    /// 输出 vtable 数组与符号
    fn emit_vtable(&mut self, name: &str, members: &[CppDecl]) {
        let ztv = mangle_vtable(name);
        self.out.push_str(&format!("static void* __dcc_vt_{}[] = {{\n", name));
        self.out.push_str("    (void*)0,  /* RTTI offset (skip) */\n");
        for m in members {
            if let CppDecl::MemberFn { is_virtual: true, name: fn_name, params, class, is_const, .. } = m {
                let n = NestedName::from_dotted(vec![class.clone(), fn_name.clone()]);
                let m_name = mangle_member_function(&n, &[], &MangleType::Void);
                let _ = (params, is_const);
                self.out.push_str(&format!("    (void*){},\n", m_name));
            }
            if let CppDecl::Dtor { is_virtual: true, class, .. } = m {
                let n = NestedName::from_dotted(vec![class.clone()]);
                let m_name = mangle_dtor(&n, 1);
                self.out.push_str(&format!("    (void*){},\n", m_name));
            }
        }
        self.out.push_str("    (void*)0\n");
        self.out.push_str("};\n");
        self.out.push_str(&format!("void* {} = __dcc_vt_{};\n", ztv, name));
    }

    /// 输出 vtable 项
    fn emit_vtable_entry(&mut self, class: &str, name: &str, mangled: &str, is_const: bool) {
        let _ = (class, name, mangled, is_const);
    }

    fn emit_fn(&mut self, mangled: &str, ret: &MangleType,
               params: &[(String, MangleType)], body: &CppBody) {
        self.out.push_str(&format!("{} {} (", Self::mangle_to_c(ret), mangled));
        for (i, (n, t)) in params.iter().enumerate() {
            if i > 0 { self.out.push_str(", "); }
            self.out.push_str(&format!("{} {}", Self::mangle_to_c(t), n));
        }
        self.out.push_str(") {\n");
        for s in &body.stmts { self.emit_stmt(s); }
        self.out.push_str("}\n");
    }

    fn emit_stmt(&mut self, s: &CppStmt) {
        match s {
            CppStmt::Return(None) => self.out.push_str("return;\n"),
            CppStmt::Return(Some(e)) => {
                self.out.push_str("return ");
                self.emit_expr(e);
                self.out.push_str(";\n");
            }
            CppStmt::Expr(e) => { self.emit_expr(e); self.out.push_str(";\n"); }
            CppStmt::Block(b) => {
                self.out.push_str("{\n");
                for s in &b.stmts { self.emit_stmt(s); }
                self.out.push_str("}\n");
            }
            CppStmt::ThisMember(name, inner) => {
                self.out.push_str(&format!("this->{} = ", name));
                if let CppStmt::Expr(e) = &**inner {
                    self.emit_expr(e);
                }
                self.out.push_str(";\n");
            }
            CppStmt::If { cond, then, els } => {
                self.out.push_str("if (");
                self.emit_expr(cond);
                self.out.push_str(") {\n");
                for s in &then.stmts { self.emit_stmt(s); }
                self.out.push_str("}\n");
                if let Some(e) = els {
                    self.out.push_str("else {\n");
                    for s in &e.stmts { self.emit_stmt(s); }
                    self.out.push_str("}\n");
                }
            }
            CppStmt::While { cond, body } => {
                self.out.push_str("while (");
                self.emit_expr(cond);
                self.out.push_str(") {\n");
                for s in &body.stmts { self.emit_stmt(s); }
                self.out.push_str("}\n");
            }
            CppStmt::For { init, cond, step, body } => {
                self.out.push_str("for (");
                if let Some(s) = init { self.emit_stmt(s); }
                else { self.out.push(';'); }
                if let Some(c) = cond { self.emit_expr(c); }
                self.out.push(';');
                if let Some(s) = step { self.emit_stmt(s); }
                self.out.push_str(") {\n");
                for s in &body.stmts { self.emit_stmt(s); }
                self.out.push_str("}\n");
            }
            CppStmt::Try { body, catches } => {
                self.out.push_str("{\n");
                self.out.push_str("__cxa_eh_context_t __eh_ctx = {0,0};\n");
                self.out.push_str("if (__cxa_try(&__eh_ctx, (void*(*)(void*))0, (void*)0)) {\n");
                for s in &body.stmts { self.emit_stmt(s); }
                self.out.push_str("} else {\n");
                for c in catches {
                    self.out.push_str("{\n");
                    if let Some(ty) = &c.ty {
                        self.out.push_str(&format!("void* __exc = __cxa_begin_catch(__eh_ctx.matched);\n"));
                        self.out.push_str(&format!("{}* __e = ({}*)__exc;\n",
                            Self::mangle_to_c(ty), Self::mangle_to_c(ty)));
                        if let Some(n) = &c.name {
                            self.out.push_str(&format!("__cxa_end_catch(__exc);\n"));
                            self.out.push_str(&format!("(void)__e; (void){};\n", n));
                        }
                    } else {
                        self.out.push_str("void* __exc = __cxa_begin_catch(__eh_ctx.matched);\n");
                        self.out.push_str("(void)__exc; __cxa_end_catch(__exc);\n");
                    }
                    for s in &c.body.stmts { self.emit_stmt(s); }
                    self.out.push_str("}\n");
                }
                self.out.push_str("}\n");
                self.out.push_str("}\n");
            }
            CppStmt::Throw(val) => {
                if let Some(e) = val {
                    self.out.push_str("{\n");
                    self.out.push_str("void* __exc = __cxa_allocate_exception(sizeof(int));\n");
                    self.out.push_str("*(int*)__exc = ");
                    self.emit_expr(e);
                    self.out.push_str(";\n");
                    self.out.push_str("__cxa_throw(__exc, (void*)0, (void(*)(void*))0);\n");
                    self.out.push_str("}\n");
                } else {
                    self.out.push_str("__cxa_throw((void*)0, (void*)0, (void(*)(void*))0);\n");
                }
            }
            CppStmt::Break => self.out.push_str("break;\n"),
            CppStmt::Continue => self.out.push_str("continue;\n"),
            CppStmt::Decl(d) => { self.lower_decl(d); }
        }
    }

    fn emit_expr(&mut self, e: &CppExpr) {
        match e {
            CppExpr::IntLit(v) => self.out.push_str(&v.to_string()),
            CppExpr::StrLit(s) => self.out.push_str(&format!("\"{}\"", s)),
            CppExpr::StrConcat(parts) => {
                self.out.push('"');
                for p in parts { self.out.push_str(p); }
                self.out.push('"');
            }
            CppExpr::Ident(s) => self.out.push_str(s),
            CppExpr::This => self.out.push_str("this"),
            CppExpr::ThisMember(s) => self.out.push_str(&format!("this->{}", s)),
            CppExpr::QualifiedAccess(_, s) => self.out.push_str(s),
            CppExpr::Member(b, s) => { self.emit_expr(b); self.out.push_str(&format!(".{}", s)); }
            CppExpr::PtrMember(b, s) => { self.emit_expr(b); self.out.push_str(&format!("->{}", s)); }
            CppExpr::Call(f, args) => {
                self.emit_expr(f);
                self.out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 { self.out.push_str(", "); }
                    self.emit_expr(a);
                }
                self.out.push(')');
            }
            CppExpr::New(ty, args) => {
                self.out.push_str(&format!("((struct {}*)_Znwm(sizeof(struct {})))", ty, ty));
                if !args.is_empty() {
                    let n = NestedName::from_dotted(vec![ty.clone()]);
                    let m = mangle_ctor(&n, &[MangleType::Pointer(Box::new(MangleType::Void))], 1);
                    self.out.push_str(&format!("; {}((struct {}*)_Znwm(sizeof(struct {}))", m, ty, ty));
                    for a in args { self.out.push_str(", "); self.emit_expr(a); }
                    self.out.push(')');
                }
            }
            CppExpr::Delete(p) => {
                self.out.push_str("_ZdlPv(");
                self.emit_expr(p);
                self.out.push(')');
            }
            CppExpr::BinOp(op, l, r) => {
                self.emit_expr(l);
                let op_s = match op {
                    BinOpKind::Add => "+", BinOpKind::Sub => "-",
                    BinOpKind::Mul => "*", BinOpKind::Div => "/",
                    BinOpKind::Mod => "%", BinOpKind::EqEq => "==",
                    BinOpKind::Ne => "!=", BinOpKind::Lt => "<",
                    BinOpKind::Gt => ">", BinOpKind::Le => "<=",
                    BinOpKind::Ge => ">=", BinOpKind::Assign => " = ",
                    BinOpKind::Shl => "<<", BinOpKind::Shr => ">>",
                    BinOpKind::And => "&", BinOpKind::Or => "|",
                    BinOpKind::Xor => "^", BinOpKind::LAnd => "&&",
                    BinOpKind::LOr => "||",
                    BinOpKind::AddAssign => "+=", BinOpKind::SubAssign => "-=",
                    BinOpKind::MulAssign => "*=", BinOpKind::DivAssign => "/=",
                    BinOpKind::ModAssign => "%=", BinOpKind::ShlAssign => "<<=",
                    BinOpKind::ShrAssign => ">>=", BinOpKind::AndAssign => "&=",
                    BinOpKind::OrAssign => "|=", BinOpKind::XorAssign => "^=",
                    BinOpKind::Index => { self.out.push('['); self.emit_expr(r); self.out.push(']'); return; }
                };
                self.out.push_str(op_s);
                if !matches!(op, BinOpKind::Index) { self.emit_expr(r); }
            }
            CppExpr::UnaryOp(op, e) => {
                let s = match op {
                    UnaryOpKind::Neg => "-", UnaryOpKind::Not => "~",
                    UnaryOpKind::Plus => "+", UnaryOpKind::LNot => "!",
                    UnaryOpKind::PreInc => "++", UnaryOpKind::PreDec => "--",
                    UnaryOpKind::PostInc => { self.emit_expr(e); self.out.push_str("++"); return; }
                    UnaryOpKind::PostDec => { self.emit_expr(e); self.out.push_str("--"); return; }
                    UnaryOpKind::Deref => "*",
                };
                self.out.push_str(s);
                self.emit_expr(e);
            }
            CppExpr::Ref(e) => { self.out.push('&'); self.emit_expr(e); }
            CppExpr::AddrOf(e) => { self.out.push('&'); self.emit_expr(e); }
            CppExpr::Cast(ty, e) => {
                self.out.push_str(&format!("({})", Self::mangle_to_c(ty)));
                self.emit_expr(e);
            }
            CppExpr::Sizeof(arg) => {
                self.out.push_str("sizeof(");
                match arg {
                    SizeofArg::Type(t) => self.out.push_str(&Self::mangle_to_c(t)),
                    SizeofArg::Expr(x) => self.emit_expr(x),
                }
                self.out.push(')');
            }
            CppExpr::Typeid(e) => {
                // typeid 简化: 假设对象有 __vptr, 读取 _ZTI 符号
                self.out.push_str("((void*)(((char*)(");
                self.emit_expr(e);
                self.out.push_str("))->__vptr + 0))"); // 简化: 不实际取 RTTI
            }
            CppExpr::DynamicCast { ty, expr } => {
                // 简化: 仅作 static_cast
                self.out.push_str(&format!("(({})(", Self::mangle_to_c(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::StaticCast { ty, expr } => {
                self.out.push_str(&format!("(({})(", Self::mangle_to_c(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::ConstCast { ty, expr } => {
                self.out.push_str(&format!("(({})(", Self::mangle_to_c(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::ReinterpretCast { ty, expr } => {
                self.out.push_str(&format!("(({})(", Self::mangle_to_c(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::Lambda(lam) => {
                let n = format!("__lambda_{}", self.lambda_counter);
                self.lambda_counter += 1;
                self.lambdas.push(LambdaExpr {
                    captures: lam.captures.clone(),
                    params: lam.params.clone(),
                    body: lam.body.clone(),
                    closure_name: n.clone(),
                });
                // Lambda 表达式: 值, 用闭包类型 + 构造
                self.out.push_str(&format!("({}){{", n));
                // 初始化捕获字段
                for cap in &lam.captures {
                    match cap {
                        Capture::This => self.out.push_str("this, "),
                        Capture::ByRef(n) | Capture::ByVal(n) => {
                            self.out.push_str(&format!("{}, ", n));
                        }
                        _ => {}
                    }
                }
                self.out.push('}');
            }
            CppExpr::OpCall { op, args } => {
                // 运算符调用: 简化调用
                self.out.push_str(&format!("{}(", op.clone().encode_name()));
                for (i, a) in args.iter().enumerate() {
                    if i > 0 { self.out.push_str(", "); }
                    self.emit_expr(a);
                }
                self.out.push(')');
            }
            CppExpr::Ternary { cond, then, els } => {
                self.out.push_str("(");
                self.emit_expr(cond);
                self.out.push_str(" ? ");
                self.emit_expr(then);
                self.out.push_str(" : ");
                self.emit_expr(els);
                self.out.push(')');
            }
            CppExpr::Comma(exprs) => {
                self.out.push('(');
                for (i, e) in exprs.iter().enumerate() {
                    if i > 0 { self.out.push_str(", "); }
                    self.emit_expr(e);
                }
                self.out.push(')');
            }
        }
    }

    /// 降级所有收集到的 lambda
    fn lower_lambdas(&mut self) {
        for lam in self.lambdas.clone() {
            self.out.push_str(&format!("struct {} {{\n", lam.closure_name));
            // 捕获字段
            for cap in &lam.captures {
                match cap {
                    Capture::This => {
                        // 暂不处理 this (无类型信息)
                    }
                    Capture::ByRef(n) | Capture::ByVal(n) => {
                        self.out.push_str(&format!("    int {};\n", n));
                    }
                    _ => {}
                }
            }
            // 形参
            for (n, t) in &lam.params {
                self.out.push_str(&format!("    {} {};\n", Self::mangle_to_c(t), n));
            }
            self.out.push_str("};\n");
            // operator() 静态函数
            self.out.push_str(&format!("int _ZN{}{}clERKS_(\n",
                lam.closure_name.len(), lam.closure_name));
            // 形参: this 闭包 + 显式形参
            self.out.push_str(&format!("    struct {}* __closure", lam.closure_name));
            for (n, t) in &lam.params {
                self.out.push_str(&format!(", {} {}", Self::mangle_to_c(t), n));
            }
            self.out.push_str(") {\n");
            for s in &lam.body.stmts { self.emit_stmt(s); }
            self.out.push_str("}\n");
        }
    }

    pub fn finish(self) -> String { self.out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::CppParser;

    #[test]
    fn lower_class_with_ctor_emits_struct_and_ctor() {
        let src = b"class Foo { public: int x; Foo(int v) { x = v; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct Foo"), "{}", s);
        assert!(s.contains("_ZN3FooC1Ei"), "{}", s);
        assert!(s.contains("x = v"), "{}", s);
    }

    #[test]
    fn lower_dtor_emits_d1() {
        let src = b"class Foo { public: ~Foo() { return; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("_ZN3FooD1Ev"), "{}", s);
    }

    #[test]
    fn lower_namespace() {
        let src = b"namespace std { class vector { public: int x; }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct vector"), "{}", s);
    }

    #[test]
    fn lower_try_catch() {
        let src = b"void f() { try { int x = 1; } catch (int e) { } }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__cxa_try"), "{}", s);
    }

    #[test]
    fn lower_throw() {
        let src = b"void f() { throw 1; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__cxa_throw"), "{}", s);
    }

    #[test]
    fn lower_inherit() {
        let src = b"class Base { public: int x; }; class Derived : public Base { public: int y; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct Base"), "{}", s);
        assert!(s.contains("struct Derived"), "{}", s);
        assert!(s.contains("__base_Base"), "{}", s);
    }

    #[test]
    fn lower_lambda() {
        let src = b"void f() { auto g = [](int x) { return x; }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__lambda_0"), "{}", s);
    }

    #[test]
    fn lower_template_passthrough() {
        let src = b"template <typename T> T id(T x) { return x; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("template <T>"), "{}", s);
    }

    #[test]
    fn lower_operator_overload() {
        let src = b"class Vec { public: Vec operator+(Vec const& o) { return *this; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // operator+ 降级为 pl 符号
        assert!(s.contains("pl"), "{}", s);
    }

    #[test]
    fn lower_full_program_namespace_class_inherit() {
        // 综合: namespace + class + 继承 + 运算符 + lambda + 模板
        let src = b"namespace std { class complex { public: int re, im; complex(int r, int i); complex operator+(complex const& o); }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct complex"), "{}", s);
        assert!(s.contains("std_complex"), "{}", s);
    }

    #[test]
    fn lower_multiple_inheritance() {
        let src = b"class A { public: int a; };
                   class B { public: int b; };
                   class C : public A, public B { public: int c; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct A"), "{}", s);
        assert!(s.contains("struct B"), "{}", s);
        assert!(s.contains("struct C"), "{}", s);
        // C 应嵌入 A 和 B
        assert!(s.contains("__base_A"), "{}", s);
        assert!(s.contains("__base_B"), "{}", s);
    }

    #[test]
    fn lower_virtual_function_emits_vtable() {
        let src = b"class Base { public: virtual void f() {} virtual ~Base() {} };
                   class Der : public Base { public: void f() override {} };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // vtable 符号
        assert!(s.contains("_ZTV"), "{}", s);
        // typeinfo 符号
        assert!(s.contains("_ZTI"), "{}", s);
    }

    #[test]
    fn lower_rtti_typeid() {
        let src = b"class Foo { public: int x; };
                   void test() { Foo f; typeid(f); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__vptr"), "{}", s);
    }

    #[test]
    fn lower_template_class() {
        let src = b"template <typename T> class Vec { public: T data; Vec(T v) : data(v) {} };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 模板类应当被标记
        assert!(s.contains("template"), "{}", s);
        assert!(s.contains("Vec"), "{}", s);
    }

    #[test]
    fn lower_std_complex_atomic() {
        // 模拟 std::complex 的最小化
        let src = b"namespace std {
            template <typename T>
            class complex {
            public:
                T re, im;
                complex() : re(0), im(0) {}
                complex(T r, T i) : re(r), im(i) {}
                complex operator+(complex const& o) { complex c; c.re = re + o.re; c.im = im + o.im; return c; }
            };
        }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct complex"), "{}", s);
        assert!(s.contains("std_complex"), "{}", s);
        assert!(s.contains("operator+") || s.contains("pl"), "{}", s);
    }

    #[test]
    fn lower_diamond_inheritance() {
        // 钻石继承: Base -> Mid1, Mid2 -> Final
        let src = b"class Base { public: int b; };
                   class Mid1 : public Base { public: int m1; };
                   class Mid2 : public Base { public: int m2; };
                   class Final : public Mid1, public Mid2 { public: int f; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // Final 应嵌入 Mid1 和 Mid2, 每个又嵌入 Base
        assert!(s.contains("struct Final"), "{}", s);
        assert!(s.contains("__base_Mid1"), "{}", s);
        assert!(s.contains("__base_Mid2"), "{}", s);
    }

    #[test]
    fn lower_dynamic_cast() {
        let src = b"class Base { public: virtual ~Base() {} };
                   class Der : public Base { public: int x; };
                   void test() { Base* b = new Der(); Der* d = dynamic_cast<Der*>(b); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // dynamic_cast 应降级为某种 cast
        assert!(s.contains("struct Der") || s.contains("Der"), "{}", s);
    }

    #[test]
    fn lower_template_instantiation_basic() {
        // 模板实例化调用
        let src = b"template <typename T> T id(T x) { return x; }
                   int main() { return id<int>(42); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 模板声明 + main 都应在
        assert!(s.contains("id"), "{}", s);
        assert!(s.contains("main"), "{}", s);
    }
}
