//! M26: C++ 模板完整单态化 (monomorphization)
//!
//! 职责:
//! 1. 收集模板声明 (模板类 / 模板函数 / 偏特化)
//! 2. 按类型实参实例化: 模板体内的类型形参替换为实参 (AST 级替换)
//! 3. 实例化缓存: 相同实参组合只实例化一次
//! 4. 模板函数隐式实参推导 (从调用实参类型推导 T)
//! 5. requires 约束求值 (概念过滤: 约束不满足则实例化失败 → SFINAE)
//!
//! 设计原则: 实例化 = 纯 AST 变换, 不依赖 lower 上下文;
//! 隐式推导需要调用点类型信息, 由 lower 计算好实参类型后调用.

use crate::cpp::mangle::MangleType;
use crate::cpp::parser::{ClassDecl, CppBody, CppDecl, CppExpr, CppStmt, SizeofArg, TemplateArgs};

/// 模板声明 (类模板或函数模板的统一包装)
#[derive(Debug, Clone)]
pub struct TemplateDecl {
    /// 模板名 (不含命名空间)
    pub name: String,
    /// 类型形参名列表 (T / U / ...)
    pub param_names: Vec<String>,
    /// 各形参默认实参
    pub defaults: Vec<Option<MangleType>>,
    /// requires 约束文本 (M26: 实例化时求值过滤)
    pub requires: Option<String>,
    /// 模板体 (Class 或 FreeFn)
    pub decl: CppDecl,
}

/// 偏特化条目: `template <typename T> class Foo<T*> { ... }`
#[derive(Debug, Clone)]
pub struct PartialSpecEntry {
    pub name: String,
    pub param_names: Vec<String>,
    /// 特化模式, 逗号分隔的多形参文本 (M23 增强)
    pub pattern_text: String,
    pub decl: CppDecl,
}

/// 模板注册表 + 实例化缓存
#[derive(Debug, Clone, Default)]
pub struct TemplateRegistry {
    pub templates: Vec<TemplateDecl>,
    pub partials: Vec<PartialSpecEntry>,
    /// 实例化缓存: key = `name<arg0,arg1,...>` 编码 → 实例化结果
    pub inst_cache: std::collections::HashMap<String, CppDecl>,
    /// 实例化顺序记录 (key 列表, 供 lower 按依赖序输出)
    pub inst_order: Vec<String>,
}

/// 把 MangleType 编码为实例化缓存 key (与 C 标识符命名一致)
pub fn type_key(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".into(),
        MangleType::Bool => "bool".into(),
        MangleType::Char => "char".into(),
        MangleType::Schar => "schar".into(),
        MangleType::Uchar => "uchar".into(),
        MangleType::Short => "short".into(),
        MangleType::Ushort => "ushort".into(),
        MangleType::Int => "int".into(),
        MangleType::Uint => "uint".into(),
        MangleType::Long => "long".into(),
        MangleType::Ulong => "ulong".into(),
        MangleType::LongLong => "llong".into(),
        MangleType::UlongLong => "ullong".into(),
        MangleType::Float => "float".into(),
        MangleType::Double => "double".into(),
        MangleType::LongDouble => "ldouble".into(),
        MangleType::Pointer(t) => format!("p_{}", type_key(t)),
        MangleType::Reference(t) => format!("r_{}", type_key(t)),
        MangleType::Class(n) => n.clone(),
        MangleType::Template { name, args } => {
            let mut s = format!("t_{}", name);
            for a in args {
                s.push('_');
                s.push_str(&type_key(a));
            }
            s
        }
        MangleType::Ellipsis => "ellipsis".into(),
        MangleType::Auto => "auto".into(),
        MangleType::DecltypeAuto => "decltype_auto".into(),
    }
}

/// 实例化后的类名: `Box<int>` → `__dcc_template_Box_int`
/// 必须与 lower.rs `mangle_to_c` 的 Template 分支一致.
pub fn instantiated_class_name(name: &str, args: &[MangleType]) -> String {
    let mut s = format!("__dcc_template_{}", name);
    for a in args {
        s.push('_');
        s.push_str(&type_key(a));
    }
    s
}

impl TemplateRegistry {
    /// 从 TemplateArgs 构造 TemplateDecl (解析阶段收集用)
    pub fn decl_from(
        targs: &TemplateArgs,
        requires: &Option<String>,
        decl: CppDecl,
    ) -> Option<TemplateDecl> {
        let name = match &decl {
            CppDecl::Class(c) => c.name.clone(),
            CppDecl::FreeFn { name, .. } => name.clone(),
            _ => return None,
        };
        Some(TemplateDecl {
            name,
            param_names: targs.params.iter().map(|p| p.name.clone()).collect(),
            defaults: targs.params.iter().map(|p| p.default.clone()).collect(),
            requires: requires.clone(),
            decl,
        })
    }

    /// 注册模板声明 (重复注册同名模板: 后者覆盖, 便于 stdlib 头重复 include)
    pub fn register(&mut self, targs: &TemplateArgs, requires: &Option<String>, decl: &CppDecl) {
        if let Some(td) = Self::decl_from(targs, requires, decl.clone()) {
            self.templates.retain(|t| t.name != td.name);
            self.templates.push(td);
        }
    }

    /// 注册偏特化
    pub fn register_partial(
        &mut self,
        name: String,
        params: &[String],
        pattern_text: String,
        decl: &CppDecl,
    ) {
        self.partials.push(PartialSpecEntry {
            name,
            param_names: params.to_vec(),
            pattern_text,
            decl: decl.clone(),
        });
    }

    /// 查找模板 (按名)
    pub fn find(&self, name: &str) -> Option<&TemplateDecl> {
        self.templates.iter().find(|t| t.name == name)
    }

    /// 实例化 key: `name<type_key(arg0),...>`
    pub fn key_for(name: &str, args: &[MangleType]) -> String {
        let mut s = format!("{}<", name);
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&type_key(a));
        }
        s.push('>');
        s
    }

    /// 解析模板实参 (默认实参填充 + requires 约束检查)
    /// 失败返回 None (约束不满足 / 实参个数不足) → SFINAE 跳过
    pub fn resolve_args(&self, td: &TemplateDecl, args: &[MangleType]) -> Option<Vec<MangleType>> {
        let mut resolved: Vec<MangleType> = Vec::new();
        for (i, _p) in td.param_names.iter().enumerate() {
            if let Some(a) = args.get(i) {
                resolved.push(a.clone());
            } else if let Some(d) = td.defaults.get(i).cloned().flatten() {
                resolved.push(d);
            } else if i == 0 && args.is_empty() && !td.defaults.is_empty() {
                // 单默认形参模板 `template <typename T = int>` 且无实参
                resolved.push(td.defaults[0].clone().unwrap());
            } else {
                return None; // 实参不足且无默认值
            }
        }
        let _ = td;
        // requires 约束求值 (概念过滤)
        if let Some(cond) = &td.requires {
            let mut ctx = crate::cpp::feature_degrade::SfinaeCtx::new();
            for (pn, pa) in td.param_names.iter().zip(resolved.iter()) {
                ctx.bind(pn.clone(), type_key(pa));
            }
            if !crate::cpp::feature_degrade::evaluate_trait_condition(cond, &ctx).unwrap_or(true) {
                return None;
            }
        }
        Some(resolved)
    }

    /// 从调用实参类型推导模板实参 (隐式实例化)
    /// call_arg_tys: 调用点实参的 MangleType (由 lower 计算)
    pub fn deduce_args(
        &self,
        td: &TemplateDecl,
        call_arg_tys: &[MangleType],
    ) -> Option<Vec<MangleType>> {
        // 已有全部默认实参的模板 (无参模板函数 `sfinae_default_arg`) → 直接取默认
        if td.defaults.iter().all(|d| d.is_some()) {
            let mut args: Vec<MangleType> = Vec::new();
            for d in &td.defaults {
                args.push(d.clone().unwrap());
            }
            return self.resolve_args(td, &args);
        }
        // 从函数参数类型推导: 形参类型中出现的形参名 ← 实参类型
        let fn_params: Vec<MangleType> = match &td.decl {
            CppDecl::FreeFn { params, .. } => params.iter().map(|(_, t)| t.clone()).collect(),
            _ => return None,
        };
        let mut bindings: Vec<(String, MangleType)> = Vec::new();
        for (pt, at) in fn_params.iter().zip(call_arg_tys.iter()) {
            collect_bindings(pt, at, &td.param_names, &mut bindings);
        }
        // 生成实参 (按形参顺序; 未推导出的用默认或失败)
        let mut args: Vec<MangleType> = Vec::new();
        for (i, pn) in td.param_names.iter().enumerate() {
            if let Some((_, a)) = bindings.iter().rev().find(|(b, _)| b == pn) {
                args.push(a.clone());
            } else {
                let d = td.defaults.get(i).cloned().flatten()?;
                args.push(d);
            }
        }
        self.resolve_args(td, &args)
    }

    /// 实例化模板类 `name<args>`; 返回实例化后的 Class 声明 (名字已改写)
    pub fn instantiate_class(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        let td = self.find(name)?;
        let resolved = self.resolve_args(td, args)?;
        let key = Self::key_for(name, &resolved);
        if let Some(cached) = self.inst_cache.get(&key) {
            return Some(cached.clone());
        }
        let inst = instantiate_decl(&td.decl, &td.param_names, &resolved, Some(name), &resolved);
        self.inst_cache.insert(key.clone(), inst.clone());
        self.inst_order.push(key);
        Some(inst)
    }

    /// 实例化模板函数 `name<args>`; 返回实例化后的 FreeFn 声明
    pub fn instantiate_fn(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        let td = self.find(name)?;
        let resolved = self.resolve_args(td, args)?;
        let key = Self::key_for(name, &resolved);
        if let Some(cached) = self.inst_cache.get(&key) {
            return Some(cached.clone());
        }
        let inst = instantiate_decl(&td.decl, &td.param_names, &resolved, None, &resolved);
        self.inst_cache.insert(key.clone(), inst.clone());
        self.inst_order.push(key);
        Some(inst)
    }

    /// 选择偏特化: 若实参匹配某偏特化模式, 返回 (特化声明, 形参名, 实参)
    pub fn pick_partial(
        &self,
        name: &str,
        args: &[MangleType],
    ) -> Option<(CppDecl, Vec<String>, Vec<MangleType>)> {
        for p in &self.partials {
            if p.name != name {
                continue;
            }
            let patterns: Vec<&str> = p.pattern_text.split(',').collect();
            let arg_keys: Vec<String> = args.iter().map(type_key).collect();
            if patterns.len() != arg_keys.len() {
                continue;
            }
            let all_match = patterns.iter().zip(arg_keys.iter()).all(|(pat, arg)| {
                // 支持指针/引用模式: `T*` vs `int*`
                let pat = pat.trim();
                let pat_key = pat.replace('*', "").trim().to_string();
                if pat.ends_with('*') && arg.starts_with("p_") {
                    // `T*` 匹配 `p_<T>` 编码
                    return pat_key == "T" || arg.contains(&pat_key);
                }
                if pat == "T" || pat == "U" || pat == "V" {
                    return true;
                }
                if pat_key.is_empty() {
                    return false;
                }
                pat_key == *arg
            });
            if all_match {
                return Some((p.decl.clone(), p.param_names.clone(), args.to_vec()));
            }
        }
        None
    }
}

/// 从形参/实参类型对中收集绑定 (形参名 → 实参类型)
/// 形如 `Class("T")` ↔ 实参 `Int`; `Pointer(Class("T"))` ↔ `Pointer(Int)` 递归
fn collect_bindings(
    pt: &MangleType,
    at: &MangleType,
    names: &[String],
    out: &mut Vec<(String, MangleType)>,
) {
    match (pt, at) {
        (MangleType::Class(n), _) if names.iter().any(|x| x == n) => {
            // 形参名直接对应实参 (整棵)
            out.push((n.clone(), at.clone()));
        }
        (MangleType::Pointer(a), MangleType::Pointer(b)) => collect_bindings(a, b, names, out),
        (MangleType::Reference(a), MangleType::Pointer(b)) => collect_bindings(a, b, names, out),
        (MangleType::Reference(a), MangleType::Reference(b)) => collect_bindings(a, b, names, out),
        (MangleType::Template { args: pa, .. }, MangleType::Template { args: aa, .. }) => {
            for (x, y) in pa.iter().zip(aa.iter()) {
                collect_bindings(x, y, names, out);
            }
        }
        _ => {}
    }
}

// ===== AST 级替换 =====

/// 把类型中的模板形参替换为实参
pub fn substitute_type(ty: &MangleType, params: &[String], args: &[MangleType]) -> MangleType {
    match ty {
        MangleType::Class(n) => {
            if let Some(idx) = params.iter().position(|p| p == n) {
                if let Some(a) = args.get(idx) {
                    return a.clone();
                }
            }
            ty.clone()
        }
        MangleType::Pointer(t) => MangleType::Pointer(Box::new(substitute_type(t, params, args))),
        MangleType::Reference(t) => {
            MangleType::Reference(Box::new(substitute_type(t, params, args)))
        }
        MangleType::Template { name, args: tas } => {
            let new_args: Vec<MangleType> = tas
                .iter()
                .map(|a| substitute_type(a, params, args))
                .collect();
            MangleType::Template {
                name: name.clone(),
                args: new_args,
            }
        }
        other => other.clone(),
    }
}

/// 递归替换表达式内出现的模板形参类型
pub fn substitute_expr(e: &CppExpr, params: &[String], args: &[MangleType]) -> CppExpr {
    match e {
        CppExpr::Cast(ty, inner) => CppExpr::Cast(
            substitute_type(ty, params, args),
            Box::new(substitute_expr(inner, params, args)),
        ),
        CppExpr::StaticCast { ty, expr } => CppExpr::StaticCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::ConstCast { ty, expr } => CppExpr::ConstCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::ReinterpretCast { ty, expr } => CppExpr::ReinterpretCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::DynamicCast { ty, expr } => CppExpr::DynamicCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::TypeidType(ty) => CppExpr::TypeidType(substitute_type(ty, params, args)),
        CppExpr::Sizeof(SizeofArg::Type(ty)) => {
            CppExpr::Sizeof(SizeofArg::Type(substitute_type(ty, params, args)))
        }
        CppExpr::Sizeof(SizeofArg::Expr(x)) => {
            CppExpr::Sizeof(SizeofArg::Expr(Box::new(substitute_expr(x, params, args))))
        }
        CppExpr::TypedInitList { ty, elems } => CppExpr::TypedInitList {
            ty: substitute_type(ty, params, args),
            elems: elems
                .iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        },
        CppExpr::Member(o, s) => {
            CppExpr::Member(Box::new(substitute_expr(o, params, args)), s.clone())
        }
        CppExpr::PtrMember(o, s) => {
            CppExpr::PtrMember(Box::new(substitute_expr(o, params, args)), s.clone())
        }
        CppExpr::MemberStar(o, s) => {
            CppExpr::MemberStar(Box::new(substitute_expr(o, params, args)), s.clone())
        }
        CppExpr::PtrMemberStar(o, s) => {
            CppExpr::PtrMemberStar(Box::new(substitute_expr(o, params, args)), s.clone())
        }
        CppExpr::Call(f, as_) => CppExpr::Call(
            Box::new(substitute_expr(f, params, args)),
            as_.iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        ),
        CppExpr::New(ty, as_) => CppExpr::New(
            match substitute_type(&MangleType::Class(ty.clone()), params, args) {
                MangleType::Class(n) => n,
                other => type_key(&other),
            },
            as_.iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        ),
        CppExpr::NewArr { ty, count } => CppExpr::NewArr {
            ty: match substitute_type(&MangleType::Class(ty.clone()), params, args) {
                MangleType::Class(n) => n,
                other => type_key(&other),
            },
            count: Box::new(substitute_expr(count, params, args)),
        },
        CppExpr::PlacementNew { ptr, ty, args: as_ } => CppExpr::PlacementNew {
            ptr: Box::new(substitute_expr(ptr, params, args)),
            ty: match substitute_type(&MangleType::Class(ty.clone()), params, args) {
                MangleType::Class(n) => n,
                other => type_key(&other),
            },
            args: as_
                .iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        },
        CppExpr::BinOp(op, l, r) => CppExpr::BinOp(
            *op,
            Box::new(substitute_expr(l, params, args)),
            Box::new(substitute_expr(r, params, args)),
        ),
        CppExpr::UnaryOp(op, x) => {
            CppExpr::UnaryOp(*op, Box::new(substitute_expr(x, params, args)))
        }
        CppExpr::Ref(x) => CppExpr::Ref(Box::new(substitute_expr(x, params, args))),
        CppExpr::AddrOf(x) => CppExpr::AddrOf(Box::new(substitute_expr(x, params, args))),
        CppExpr::Delete(x) => CppExpr::Delete(Box::new(substitute_expr(x, params, args))),
        CppExpr::DeleteArr(x) => CppExpr::DeleteArr(Box::new(substitute_expr(x, params, args))),
        CppExpr::Noexcept(x) => CppExpr::Noexcept(Box::new(substitute_expr(x, params, args))),
        CppExpr::Typeid(x) => CppExpr::Typeid(Box::new(substitute_expr(x, params, args))),
        CppExpr::MultiIndex(o, idx) => CppExpr::MultiIndex(
            Box::new(substitute_expr(o, params, args)),
            idx.iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        ),
        CppExpr::Ternary { cond, then, els } => CppExpr::Ternary {
            cond: Box::new(substitute_expr(cond, params, args)),
            then: Box::new(substitute_expr(then, params, args)),
            els: Box::new(substitute_expr(els, params, args)),
        },
        CppExpr::Comma(xs) => CppExpr::Comma(
            xs.iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        ),
        CppExpr::InitList(xs) => CppExpr::InitList(
            xs.iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        ),
        CppExpr::OpCall { op, args: as_ } => CppExpr::OpCall {
            op: op.clone(),
            args: as_
                .iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        },
        CppExpr::Lambda(lam) => CppExpr::Lambda(crate::cpp::parser::LambdaExpr {
            captures: lam.captures.clone(),
            params: lam
                .params
                .iter()
                .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                .collect(),
            body: substitute_body(&lam.body, params, args),
            closure_name: lam.closure_name.clone(),
        }),
        // Ident / 字面量 / 模板名等无需替换
        other => other.clone(),
    }
}

/// 递归替换语句内的模板形参类型
pub fn substitute_stmt(s: &CppStmt, params: &[String], args: &[MangleType]) -> CppStmt {
    match s {
        CppStmt::Expr(e) => CppStmt::Expr(substitute_expr(e, params, args)),
        CppStmt::Return(e) => CppStmt::Return(e.as_ref().map(|x| substitute_expr(x, params, args))),
        CppStmt::If { cond, then, els } => CppStmt::If {
            cond: substitute_expr(cond, params, args),
            then: Box::new(substitute_body(then, params, args)),
            els: els
                .as_ref()
                .map(|x| Box::new(substitute_body(x, params, args))),
        },
        CppStmt::IfConstexpr { cond, then, els } => CppStmt::IfConstexpr {
            cond: substitute_expr(cond, params, args),
            then: Box::new(substitute_body(then, params, args)),
            els: els
                .as_ref()
                .map(|x| Box::new(substitute_body(x, params, args))),
        },
        CppStmt::While { cond, body } => CppStmt::While {
            cond: substitute_expr(cond, params, args),
            body: Box::new(substitute_body(body, params, args)),
        },
        CppStmt::For {
            init,
            cond,
            step,
            body,
        } => CppStmt::For {
            init: init
                .as_ref()
                .map(|x| Box::new(substitute_stmt(x, params, args))),
            cond: cond.as_ref().map(|x| substitute_expr(x, params, args)),
            step: step
                .as_ref()
                .map(|x| Box::new(substitute_stmt(x, params, args))),
            body: Box::new(substitute_body(body, params, args)),
        },
        CppStmt::Block(b) => CppStmt::Block(substitute_body(b, params, args)),
        CppStmt::ThisMember(n, inner) => {
            CppStmt::ThisMember(n.clone(), Box::new(substitute_stmt(inner, params, args)))
        }
        CppStmt::LocalVar { ty, name, init } => CppStmt::LocalVar {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            init: init.as_ref().map(|x| substitute_expr(x, params, args)),
        },
        CppStmt::StaticLocal { ty, name, init } => CppStmt::StaticLocal {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            init: init.as_ref().map(|x| substitute_expr(x, params, args)),
        },
        CppStmt::Try { body, catches } => CppStmt::Try {
            body: Box::new(substitute_body(body, params, args)),
            catches: catches
                .iter()
                .map(|c| crate::cpp::parser::CatchClause {
                    ty: c.ty.as_ref().map(|t| substitute_type(t, params, args)),
                    name: c.name.clone(),
                    body: substitute_body(&c.body, params, args),
                })
                .collect(),
        },
        CppStmt::Throw(e) => CppStmt::Throw(e.as_ref().map(|x| substitute_expr(x, params, args))),
        CppStmt::CoAwait(e) => CppStmt::CoAwait(substitute_expr(e, params, args)),
        CppStmt::CoYield(e) => CppStmt::CoYield(substitute_expr(e, params, args)),
        CppStmt::CoReturn(e) => {
            CppStmt::CoReturn(e.as_ref().map(|x| substitute_expr(x, params, args)))
        }
        CppStmt::Decl(d) => CppStmt::Decl(substitute_decl(d, params, args, None)),
        other => other.clone(),
    }
}

/// 递归替换语句体
pub fn substitute_body(b: &CppBody, params: &[String], args: &[MangleType]) -> CppBody {
    CppBody {
        stmts: b
            .stmts
            .iter()
            .map(|s| substitute_stmt(s, params, args))
            .collect(),
    }
}

/// 递归替换声明内的模板形参类型
/// class_rename: 类模板实例化时新的类名 (成员函数/ctor 的 class 字段同步改写)
pub fn substitute_decl(
    d: &CppDecl,
    params: &[String],
    args: &[MangleType],
    class_rename: Option<&str>,
) -> CppDecl {
    match d {
        CppDecl::Class(c) => {
            let new_name = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| c.name.clone());
            CppDecl::Class(ClassDecl {
                name: new_name,
                is_struct: c.is_struct,
                bases: c.bases.clone(),
                members: c
                    .members
                    .iter()
                    .map(|m| substitute_decl(m, params, args, class_rename))
                    .collect(),
            })
        }
        CppDecl::FreeFn {
            ret,
            name,
            params: ps,
            body,
        } => CppDecl::FreeFn {
            ret: substitute_type(ret, params, args),
            name: name.clone(),
            params: ps
                .iter()
                .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                .collect(),
            body: substitute_body(body, params, args),
        },
        CppDecl::MemberFn {
            class,
            ret,
            name,
            params: ps,
            body,
            is_virtual,
            is_override,
            is_const,
            is_final,
            is_noexcept,
            is_volatile,
            is_friend,
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| class.clone());
            CppDecl::MemberFn {
                class: new_class,
                ret: substitute_type(ret, params, args),
                name: name.clone(),
                params: ps
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                    .collect(),
                body: substitute_body(body, params, args),
                is_virtual: *is_virtual,
                is_override: *is_override,
                is_const: *is_const,
                is_final: *is_final,
                is_noexcept: *is_noexcept,
                is_volatile: *is_volatile,
                is_friend: *is_friend,
            }
        }
        CppDecl::Ctor {
            class,
            params: ps,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| class.clone());
            CppDecl::Ctor {
                class: new_class,
                params: ps
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                    .collect(),
                inits: inits
                    .iter()
                    .map(|(n, e)| (n.clone(), substitute_expr(e, params, args)))
                    .collect(),
                body: substitute_body(body, params, args),
                is_virtual: *is_virtual,
                is_explicit: *is_explicit,
                is_noexcept: *is_noexcept,
            }
        }
        CppDecl::Dtor {
            class,
            body,
            is_virtual,
            is_noexcept,
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| class.clone());
            CppDecl::Dtor {
                class: new_class,
                body: substitute_body(body, params, args),
                is_virtual: *is_virtual,
                is_noexcept: *is_noexcept,
            }
        }
        CppDecl::MemberVar {
            ty,
            name,
            is_mutable,
        } => CppDecl::MemberVar {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            is_mutable: *is_mutable,
        },
        CppDecl::OperatorFn {
            class,
            op,
            params: ps,
            body,
            ret,
            is_unary,
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .or_else(|| class.clone());
            CppDecl::OperatorFn {
                class: new_class,
                op: op.clone(),
                params: ps
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                    .collect(),
                body: substitute_body(body, params, args),
                ret: substitute_type(ret, params, args),
                is_unary: *is_unary,
            }
        }
        CppDecl::Namespace { name, body } => CppDecl::Namespace {
            name: name.clone(),
            body: body
                .iter()
                .map(|x| substitute_decl(x, params, args, class_rename))
                .collect(),
        },
        // 嵌套模板声明: 保留 (实例化时按需再次实例化)
        CppDecl::Template {
            params: tp,
            decl: inner,
            requires,
            is_export,
        } => CppDecl::Template {
            params: tp.clone(),
            decl: Box::new(substitute_decl(inner, params, args, class_rename)),
            requires: requires.clone(),
            is_export: *is_export,
        },
        other => other.clone(),
    }
}

/// 实例化入口: 替换模板体内所有类型形参
/// class_rename: 类模板实例化时的新类名
pub fn instantiate_decl(
    decl: &CppDecl,
    params: &[String],
    args: &[MangleType],
    _orig_name: Option<&str>,
    _orig_args: &[MangleType],
) -> CppDecl {
    let rename = match decl {
        CppDecl::Class(_) => Some(instantiated_class_name(
            _orig_name.unwrap_or(""),
            _orig_args,
        )),
        _ => None,
    };
    substitute_decl(decl, params, args, rename.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, args: &[MangleType]) -> MangleType {
        MangleType::Template {
            name: name.to_string(),
            args: args.to_vec(),
        }
    }

    #[test]
    fn substitute_type_basic() {
        let params = ["T".to_string(), "U".to_string()];
        let args = [MangleType::Int, MangleType::Class("MyClass".to_string())];
        assert_eq!(
            substitute_type(&MangleType::Class("T".to_string()), &params, &args),
            MangleType::Int
        );
        assert_eq!(
            substitute_type(&MangleType::Class("U".to_string()), &params, &args),
            MangleType::Class("MyClass".to_string())
        );
        // 非形参保留
        assert_eq!(
            substitute_type(&MangleType::Class("X".to_string()), &params, &args),
            MangleType::Class("X".to_string())
        );
        // 指针递归
        assert_eq!(
            substitute_type(
                &MangleType::Pointer(Box::new(MangleType::Class("T".to_string()))),
                &params,
                &args
            ),
            MangleType::Pointer(Box::new(MangleType::Int))
        );
        // 模板嵌套: vector<T>
        assert_eq!(
            substitute_type(
                &t("vector", &[MangleType::Class("T".to_string())]),
                &params,
                &args
            ),
            t("vector", &[MangleType::Int])
        );
    }

    #[test]
    fn instantiate_class_renames_members() {
        let mut reg = TemplateRegistry::default();
        // template <class T> struct Box { T payload; Box(T v) : payload(v) {} };
        let inner = CppDecl::Class(ClassDecl {
            name: "Box".to_string(),
            is_struct: true,
            bases: vec![],
            members: vec![
                CppDecl::MemberVar {
                    ty: MangleType::Class("T".to_string()),
                    name: "payload".to_string(),
                    is_mutable: false,
                },
                CppDecl::Ctor {
                    class: "Box".to_string(),
                    params: vec![("v".to_string(), MangleType::Class("T".to_string()))],
                    inits: vec![("payload".to_string(), CppExpr::Ident("v".to_string()))],
                    body: CppBody { stmts: vec![] },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
            ],
        });
        reg.register(
            &TemplateArgs {
                params: vec![crate::cpp::parser::TemplateParam {
                    name: "T".to_string(),
                    default: None,
                }],
            },
            &None,
            &inner,
        );
        let inst = reg
            .instantiate_class("Box", &[MangleType::Int])
            .expect("instantiate");
        match inst {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "__dcc_template_Box_int");
                assert_eq!(c.members.len(), 2);
                match &c.members[0] {
                    CppDecl::MemberVar { ty, name, .. } => {
                        assert_eq!(*ty, MangleType::Int);
                        assert_eq!(name, "payload");
                    }
                    _ => panic!("member 0 not MemberVar"),
                }
                match &c.members[1] {
                    CppDecl::Ctor { class, params, .. } => {
                        assert_eq!(class, "__dcc_template_Box_int");
                        assert_eq!(params[0].1, MangleType::Int);
                    }
                    _ => panic!("member 1 not Ctor"),
                }
            }
            _ => panic!("not class"),
        }
        // 缓存: 再次实例化返回相同结果
        let inst2 = reg
            .instantiate_class("Box", &[MangleType::Int])
            .expect("cached");
        match inst2 {
            CppDecl::Class(c) => assert_eq!(c.name, "__dcc_template_Box_int"),
            _ => panic!("not class"),
        }
    }

    #[test]
    fn deduce_args_from_call() {
        let mut reg = TemplateRegistry::default();
        // template <typename T, typename U> T sfinae_first(T a, U b) { return a; }
        let inner = CppDecl::FreeFn {
            ret: MangleType::Class("T".to_string()),
            name: "sfinae_first".to_string(),
            params: vec![
                ("a".to_string(), MangleType::Class("T".to_string())),
                ("b".to_string(), MangleType::Class("U".to_string())),
            ],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::Ident("a".to_string())))],
            },
        };
        let targs = TemplateArgs {
            params: vec![
                crate::cpp::parser::TemplateParam {
                    name: "T".to_string(),
                    default: None,
                },
                crate::cpp::parser::TemplateParam {
                    name: "U".to_string(),
                    default: None,
                },
            ],
        };
        reg.register(&targs, &None, &inner);
        let td = reg.find("sfinae_first").unwrap();
        let args = reg
            .deduce_args(td, &[MangleType::Long, MangleType::Double])
            .expect("deduce");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], MangleType::Long);
        assert_eq!(args[1], MangleType::Double);
    }

    #[test]
    fn deduce_args_default_only() {
        let mut reg = TemplateRegistry::default();
        // template <typename T = int> int sfinae_default_arg() { return 42; }
        let inner = CppDecl::FreeFn {
            ret: MangleType::Int,
            name: "sfinae_default_arg".to_string(),
            params: vec![],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::IntLit(42)))],
            },
        };
        let targs = TemplateArgs {
            params: vec![crate::cpp::parser::TemplateParam {
                name: "T".to_string(),
                default: Some(MangleType::Int),
            }],
        };
        reg.register(&targs, &None, &inner);
        let td = reg.find("sfinae_default_arg").unwrap();
        let args = reg.deduce_args(td, &[]).expect("default args");
        assert_eq!(args, vec![MangleType::Int]);
    }

    #[test]
    fn requires_constraint_filters() {
        let mut reg = TemplateRegistry::default();
        // template <typename T> requires sizeof(T) <= 4 T pick(T x) { return x; }
        let inner = CppDecl::FreeFn {
            ret: MangleType::Class("T".to_string()),
            name: "pick".to_string(),
            params: vec![("x".to_string(), MangleType::Class("T".to_string()))],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::Ident("x".to_string())))],
            },
        };
        let targs = TemplateArgs {
            params: vec![crate::cpp::parser::TemplateParam {
                name: "T".to_string(),
                default: None,
            }],
        };
        reg.register(&targs, &Some("sizeof(T) <= 4".to_string()), &inner);
        // int 满足 sizeof(int) <= 4
        assert!(reg.instantiate_fn("pick", &[MangleType::Int]).is_some());
        // long long 不满足 → SFINAE 跳过
        assert!(reg
            .instantiate_fn("pick", &[MangleType::LongLong])
            .is_none());
    }

    #[test]
    fn partial_spec_pick() {
        let mut reg = TemplateRegistry::default();
        reg.partials.push(PartialSpecEntry {
            name: "Foo".to_string(),
            param_names: vec!["T".to_string()],
            pattern_text: "T*".to_string(),
            decl: CppDecl::Class(ClassDecl {
                name: "Foo".to_string(),
                is_struct: true,
                bases: vec![],
                members: vec![],
            }),
        });
        // int* 匹配 T*
        assert!(reg
            .pick_partial("Foo", &[MangleType::Pointer(Box::new(MangleType::Int))])
            .is_some());
        // int 不匹配
        assert!(reg.pick_partial("Foo", &[MangleType::Int]).is_none());
    }
}
