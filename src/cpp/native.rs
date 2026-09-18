//! C++ 原生后端桥接: `cpp::Sema → abi_layout → lower_ir → ast::Program`
//!
//! 目标: 让 `run_cpp_driver` 在 `--cxx-native=on|auto` 模式下,
//! 直接把 C++ 声明降到现有 dcc C 主流水线 (`ir::lower` → 优化 →
//! `codegen::emit`), 不再经过 legacy `Lower::lower_to_c` 临时 `.c` 中转.
//!
//! 桥接阶段 (Stage 3) 覆盖的子集:
//!   - free function
//!   - 普通 class (无继承 / 无虚函数) 作为 aggregate 进入 IR
//!   - 成员函数 / 构造函数 / 析构函数 (带显式 this 形参)
//!   - 引用类型 (T&) 在 ABI 层降级为指针
//!
//! 不支持的特性 (会返回 `Err` 让 auto-mode 回退 legacy):
//!   - 异常
//!   - namespace / using
//!   - Dependent / MemberPointer 类型

use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::cpp::abi_layout::{AbiCalculator, ClassAbi, ItaniumAbiCalculator};
use crate::cpp::feature_matrix::{CppFeature, CppPath};
use crate::cpp::hir::HirTranslationUnit;
use crate::cpp::lower_ir::{IrLowerer, LowerIrError, LowerIrOutput};
use crate::cpp::mangle::MangleType;
use crate::cpp::parser::{ClassDecl, CppBody, CppDecl, CppExpr, CppStmt, TemplateArgs};
use crate::cpp::sema::{Access, ClassId, CxxSymbol, CxxSymbolKind, CxxType, Sema};
use crate::cpp::template::{instantiated_class_name, TemplateRegistry};
use crate::diagnostics::Span;
use crate::ssa;

/// 桥接入口错误
#[derive(Debug, Clone)]
pub enum NativeError {
    /// HIR/Sema 阶段不支持的 C++ 特性 (auto-mode 应 fallback legacy)
    Unsupported(String),
    /// 内部不一致 (owner class 缺失等)
    Internal(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Unsupported(s) => write!(f, "native backend unsupported: {}", s),
            NativeError::Internal(s) => write!(f, "native backend internal error: {}", s),
        }
    }
}

impl std::convert::From<LowerIrError> for NativeError {
    fn from(e: LowerIrError) -> Self {
        match e {
            LowerIrError::Unsupported(s) => NativeError::Unsupported(s),
            LowerIrError::Internal(s) => NativeError::Internal(s),
        }
    }
}

/// 桥接结果: 已生成的可走 C 主流水线的 ast::Program
pub struct NativeLowered {
    pub prog: ast::Program,
    pub vtables: Vec<(String, Vec<crate::cpp::lower_ir::VTableEntry>)>,
    pub rodata: Vec<String>,
}

// === Stage 8: 模板实例化 ===

/// 模板函数实例化后的 C 名字: `identity<int>` -> `__dcc_tplfn_identity_int`
fn tplfn_c_name(name: &str, args: &[MangleType]) -> String {
    use crate::cpp::template::type_key;
    let keys: Vec<String> = args.iter().map(type_key).collect();
    format!("__dcc_tplfn_{}_{}", name, keys.join("_"))
}

/// 从声明列表中收集所有模板声明，构建 TemplateRegistry
fn collect_templates(decls: &[CppDecl]) -> TemplateRegistry {
    let mut reg = TemplateRegistry::default();
    for d in decls {
        collect_templates_from_decl(d, &mut reg);
    }
    reg
}

/// 递归从单个声明中收集模板
fn collect_templates_from_decl(d: &CppDecl, reg: &mut TemplateRegistry) {
    match d {
        CppDecl::Template {
            params,
            decl,
            requires,
            ..
        } => {
            reg.register(params, requires, decl);
            // 递归收集嵌套模板
            collect_templates_from_decl(decl, reg);
        }
        CppDecl::PartialSpec {
            params,
            pattern_text,
            decl,
        } => {
            if let CppDecl::Class(c) = &**decl {
                let names: Vec<String> = params.params.iter().map(|p| p.name.clone()).collect();
                reg.register_partial(c.name.clone(), &names, pattern_text.clone(), decl);
            }
        }
        // G13: Collect full specializations
        CppDecl::FullSpec { primary_name, specific_args, body } => {
            reg.register_full(primary_name.clone(), specific_args.clone(), body);
        }
        CppDecl::Namespace { body, .. } => {
            for inner in body {
                collect_templates_from_decl(inner, reg);
            }
        }
        CppDecl::Class(c) => {
            for m in &c.members {
                collect_templates_from_decl(m, reg);
            }
        }
        _ => {}
    }
}

/// 重命名模板函数实例化后的函数名
fn rename_tpl_fn_inst(decl: &CppDecl, name: &str, args: &[MangleType]) -> CppDecl {
    let new_name = tplfn_c_name(name, args);
    match decl {
        CppDecl::FreeFn { ret, name: _, params, body, is_noexcept,
            trailing_ret_type } => CppDecl::FreeFn {
            ret: ret.clone(),
            name: new_name,
            params: params.clone(),
            body: body.clone(),
            is_noexcept: *is_noexcept,
            trailing_ret_type: trailing_ret_type.clone(),
        },
        other => other.clone(),
    }
}

/// 扫描声明中的模板类型引用，触发实例化
/// 返回实例化后的声明列表
fn instantiate_templates_from_decls(
    reg: &mut TemplateRegistry,
    decls: &[CppDecl],
) -> Result<Vec<CppDecl>, NativeError> {
    let mut instantiated: Vec<CppDecl> = Vec::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 迭代处理：实例化可能引入新的模板引用，需要反复处理直到稳定
    // 必须累积所有声明（含原始 + 已实例化）作为扫描集，否则嵌套模板引用会丢失
    let mut all_decls: Vec<CppDecl> = decls.to_vec();
    loop {
        let before_count = reg.inst_cache.len();

        // 收集所有模板类型引用
        let mut tpl_refs: Vec<(String, Vec<MangleType>)> = Vec::new();
        for d in &all_decls {
            collect_template_refs_from_decl(d, &mut tpl_refs);
        }

        // 对每个引用触发实例化
        for (name, args) in &tpl_refs {
            let key = TemplateRegistry::key_for(name, args);
            if visited.contains(&key) {
                continue;
            }
            visited.insert(key.clone());

            // 尝试类模板实例化
            if let Some(inst) = reg.instantiate_class(name, args) {
                instantiated.push(inst.clone());
                all_decls.push(inst);
            }
            // 函数模板由调用点触发，这里先不处理
        }

        // 扫描函数体中的模板函数调用
        let mut fn_tpl_calls: Vec<(String, Vec<MangleType>)> = Vec::new();
        for d in &all_decls {
            collect_template_fn_calls_from_decl(d, &mut fn_tpl_calls);
        }

        for (name, args) in &fn_tpl_calls {
            let key = TemplateRegistry::key_for(name, args);
            if visited.contains(&key) {
                continue;
            }
            visited.insert(key.clone());

            if let Some(inst) = reg.instantiate_fn(name, args) {
                // 重命名模板函数实例，避免命名冲突
                let renamed = rename_tpl_fn_inst(&inst, name, args);
                instantiated.push(renamed.clone());
                all_decls.push(renamed);
            }
        }

        // 如果没有新实例化，结束循环
        if reg.inst_cache.len() == before_count {
            break;
        }
    }

    Ok(instantiated)
}

/// 从声明中收集所有模板类型引用 (类模板)
fn collect_template_refs_from_decl(d: &CppDecl, out: &mut Vec<(String, Vec<MangleType>)>) {
    match d {
        CppDecl::GlobalVar { ty, .. } => {
            collect_template_refs_from_type(ty, out);
        }
        CppDecl::FreeFn { ret, params, body, .. } => {
            collect_template_refs_from_type(ret, out);
            for (_, t) in params {
                collect_template_refs_from_type(t, out);
            }
            collect_template_refs_from_body(body, out);
        }
        CppDecl::Class(c) => {
            // 基类中的模板引用
            for base in &c.bases {
                if let Some(args) = &base.template_args {
                    out.push((base.name.clone(), args.clone()));
                }
            }
            for m in &c.members {
                collect_template_refs_from_decl(m, out);
            }
        }
        CppDecl::MemberVar { ty, .. } => {
            collect_template_refs_from_type(ty, out);
        }
        CppDecl::MemberFn { ret, params, body, .. } => {
            collect_template_refs_from_type(ret, out);
            for (_, t) in params {
                collect_template_refs_from_type(t, out);
            }
            collect_template_refs_from_body(body, out);
        }
        CppDecl::Ctor { params, body, .. } => {
            for (_, t) in params {
                collect_template_refs_from_type(t, out);
            }
            collect_template_refs_from_body(body, out);
        }
        CppDecl::Dtor { body, .. } => {
            collect_template_refs_from_body(body, out);
        }
        CppDecl::Typedef { target, .. } => {
            collect_template_refs_from_type(target, out);
        }
        CppDecl::Namespace { body, .. } => {
            for inner in body {
                collect_template_refs_from_decl(inner, out);
            }
        }
        _ => {}
    }
}

/// 从类型中收集模板引用
fn collect_template_refs_from_type(ty: &MangleType, out: &mut Vec<(String, Vec<MangleType>)>) {
    match ty {
        MangleType::Template { name, args } => {
            out.push((name.clone(), args.clone()));
            // 递归收集嵌套模板参数中的引用
            for a in args {
                collect_template_refs_from_type(a, out);
            }
        }
        MangleType::Pointer(t) | MangleType::Reference(t) => {
            collect_template_refs_from_type(t, out);
        }
        MangleType::TplMember { name, args, .. } => {
            out.push((name.clone(), args.clone()));
            for a in args {
                collect_template_refs_from_type(a, out);
            }
        }
        _ => {}
    }
}

/// 从函数体中收集模板类型引用
fn collect_template_refs_from_body(body: &CppBody, out: &mut Vec<(String, Vec<MangleType>)>) {
    for stmt in &body.stmts {
        collect_template_refs_from_stmt(stmt, out);
    }
}

/// 从语句中收集模板类型引用
fn collect_template_refs_from_stmt(s: &CppStmt, out: &mut Vec<(String, Vec<MangleType>)>) {
    match s {
        CppStmt::LocalVar { ty, init, .. } | CppStmt::StaticLocal { ty, init, .. } => {
            collect_template_refs_from_type(ty, out);
            if let Some(e) = init {
                collect_template_refs_from_expr(e, out);
            }
        }
        CppStmt::Expr(e) | CppStmt::Return(Some(e)) => {
            collect_template_refs_from_expr(e, out);
        }
        CppStmt::If { cond, then, els } => {
            collect_template_refs_from_expr(cond, out);
            collect_template_refs_from_body(then, out);
            if let Some(e) = els {
                collect_template_refs_from_body(e, out);
            }
        }
        CppStmt::While { cond, body } => {
            collect_template_refs_from_expr(cond, out);
            collect_template_refs_from_body(body, out);
        }
        CppStmt::For { init, cond, step, body } => {
            if let Some(i) = init {
                collect_template_refs_from_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_template_refs_from_expr(c, out);
            }
            if let Some(s) = step {
                collect_template_refs_from_stmt(s, out);
            }
            collect_template_refs_from_body(body, out);
        }
        CppStmt::Block(b) => collect_template_refs_from_body(b, out),
        CppStmt::Decl(d) => collect_template_refs_from_decl(d, out),
        _ => {}
    }
}

/// 从表达式中收集模板类型引用
fn collect_template_refs_from_expr(e: &CppExpr, out: &mut Vec<(String, Vec<MangleType>)>) {
    match e {
        CppExpr::Cast(ty, inner) | CppExpr::StaticCast { ty, expr: inner } => {
            collect_template_refs_from_type(ty, out);
            collect_template_refs_from_expr(inner, out);
        }
        CppExpr::Call(f, args) => {
            collect_template_refs_from_expr(f, out);
            for a in args {
                collect_template_refs_from_expr(a, out);
            }
        }
        CppExpr::New(_, args) => {
            for a in args {
                collect_template_refs_from_expr(a, out);
            }
        }
        CppExpr::NewArr { count, .. } => {
            collect_template_refs_from_expr(count, out);
        }
        CppExpr::BinOp(_, l, r) => {
            collect_template_refs_from_expr(l, out);
            collect_template_refs_from_expr(r, out);
        }
        CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => {
            collect_template_refs_from_expr(x, out);
        }
        CppExpr::Member(o, _) | CppExpr::PtrMember(o, _) => {
            collect_template_refs_from_expr(o, out);
        }
        CppExpr::TypedInitList { ty, elems } => {
            collect_template_refs_from_type(ty, out);
            for e in elems {
                collect_template_refs_from_expr(e, out);
            }
        }
        CppExpr::TplStaticAccess { name, args, .. } => {
            out.push((name.clone(), args.clone()));
        }
        _ => {}
    }
}

/// 从声明中收集模板函数调用
fn collect_template_fn_calls_from_decl(d: &CppDecl, out: &mut Vec<(String, Vec<MangleType>)>) {
    match d {
        CppDecl::FreeFn { body, .. } => {
            collect_template_fn_calls_from_body(body, out);
        }
        CppDecl::MemberFn { body, .. } => {
            collect_template_fn_calls_from_body(body, out);
        }
        CppDecl::Ctor { body, .. } => {
            collect_template_fn_calls_from_body(body, out);
        }
        CppDecl::Dtor { body, .. } => {
            collect_template_fn_calls_from_body(body, out);
        }
        CppDecl::Class(c) => {
            for m in &c.members {
                collect_template_fn_calls_from_decl(m, out);
            }
        }
        CppDecl::Namespace { body, .. } => {
            for inner in body {
                collect_template_fn_calls_from_decl(inner, out);
            }
        }
        _ => {}
    }
}

/// 从函数体中收集模板函数调用
fn collect_template_fn_calls_from_body(body: &CppBody, out: &mut Vec<(String, Vec<MangleType>)>) {
    for stmt in &body.stmts {
        collect_template_fn_calls_from_stmt(stmt, out);
    }
}

/// 从语句中收集模板函数调用
fn collect_template_fn_calls_from_stmt(s: &CppStmt, out: &mut Vec<(String, Vec<MangleType>)>) {
    match s {
        CppStmt::Expr(e) | CppStmt::Return(Some(e)) => {
            collect_template_fn_calls_from_expr(e, out);
        }
        CppStmt::LocalVar { init, .. } | CppStmt::StaticLocal { init, .. } => {
            if let Some(e) = init {
                collect_template_fn_calls_from_expr(e, out);
            }
        }
        CppStmt::If { cond, then, els } => {
            collect_template_fn_calls_from_expr(cond, out);
            collect_template_fn_calls_from_body(then, out);
            if let Some(e) = els {
                collect_template_fn_calls_from_body(e, out);
            }
        }
        CppStmt::While { cond, body } => {
            collect_template_fn_calls_from_expr(cond, out);
            collect_template_fn_calls_from_body(body, out);
        }
        CppStmt::For { init, cond, step, body } => {
            if let Some(i) = init {
                collect_template_fn_calls_from_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_template_fn_calls_from_expr(c, out);
            }
            if let Some(s) = step {
                collect_template_fn_calls_from_stmt(s, out);
            }
            collect_template_fn_calls_from_body(body, out);
        }
        CppStmt::Block(b) => collect_template_fn_calls_from_body(b, out),
        _ => {}
    }
}

/// 从表达式中收集模板函数调用
fn collect_template_fn_calls_from_expr(e: &CppExpr, out: &mut Vec<(String, Vec<MangleType>)>) {
    match e {
        CppExpr::Call(f, args) => {
            // 检查是否是模板函数调用: foo<int>(...)
            if let CppExpr::TemplateIdent { name, args: targs } = &**f {
                out.push((name.clone(), targs.clone()));
            }
            // 递归处理参数
            for a in args {
                collect_template_fn_calls_from_expr(a, out);
            }
            collect_template_fn_calls_from_expr(f, out);
        }
        CppExpr::BinOp(_, l, r) => {
            collect_template_fn_calls_from_expr(l, out);
            collect_template_fn_calls_from_expr(r, out);
        }
        CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => {
            collect_template_fn_calls_from_expr(x, out);
        }
        CppExpr::Member(o, _) | CppExpr::PtrMember(o, _) => {
            collect_template_fn_calls_from_expr(o, out);
        }
        CppExpr::Cast(_, inner) | CppExpr::StaticCast { expr: inner, .. } => {
            collect_template_fn_calls_from_expr(inner, out);
        }
        _ => {}
    }
}

/// 将声明中的模板类型替换为实例化后的类名
/// 例如: `Foo<int>` -> `__dcc_template_Foo_int`
fn replace_template_types_in_decls(decls: &[CppDecl], reg: &TemplateRegistry) -> Vec<CppDecl> {
    decls
        .iter()
        .map(|d| replace_template_types_in_decl(d, reg))
        .collect()
}

/// 替换单个声明中的模板类型
fn replace_template_types_in_decl(d: &CppDecl, reg: &TemplateRegistry) -> CppDecl {
    match d {
        CppDecl::GlobalVar { ty, name, init, is_static, is_volatile } => CppDecl::GlobalVar {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            init: init.as_ref().map(|e| replace_template_types_in_expr(e, reg)),
            is_static: *is_static,
            is_volatile: *is_volatile,
        },
        CppDecl::FreeFn { ret, name, params, body, is_noexcept,
            trailing_ret_type } => CppDecl::FreeFn {
            ret: replace_template_type(ret, reg),
            name: name.clone(),
            params: params
                .iter()
                .map(|(n, t)| (n.clone(), replace_template_type(t, reg)))
                .collect(),
            body: replace_template_types_in_body(body, reg),
            is_noexcept: *is_noexcept,
            trailing_ret_type: trailing_ret_type.as_ref().map(|t| replace_template_type(t, reg)),
        },
        CppDecl::Class(c) => {
            let new_bases = c
                .bases
                .iter()
                .map(|base| {
                    if let Some(args) = &base.template_args {
                        let new_name = instantiated_class_name(&base.name, args);
                        crate::cpp::parser::BaseSpec {
                            name: new_name,
                            template_args: None,
                            ..base.clone()
                        }
                    } else {
                        base.clone()
                    }
                })
                .collect();
            CppDecl::Class(ClassDecl {
                name: c.name.clone(),
                is_struct: c.is_struct,
                is_union: c.is_union,
                bases: new_bases,
                members: c
                    .members
                    .iter()
                    .map(|m| replace_template_types_in_decl(m, reg))
                    .collect(),
            })
        }
        CppDecl::MemberVar { ty, name, is_mutable, is_volatile } => CppDecl::MemberVar {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            is_mutable: *is_mutable,
            is_volatile: *is_volatile,
        },
        CppDecl::MemberFn {
            class,
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
        } => CppDecl::MemberFn {
            class: class.clone(),
            ret: replace_template_type(ret, reg),
            name: name.clone(),
            params: params
                .iter()
                .map(|(n, t)| (n.clone(), replace_template_type(t, reg)))
                .collect(),
            body: replace_template_types_in_body(body, reg),
            is_virtual: *is_virtual,
            is_override: *is_override,
            is_const: *is_const,
            is_final: *is_final,
            is_noexcept: *is_noexcept,
            is_volatile: *is_volatile,
            is_friend: *is_friend,
        },
        CppDecl::Ctor {
            class,
            params,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
        } => CppDecl::Ctor {
            class: class.clone(),
            params: params
                .iter()
                .map(|(n, t)| (n.clone(), replace_template_type(t, reg)))
                .collect(),
            inits: inits
                .iter()
                .map(|(n, e)| (n.clone(), replace_template_types_in_expr(e, reg)))
                .collect(),
            body: replace_template_types_in_body(body, reg),
            is_virtual: *is_virtual,
            is_explicit: *is_explicit,
            is_noexcept: *is_noexcept,
        },
        CppDecl::Dtor {
            class,
            body,
            is_virtual,
            is_noexcept,
        } => CppDecl::Dtor {
            class: class.clone(),
            body: replace_template_types_in_body(body, reg),
            is_virtual: *is_virtual,
            is_noexcept: *is_noexcept,
        },
        CppDecl::Typedef { target, name } => CppDecl::Typedef {
            target: replace_template_type(target, reg),
            name: name.clone(),
        },
        CppDecl::Namespace { name, body } => CppDecl::Namespace {
            name: name.clone(),
            body: replace_template_types_in_decls(body, reg),
        },
        // 模板声明本身保留（会被过滤掉），但内部不替换
        other => other.clone(),
    }
}

/// 替换类型中的模板引用
fn replace_template_type(ty: &MangleType, reg: &TemplateRegistry) -> MangleType {
    match ty {
        MangleType::Template { name, args } => {
            // 检查是否是已实例化的类模板
            let key = TemplateRegistry::key_for(name, args);
            if reg.inst_cache.contains_key(&key) {
                // 替换为实例化后的类名
                MangleType::Class(instantiated_class_name(name, args))
            } else {
                // 未实例化，递归替换参数后保留
                let new_args: Vec<MangleType> =
                    args.iter().map(|a| replace_template_type(a, reg)).collect();
                MangleType::Template {
                    name: name.clone(),
                    args: new_args,
                }
            }
        }
        MangleType::Pointer(t) => {
            MangleType::Pointer(Box::new(replace_template_type(t, reg)))
        }
        MangleType::Reference(t) => {
            MangleType::Reference(Box::new(replace_template_type(t, reg)))
        }
        MangleType::TplMember { name, args, member } => {
            // 模板成员访问：先替换模板类名
            let key = TemplateRegistry::key_for(name, args);
            if reg.inst_cache.contains_key(&key) {
                // 简化：转为普通类成员访问类型
                MangleType::Class(format!(
                    "{}::{}",
                    instantiated_class_name(name, args),
                    member
                ))
            } else {
                let new_args: Vec<MangleType> =
                    args.iter().map(|a| replace_template_type(a, reg)).collect();
                MangleType::TplMember {
                    name: name.clone(),
                    args: new_args,
                    member: member.clone(),
                }
            }
        }
        other => other.clone(),
    }
}

/// 替换函数体中的模板类型
fn replace_template_types_in_body(body: &CppBody, reg: &TemplateRegistry) -> CppBody {
    CppBody {
        stmts: body
            .stmts
            .iter()
            .map(|s| replace_template_types_in_stmt(s, reg))
            .collect(),
        is_decl: body.is_decl,
    }
}

/// 替换语句中的模板类型
fn replace_template_types_in_stmt(s: &CppStmt, reg: &TemplateRegistry) -> CppStmt {
    match s {
        CppStmt::LocalVar { ty, name, init, is_volatile } => CppStmt::LocalVar {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            init: init.as_ref().map(|e| replace_template_types_in_expr(e, reg)),
            is_volatile: *is_volatile,
        },
        CppStmt::StaticLocal { ty, name, init, is_volatile } => CppStmt::StaticLocal {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            init: init.as_ref().map(|e| replace_template_types_in_expr(e, reg)),
            is_volatile: *is_volatile,
        },
        CppStmt::Expr(e) => CppStmt::Expr(replace_template_types_in_expr(e, reg)),
        CppStmt::Return(e) => {
            CppStmt::Return(e.as_ref().map(|x| replace_template_types_in_expr(x, reg)))
        }
        CppStmt::If { cond, then, els } => CppStmt::If {
            cond: replace_template_types_in_expr(cond, reg),
            then: Box::new(replace_template_types_in_body(then, reg)),
            els: els
                .as_ref()
                .map(|x| Box::new(replace_template_types_in_body(x, reg))),
        },
        CppStmt::While { cond, body } => CppStmt::While {
            cond: replace_template_types_in_expr(cond, reg),
            body: Box::new(replace_template_types_in_body(body, reg)),
        },
        CppStmt::For { init, cond, step, body } => CppStmt::For {
            init: init
                .as_ref()
                .map(|x| Box::new(replace_template_types_in_stmt(x, reg))),
            cond: cond.as_ref().map(|x| replace_template_types_in_expr(x, reg)),
            step: step
                .as_ref()
                .map(|x| Box::new(replace_template_types_in_stmt(x, reg))),
            body: Box::new(replace_template_types_in_body(body, reg)),
        },
        CppStmt::Block(b) => CppStmt::Block(replace_template_types_in_body(b, reg)),
        CppStmt::Decl(d) => CppStmt::Decl(replace_template_types_in_decl(d, reg)),
        other => other.clone(),
    }
}

/// 替换表达式中的模板类型
fn replace_template_types_in_expr(e: &CppExpr, reg: &TemplateRegistry) -> CppExpr {
    match e {
        CppExpr::Cast(ty, inner) => CppExpr::Cast(
            replace_template_type(ty, reg),
            Box::new(replace_template_types_in_expr(inner, reg)),
        ),
        CppExpr::StaticCast { ty, expr } => CppExpr::StaticCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::ConstCast { ty, expr } => CppExpr::ConstCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::ReinterpretCast { ty, expr } => CppExpr::ReinterpretCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::DynamicCast { ty, expr } => CppExpr::DynamicCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::TypeidType(ty) => CppExpr::TypeidType(replace_template_type(ty, reg)),
        CppExpr::Call(f, args) => CppExpr::Call(
            Box::new(replace_template_types_in_expr(f, reg)),
            args.iter()
                .map(|a| replace_template_types_in_expr(a, reg))
                .collect(),
        ),
        CppExpr::BinOp(op, l, r) => CppExpr::BinOp(
            *op,
            Box::new(replace_template_types_in_expr(l, reg)),
            Box::new(replace_template_types_in_expr(r, reg)),
        ),
        CppExpr::UnaryOp(op, x) => {
            CppExpr::UnaryOp(*op, Box::new(replace_template_types_in_expr(x, reg)))
        }
        CppExpr::Ref(x) => CppExpr::Ref(Box::new(replace_template_types_in_expr(x, reg))),
        CppExpr::AddrOf(x) => CppExpr::AddrOf(Box::new(replace_template_types_in_expr(x, reg))),
        CppExpr::Member(o, s) => CppExpr::Member(
            Box::new(replace_template_types_in_expr(o, reg)),
            s.clone(),
        ),
        CppExpr::PtrMember(o, s) => CppExpr::PtrMember(
            Box::new(replace_template_types_in_expr(o, reg)),
            s.clone(),
        ),
        CppExpr::TypedInitList { ty, elems } => CppExpr::TypedInitList {
            ty: replace_template_type(ty, reg),
            elems: elems
                .iter()
                .map(|x| replace_template_types_in_expr(x, reg))
                .collect(),
        },
        CppExpr::TplStaticAccess { name, args, member } => {
            // 模板静态成员访问：替换为实例化类的成员访问
            let key = TemplateRegistry::key_for(name, args);
            if reg.inst_cache.contains_key(&key) {
                let class_name = instantiated_class_name(name, args);
                CppExpr::Member(
                    Box::new(CppExpr::Ident(class_name)),
                    member.clone(),
                )
            } else {
                e.clone()
            }
        }
        CppExpr::TemplateIdent { name, args } => {
            // 模板标识：可能是函数模板调用，也可能是类模板构造
            // 先检查是否是类模板实例
            let key = TemplateRegistry::key_for(name, args);
            if reg.inst_cache.contains_key(&key) {
                // 检查是类模板还是函数模板
                if let Some(cached) = reg.inst_cache.get(&key) {
                    match cached {
                        CppDecl::Class(_) => {
                            // 类模板：替换为实例化后的类名（用于构造函数调用）
                            let class_name = instantiated_class_name(name, args);
                            CppExpr::Ident(class_name)
                        }
                        CppDecl::FreeFn { .. } => {
                            // 函数模板：替换为实例化后的函数名
                            let fn_name = tplfn_c_name(name, args);
                            CppExpr::Ident(fn_name)
                        }
                        _ => e.clone(),
                    }
                } else {
                    e.clone()
                }
            } else {
                e.clone()
            }
        }
        other => other.clone(),
    }
}

/// 过滤掉模板声明（Template / PartialSpec / Concept 等），只保留普通声明
fn filter_template_decls(decls: &[CppDecl]) -> Vec<CppDecl> {
    decls
        .iter()
        .filter(|d| !matches!(d, CppDecl::Template { .. } | CppDecl::PartialSpec { .. } | CppDecl::Concept { .. }))
        .cloned()
        .collect()
}

/// 把 C++ 顶层声明通过原生路径降级为 ast::Program.
///
/// 流程:
/// 0. 模板收集 + 实例化 (Stage 8)
/// 1. `Sema` 构建 HIR + 收集符号表
/// 2. 两阶段 ABI 计算: 先注册所有 class + method, 再 compute
/// 3. `IrLowerer` 产出 SSA 函数 + vtable 数据
/// 4. 把每个 SSA 函数 `rebuild_block` 回 ast::Block, 组装 FuncDecl
/// 5. 为每个 class 生成 `Decl::StructDef`, 名字/字段顺序与 `lower_type` 一致
/// 6. 把 vtable 数据降级为 `Decl::Var` (extern 数组) 占位 (Stage 5 完整发射)
pub fn lower_decls_native(decls: &[CppDecl]) -> Result<NativeLowered, NativeError> {
    // Step 0: 收集模板并实例化 (Stage 8)
    let mut reg = collect_templates(decls);
    let instantiated = instantiate_templates_from_decls(&mut reg, decls)?;

    // 合并声明: 实例化声明 + 原始声明（过滤掉模板声明）
    let filtered = filter_template_decls(decls);
    let replaced = replace_template_types_in_decls(&filtered, &reg);
    let replaced_insts = replace_template_types_in_decls(&instantiated, &reg);

    // 实例化声明在前，确保 Sema 先处理它们
    let mut all_decls: Vec<CppDecl> = Vec::new();
    all_decls.extend(replaced_insts);
    all_decls.extend(replaced);

    // Step 1: 构建 Sema + HIR
    let mut sema = Sema::new();
    if let Err(e) = sema.build_hir_from_decls(&all_decls) {
        return Err(NativeError::Unsupported(e));
    }
    // Debug: DCC_CXX_HIR_DUMP=path 时把 HIR 翻译单元 dump 出来
    if let Ok(p) = std::env::var("DCC_CXX_HIR_DUMP") {
        let _ = std::fs::write(&p, format!("{:#?}\n", sema.hir()));
    }
    // 先克隆符号表 (sema.into_hir 只消费 hir, 但 clone 更简洁安全)
    let symbols: Vec<CxxSymbol> = sema.symbols().to_vec();
    // 收集所有类的基类信息 (用于 ABI 布局)
    let (class_bases, class_virtual_bases): (
        HashMap<ClassId, Vec<ClassId>>,
        HashMap<ClassId, Vec<ClassId>>,
    ) = collect_class_bases(&sema, &symbols);
    // Step 2: 计算 ABI (两阶段: 先全部注册, 再 compute)
    let abi_map: HashMap<ClassId, ClassAbi> =
        build_class_abis(&sema, &symbols, &class_bases, &class_virtual_bases)?;
    // Debug: DCC_CXX_ABI_DUMP=path 时把 ABI 布局表 dump 出来
    if let Ok(p) = std::env::var("DCC_CXX_ABI_DUMP") {
        let mut s = String::new();
        for (cid, abi) in &abi_map {
            s.push_str(&format!("class {:?} -> {:#?}\n", cid, abi));
        }
        let _ = std::fs::write(&p, s);
    }
    let hir: HirTranslationUnit = sema.into_hir();

    // Step 3: lower HIR -> SSA
    let mut lowerer = IrLowerer::new(&hir, &symbols);
    for (cid, abi) in &abi_map {
        lowerer.register_abi(*cid, abi.clone());
    }
    lowerer.lower()?;
    let output: LowerIrOutput = lowerer.finish();

    // Step 4 + 5: SSA 函数 + struct 定义 -> ast::Program
    let prog = build_program(&output, &abi_map, &hir, &symbols)?;
    // Debug: DCC_CXX_IR_DUMP=path 时把最终 ast::Program dump 出来
    if let Ok(p) = std::env::var("DCC_CXX_IR_DUMP") {
        let _ = std::fs::write(&p, format!("{:#?}\n", prog));
    }

    // Step 6: vtable 数据暂作 rodata 记号, Stage 5 完整发射.
    Ok(NativeLowered {
        prog,
        vtables: output.vtables,
        rodata: output.rodata,
    })
}

// === ABI 计算 (两阶段) ===

/// 从 Sema 中收集每个类的直接基类列表（非虚基类和虚基类分开）。
/// 返回 (non_virtual_bases, virtual_bases)。
fn collect_class_bases(
    sema: &Sema,
    symbols: &[CxxSymbol],
) -> (HashMap<ClassId, Vec<ClassId>>, HashMap<ClassId, Vec<ClassId>>) {
    let mut non_virtual = HashMap::new();
    let mut virtual_bases = HashMap::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let mut nv_bases = Vec::new();
        let mut v_bases = Vec::new();
        if let Some(info) = sema.get_class_info(cid) {
            for base in &info.bases {
                if base.is_virtual {
                    v_bases.push(base.class);
                } else {
                    nv_bases.push(base.class);
                }
            }
        }
        non_virtual.insert(cid, nv_bases);
        virtual_bases.insert(cid, v_bases);
    }
    (non_virtual, virtual_bases)
}

fn build_class_abis(
    sema: &Sema,
    symbols: &[CxxSymbol],
    class_bases: &HashMap<ClassId, Vec<ClassId>>,
    class_virtual_bases: &HashMap<ClassId, Vec<ClassId>>,
) -> Result<HashMap<ClassId, ClassAbi>, NativeError> {
    let mut calc = ItaniumAbiCalculator::new();

    // 第一遍: 注册所有 class (含基类), 即使基类也先注册避免 compute 递归失败
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let mut fields: Vec<(String, CxxType)> = Vec::new();
        for s2 in symbols {
            if s2.owner == Some(sym.id)
                && matches!(s2.kind, CxxSymbolKind::Field | CxxSymbolKind::Variable)
                && !s2.name.is_empty()
            {
                fields.push((s2.name.clone(), s2.ty.clone()));
            }
        }
        let bases = class_bases.get(&cid).cloned().unwrap_or_default();
        let virtual_bases = class_virtual_bases.get(&cid).cloned().unwrap_or_default();
        // Stage 4: has_virtual 由方法注册阶段决定，这里先置 false
        calc.register_class(cid, bases, virtual_bases, fields, false);
    }

    // 第二遍: 注册方法 (需要 class 已经注册好; 现在补充 has_virtual / method 列表)
    // 注: ItaniumAbiCalculator::register_method 会修改 has_virtual, 必须在 register_class 之后
    for sym in symbols {
        if !matches!(
            sym.kind,
            CxxSymbolKind::Method | CxxSymbolKind::Constructor | CxxSymbolKind::Destructor
        ) {
            continue;
        }
        let Some(owner_sym_id) = sym.owner else {
            continue;
        };
        let cid = ClassId::from(owner_sym_id);
        let is_virtual = sema
            .get_class_info(cid)
            .map(|info| info.virtual_methods.iter().any(|vm| vm.symbol == sym.id))
            .unwrap_or(false);
        let owner_class = sema
            .get_class_info(cid)
            .and_then(|info| {
                info.virtual_methods
                    .iter()
                    .find(|vm| vm.symbol == sym.id)
                    .and_then(|vm| vm.overrides)
            })
            .and_then(|base_method| symbols.iter().find(|s| s.id == base_method))
            .and_then(|base_sym| base_sym.owner)
            .map(ClassId::from)
            .unwrap_or(cid);
        calc.register_method_with_symbol(
            cid,
            sym.name.clone(),
            sym.name.clone(),
            is_virtual,
            owner_class,
        );
    }

    // 第三遍: compute (顺序无关, 因为已注册所有依赖)
    let mut abis = HashMap::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let abi = calc.compute_class(cid);
        abis.insert(cid, abi);
    }
    Ok(abis)
}

// === Program 组装 ===

fn build_program(
    output: &LowerIrOutput,
    abi_map: &HashMap<ClassId, ClassAbi>,
    hir: &HirTranslationUnit,
    symbols: &[CxxSymbol],
) -> Result<ast::Program, NativeError> {
    let mut prog = ast::Program {
        decls: Vec::new(),
        fenv_access: false,
        fp_contract: false,
        cx_limited_range: false,
    };

    // 先发射 struct 定义 (Decl::StructDef), 名字与字段顺序与 lower_type 一致
    let mut emitted_structs: HashSet<String> = HashSet::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let Some(abi) = abi_map.get(&cid) else {
            continue;
        };
        let name = sym.name.clone();
        if emitted_structs.contains(&name) {
            continue;
        }
        emitted_structs.insert(name.clone());

        // 收集展平字段 (基类在前, 自身字段在后; 与 lower_ir::lower_class_type 保持一致)
        let fields = collect_flat_struct_fields(cid, abi, abi_map, symbols)?;
        let struct_def = ast::Decl::StructDef(ast::StructDef {
            name: ast::Ident {
                name,
                span: dummy_span(),
            },
            fields,
            is_union: false,
            span: dummy_span(),
        });
        prog.decls.push(struct_def);
    }

    // 再发射函数 (FuncDecl): SSA -> ast::Block -> FuncDecl
    for func in &output.functions {
        let decl = build_func_decl(func)?;
        prog.decls.push(decl);
    }

    // 最后发射 vtable 占位 (Decl::Var, extern, Stage 5 替换为真实数据)
    for (name, _entries) in &output.vtables {
        prog.decls.push(ast::Decl::Var(ast::VarDecl {
            name: ast::Ident {
                name: name.clone(),
                span: dummy_span(),
            },
            ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar))),
            init: None,
            span: dummy_span(),
            is_extern: true,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: Vec::new(),
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: ast::QualifierFlags::NONE,
            addr_space: ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        }));
    }

    // 发射 RTTI typeinfo 符号 (extern 声明，假设由 libstdc++ 或链接时提供)
    // typeinfo 符号以 _ZTI 开头（Itanium ABI mangling）
    for sym in &output.rodata {
        if sym.starts_with("_ZTI") {
            prog.decls.push(ast::Decl::Var(ast::VarDecl {
                name: ast::Ident {
                    name: sym.clone(),
                    span: dummy_span(),
                },
                ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar))),
                init: None,
                span: dummy_span(),
                is_extern: true,
                is_static: false,
                section: None,
                alignment: None,
                is_weak: false,
                visibility: None,
                is_unused: false,
                register_name: None,
                cleanup_fn: None,
                is_thread_local: false,
                no_sanitize: Vec::new(),
                alias_name: None,
                copy_fn: None,
                is_retain: false,
                is_used_attr: false,
                quals: ast::QualifierFlags::NONE,
                addr_space: ast::AddrSpace::None,
                tls_model: None,
                asm_label: None,
            }));
        }
    }

    // 发射 dcc++ runtime symbols（如 dccpp_dur_free, __dcc_eh_throw 等）
    // 作为 extern var 占位符（实际是函数，但目前编译器仅需要声明存在）
    for sym in &output.external_symbols {
        prog.decls.push(ast::Decl::Var(ast::VarDecl {
            name: ast::Ident {
                name: sym.clone(),
                span: dummy_span(),
            },
            ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar))),
            init: None,
            span: dummy_span(),
            is_extern: true,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: Vec::new(),
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: ast::QualifierFlags::NONE,
            addr_space: ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        }));
    }
    let _ = hir; // 当前未直接读 (已通过 lower_ir 用过), 保留参数便于扩展
    Ok(prog)
}

/// 与 lower_ir::lower_class_type 一致的字段展平顺序.
fn collect_flat_struct_fields(
    cid: ClassId,
    abi: &ClassAbi,
    abi_map: &HashMap<ClassId, ClassAbi>,
    symbols: &[CxxSymbol],
) -> Result<Vec<ast::Field>, NativeError> {
    let mut out = Vec::new();
    // 非虚基类在前
    for base in &abi.bases {
        let Some(base_abi) = abi_map.get(&base.base) else {
            return Err(NativeError::Internal(format!(
                "missing ABI for base class {}",
                base.base.0
            )));
        };
        let mut sub = collect_flat_struct_fields(base.base, base_abi, abi_map, symbols)?;
        // 把子字段的 offset 加上 base.offset (lower_ir 同样做法)
        for f in &mut sub {
            f.offset += base.offset as usize;
        }
        out.extend(sub);
    }
    // 自身字段 (从 symbols 表取真实类型)
    for fl in &abi.fields {
        let field_ty = lookup_field_type(cid, &fl.name, symbols).unwrap_or(ast::Type::Int);
        out.push(ast::Field {
            name: fl.name.clone(),
            ty: field_ty,
            offset: fl.offset as usize,
            bit_width: None,
            bit_offset: 0,
            quals: ast::QualifierFlags::NONE,
            ptr_quals: Vec::new(),
            field_align: None,
            field_packed: false,
        });
    }
    // 虚基类字段在最后 (Stage 6)
    for vb in &abi.virtual_bases {
        let Some(vb_abi) = abi_map.get(&vb.base) else {
            return Err(NativeError::Internal(format!(
                "missing ABI for virtual base class {}",
                vb.base.0
            )));
        };
        let mut sub = collect_flat_struct_fields(vb.base, vb_abi, abi_map, symbols)?;
        // 虚基类的偏移: 从 vtable 中读取的 vbase offset
        // 这里我们需要计算实际偏移。由于我们把虚基类放在对象末尾，
        // 偏移值存储在 vtable 的 VBaseOffset 条目中。
        // 为了获取这个值，我们从 primary vtable 中查找。
        let vbase_offset = abi
            .vtables
            .first()
            .and_then(|vt| {
                vt.components.iter().find_map(|comp| {
                    if let crate::cpp::abi_layout::VTableComponent::VBaseOffset {
                        target,
                        offset,
                    } = comp
                    {
                        if *target == vb.base {
                            Some(*offset as usize)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(0);
        for f in &mut sub {
            f.offset += vbase_offset;
        }
        out.extend(sub);
    }
    Ok(out)
}

/// 在符号表里查字段类型 (owner == cid, name == f.name).
fn lookup_field_type(cid: ClassId, name: &str, symbols: &[CxxSymbol]) -> Option<ast::Type> {
    let owner_sym = cid.as_symbol();
    symbols
        .iter()
        .find(|s| s.owner == Some(owner_sym) && s.name == name)
        .and_then(|s| cxx_ty_to_ast_ty(&s.ty))
}

/// CxxType -> ast::Type 的最小子集映射 (与 lower_ir::lower_type 保持一致).
/// 失败时返回 None, 让 fallback 兜底.
fn cxx_ty_to_ast_ty(ty: &CxxType) -> Option<ast::Type> {
    // 复用 lower_ir 的 lower_type: 构造一个临时 lowerer.
    // 如果类型涉及 Class 且没有注册 ABI, 会失败并返回 None.
    let hir = HirTranslationUnit::default();
    let symbols: Vec<CxxSymbol> = Vec::new();
    let lowerer = IrLowerer::new(&hir, &symbols);
    match lowerer.lower_type(ty) {
        Ok(t) => Some(t),
        Err(_) => None,
    }
}

// === FuncDecl 组装 ===

fn build_func_decl(func: &ssa::Function) -> Result<ast::Decl, NativeError> {
    let name = func.name.clone();
    let ret_ty = func.ret_ty.clone();

    // SSA -> AST: 重建参数列表
    let mut params = Vec::new();
    for p in &func.params {
        params.push(ast::Param {
            ty: p.ty.clone(),
            name: Some(ast::Ident {
                name: p.base.clone(),
                span: dummy_span(),
            }),
            span: dummy_span(),
            quals: ast::QualifierFlags::NONE,
            vla_dim: None,
        });
    }

    // SSA -> AST: 函数体
    let body = ssa::rebuild_block(func);

    // ast::FuncDecl 没有 Default, 需逐字段构造.
    // Stage 3 仅生成最小必要字段; 其他属性使用默认值.
    let func_decl = ast::FuncDecl {
        ret_ty,
        name: ast::Ident {
            name,
            span: dummy_span(),
        },
        params,
        body: Some(body),
        span: dummy_span(),
        is_extern: false,
        is_static: false,
        section: None,
        alignment: None,
        is_weak: false,
        visibility: None,
        is_unused: false,
        is_cold: func.is_cold,
        is_hot: func.is_hot,
        is_noinline: false,
        is_always_inline: false,
        is_inline: false,
        is_variadic: false,
        is_constructor: false,
        is_destructor: false,
        alias_name: None,
        is_nested: false,
        upvars: Vec::new(),
        is_old_style: false,
        format_kind: None,
        format_string_idx: None,
        format_first_arg: None,
        no_sanitize: Vec::new(),
        target_clones: Vec::new(),
        returns_nonnull: false,
        ctor_priority: None,
        dtor_priority: None,
        is_retain: false,
        is_used_attr: false,
        is_flatten: false,
        error_msg: None,
        warning_msg: None,
        deprecated_msg: None,
        ifunc_resolver: None,
        asm_output_template: None,
        is_global: true,
        is_device: false,
        is_host: false,
        is_naked: false,
        is_noreturn: func.is_noreturn,
        is_pure: false,
        is_const_attr: false,
        returns_twice: false,
        no_instrument: false,
        is_malloc: false,
        malloc_dealloc: None,
        malloc_dealloc_argno: None,
        alloc_size: None,
        assume_aligned: None,
        warn_unused_result: false,
        is_sentinel: false,
        format_arg_idx: None,
        is_nodiscard: false,
        asm_label: None,
    };
    Ok(ast::Decl::Func(func_decl))
}

fn dummy_span() -> Span {
    Span::point("<native>", 1, 1)
}

// === Feature detection (Stage 10) ===

/// 递归扫描声明列表，检测使用到的所有 C++ 特性。
/// 用于 auto-mode 下判断是否可以走 native path，以及输出诊断信息。
pub fn detect_features(decls: &[CppDecl]) -> HashSet<CppFeature> {
    let mut features = HashSet::new();
    for d in decls {
        detect_features_from_decl(d, &mut features);
    }
    features
}

/// 从单个声明中检测特性
fn detect_features_from_decl(d: &CppDecl, features: &mut HashSet<CppFeature>) {
    match d {
        CppDecl::FreeFn { ret, params, body, .. } => {
            features.insert(CppFeature::FreeFunction);
            detect_features_from_type(ret, features);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        }
        CppDecl::MemberFn {
            ret,
            params,
            body,
            is_virtual,
            ..
        } => {
            features.insert(CppFeature::MemberFunction);
            if *is_virtual {
                features.insert(CppFeature::VirtualFunction);
                features.insert(CppFeature::VPtrInit);
            }
            detect_features_from_type(ret, features);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        }
        CppDecl::Ctor { params, body, .. } => {
            features.insert(CppFeature::CtorDtor);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        }
        CppDecl::Dtor { body, is_virtual, .. } => {
            features.insert(CppFeature::CtorDtor);
            if *is_virtual {
                features.insert(CppFeature::VirtualFunction);
                features.insert(CppFeature::VPtrInit);
            }
            detect_features_from_body(body, features);
        }
        CppDecl::Class(c) => {
            features.insert(CppFeature::PlainClass);
            // 检查继承
            let has_base = !c.bases.is_empty();
            let has_virtual_base = c.bases.iter().any(|b| b.is_virtual);
            if has_virtual_base {
                features.insert(CppFeature::VirtualInheritance);
                // diamond 检测：只有当存在被多路径共享的虚基时才标记
                // 仅检查本层：若同一虚基类在 c.bases 中出现 ≥2 次，直接视为 diamond
                let mut vb_count: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for b in &c.bases {
                    if b.is_virtual {
                        *vb_count.entry(b.name.as_str()).or_insert(0) += 1;
                    }
                }
                let has_diamond_in_self = vb_count.values().any(|&n| n >= 2);
                // 跨层 diamond 需要完整继承图分析；此处保守只在本层重复时标记
                // 跨层 diamond 由 can_compile_native 进一步判断
                if has_diamond_in_self {
                    features.insert(CppFeature::DiamondVirtualInheritance);
                }
            }
            if has_base {
                features.insert(CppFeature::NonVirtualInheritance);
                features.insert(CppFeature::BaseCtorOrder);
            }
            // 检查成员
            for m in &c.members {
                detect_features_from_decl(m, features);
            }
            // 检查是否有虚函数（用于 vptr 初始化）
            let has_virtual_method = c.members.iter().any(|m| matches!(
                m,
                CppDecl::MemberFn { is_virtual: true, .. }
                    | CppDecl::Dtor { is_virtual: true, .. }
            ));
            if has_virtual_method {
                features.insert(CppFeature::ThisAdjustThunk);
            }
        }
        CppDecl::Namespace { body, .. } => {
            features.insert(CppFeature::Namespace);
            for inner in body {
                detect_features_from_decl(inner, features);
            }
        }
        CppDecl::Using { .. } | CppDecl::UsingEnum { .. } => {
            features.insert(CppFeature::Namespace);
        }
        CppDecl::OperatorFn { body, ret, params, .. } => {
            features.insert(CppFeature::OperatorOverload);
            detect_features_from_type(ret, features);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        }
        CppDecl::Template { decl, .. } => {
            // 根据内部声明类型判断是函数模板还是类模板
            match &**decl {
                CppDecl::Class(_) => {
                    features.insert(CppFeature::ClassTemplate);
                }
                CppDecl::FreeFn { .. } | CppDecl::MemberFn { .. } => {
                    features.insert(CppFeature::FunctionTemplate);
                }
                _ => {}
            }
            detect_features_from_decl(decl, features);
        }
        CppDecl::PartialSpec { .. } => {
            features.insert(CppFeature::PartialSpecialization);
        }
        CppDecl::Concept { .. } => {
            features.insert(CppFeature::Concept);
        }
        CppDecl::Module { .. } | CppDecl::Import { .. } | CppDecl::PrivateFragment => {
            features.insert(CppFeature::Module);
        }
        CppDecl::ExportBlock(body) => {
            features.insert(CppFeature::Module);
            for inner in body {
                detect_features_from_decl(inner, features);
            }
        }
        CppDecl::GlobalVar { ty, init, .. } => {
            detect_features_from_type(ty, features);
            if let Some(e) = init {
                detect_features_from_expr(e, features);
            }
        }
        CppDecl::MemberVar { ty, .. } => {
            detect_features_from_type(ty, features);
        }
        CppDecl::Typedef { target, .. } => {
            detect_features_from_type(target, features);
        }
        CppDecl::AnonAggr { members, .. } => {
            for m in members {
                detect_features_from_decl(m, features);
            }
        }
        CppDecl::InheritingCtor { .. } => {
            features.insert(CppFeature::NonVirtualInheritance);
        }
        _ => {}
    }
}

/// 从类型中检测特性
fn detect_features_from_type(ty: &MangleType, features: &mut HashSet<CppFeature>) {
    match ty {
        MangleType::Reference(_) => {
            features.insert(CppFeature::ReferenceType);
        }
        MangleType::Template { args, .. } => {
            features.insert(CppFeature::ClassTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        }
        MangleType::Pointer(t) | MangleType::Reference(t) => {
            detect_features_from_type(t, features);
        }
        MangleType::TplMember { args, .. } => {
            features.insert(CppFeature::ClassTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        }
        _ => {}
    }
}

/// 从函数体中检测特性
fn detect_features_from_body(body: &CppBody, features: &mut HashSet<CppFeature>) {
    for stmt in &body.stmts {
        detect_features_from_stmt(stmt, features);
    }
}

/// 从语句中检测特性
fn detect_features_from_stmt(s: &CppStmt, features: &mut HashSet<CppFeature>) {
    match s {
        CppStmt::Expr(e) | CppStmt::Return(Some(e)) => {
            detect_features_from_expr(e, features);
        }
        CppStmt::If { cond, then, els } => {
            detect_features_from_expr(cond, features);
            detect_features_from_body(then, features);
            if let Some(e) = els {
                detect_features_from_body(e, features);
            }
        }
        CppStmt::IfConstexpr { cond, then, els } => {
            detect_features_from_expr(cond, features);
            detect_features_from_body(then, features);
            if let Some(e) = els {
                detect_features_from_body(e, features);
            }
        }
        CppStmt::While { cond, body } => {
            detect_features_from_expr(cond, features);
            detect_features_from_body(body, features);
        }
        CppStmt::For { init, cond, step, body } => {
            if let Some(i) = init {
                detect_features_from_stmt(i, features);
            }
            if let Some(c) = cond {
                detect_features_from_expr(c, features);
            }
            if let Some(s) = step {
                detect_features_from_stmt(s, features);
            }
            detect_features_from_body(body, features);
        }
        CppStmt::Block(b) => detect_features_from_body(b, features),
        CppStmt::ThisMember(_, stmt) => {
            detect_features_from_stmt(stmt, features);
        }
        CppStmt::LocalVar { ty, init, .. } => {
            detect_features_from_type(ty, features);
            if let Some(e) = init {
                detect_features_from_expr(e, features);
            }
        }
        CppStmt::StaticLocal { ty, init, .. } => {
            features.insert(CppFeature::StaticInitGuard);
            detect_features_from_type(ty, features);
            if let Some(e) = init {
                detect_features_from_expr(e, features);
            }
        }
        CppStmt::Try { body, catches } => {
            features.insert(CppFeature::Exception);
            features.insert(CppFeature::DtorCleanup);
            detect_features_from_body(body, features);
            for c in catches {
                detect_features_from_body(&c.body, features);
            }
        }
        CppStmt::Throw(_) => {
            features.insert(CppFeature::Exception);
        }
        CppStmt::Switch { expr, arms, .. } => {
            detect_features_from_expr(expr, features);
            for (_, body) in arms {
                detect_features_from_body(body, features);
            }
        }
        CppStmt::CoAwait(_) | CppStmt::CoYield(_) | CppStmt::CoReturn(_) => {
            features.insert(CppFeature::Coroutine);
        }
        CppStmt::Decl(d) => detect_features_from_decl(d, features),
        _ => {}
    }
}

/// 从表达式中检测特性
fn detect_features_from_expr(e: &CppExpr, features: &mut HashSet<CppFeature>) {
    match e {
        CppExpr::Typeid(_) | CppExpr::TypeidType(_) => {
            features.insert(CppFeature::TypeId);
        }
        CppExpr::DynamicCast { expr, .. } => {
            features.insert(CppFeature::DynamicCast);
            detect_features_from_expr(expr, features);
        }
        CppExpr::New(_, _) | CppExpr::NewArr { .. } | CppExpr::PlacementNew { .. }
        | CppExpr::Delete(_) | CppExpr::DeleteArr(_) => {
            features.insert(CppFeature::NewDelete);
        }
        CppExpr::InitList(_) | CppExpr::TypedInitList { .. } => {
            features.insert(CppFeature::InitializerList);
        }
        CppExpr::Call(f, args) => {
            detect_features_from_expr(f, features);
            for a in args {
                detect_features_from_expr(a, features);
            }
        }
        CppExpr::BinOp(_, l, r) => {
            detect_features_from_expr(l, features);
            detect_features_from_expr(r, features);
        }
        CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => {
            detect_features_from_expr(x, features);
        }
        CppExpr::Member(o, _) | CppExpr::PtrMember(o, _)
        | CppExpr::MemberStar(o, _) | CppExpr::PtrMemberStar(o, _) => {
            detect_features_from_expr(o, features);
        }
        CppExpr::Cast(ty, inner)
        | CppExpr::StaticCast { ty, expr: inner }
        | CppExpr::ConstCast { ty, expr: inner }
        | CppExpr::ReinterpretCast { ty, expr: inner } => {
            detect_features_from_type(ty, features);
            detect_features_from_expr(inner, features);
        }
        CppExpr::TemplateIdent { args, .. } => {
            features.insert(CppFeature::FunctionTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        }
        CppExpr::TplStaticAccess { args, .. } => {
            features.insert(CppFeature::ClassTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        }
        CppExpr::OpCall { args, .. } => {
            features.insert(CppFeature::OperatorOverload);
            for a in args {
                detect_features_from_expr(a, features);
            }
        }
        CppExpr::Ternary { cond, then, els } => {
            detect_features_from_expr(cond, features);
            detect_features_from_expr(then, features);
            detect_features_from_expr(els, features);
        }
        CppExpr::Comma(elems) => {
            for e in elems {
                detect_features_from_expr(e, features);
            }
        }
        CppExpr::MultiIndex(obj, idxs) => {
            detect_features_from_expr(obj, features);
            for i in idxs {
                detect_features_from_expr(i, features);
            }
        }
        CppExpr::Lambda(_) => {
            // lambda 暂不单独作为 feature，降级为普通函数处理
        }
        CppExpr::Sizeof(arg) => {
            if let crate::cpp::parser::SizeofArg::Type(t) = arg {
                detect_features_from_type(t, features);
            }
        }
        _ => {}
    }
}

/// 检查声明列表是否可以全部走 native path。
/// 返回 Ok(()) 表示所有使用到的特性都已支持，
/// 返回 Err(Vec<CppFeature>) 列出不支持的特性。
pub fn can_compile_native(decls: &[CppDecl]) -> Result<(), Vec<CppFeature>> {
    let features = detect_features(decls);
    let mut unsupported: Vec<CppFeature> = features
        .iter()
        .copied()
        .filter(|f| !f.native_supported())
        .collect();
    // 按 name 排序，保证输出稳定
    unsupported.sort_by_key(|f| f.name());
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(unsupported)
    }
}

// === Feature gating 辅助 ===

/// 检查一组声明是否全部走 native path 可行 (auto-mode 用作 fallback 决策).
/// 不支持特性 (虚函数/模板/异常/...) 任一出现 -> 返回 false.
pub fn native_supported(decls: &[CppDecl]) -> Result<(), NativeError> {
    // Stage 10: 复用 can_compile_native 的检测结果
    match can_compile_native(decls) {
        Ok(()) => Ok(()),
        Err(unsupported) => {
            let names: Vec<&str> = unsupported.iter().map(|f| f.name()).collect();
            Err(NativeError::Unsupported(format!(
                "unsupported features: {}",
                names.join(", ")
            )))
        }
    }
}

/// 当前 native path 标识 (用于 verbose 输出)
pub fn current_path() -> CppPath {
    CppPath::Native
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::CppParser;

    #[test]
    fn lower_free_function_native() {
        let src = b"int add(int a, int b) { return a + b; }\n\
int main() { return add(1, 2); }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");
        // 至少应该有 add 和 main 两个函数 + 没有额外 struct
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert_eq!(fn_count, 2);
    }

    #[test]
    fn lower_plain_class_native() {
        // Stage 3 基础: class 定义 + 成员函数 + 构造函数 (框架验证)
        // 隐式 this->field 访问和 sizeof(class) 留到 Stage 4,
        // 这里只验证 struct + 函数签名走通
        let src = b"\
class Point {\n\
public:\n\
    int x;\n\
    int y;\n\
    Point() {}\n\
    int dummy() { return 42; }\n\
};\n\
int global = 0;\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");
        // 应该有 1 个 struct (Point)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 1);
        // 至少 2 个函数 (ctor + dummy = 2)
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert!(
            fn_count >= 2,
            "expected at least 2 functions, got {}",
            fn_count
        );
    }

    #[test]
    fn lower_single_inheritance_native() {
        // Stage 4: 单继承 - 验证基类字段被展平到派生类 struct
        let src = b"\
class Base {\n\
public:\n\
    int b;\n\
};\n\
class Derived : public Base {\n\
public:\n\
    int d;\n\
    Derived() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 2 个 struct (Base + Derived)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 2);

        // 找到 Derived 的 struct 定义，验证包含基类字段 b
        let derived_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "Derived" => Some(sd),
            _ => None,
        });
        assert!(derived_struct.is_some(), "Derived struct not found");
        let sd = derived_struct.unwrap();
        // Derived 应该有 2 个字段: b (from Base) + d (自己)
        assert_eq!(sd.fields.len(), 2);
        assert_eq!(sd.fields[0].name, "b");
        assert_eq!(sd.fields[0].offset, 0);
        assert_eq!(sd.fields[1].name, "d");
        assert_eq!(sd.fields[1].offset, 4);
    }

    #[test]
    fn lower_multiple_inheritance_native() {
        // Stage 4: 多重非虚继承 - 验证两个基类都被展平
        let src = b"\
class A {\n\
public:\n\
    int a;\n\
};\n\
class B {\n\
public:\n\
    double b;\n\
};\n\
class C : public A, public B {\n\
public:\n\
    int c;\n\
    C() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");

        // 找到 C 的 struct 定义
        let c_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "C" => Some(sd),
            _ => None,
        });
        assert!(c_struct.is_some(), "C struct not found");
        let sd = c_struct.unwrap();
        // C 应该有 3 个字段: a (from A) + b (from B) + c (自己)
        assert_eq!(sd.fields.len(), 3);
        assert_eq!(sd.fields[0].name, "a");
        assert_eq!(sd.fields[0].offset, 0);
        assert_eq!(sd.fields[1].name, "b");
        // B 对齐到 8 字节，所以 b 的 offset 是 8
        assert_eq!(sd.fields[1].offset, 8);
        assert_eq!(sd.fields[2].name, "c");
        assert_eq!(sd.fields[2].offset, 16);
    }

    #[test]
    fn native_supported_allows_non_virtual_inheritance() {
        // Stage 4: 非虚继承应该被 native_supported 接受
        let src = b"\
class Base { int x; };\n\
class Derived : public Base { int y; };\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(
            result.is_ok(),
            "non-virtual inheritance should be supported"
        );
    }

    #[test]
    fn lower_virtual_class_builds_vtable() {
        let src = b"\
class Base {\n\
public:\n\
    virtual int value() { return 1; }\n\
    Base() {}\n\
};\n\
class Derived : public Base {\n\
public:\n\
    int value() override { return 2; }\n\
    Derived() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");
        assert_eq!(out.vtables.len(), 2);
        assert!(out.vtables.iter().all(|(_, entries)| entries.len() >= 3));
        assert!(out
            .vtables
            .iter()
            .flat_map(|(_, entries)| entries)
            .any(|entry| matches!(entry, crate::cpp::lower_ir::VTableEntry::SymAddr(name) if name == "value")));
    }

    #[test]
    fn native_supported_allows_virtual_inheritance() {
        // Stage 6: 虚继承应该被 native_supported 接受
        let src = b"\
class Base { int x; };\n\
class Derived : public virtual Base { int y; };\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(
            result.is_ok(),
            "virtual inheritance should be supported in Stage 6"
        );
    }

    #[test]
    fn lower_virtual_inheritance_native() {
        // Stage 6: 简单虚继承 - 验证类能正确 lowering
        let src = b"\
class Base {\n\
public:\n\
    virtual int value() { return 1; }\n\
    int x;\n\
    Base() {}\n\
};\n\
class Derived : public virtual Base {\n\
public:\n\
    int y;\n\
    Derived() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 2 个 struct (Base + Derived)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 2);

        // 找到 Derived 的 struct 定义
        let derived_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "Derived" => Some(sd),
            _ => None,
        });
        assert!(derived_struct.is_some(), "Derived struct not found");
        let sd = derived_struct.unwrap();

        // Derived 应该包含基类字段 x (virtual base) 和自己的字段 y
        // 注意: 虚基类字段在末尾
        let has_x = sd.fields.iter().any(|f| f.name == "x");
        let has_y = sd.fields.iter().any(|f| f.name == "y");
        assert!(has_x, "Derived should have field x (from virtual Base)");
        assert!(has_y, "Derived should have field y");

        // vtable 应该有 vbase offset 条目
        assert!(!out.vtables.is_empty());
    }

    #[test]
    fn lower_diamond_virtual_inheritance_native() {
        // Stage 6: 菱形虚继承
        let src = b"\
class A {\n\
public:\n\
    virtual void foo() {}\n\
    int a;\n\
    A() {}\n\
};\n\
class B : public virtual A {\n\
public:\n\
    int b;\n\
    B() {}\n\
};\n\
class C : public virtual A {\n\
public:\n\
    int c;\n\
    C() {}\n\
};\n\
class D : public B, public C {\n\
public:\n\
    int d;\n\
    D() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 4 个 struct (A, B, C, D)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 4);

        // 找到 D 的 struct 定义
        let d_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "D" => Some(sd),
            _ => None,
        });
        assert!(d_struct.is_some(), "D struct not found");
        let sd = d_struct.unwrap();

        // D 应该包含字段: b (from B), c (from C), d (自己), a (from virtual A)
        let has_a = sd.fields.iter().any(|f| f.name == "a");
        let has_b = sd.fields.iter().any(|f| f.name == "b");
        let has_c = sd.fields.iter().any(|f| f.name == "c");
        let has_d = sd.fields.iter().any(|f| f.name == "d");
        assert!(has_a, "D should have field a (from virtual A)");
        assert!(has_b, "D should have field b (from B)");
        assert!(has_c, "D should have field c (from C)");
        assert!(has_d, "D should have field d");

        // 验证 vtable 存在
        assert!(!out.vtables.is_empty());
    }

    // === Stage 7: RTTI 测试 ===

    #[test]
    fn native_supported_allows_typeid() {
        // Stage 7: typeid 应该被 native_supported 接受
        let src = b"\
class Foo { int x; };\n\
void test() { typeid(Foo); }\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "typeid should be supported in Stage 7");
    }

    #[test]
    fn native_supported_allows_dynamic_cast() {
        // Stage 7: dynamic_cast 应该被 native_supported 接受
        let src = b"\
class Base { virtual void foo() {} };\n\
class Derived : public Base {};\n\
void test(Base* b) { dynamic_cast<Derived*>(b); }\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(
            result.is_ok(),
            "dynamic_cast should be supported in Stage 7"
        );
    }

    #[test]
    fn lower_typeid_emits_typeinfo_symbol() {
        // 验证 typeid 表达式会生成 typeinfo 符号（在 rodata 中）
        let src = b"\
class Foo {\n\
public:\n\
    virtual void bar() {}\n\
    Foo() {}\n\
};\n\
void* test() {\n\
    void* p = (void*)typeid(Foo);\n\
    return p;\n\
}\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // rodata 中应该有 _ZTI 开头的 typeinfo 符号
        let has_typeinfo = out.rodata.iter().any(|s| s.starts_with("_ZTI"));
        assert!(has_typeinfo, "should have typeinfo symbol in rodata");

        // 验证程序中有 typeinfo 的 extern 声明
        let has_typeinfo_decl = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Var(vd) => vd.name.name.starts_with("_ZTI") && vd.is_extern,
            _ => false,
        });
        assert!(has_typeinfo_decl, "should have extern typeinfo declaration");
    }

    #[test]
    fn lower_dynamic_cast_polymorphic() {
        // 验证多态类型的 dynamic_cast 能正确 lowering
        let src = b"\
class Base {\n\
public:\n\
    virtual int value() { return 1; }\n\
    Base() {}\n\
};\n\
class Derived : public Base {\n\
public:\n\
    int value() override { return 2; }\n\
    Derived() {}\n\
};\n\
Derived* test(Base* b) {\n\
    return dynamic_cast<Derived*>(b);\n\
}\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有函数（test + 构造函数等）
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert!(fn_count >= 1, "should have at least test function");

        // rodata 中应该有 typeinfo 符号（src 和 dst 两个）
        let typeinfo_count = out.rodata.iter().filter(|s| s.starts_with("_ZTI")).count();
        assert!(
            typeinfo_count >= 2,
            "should have at least 2 typeinfo symbols (src and dst)"
        );
    }

    // === Stage 8: 模板测试 ===

    #[test]
    fn native_supported_allows_templates() {
        // Stage 8: 模板声明应该被 native_supported 接受
        let src = b"\
template <typename T> class Box { public: T value; };
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "templates should be supported in Stage 8");
    }

    #[test]
    fn lower_class_template_simple() {
        // Stage 8: 简单类模板实例化（从变量声明触发）
        let src = b"\
template <typename T>
struct Box {
    T value;
};
int test() {
    Box<int> b;
    b.value = 42;
    return b.value;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有实例化的模板类 struct
        let has_template_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Box"),
            _ => false,
        });
        assert!(has_template_class, "should have instantiated template class");

        // 应该有 test 函数
        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    #[test]
    fn lower_class_template_global_var() {
        // Stage 8: 从全局变量触发类模板实例化
        let src = b"\
template <typename T>
struct Pair {
    T first;
    T second;
};
Pair<int> global_pair;
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有实例化的模板类 struct
        let has_template_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Pair"),
            _ => false,
        });
        assert!(has_template_class, "should have instantiated Pair<int> class");
    }

    #[test]
    fn lower_class_template_multiple_params() {
        // Stage 8: 多类型参数的类模板
        let src = b"\
template <typename T, typename U>
struct Tuple2 {
    T first;
    U second;
};
Tuple2<int, double> global_tuple;
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有实例化的模板类 struct
        let has_template_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Tuple2"),
            _ => false,
        });
        assert!(has_template_class, "should have instantiated Tuple2<int, double> class");
    }

    #[test]
    fn lower_class_template_inheritance() {
        // Stage 8: 模板基类的继承
        let src = b"\
template <typename T>
class Base {
public:
    T base_value;
};
class Derived : public Base<int> {
public:
    int derived_value;
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 Derived struct
        let has_derived = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name == "Derived",
            _ => false,
        });
        assert!(has_derived, "should have Derived class");

        // 应该有实例化的 Base<int> class
        let has_base = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Base"),
            _ => false,
        });
        assert!(has_base, "should have instantiated Base<int> class");
    }

    #[test]
    fn lower_function_template_explicit() {
        // Stage 8: 显式指定模板实参的函数模板调用
        let src = b"\
template <typename T>
T identity(T x) {
    return x;
}
int test() {
    return identity<int>(42);
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有函数（test + 实例化的 identity<int>）
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert!(fn_count >= 2, "should have at least 2 functions (test + identity<int>)");
    }

    // === Stage 9: 异常处理测试 ===

    #[test]
    fn native_supported_allows_exception() {
        // Stage 9: 异常 (throw/catch) 应该被 native_supported 接受
        let src = b"\
void test() {
    try {
        throw 1;
    } catch (int x) {
    }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "exceptions should be supported in Stage 9");
    }

    #[test]
    fn lower_try_catch_simple() {
        // 简单的 try/catch 语句 lowering
        let src = b"\
int test() {
    int result = 0;
    try {
        throw 42;
    } catch (int e) {
        result = e;
    }
    return result;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 test 函数
        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    #[test]
    fn lower_throw_expression() {
        // throw 表达式 lowering（无 catch，直接 throw）
        let src = b"\
void test() {
    throw 1;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 test 函数
        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    #[test]
    fn lower_multiple_catch_handlers() {
        // 多个 catch handler
        let src = b"\
int test(int code) {
    int result = 0;
    try {
        if (code == 1) {
            throw 1;
        } else {
            throw 2;
        }
    } catch (int e) {
        result = e;
    } catch (...) {
        result = 99;
    }
    return result;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    // === Stage 10: Feature detection 测试 ===

    #[test]
    fn detect_features_free_function() {
        let src = b"int add(int a, int b) { return a + b; }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::FreeFunction));
        assert!(!features.contains(&CppFeature::PlainClass));
    }

    #[test]
    fn detect_features_plain_class() {
        let src = b"\
class Point {
public:
    int x;
    int y;
    int get_x() { return x; }
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::PlainClass));
        assert!(features.contains(&CppFeature::MemberFunction));
    }

    #[test]
    fn detect_features_virtual_function() {
        let src = b"\
class Base {
public:
    virtual int value() { return 1; }
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::VirtualFunction));
        assert!(features.contains(&CppFeature::VPtrInit));
    }

    #[test]
    fn detect_features_virtual_inheritance() {
        let src = b"\
class Base { int x; };
class Derived : public virtual Base { int y; };
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::VirtualInheritance));
        assert!(features.contains(&CppFeature::NonVirtualInheritance));
    }

    #[test]
    fn detect_features_namespace() {
        let src = b"\
namespace foo {
    int bar() { return 42; }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Namespace));
    }

    #[test]
    fn detect_features_using_decl() {
        let src = b"using std::vector;\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Namespace));
    }

    #[test]
    fn detect_features_typeid() {
        let src = b"\
#include <typeinfo>
void test() { typeid(int); }
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::TypeId));
    }

    #[test]
    fn detect_features_dynamic_cast() {
        let src = b"\
class Base { virtual void foo() {} };
class Derived : public Base {};
void test(Base* b) { dynamic_cast<Derived*>(b); }
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::DynamicCast));
    }

    #[test]
    fn detect_features_exception() {
        let src = b"\
void test() {
    try {
        throw 1;
    } catch (int e) {
    }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Exception));
        assert!(features.contains(&CppFeature::DtorCleanup));
    }

    #[test]
    fn detect_features_template_class() {
        let src = b"\
template <typename T>
class Box {
public:
    T value;
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::ClassTemplate));
    }

    #[test]
    fn detect_features_template_function() {
        let src = b"\
template <typename T>
T identity(T x) { return x; }
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::FunctionTemplate));
    }

    #[test]
    fn detect_features_new_delete() {
        let src = b"\
void test() {
    int* p = new int(42);
    delete p;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::NewDelete));
    }

    #[test]
    fn detect_features_initializer_list() {
        // 使用 return {a, b} 形式的初始化列表来触发 InitializerList 特性检测
        // 这种语法在 C++ 中作为函数返回值的列表初始化
        let src = b"\
struct Point {
    int x;
    int y;
};
Point make_point(int a, int b) {
    return {a, b};
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(
            features.contains(&CppFeature::InitializerList),
            "expected InitializerList feature, got: {:?}",
            features.iter().map(|f| f.name()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn detect_features_static_local() {
        let src = b"\
int& counter() {
    static int c = 0;
    return c;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::StaticInitGuard));
    }

    #[test]
    fn detect_features_coroutine() {
        let src = b"\
#include <coroutine>
void test() {
    co_return;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Coroutine));
    }

    #[test]
    fn detect_features_concept() {
        let src = b"\
template <typename T>
concept Addable = requires(T a, T b) { a + b; };
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Concept));
    }

    #[test]
    fn detect_features_module() {
        let src = b"module foo;\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Module));
    }

    #[test]
    fn can_compile_native_simple() {
        let src = b"int add(int a, int b) { return a + b; }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    #[test]
    fn can_compile_native_with_class() {
        let src = b"\
class Point {
public:
    int x;
    int y;
    Point() {}
    int sum() { return x + y; }
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    #[test]
    fn can_compile_native_with_virtual() {
        let src = b"\
class Base {
public:
    virtual int value() { return 1; }
    Base() {}
};
class Derived : public Base {
public:
    int value() override { return 2; }
    Derived() {}
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    /// namespace: sema 已支持作用域内名字查找与 `ns_fn` 形式 linkage name,
    /// native 后端可直接编译。
    #[test]
    fn can_compile_native_ok_for_namespace() {
        let src = b"\
namespace foo {
    int bar() { return 42; }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    /// new/delete: native 后端按 Itanium ABI 调用 operator new / operator delete,
    /// 链接期由 C++ 运行时提供实现。
    #[test]
    fn can_compile_native_ok_for_new_delete() {
        let src = b"\
void test() {
    int* p = new int;
    delete p;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    /// concept: 仅作为约束声明参与重载决议, 不生成任何代码, native 后端可直接编译。
    #[test]
    fn can_compile_native_ok_for_concept() {
        let src = b"\
template <typename T>
concept Foo = true;
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }
}
