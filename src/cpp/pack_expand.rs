//! G12/M52: 函数模板**参数包摊平** (parameter-pack flattening).
//!
//! 单态化的前半段 (`template::substitute_*`) 只做"名字 → 类型"的替换, 它无法
//! 表达"一个形参变成 N 个形参"这种**结构**变化. 本模块是后半段: 输入已经替换
//! 过模板形参的实例声明, 其中仍残留三处只有包才有的形状
//!
//!   1. 形参类型 `MangleType::VariadicPack([模式])`  (`Args... args`)
//!   2. 列表语境里的 `CppExpr::PackExpansion(模式)`   (`printf(fmt, args...)`)
//!   3. `CppExpr::Sizeof(SizeofArg::Pack(名))`        (`sizeof...(Args)`)
//!
//! 全部改写成一个**完全非变参**的具体函数:
//!
//! ```text
//! template <typename... A> void print(const char* fmt, A... a) { printf(fmt, a...); }
//!   以 A = {int, double} 实例化
//! => void __dcc_tplfn_print_3(const char* fmt, int a_0, double a_1) { printf(fmt, a_0, a_1); }
//! ```
//!
//! 设计约束 (与任务书"禁止静默降级"一致):
//!   - 展开不出结果的每一种情况都返回 `Err`, 由调用方硬报错, 绝不留下
//!     静默丢代码的路径;
//!   - 没有包实参时 (非变参模板) 本模块直接返回 `Ok` 且**不改动** AST,
//!     保证对既有模板零影响.

use crate::cpp::mangle::MangleType;
use crate::cpp::parser::{CppBody, CppDecl, CppExpr, CppStmt, SizeofArg};
use crate::cpp::template::substitute_type;
use std::collections::HashMap;

/// 一次实例化 (模板形参 → 模板实参) 的包上下文.
struct PackCtx {
    /// 模板形参名, 与 `args` 一一对应
    params: Vec<String>,
    /// 模板实参; 包形参的位置上是 `VariadicPack([...])`
    args: Vec<MangleType>,
    /// 模板形参 pack 名 → 包长度 (`sizeof...(Args)`)
    sizes: HashMap<String, usize>,
    /// 函数形参 pack 的**变量名** → 摊平后逐个元素的变量名
    /// (`args` → `["args_0", "args_1"]`)
    vars: HashMap<String, Vec<String>>,
}

impl PackCtx {
    fn new(params: &[String], args: &[MangleType]) -> Self {
        let mut sizes = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            if let Some(MangleType::VariadicPack(v)) = args.get(i) {
                sizes.insert(p.clone(), v.len());
            }
        }
        PackCtx {
            params: params.to_vec(),
            args: args.to_vec(),
            sizes,
            vars: HashMap::new(),
        }
    }

    /// 把包实参表按第 `k` 个元素**标量化**: `VariadicPack(v)` → `v[k]`,
    /// 其余原样. 供模式替换 (`substitute_type`) 复用现成实现.
    fn scalar_args(&self, k: usize) -> Result<Vec<MangleType>, String> {
        self.args
            .iter()
            .map(|a| match a {
                MangleType::VariadicPack(v) => v
                    .get(k)
                    .cloned()
                    .ok_or_else(|| format!("参数包长度不一致: 需要第 {k} 个元素, 包只有 {}", v.len())),
                other => Ok(other.clone()),
            })
            .collect()
    }

    /// 一个**类型模式**里引用到的包长度集合 (必须唯一, 见 C++ 的 zip 规则:
    /// 同一展开模式里的所有包长度必须相同).
    fn pattern_sizes(&self, ty: &MangleType, out: &mut Vec<usize>) {
        match ty {
            MangleType::Class(n) => {
                if let Some(&s) = self.sizes.get(n) {
                    out.push(s);
                }
            },
            MangleType::Pointer(t)
            | MangleType::Reference(t)
            | MangleType::Array(t, _) => self.pattern_sizes(t, out),
            MangleType::Template { args: ta, .. } | MangleType::VariadicPack(ta) => {
                for a in ta {
                    self.pattern_sizes(a, out);
                }
            },
            MangleType::TplMember { args: ta, .. } => {
                for a in ta {
                    self.pattern_sizes(a, out);
                }
            },
            MangleType::Function { ret, params } => {
                self.pattern_sizes(ret, out);
                for p in params {
                    self.pattern_sizes(p, out);
                }
            },
            _ => {},
        }
    }

    /// 类型模式的展开长度.
    fn type_pattern_len(&self, pat: &MangleType) -> Result<usize, String> {
        let mut sizes = Vec::new();
        self.pattern_sizes(pat, &mut sizes);
        unify_pack_len(&sizes, "形参包")
    }

    /// 表达式模式的展开长度: 引用了哪些包 (函数形参包 `args` / 模板形参包
    /// `Args` / 类型里的包), 长度必须一致.
    fn expr_pattern_len(&self, pat: &CppExpr) -> Result<usize, String> {
        let mut sizes = Vec::new();
        self.collect_expr_pattern_sizes(pat, &mut sizes);
        if sizes.is_empty() {
            return Err(
                "参数包展开模式里没有引用任何参数包 (如 `1...`): 展开长度无法确定".to_string(),
            );
        }
        unify_pack_len(&sizes, "展开模式")
    }

    fn collect_expr_pattern_sizes(&self, e: &CppExpr, out: &mut Vec<usize>) {
        match e {
            CppExpr::Ident(n) => {
                if let Some(v) = self.vars.get(n) {
                    out.push(v.len());
                } else if let Some(&s) = self.sizes.get(n) {
                    // 模板形参包名出现在表达式语境 (作类型用, 如 `T{}`)
                    out.push(s);
                }
            },
            CppExpr::Sizeof(SizeofArg::Pack(n)) => {
                if let Some(v) = self.vars.get(n) {
                    out.push(v.len());
                } else if let Some(&s) = self.sizes.get(n) {
                    out.push(s);
                }
            },
            CppExpr::Cast(ty, _)
            | CppExpr::TypeidType(ty)
            | CppExpr::Sizeof(SizeofArg::Type(ty))
            | CppExpr::StaticCast { ty, .. }
            | CppExpr::ConstCast { ty, .. }
            | CppExpr::ReinterpretCast { ty, .. }
            | CppExpr::DynamicCast { ty, .. } => self.pattern_sizes(ty, out),
            CppExpr::TemplateIdent { args, .. } | CppExpr::TplStaticAccess { args, .. } => {
                for a in args {
                    self.pattern_sizes(a, out);
                }
            },
            CppExpr::TypedInitList { ty, elems } => {
                self.pattern_sizes(ty, out);
                for x in elems {
                    self.collect_expr_pattern_sizes(x, out);
                }
            },
            other => {
                // 只读遍历: 先克隆再借可变子节点 (长度收集不改原树,
                // 成本只在含包展开的列表现场付一次)
                let mut c = other.clone();
                for x in c.sub_exprs_mut() {
                    self.collect_expr_pattern_sizes(x, out);
                }
            },
        }
    }

    /// 把第 `k` 份展开落回模式克隆件: 包变量改名 + 类型标量化 + sizeof... 求值.
    fn instantiate_pattern(&mut self, e: &mut CppExpr, k: usize) -> Result<(), String> {
        match e {
            CppExpr::Ident(n) => {
                if let Some(names) = self.vars.get(n).cloned() {
                    *e = CppExpr::Ident(names[k].clone());
                }
                return Ok(());
            },
            CppExpr::Sizeof(SizeofArg::Pack(n)) => {
                let len = self.pack_len_by_name(n)?;
                *e = CppExpr::IntLit(len as i64);
                return Ok(());
            },
            CppExpr::PackExpansion(inner) => {
                return Err(format!(
                    "不支持嵌套的参数包展开 (内层模式 {})",
                    inner_ident_hint(inner)
                ));
            },
            _ => {},
        }
        let scalar = self.scalar_args(k)?;
        // 类型位置: 模式里引用的模板形参 (含包形参) 按第 k 份实参替换
        match e {
            CppExpr::Cast(ty, _) => *ty = substitute_type(ty, &self.params, &scalar),
            CppExpr::TypeidType(ty) => *ty = substitute_type(ty, &self.params, &scalar),
            CppExpr::Sizeof(SizeofArg::Type(ty)) => *ty = substitute_type(ty, &self.params, &scalar),
            CppExpr::StaticCast { ty, .. }
            | CppExpr::ConstCast { ty, .. }
            | CppExpr::ReinterpretCast { ty, .. }
            | CppExpr::DynamicCast { ty, .. } => {
                *ty = substitute_type(ty, &self.params, &scalar)
            },
            CppExpr::TemplateIdent { args, .. } => {
                for a in args {
                    *a = substitute_type(a, &self.params, &scalar);
                }
            },
            CppExpr::TplStaticAccess { args, .. } => {
                for a in args {
                    *a = substitute_type(a, &self.params, &scalar);
                }
            },
            CppExpr::TypedInitList { ty, .. } => {
                *ty = substitute_type(ty, &self.params, &scalar);
            },
            _ => {},
        }
        for sub in e.sub_exprs_mut() {
            self.instantiate_pattern(sub, k)?;
        }
        Ok(())
    }

    /// `sizeof...(名字)`: 名字可以是模板形参包 (`Args`) 或函数形参包 (`args`).
    fn pack_len_by_name(&self, name: &str) -> Result<usize, String> {
        if let Some(v) = self.vars.get(name) {
            return Ok(v.len());
        }
        if let Some(&s) = self.sizes.get(name) {
            return Ok(s);
        }
        Err(format!(
            "`sizeof...({name})` 里的 `{name}` 不是参数包 (已知包: {:?})",
            self.sizes.keys().collect::<Vec<_>>()
        ))
    }
}

/// 同一展开模式里所有包的长度必须一致 (C++ [temp.expansion] 的 zip 规则).
fn unify_pack_len(sizes: &[usize], what: &str) -> Result<usize, String> {
    match sizes.first() {
        None => Err(format!("{what}里没有引用任何参数包")),
        Some(&n) => {
            if sizes.iter().all(|&s| s == n) {
                Ok(n)
            } else {
                Err(format!(
                    "{what}展开里的参数包长度不一致: {sizes:?} (C++ 要求同一展开模式里的所有包等长)"
                ))
            }
        },
    }
}

/// 入口: 摊平一次实例化产物里残留的参数包.
///
/// `params`/`args` 是模板形参名与**已解析**的模板实参 (包形参位置上是
/// `VariadicPack`). 没有包实参时不改动 AST, 直接返回 Ok.
pub fn expand_packs_in(
    decl: &mut CppDecl,
    params: &[String],
    args: &[MangleType],
) -> Result<(), String> {
    if !args
        .iter()
        .any(|a| matches!(a, MangleType::VariadicPack(_)))
    {
        return Ok(());
    }
    let mut ctx = PackCtx::new(params, args);
    expand_decl(decl, &mut ctx)
}

fn expand_decl(d: &mut CppDecl, ctx: &mut PackCtx) -> Result<(), String> {
    match d {
        CppDecl::FreeFn { params, body, .. } => expand_fn(params, Some(body), None, ctx),
        CppDecl::MemberFn { params, body, .. } => expand_fn(params, Some(body), None, ctx),
        CppDecl::OperatorFn { params, body, .. } => expand_fn(params, Some(body), None, ctx),
        CppDecl::Ctor {
            params,
            inits,
            body,
            ..
        } => expand_fn(params, Some(body), Some(inits), ctx),
        CppDecl::Dtor { body, .. } => expand_body(body, ctx),
        CppDecl::Class(c) => {
            for m in &mut c.members {
                expand_decl(m, ctx)?;
            }
            Ok(())
        },
        CppDecl::AnonAggr { members, .. } => {
            for m in members {
                expand_decl(m, ctx)?;
            }
            Ok(())
        },
        CppDecl::Template { decl, .. } => expand_decl(decl, ctx),
        CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
            for x in body {
                expand_decl(x, ctx)?;
            }
            Ok(())
        },
        CppDecl::ExternTemplate(decl) => expand_decl(decl, ctx),
        // 其余声明 (变量/typedef/枚举/…) 不携带函数体; 包形参若残留在变量类型上
        // 会由下面的 `expand_fn` 之外的调用点检查兜住, 这里不静默改写.
        _ => Ok(()),
    }
}

fn expand_fn(
    params: &mut Vec<(String, MangleType)>,
    body: Option<&mut CppBody>,
    inits: Option<&mut Vec<(String, CppExpr)>>,
    ctx: &mut PackCtx,
) -> Result<(), String> {
    let old = std::mem::take(params);
    let mut kept: Vec<(String, MangleType)> = Vec::with_capacity(old.len());
    for (vn, ty) in old {
        if let MangleType::VariadicPack(pat) = &ty {
            // parser 约定: `VariadicPack` 的 payload 在**形参类型**位置上是单元素
            // 的展开模式 (`Args... args` → `VariadicPack([Class("Args")])`).
            let pat = match pat.as_slice() {
                [one] => one.clone(),
                others => {
                    return Err(format!(
                        "形参包 `{vn}` 的模式含 {} 个类型 (parser 约定应为 1 个)",
                        others.len()
                    ))
                },
            };
            let n = ctx.type_pattern_len(&pat)?;
            let mut names = Vec::with_capacity(n);
            for k in 0..n {
                let scalar = ctx.scalar_args(k)?;
                let elem_ty = substitute_type(&pat, &ctx.params, &scalar);
                let elem_name = format!("{vn}_{k}");
                names.push(elem_name.clone());
                kept.push((elem_name, elem_ty));
            }
            ctx.vars.insert(vn, names);
        } else {
            kept.push((vn, ty.clone()));
        }
    }
    *params = kept;
    if let Some(inits) = inits {
        for (_, e) in inits.iter_mut() {
            expand_expr(e, ctx)?;
        }
    }
    if let Some(body) = body {
        expand_body(body, ctx)?;
    }
    Ok(())
}

fn expand_body(b: &mut CppBody, ctx: &mut PackCtx) -> Result<(), String> {
    for s in &mut b.stmts {
        expand_stmt(s, ctx)?;
    }
    Ok(())
}

fn expand_stmt(s: &mut CppStmt, ctx: &mut PackCtx) -> Result<(), String> {
    {
        let exprs = s.sub_exprs_mut();
        for e in exprs {
            expand_expr(e, ctx)?;
        }
    }
    {
        let bodies = s.sub_bodies_mut();
        for b in bodies {
            expand_body(b, ctx)?;
        }
    }
    {
        let inner = s.sub_stmts_mut();
        for x in inner {
            expand_stmt(x, ctx)?;
        }
    }
    if let CppStmt::Decl(d) = s {
        expand_decl(d, ctx)?;
    }
    // 局部变量声明的包类型 (`A... x;` 不是合法 C++, 而模板体里残留它意味着
    // 上面的替换没把它当包处理) -> 硬报错, 不静默交给后端.
    if let CppStmt::LocalVar { ty, .. } | CppStmt::StaticLocal { ty, .. } = s {
        if matches!(ty, MangleType::VariadicPack(_)) {
            return Err(format!(
                "局部变量声明的类型仍是参数包 ({ty:?}): 模板实例化未处理该语境"
            ));
        }
    }
    // 多 init-declarator 的共享 decl-specifier 同理 (`Pack... a, b;` 残留
    // = 实例化没处理该语境); 各 declarator 的初始化式已经走过 sub_exprs_mut.
    if let CppStmt::LocalVarList { spec, .. } = s {
        if matches!(spec.ty, MangleType::VariadicPack(_)) {
            return Err(format!(
                "局部变量声明的类型仍是参数包 ({:?}): 模板实例化未处理该语境",
                spec.ty
            ));
        }
    }
    Ok(())
}

fn expand_expr(e: &mut CppExpr, ctx: &mut PackCtx) -> Result<(), String> {
    // lambda 体: 同一个模板实例, 复用同一份包上下文
    if let CppExpr::Lambda(l) = e {
        expand_body(&mut l.body, ctx)?;
    }
    // 1. 本节点的列表语境: `f(a, args..., b)` → 摊平
    {
        let lists = e.pack_list_mut();
        for list in lists {
            expand_list(list, ctx)?;
        }
    }
    // 2. `sizeof...(P)` → 字面量
    if let CppExpr::Sizeof(SizeofArg::Pack(n)) = e {
        let len = ctx.pack_len_by_name(n)?;
        *e = CppExpr::IntLit(len as i64);
        return Ok(());
    }
    // 3. 列表之外的包展开: C++ 里非法, 或本模块尚未支持的语境 —— 硬报错
    if let CppExpr::PackExpansion(inner) = e {
        return Err(format!(
            "参数包展开出现在不支持的语境 (仅实参列表/初始化列表可展开), 模式 = {:?}",
            inner_ident_hint(inner)
        ));
    }
    // 4. 递归子表达式
    let sub = e.sub_exprs_mut();
    for x in sub {
        expand_expr(x, ctx)?;
    }
    Ok(())
}

/// 错误信息里给一个尽量可读的模式提示.
fn inner_ident_hint(e: &CppExpr) -> String {
    match e {
        CppExpr::Ident(n) => n.clone(),
        other => format!("{other:?}"),
    }
}

fn expand_list(list: &mut Vec<CppExpr>, ctx: &mut PackCtx) -> Result<(), String> {
    if !list
        .iter()
        .any(|e| matches!(e, CppExpr::PackExpansion(_)))
    {
        return Ok(());
    }
    let old = std::mem::take(list);
    let mut out: Vec<CppExpr> = Vec::with_capacity(old.len());
    for item in old {
        match item {
            CppExpr::PackExpansion(pat) => {
                let n = ctx.expr_pattern_len(&pat)?;
                for k in 0..n {
                    let mut inst = (*pat).clone();
                    ctx.instantiate_pattern(&mut inst, k)?;
                    out.push(inst);
                }
            },
            mut other => {
                expand_expr(&mut other, ctx)?;
                out.push(other);
            },
        }
    }
    *list = out;
    Ok(())
}


// ===== M52: 参数包摊平单元测试 =====

#[cfg(test)]
mod pack_expand_tests {
    use super::*;
    use crate::cpp::parser::{BinOpKind, CppParser, CppStmt, TemplateArgs};
    use crate::cpp::template::TemplateRegistry;

    /// 源码 → 模板注册表 (只收顶层函数/类模板, 与 native.rs 的 collect_templates 同构)
    fn registry_of(src: &str) -> TemplateRegistry {
        let mut p = CppParser::new(src.as_bytes(), "pack_expand_unit.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        assert!(
            !decls.iter().any(|d| matches!(d, CppDecl::Opaque(_))),
            "夹具被降级成 Opaque, 解析没通过: {decls:?}"
        );
        let mut reg = TemplateRegistry::default();
        for d in &decls {
            if let CppDecl::Template {
                params, requires, decl, ..
            } = d
            {
                reg.register(params, requires, decl);
            }
        }
        reg
    }

    fn pack_of(tys: &[MangleType]) -> MangleType {
        MangleType::VariadicPack(tys.to_vec())
    }

    /// 手工造一个只有函数体的自由函数声明 (绕过 parser, 专测错误路径)
    fn fn_decl(name: &str, params: Vec<(String, MangleType)>, body: Vec<CppStmt>) -> CppDecl {
        CppDecl::FreeFn {
            ret: MangleType::Int,
            name: name.to_string(),
            params,
            body: CppBody {
                stmts: body,
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        }
    }

    /// 递归找第一个 `Call` 的实参表 (测试里用来数展开后的实参个数)
    fn find_call_args(e: &CppExpr) -> Option<Vec<CppExpr>> {
        if let CppExpr::Call(_, as_) = e {
            return Some(as_.clone());
        }
        let mut c = e.clone();
        for sub in c.sub_exprs_mut() {
            if let Some(v) = find_call_args(sub) {
                return Some(v);
            }
        }
        None
    }

    fn ident_names(as_: &[CppExpr]) -> Vec<String> {
        as_.iter()
            .map(|a| match a {
                CppExpr::Ident(n) => n.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// 形参包 `A... args` 摊平成 `int args_0, int args_1, int args_2`,
    /// 且 `sizeof...(args)` 折成字面量 3 —— 实例里不许再残留任何包形状.
    #[test]
    fn param_pack_flattens_and_sizeof_pack_folds() {
        let mut reg = registry_of(
            "template <typename... A> int nargs(A... args) { return (int)sizeof...(args); }\n",
        );
        let idx = reg.candidates_named("nargs")[0].0;
        let inst = reg
            .instantiate_fn_at(
                idx,
                "nargs",
                &[pack_of(&[
                    MangleType::Int,
                    MangleType::Int,
                    MangleType::Int,
                ])],
            )
            .expect("实例化成功");
        assert_eq!(reg.pack_expand_error, None);
        let (params, body) = match &inst {
            CppDecl::FreeFn { params, body, .. } => (params, body),
            other => panic!("期望 FreeFn, 得到 {other:?}"),
        };
        let names: Vec<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["args_0", "args_1", "args_2"]);
        assert!(params.iter().all(|(_, t)| *t == MangleType::Int));
        let dbg = format!("{body:?}");
        assert!(
            !dbg.contains("VariadicPack") && !dbg.contains("PackExpansion") && !dbg.contains("Pack("),
            "实例里仍残留包形状: {dbg}"
        );
        // `sizeof...(args)` 已折成字面量 3 (仍套在原 `(int)` 转换里)
        match &body.stmts[..] {
            [CppStmt::Return(Some(CppExpr::Cast(MangleType::Int, inner)))] => {
                assert!(matches!(**inner, CppExpr::IntLit(3)), "未折叠: {inner:?}");
            },
            other => panic!("期望 `return (int)3;`, 得到 {other:?}"),
        }
    }

    /// 空包: `nargs()` → 零个形参, `sizeof...(args)` → 0.
    #[test]
    fn empty_pack_yields_no_params_and_sizeof_zero() {
        let mut reg = registry_of(
            "template <typename... A> int nargs(A... args) { return (int)sizeof...(args); }\n",
        );
        let idx = reg.candidates_named("nargs")[0].0;
        let inst = reg
            .instantiate_fn_at(idx, "nargs", &[pack_of(&[])])
            .expect("实例化成功");
        assert_eq!(reg.pack_expand_error, None);
        match &inst {
            CppDecl::FreeFn { params, body, .. } => {
                assert!(params.is_empty(), "空包应摊成零形参: {params:?}");
                // `sizeof...(args)` 在空包时折成字面量 0
                match &body.stmts[..] {
                    [CppStmt::Return(Some(CppExpr::Cast(MangleType::Int, inner)))] => {
                        assert!(matches!(**inner, CppExpr::IntLit(0)), "未折叠成 0: {inner:?}");
                    },
                    other => panic!("期望 `return (int)0;`, 得到 {other:?}"),
                }
            },
            other => panic!("期望 FreeFn, 得到 {other:?}"),
        }
    }

    /// `printf(fmt, args...)` 的实参列表被 zip 成 `printf(fmt, args_0, args_1)`;
    /// 非包形参 (`const char* fmt`) 原位保留.
    #[test]
    fn pack_expansion_in_argument_list_is_zipped() {
        let mut reg = registry_of(
            "template <typename... A> int emit(const char* fmt, A... args) { return printf(fmt, args...); }\n",
        );
        let idx = reg.candidates_named("emit")[0].0;
        let inst = reg
            .instantiate_fn_at(
                idx,
                "emit",
                &[MangleType::Pointer(MangleType::Char.into()), pack_of(&[MangleType::Int, MangleType::Double])],
            )
            .expect("实例化成功");
        assert_eq!(reg.pack_expand_error, None);
        let body = match &inst {
            CppDecl::FreeFn { body, params, .. } => {
                assert_eq!(
                    params.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
                    ["fmt", "args_0", "args_1"]
                );
                body
            },
            other => panic!("期望 FreeFn, 得到 {other:?}"),
        };
        let call = find_call_args(&CppExpr::Ident("x".to_string()));
        debug_assert!(call.is_none());
        let args = body
            .stmts
            .iter()
            .find_map(|s| match s {
                CppStmt::Return(Some(e)) | CppStmt::Expr(e) => find_call_args(e),
                _ => None,
            })
            .expect("body 里应有 printf 调用");
        assert_eq!(
            ident_names(&args),
            ["fmt", "args_0", "args_1"],
            "包展开错位: {args:?}"
        );
    }

    /// 没有包实参时本模块必须**一个节点都不改** (非变参模板零影响).
    #[test]
    fn non_pack_instantiation_is_left_untouched() {
        let mut reg = registry_of("template <typename T> T idf(T x) { return x; }\n");
        let idx = reg.candidates_named("idf")[0].0;
        let inst = reg
            .instantiate_fn_at(idx, "idf", &[MangleType::Int])
            .expect("实例化成功");
        assert_eq!(reg.pack_expand_error, None);
        let before = format!("{inst:?}");
        let mut d = inst.clone();
        expand_packs_in(&mut d, &["T".to_string()], &[MangleType::Int]).expect("非包 = Ok");
        assert_eq!(format!("{d:?}"), before, "非包实例被改动了");
    }

    /// `sizeof...(X)` 里 X 不是参数包 → 硬报错 (不能悄悄当成 0 或 sizeof(X)).
    #[test]
    fn sizeof_pack_of_non_pack_is_hard_error() {
        let mut d = fn_decl(
            "f",
            vec![("args".to_string(), pack_of(&[MangleType::Class("A".to_string())]))],
            vec![CppStmt::Return(Some(CppExpr::Sizeof(SizeofArg::Pack(
                "nope".to_string(),
            ))))],
        );
        let e = expand_packs_in(
            &mut d,
            &["A".to_string()],
            &[pack_of(&[MangleType::Int])],
        )
        .expect_err("应报错");
        assert!(e.contains("不是参数包"), "{e}");
    }

    /// 列表语境之外 (二元表达式操作数) 的 `args...` → 硬报错, 不静默丢代码.
    #[test]
    fn pack_expansion_outside_argument_list_is_hard_error() {
        let pat = CppExpr::PackExpansion(Box::new(CppExpr::Ident("args".to_string())));
        let mut d = fn_decl(
            "f",
            vec![("args".to_string(), pack_of(&[MangleType::Class("A".to_string())]))],
            vec![CppStmt::Return(Some(CppExpr::BinOp(
                BinOpKind::Add,
                Box::new(pat),
                Box::new(CppExpr::IntLit(1)),
            )))],
        );
        let e = expand_packs_in(
            &mut d,
            &["A".to_string()],
            &[pack_of(&[MangleType::Int, MangleType::Int])],
        )
        .expect_err("应报错");
        assert!(e.contains("语境"), "{e}");
    }

    /// 同一展开模式里两个长度不等的包 → 硬报错 (C++ [temp.expansion] zip 规则).
    #[test]
    fn unequal_packs_in_one_pattern_is_hard_error() {
        let pair = CppExpr::PackExpansion(Box::new(CppExpr::BinOp(
            BinOpKind::Add,
            Box::new(CppExpr::Ident("a".to_string())),
            Box::new(CppExpr::Ident("b".to_string())),
        )));
        let mut d = fn_decl(
            "f",
            vec![
                ("a".to_string(), pack_of(&[MangleType::Class("A".to_string())])),
                ("b".to_string(), pack_of(&[MangleType::Class("B".to_string())])),
            ],
            vec![CppStmt::Expr(CppExpr::Call(
                Box::new(CppExpr::Ident("g".to_string())),
                vec![pair],
            ))],
        );
        let e = expand_packs_in(
            &mut d,
            &["A".to_string(), "B".to_string()],
            &[
                pack_of(&[MangleType::Int, MangleType::Int]),
                pack_of(&[MangleType::Int]),
            ],
        )
        .expect_err("应报错");
        assert!(e.contains("长度不一致"), "{e}");
    }

    /// 展开模式里一个包都没引用 (如 `1...`) → 长度无从确定, 硬报错.
    #[test]
    fn pattern_without_any_pack_is_hard_error() {
        let mut d = fn_decl(
            "f",
            vec![("args".to_string(), pack_of(&[MangleType::Class("A".to_string())]))],
            vec![CppStmt::Expr(CppExpr::Call(
                Box::new(CppExpr::Ident("g".to_string())),
                vec![CppExpr::PackExpansion(Box::new(CppExpr::IntLit(1)))],
            ))],
        );
        let e = expand_packs_in(
            &mut d,
            &["A".to_string()],
            &[pack_of(&[MangleType::Int])],
        )
        .expect_err("应报错");
        assert!(e.contains("没有引用任何参数包"), "{e}");
    }

    /// `TemplateArgs` 只在测试里构造注册表时需要 (保证导入被使用, 也便于扩展).
    #[allow(dead_code)]
    fn _assert_template_args_is_importable() -> TemplateArgs {
        TemplateArgs { params: vec![] }
    }
}
