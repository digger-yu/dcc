//! C++ → dcc C 风格声明 降级 (M7 最小).
//!
//! 策略: 把 C++ 类 / 成员函数 / 构造析构 / this-> 翻译成纯 C 代码,
//! 复用 dcc 的 C 前端处理. 关键映射:
//!   class Foo { ... };              -> struct Foo { ... };
//!   int Foo::bar(int x) { ... }     -> void _ZN3Foo3barEi(struct Foo *this, int x) { ... }
//!   Foo::Foo(int x) { this->x=x; }  -> void _ZN3FooC1Ei(struct Foo *this, int x) { this->x=x; }
//!   Foo::~Foo() { ... }             -> void _ZN3FooD1Ev(struct Foo *this) { ... }
//!   T& ref                          -> T* ref (引用降级为指针)
//!   new Foo(...)                    -> ((struct Foo*)_Znwm(sizeof(struct Foo))); Foo::Foo(...)
//!   delete p                        -> Foo::~Foo(p); _ZdlPv(p);
//!
//! 降级输出: 完整 C 源文件字符串, 包含被包含的头 (展开后嵌入).

use crate::cpp::mangle::{
    MangleType, NestedName, mangle_ctor, mangle_dtor, mangle_member_function,
};
use crate::cpp::parser::{CppDecl, CppExpr, CppStmt};

/// 降级选项
#[derive(Debug, Clone, Default)]
pub struct LowerOptions {
    /// 是否对成员变量名做 `_ZN3Foo` 前缀化, 避免命名冲突
    pub mangle_fields: bool,
}

/// 降级器
pub struct Lower {
    pub out: String,
    pub opt: LowerOptions,
}

impl Lower {
    pub fn new() -> Self { Self { out: String::new(), opt: LowerOptions::default() } }
    pub fn with_options(opt: LowerOptions) -> Self { Self { out: String::new(), opt } }

    /// 把类型映射到 C 类型字符串
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

    /// 入口: 顶层声明列表
    pub fn lower_decls(&mut self, decls: &[CppDecl]) {
        self.out.push_str("/* dcc C++ frontend M7 lowered output */\n");
        self.out.push_str("#include <stddef.h>\n");
        self.out.push_str("extern void *_Znwm(size_t);\n");
        self.out.push_str("extern void _ZdlPv(void*);\n");
        for d in decls { self.lower_decl(d); }
    }

    fn lower_decl(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class { name, members } => {
                self.out.push_str(&format!("struct {} {{\n", name));
                for m in members { self.lower_class_member(m); }
                self.out.push_str("};\n");
            }
            CppDecl::FreeFn { ret, name, params, body } => {
                let n = NestedName::from_dotted(vec![name.clone()]);
                let param_tys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                let m = mangle_member_function(&n, &[], ret);
                self.emit_fn(&m, ret, params, body);
                let _ = param_tys;
            }
            CppDecl::MemberFn { class, ret, name, params, body } => {
                let n = NestedName::from_dotted(vec![class.clone(), name.clone()]);
                let m = mangle_member_function(&n, &[], ret);
                // 隐式 this 形参
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class.clone())));
                let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                all_params.extend(params.iter().cloned());
                self.emit_fn(&m, ret, &all_params, body);
            }
            CppDecl::Ctor { class, params, body } => {
                let n = NestedName::from_dotted(vec![class.clone()]);
                let m = mangle_ctor(&n, params.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>().as_slice(), 1);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class.clone())));
                let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                all_params.extend(params.iter().cloned());
                self.emit_fn(&m, &MangleType::Void, &all_params, body);
            }
            CppDecl::Dtor { class, body } => {
                let n = NestedName::from_dotted(vec![class.clone()]);
                let m = mangle_dtor(&n, 1);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class.clone())));
                let all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                self.emit_fn(&m, &MangleType::Void, &all_params, body);
            }
            CppDecl::MemberVar { .. } => {
                // 顶层 MemberVar (M7: 类内更常见, 顶层忽略)
            }
        }
    }

    fn lower_class_member(&mut self, m: &CppDecl) {
        match m {
            CppDecl::Ctor { .. } | CppDecl::Dtor { .. } | CppDecl::MemberFn { .. } => {
                // 内联函数体: 生成静态 inline 包装 (M7: 直接定义, 隐式 this 形参)
                self.lower_decl(m);
            }
            CppDecl::MemberVar { ty, name } => {
                // 成员变量: 降级为 struct 字段
                self.out.push_str(&format!("    {} {};\n", Self::mangle_to_c(ty), name));
            }
            CppDecl::Class { .. } => {
                // 嵌套类暂不支持
            }
            CppDecl::FreeFn { .. } => {
                // 类内自由函数暂不支持
            }
        }
    }

    fn emit_fn(&mut self, mangled: &str, ret: &MangleType,
               params: &[(String, MangleType)], body: &crate::cpp::parser::CppBody) {
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
                // this->name = inner   ->   this->name = <inner>
                self.out.push_str(&format!("this->{} = ", name));
                match &**inner {
                    CppStmt::Expr(e) => { self.emit_expr(e); self.out.push_str(";\n"); }
                    _ => self.out.push_str("/* unsupported inner */;\n"),
                }
            }
            _ => self.out.push_str("/* unsupported stmt */\n"),
        }
    }

    fn emit_expr(&mut self, e: &CppExpr) {
        match e {
            CppExpr::IntLit(v) => self.out.push_str(&v.to_string()),
            CppExpr::StrLit(s) => self.out.push_str(&format!("\"{}\"", s)),
            CppExpr::Ident(s) => self.out.push_str(s),
            CppExpr::This => self.out.push_str("this"),
            CppExpr::ThisMember(s) => self.out.push_str(&format!("this->{}", s)),
            CppExpr::QualifiedAccess(_, s) => self.out.push_str(s),
            CppExpr::Member(b, s) => { self.emit_expr(b); self.out.push_str(&format!("->{}", s)); }
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
                    // 调用构造函数 (M7: 假定 1 形参 ctor)
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
                    crate::cpp::parser::BinOpKind::Add => "+",
                    crate::cpp::parser::BinOpKind::Sub => "-",
                    crate::cpp::parser::BinOpKind::Mul => "*",
                    crate::cpp::parser::BinOpKind::Div => "/",
                    crate::cpp::parser::BinOpKind::Mod => "%",
                    crate::cpp::parser::BinOpKind::EqEq => "==",
                    crate::cpp::parser::BinOpKind::Lt => "<",
                    crate::cpp::parser::BinOpKind::Assign => " = ",
                    crate::cpp::parser::BinOpKind::Shl => "<<",
                    crate::cpp::parser::BinOpKind::Shr => ">>",
                };
                self.out.push_str(op_s);
                self.emit_expr(r);
            }
            CppExpr::UnaryOp(op, e) => {
                let s = match op {
                    crate::cpp::parser::UnaryOpKind::Neg => "-",
                    crate::cpp::parser::UnaryOpKind::Not => "!",
                    crate::cpp::parser::UnaryOpKind::Plus => "+",
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
        }
    }

    /// 输出完整 C 源
    pub fn finish(self) -> String { self.out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::{CppParser, CppDecl};

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
}
