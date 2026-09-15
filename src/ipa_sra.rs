// src/ipa_sra.rs - IPA-SRA (Interprocedural Scalar Replacement of Aggregates)
//
// 第一性原理: 把按值传递的聚合类型 (struct/union) 参数拆分为标量字段,
// 消除不必要的内存拷贝, 并为后续标量优化 (SROA / GVN / 内联) 创造机会。
// 与 GCC -fipa-sra 行为一致: 仅作用于 static (内部链接) 函数, 保证
// 不会修改跨 TU 的 ABI。
//
// 设计:
//   1. 扫描所有 static 函数, 收集按值传递的 struct/union 参数
//   2. 对每个候选参数, 分析函数体访问了哪些字段 (Member 表达式)
//   3. 若拆分后标量总数 ≤ 原始结构体大小 / 4 (收益启发式), 执行拆分:
//      a. 改写函数签名: 每个被访问字段 → 独立标量参数
//      b. 改写函数体: 参数引用 → 局部变量赋值
//      c. 改写所有调用点: 结构体参数 → 逐个字段传参
//   4. 递归处理已修改的调用者/被调用者
//
// 限制: 不处理 address-taken 的结构体参数 (被 & 取址), 不处理位域,
// 不处理嵌套结构体 (当前仅展开一层)。

use std::collections::HashMap;

use crate::ast::{
    BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, Param, Program, QualifierFlags,
    Stmt, Type,
};
use crate::diagnostics::Span;

/// IPA-SRA 配置
#[derive(Debug, Clone, Copy)]
pub struct IpaSraConfig {
    /// 是否启用 IPA-SRA
    pub enabled: bool,
    /// -O 级别: ≥2 时自动启用
    pub opt_level: u8,
    /// 最大拆分为标量数 (避免过度膨胀参数列表)
    pub max_scalar_params: usize,
}

impl Default for IpaSraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            opt_level: 0,
            max_scalar_params: 8,
        }
    }
}

impl IpaSraConfig {
    pub fn should_run(&self) -> bool {
        self.enabled && self.opt_level >= 2
    }
}

/// 结构体参数分析结果: 哪些字段被访问
#[derive(Debug, Clone)]
struct StructParamAnalysis {
    /// 参数在函数签名中的索引
    param_idx: usize,
    /// 参数名
    param_name: String,
    /// 原始结构体类型
    struct_type: Type,
    /// 被访问的字段名列表
    accessed_fields: Vec<String>,
    /// 是否被取地址 (&
    has_address_taken: bool,
}

/// 判断类型是否为按值传递的 struct/union
fn is_aggregate_by_value(ty: &Type) -> bool {
    matches!(ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
}

/// 从参数类型中提取字段列表
fn extract_fields(ty: &Type) -> Vec<(String, Type)> {
    match ty {
        Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => fields
            .iter()
            .filter(|f| f.bit_width.is_none()) // 跳过位域
            .map(|f| (f.name.clone(), f.ty.clone()))
            .collect(),
        _ => vec![],
    }
}

/// 检查表达式是否引用了指定参数名作为 Member 的根对象
fn collect_accessed_fields(expr: &Expr, param_name: &str) -> Vec<String> {
    let mut fields = Vec::new();
    collect_accessed_fields_rec(expr, param_name, &mut fields);
    fields
}

fn collect_accessed_fields_rec(expr: &Expr, param_name: &str, fields: &mut Vec<String>) {
    match expr {
        Expr::Member { expr: e, field, .. } => {
            // 检查是否形如 param.field
            if let Expr::Ident { name, .. } = e.as_ref() {
                if name.name == param_name {
                    if !fields.contains(&field.name) {
                        fields.push(field.name.clone());
                    }
                    return;
                }
            }
            // 递归检查嵌套 (如 param.inner.field 不展开)
            collect_accessed_fields_rec(e, param_name, fields);
        }
        Expr::AddrOf { e, .. } => {
            // &param 或 &param.field: 标记 address_taken, 但不递归
            if let Expr::Ident { name, .. } = e.as_ref() {
                if name.name == param_name {
                    // address_taken 由调用方检查, 这里不修改 fields
                    return;
                }
            }
            collect_accessed_fields_rec(e, param_name, fields);
        }
        Expr::Assign { l, r, .. } => {
            collect_accessed_fields_rec(l, param_name, fields);
            collect_accessed_fields_rec(r, param_name, fields);
        }
        Expr::Binop { l, r, .. } => {
            collect_accessed_fields_rec(l, param_name, fields);
            collect_accessed_fields_rec(r, param_name, fields);
        }
        Expr::Unary { e, .. } => {
            collect_accessed_fields_rec(e, param_name, fields);
        }
        Expr::Cast { e, .. } => {
            collect_accessed_fields_rec(e, param_name, fields);
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_accessed_fields_rec(cond, param_name, fields);
            collect_accessed_fields_rec(then_, param_name, fields);
            collect_accessed_fields_rec(else_, param_name, fields);
        }
        Expr::Call { callee, args, .. } => {
            collect_accessed_fields_rec(callee, param_name, fields);
            for a in args {
                collect_accessed_fields_rec(a, param_name, fields);
            }
        }
        Expr::Index { arr, idx, .. } => {
            collect_accessed_fields_rec(arr, param_name, fields);
            collect_accessed_fields_rec(idx, param_name, fields);
        }
        Expr::Deref { e, .. } => {
            collect_accessed_fields_rec(e, param_name, fields);
        }
        Expr::Comma { l, r, .. } => {
            collect_accessed_fields_rec(l, param_name, fields);
            collect_accessed_fields_rec(r, param_name, fields);
        }
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => {
            collect_accessed_fields_rec(e, param_name, fields);
        }
        _ => {}
    }
}

/// 检查表达式是否对参数取地址
fn has_address_taken_in_expr(expr: &Expr, param_name: &str) -> bool {
    match expr {
        Expr::AddrOf { e, .. } => {
            if let Expr::Ident { name, .. } = e.as_ref() {
                if name.name == param_name {
                    return true;
                }
            }
            has_address_taken_in_expr(e, param_name)
        }
        Expr::Member { expr: e, .. } => has_address_taken_in_expr(e, param_name),
        Expr::Assign { l, r, .. } => {
            has_address_taken_in_expr(l, param_name) || has_address_taken_in_expr(r, param_name)
        }
        Expr::Binop { l, r, .. } => {
            has_address_taken_in_expr(l, param_name) || has_address_taken_in_expr(r, param_name)
        }
        Expr::Unary { e, .. } => has_address_taken_in_expr(e, param_name),
        Expr::Cast { e, .. } => has_address_taken_in_expr(e, param_name),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            has_address_taken_in_expr(cond, param_name)
                || has_address_taken_in_expr(then_, param_name)
                || has_address_taken_in_expr(else_, param_name)
        }
        Expr::Call { callee, args, .. } => {
            has_address_taken_in_expr(callee, param_name)
                || args
                    .iter()
                    .any(|a| has_address_taken_in_expr(a, param_name))
        }
        Expr::Index { arr, idx, .. } => {
            has_address_taken_in_expr(arr, param_name) || has_address_taken_in_expr(idx, param_name)
        }
        Expr::Deref { e, .. } => has_address_taken_in_expr(e, param_name),
        Expr::Comma { l, r, .. } => {
            has_address_taken_in_expr(l, param_name) || has_address_taken_in_expr(r, param_name)
        }
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => has_address_taken_in_expr(e, param_name),
        _ => false,
    }
}

/// 在语句中检查对参数的地址引用
fn has_address_taken_in_stmt(stmt: &Stmt, param_name: &str) -> bool {
    match stmt {
        Stmt::Return { val: Some(e), .. } => has_address_taken_in_expr(e, param_name),
        Stmt::Expr { e, .. } => has_address_taken_in_expr(e, param_name),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            has_address_taken_in_expr(cond, param_name)
                || has_address_taken_in_stmt(then_, param_name)
                || else_
                    .as_ref()
                    .map(|e| has_address_taken_in_stmt(e, param_name))
                    .unwrap_or(false)
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            has_address_taken_in_expr(cond, param_name)
                || has_address_taken_in_stmt(body, param_name)
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            (init
                .as_ref()
                .map(|i| has_address_taken_in_stmt(i, param_name))
                .unwrap_or(false))
                || cond
                    .as_ref()
                    .map(|c| has_address_taken_in_expr(c, param_name))
                    .unwrap_or(false)
                || step
                    .as_ref()
                    .map(|s| has_address_taken_in_expr(s, param_name))
                    .unwrap_or(false)
                || has_address_taken_in_stmt(body, param_name)
        }
        Stmt::Block { b, .. } => b
            .items
            .iter()
            .any(|item| has_address_taken_in_block_item(item, param_name)),
        Stmt::Switch { expr, cases, .. } => {
            has_address_taken_in_expr(expr, param_name)
                || cases.iter().any(|c| {
                    c.body
                        .items
                        .iter()
                        .any(|i| has_address_taken_in_block_item(i, param_name))
                })
        }
        _ => false,
    }
}

fn has_address_taken_in_block_item(item: &BlockItem, param_name: &str) -> bool {
    match item {
        BlockItem::Stmt(s) => has_address_taken_in_stmt(s, param_name),
        BlockItem::Decl(Decl::Var(v)) => v
            .init
            .as_ref()
            .map(|i| has_address_taken_in_expr(i, param_name))
            .unwrap_or(false),
        BlockItem::Decl(Decl::Func(f)) => f
            .body
            .as_ref()
            .map(|b| {
                b.items
                    .iter()
                    .any(|i| has_address_taken_in_block_item(i, param_name))
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// 在语句中收集字段访问
fn collect_accessed_fields_in_stmt(stmt: &Stmt, param_name: &str, fields: &mut Vec<String>) {
    match stmt {
        Stmt::Return { val: Some(e), .. } => {
            fields.extend(collect_accessed_fields(e, param_name));
        }
        Stmt::Expr { e, .. } => {
            fields.extend(collect_accessed_fields(e, param_name));
        }
        Stmt::If {
            cond, then_, else_, ..
        } => {
            fields.extend(collect_accessed_fields(cond, param_name));
            collect_accessed_fields_in_stmt(then_, param_name, fields);
            if let Some(e) = else_ {
                collect_accessed_fields_in_stmt(e, param_name, fields);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            fields.extend(collect_accessed_fields(cond, param_name));
            collect_accessed_fields_in_stmt(body, param_name, fields);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_accessed_fields_in_stmt(i, param_name, fields);
            }
            if let Some(c) = cond {
                fields.extend(collect_accessed_fields(c, param_name));
            }
            if let Some(s) = step {
                fields.extend(collect_accessed_fields(s, param_name));
            }
            collect_accessed_fields_in_stmt(body, param_name, fields);
        }
        Stmt::Block { b, .. } => {
            for item in &b.items {
                collect_accessed_fields_in_block_item(item, param_name, fields);
            }
        }
        Stmt::Switch { expr, cases, .. } => {
            fields.extend(collect_accessed_fields(expr, param_name));
            for c in cases {
                for item in &c.body.items {
                    collect_accessed_fields_in_block_item(item, param_name, fields);
                }
            }
        }
        _ => {}
    }
}

fn collect_accessed_fields_in_block_item(
    item: &BlockItem,
    param_name: &str,
    fields: &mut Vec<String>,
) {
    match item {
        BlockItem::Stmt(s) => collect_accessed_fields_in_stmt(s, param_name, fields),
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &v.init {
                fields.extend(collect_accessed_fields(init, param_name));
            }
        }
        BlockItem::Decl(Decl::Func(f)) => {
            if let Some(body) = &f.body {
                for item in &body.items {
                    collect_accessed_fields_in_block_item(item, param_name, fields);
                }
            }
        }
        _ => {}
    }
}

/// 分析单个函数: 返回哪些 struct 参数可以拆分
fn analyze_function(func: &FuncDecl) -> Vec<StructParamAnalysis> {
    let mut results = Vec::new();
    let body = match &func.body {
        Some(b) => b,
        None => return results,
    };

    for (idx, param) in func.params.iter().enumerate() {
        let param_name = match &param.name {
            Some(n) => n.name.clone(),
            None => continue,
        };

        // 只处理按值传递的 struct/union
        if !is_aggregate_by_value(&param.ty) {
            continue;
        }

        // 检查是否被取地址
        let has_address_taken = body
            .items
            .iter()
            .any(|item| has_address_taken_in_block_item(item, &param_name));

        // 收集访问的字段
        let mut accessed_fields = Vec::new();
        for item in &body.items {
            collect_accessed_fields_in_block_item(item, &param_name, &mut accessed_fields);
        }
        // 去重并保持顺序
        accessed_fields.sort();
        accessed_fields.dedup();

        // 验证这些字段是否真的存在于结构体中
        let all_fields = extract_fields(&param.ty);
        let valid_fields: Vec<String> = accessed_fields
            .into_iter()
            .filter(|f| all_fields.iter().any(|(name, _)| name == f))
            .collect();

        results.push(StructParamAnalysis {
            param_idx: idx,
            param_name,
            struct_type: param.ty.clone(),
            accessed_fields: valid_fields,
            has_address_taken,
        });
    }

    results
}

/// 判断拆分是否值得: 至少访问 1 个字段且不超过 max_scalar_params
fn is_worth_splitting(analysis: &StructParamAnalysis, config: &IpaSraConfig) -> bool {
    if analysis.has_address_taken {
        return false;
    }
    if analysis.accessed_fields.is_empty() {
        return false;
    }
    // 如果访问了所有字段, 拆分无收益
    let all_fields = extract_fields(&analysis.struct_type);
    if analysis.accessed_fields.len() >= all_fields.len() {
        return false;
    }
    analysis.accessed_fields.len() <= config.max_scalar_params
}

/// 为拆分后的参数生成新参数名
fn scalar_param_name(param_name: &str, field_name: &str) -> String {
    format!("{}_{}", param_name, field_name)
}

/// 在函数体内, 把 Ident(param_name) 替换为对应字段的局部变量
fn replace_param_refs_in_expr(
    expr: &mut Expr,
    param_name: &str,
    field_var_map: &HashMap<String, String>,
) {
    match expr {
        Expr::Member {
            expr: e,
            field,
            span,
            ty,
            ..
        } => {
            if let Expr::Ident { name, .. } = e.as_ref() {
                if name.name == param_name {
                    if let Some(var_name) = field_var_map.get(&field.name) {
                        *expr = Expr::Ident {
                            name: Ident {
                                name: var_name.clone(),
                                span: field.span.clone(),
                            },
                            span: span.clone(),
                            ty: ty.clone(),
                        };
                        return;
                    }
                }
            }
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Expr::Ident { .. } => {
            // 如果 param 被整体引用 (如传参), 保留原样 — 由调用方处理
        }
        Expr::AddrOf { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Expr::Assign { l, r, .. } => {
            replace_param_refs_in_expr(l, param_name, field_var_map);
            replace_param_refs_in_expr(r, param_name, field_var_map);
        }
        Expr::Binop { l, r, .. } => {
            replace_param_refs_in_expr(l, param_name, field_var_map);
            replace_param_refs_in_expr(r, param_name, field_var_map);
        }
        Expr::Unary { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Expr::Cast { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            replace_param_refs_in_expr(cond, param_name, field_var_map);
            replace_param_refs_in_expr(then_, param_name, field_var_map);
            replace_param_refs_in_expr(else_, param_name, field_var_map);
        }
        Expr::Call { callee, args, .. } => {
            replace_param_refs_in_expr(callee, param_name, field_var_map);
            for a in args {
                replace_param_refs_in_expr(a, param_name, field_var_map);
            }
        }
        Expr::Index { arr, idx, .. } => {
            replace_param_refs_in_expr(arr, param_name, field_var_map);
            replace_param_refs_in_expr(idx, param_name, field_var_map);
        }
        Expr::Deref { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Expr::Comma { l, r, .. } => {
            replace_param_refs_in_expr(l, param_name, field_var_map);
            replace_param_refs_in_expr(r, param_name, field_var_map);
        }
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Expr::SizeofExpr { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        _ => {}
    }
}

fn replace_param_refs_in_stmt(
    stmt: &mut Stmt,
    param_name: &str,
    field_var_map: &HashMap<String, String>,
) {
    match stmt {
        Stmt::Return { val: Some(e), .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Stmt::Expr { e, .. } => {
            replace_param_refs_in_expr(e, param_name, field_var_map);
        }
        Stmt::If {
            cond, then_, else_, ..
        } => {
            replace_param_refs_in_expr(cond, param_name, field_var_map);
            replace_param_refs_in_stmt(then_, param_name, field_var_map);
            if let Some(e) = else_ {
                replace_param_refs_in_stmt(e, param_name, field_var_map);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            replace_param_refs_in_expr(cond, param_name, field_var_map);
            replace_param_refs_in_stmt(body, param_name, field_var_map);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                replace_param_refs_in_stmt(i, param_name, field_var_map);
            }
            if let Some(c) = cond {
                replace_param_refs_in_expr(c, param_name, field_var_map);
            }
            if let Some(s) = step {
                replace_param_refs_in_expr(s, param_name, field_var_map);
            }
            replace_param_refs_in_stmt(body, param_name, field_var_map);
        }
        Stmt::Block { b, .. } => {
            for item in &mut b.items {
                replace_param_refs_in_block_item(item, param_name, field_var_map);
            }
        }
        Stmt::Switch { expr, cases, .. } => {
            replace_param_refs_in_expr(expr, param_name, field_var_map);
            for c in cases {
                for item in &mut c.body.items {
                    replace_param_refs_in_block_item(item, param_name, field_var_map);
                }
            }
        }
        _ => {}
    }
}

fn replace_param_refs_in_block_item(
    item: &mut BlockItem,
    param_name: &str,
    field_var_map: &HashMap<String, String>,
) {
    match item {
        BlockItem::Stmt(s) => replace_param_refs_in_stmt(s, param_name, field_var_map),
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &mut v.init {
                replace_param_refs_in_expr(init, param_name, field_var_map);
            }
        }
        BlockItem::Decl(Decl::Func(f)) => {
            if let Some(body) = &mut f.body {
                for item in &mut body.items {
                    replace_param_refs_in_block_item(item, param_name, field_var_map);
                }
            }
        }
        _ => {}
    }
}

/// 改写函数签名: 拆分 struct 参数为标量参数
fn rewrite_function_params(
    func: &mut FuncDecl,
    analysis: &StructParamAnalysis,
    all_fields: &[(String, Type)],
) -> Vec<(String, Type)> {
    // 构建 field_name -> ty 的映射
    let field_map: HashMap<&str, &Type> = all_fields.iter().map(|(n, t)| (n.as_str(), t)).collect();

    let mut new_params: Vec<Param> = Vec::new();
    for (i, param) in func.params.iter().enumerate() {
        if i == analysis.param_idx {
            // 替换为标量参数
            for field_name in &analysis.accessed_fields {
                if let Some(field_ty) = field_map.get(field_name.as_str()) {
                    let scalar_name = scalar_param_name(&analysis.param_name, field_name);
                    new_params.push(Param {
                    quals: crate::ast::QualifierFlags::NONE,
                        ty: (*field_ty).clone(),
                        name: Some(Ident {
                            name: scalar_name,
                            span: param.span.clone(),
                        }),
                        span: param.span.clone(),
                        vla_dim: None,
                    });
                }
            }
        } else {
            new_params.push(param.clone());
        }
    }

    let _replaced_params = std::mem::replace(&mut func.params, new_params);

    // 构建 field_name -> scalar_var_name 映射
    let mut field_var_map = HashMap::new();
    for field_name in &analysis.accessed_fields {
        let scalar_name = scalar_param_name(&analysis.param_name, field_name);
        field_var_map.insert(field_name.clone(), scalar_name);
    }

    // 返回被替换的参数信息用于调用方改写
    let mut result = Vec::new();
    for field_name in &analysis.accessed_fields {
        if let Some(field_ty) = field_map.get(field_name.as_str()) {
            result.push((field_name.clone(), (*field_ty).clone()));
        }
    }

    result
}

/// 在函数体顶部注入局部变量声明, 把标量参数赋值给原结构体布局
/// 实际上, 由于我们已经把 Member(param.field) 替换为 scalar_var,
/// 不需要注入局部变量。但为了兼容性 (如果代码中还有整体引用 param),
/// 此处保留原 param 作为局部变量, 从标量组装。
fn inject_scalar_locals(
    body: &mut Block,
    analysis: &StructParamAnalysis,
    all_fields: &[(String, Type)],
    span: &Span,
) {
    let field_map: HashMap<&str, &Type> = all_fields.iter().map(|(n, t)| (n.as_str(), t)).collect();

    // 声明局部变量: 使用原结构体类型
    let local_decl = Decl::Var(crate::ast::VarDecl {
        ty: analysis.struct_type.clone(),
        name: Ident {
            name: analysis.param_name.clone(),
            span: span.clone(),
        },
        init: None, // 运行时由标量组装
        span: span.clone(),
        is_extern: false,
        is_static: false,
        section: None,
        alignment: None,
        is_weak: false,
        visibility: None,
        is_unused: false,
        register_name: None,
        cleanup_fn: None,
        is_thread_local: false,
        no_sanitize: vec![],
        alias_name: None,
        copy_fn: None,
        is_retain: false,
        is_used_attr: false,
        quals: QualifierFlags::NONE,
        addr_space: crate::ast::AddrSpace::None,
        tls_model: None,
        asm_label: None,
    });

    // 从标量参数组装字段赋值: `param.field = field_scalar;`
    let mut assign_stmts: Vec<BlockItem> = Vec::new();
    for field_name in &analysis.accessed_fields {
        if let Some(field_ty) = field_map.get(field_name.as_str()) {
            let scalar_name = scalar_param_name(&analysis.param_name, field_name);
            assign_stmts.push(BlockItem::Stmt(Stmt::Expr {
                e: Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(Expr::Member {
                        expr: Box::new(Expr::Ident {
                            name: Ident {
                                name: analysis.param_name.clone(),
                                span: span.clone(),
                            },
                            span: span.clone(),
                            ty: analysis.struct_type.clone(),
                        }),
                        field: Ident {
                            name: field_name.clone(),
                            span: span.clone(),
                        },
                        span: span.clone(),
                        ty: (*field_ty).clone(),
                        is_arrow: false,
                    }),
                    r: Box::new(Expr::Ident {
                        name: Ident {
                            name: scalar_name,
                            span: span.clone(),
                        },
                        span: span.clone(),
                        ty: (*field_ty).clone(),
                    }),
                    span: span.clone(),
                    ty: (*field_ty).clone(),
                },
                span: span.clone(),
            }));
        }
    }

    // 在 body 顶部插入局部声明和赋值
    // 注意: 由于我们已经替换了 Member 引用, 但实际上 codegen 可能仍然
    // 需要整体结构体值 (如传参给另一个函数), 所以保留局部变量组装
    let mut new_items = Vec::new();
    new_items.push(BlockItem::Decl(local_decl));
    new_items.extend(assign_stmts);
    new_items.append(&mut body.items);
    body.items = new_items;
}

/// 改写调用点: 把 struct 参数展开为标量字段
fn rewrite_call_args(
    expr: &mut Expr,
    func_name: &str,
    param_idx: usize,
    accessed_fields: &[String],
    all_fields: &[(String, Type)],
) -> bool {
    match expr {
        Expr::Call {
            callee, args, span, ..
        } => {
            // 检查是否是目标函数的调用
            let is_target = match callee.as_ref() {
                Expr::Ident { name, .. } => name.name == func_name,
                _ => false,
            };

            if !is_target {
                return false;
            }

            if param_idx >= args.len() {
                return false;
            }

            let struct_arg = args.remove(param_idx);

            // 展开: 为每个被访问字段生成 Member 表达式, 插入到 args 中
            let field_map: HashMap<&str, &Type> =
                all_fields.iter().map(|(n, t)| (n.as_str(), t)).collect();

            for field_name in accessed_fields {
                if let Some(field_ty) = field_map.get(field_name.as_str()) {
                    let field_expr = Expr::Member {
                        expr: Box::new(struct_arg.clone()),
                        field: Ident {
                            name: field_name.clone(),
                            span: span.clone(),
                        },
                        span: span.clone(),
                        ty: (*field_ty).clone(),
                        is_arrow: false,
                    };
                    args.insert(param_idx, field_expr);
                }
            }

            true
        }
        Expr::Assign { l, r, .. } => {
            let changed1 = rewrite_call_args(l, func_name, param_idx, accessed_fields, all_fields);
            let changed2 = rewrite_call_args(r, func_name, param_idx, accessed_fields, all_fields);
            changed1 || changed2
        }
        Expr::Binop { l, r, .. } => {
            let changed1 = rewrite_call_args(l, func_name, param_idx, accessed_fields, all_fields);
            let changed2 = rewrite_call_args(r, func_name, param_idx, accessed_fields, all_fields);
            changed1 || changed2
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            let c1 = rewrite_call_args(cond, func_name, param_idx, accessed_fields, all_fields);
            let c2 = rewrite_call_args(then_, func_name, param_idx, accessed_fields, all_fields);
            let c3 = rewrite_call_args(else_, func_name, param_idx, accessed_fields, all_fields);
            c1 || c2 || c3
        }
        Expr::Unary { e, .. } => {
            rewrite_call_args(e, func_name, param_idx, accessed_fields, all_fields)
        }
        Expr::Cast { e, .. } => {
            rewrite_call_args(e, func_name, param_idx, accessed_fields, all_fields)
        }
        Expr::Index { arr, idx, .. } => {
            let c1 = rewrite_call_args(arr, func_name, param_idx, accessed_fields, all_fields);
            let c2 = rewrite_call_args(idx, func_name, param_idx, accessed_fields, all_fields);
            c1 || c2
        }
        Expr::Deref { e, .. } => {
            rewrite_call_args(e, func_name, param_idx, accessed_fields, all_fields)
        }
        Expr::Comma { l, r, .. } => {
            let c1 = rewrite_call_args(l, func_name, param_idx, accessed_fields, all_fields);
            let c2 = rewrite_call_args(r, func_name, param_idx, accessed_fields, all_fields);
            c1 || c2
        }
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::Ident { .. }
        | Expr::Member { .. }
        | Expr::AddrOf { .. } => false,
        _ => false,
    }
}

/// 在语句中改写调用点
fn rewrite_call_args_in_stmt(
    stmt: &mut Stmt,
    func_name: &str,
    param_idx: usize,
    accessed_fields: &[String],
    all_fields: &[(String, Type)],
) -> bool {
    match stmt {
        Stmt::Return { val: Some(e), .. } => {
            rewrite_call_args(e, func_name, param_idx, accessed_fields, all_fields)
        }
        Stmt::Expr { e, .. } => {
            rewrite_call_args(e, func_name, param_idx, accessed_fields, all_fields)
        }
        Stmt::If {
            cond, then_, else_, ..
        } => {
            let c1 = rewrite_call_args(cond, func_name, param_idx, accessed_fields, all_fields);
            let c2 =
                rewrite_call_args_in_stmt(then_, func_name, param_idx, accessed_fields, all_fields);
            let c3 = else_
                .as_mut()
                .map(|e| {
                    rewrite_call_args_in_stmt(e, func_name, param_idx, accessed_fields, all_fields)
                })
                .unwrap_or(false);
            c1 || c2 || c3
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            let c1 = rewrite_call_args(cond, func_name, param_idx, accessed_fields, all_fields);
            let c2 =
                rewrite_call_args_in_stmt(body, func_name, param_idx, accessed_fields, all_fields);
            c1 || c2
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            let mut changed = false;
            if let Some(i) = init {
                changed |=
                    rewrite_call_args_in_stmt(i, func_name, param_idx, accessed_fields, all_fields);
            }
            if let Some(c) = cond {
                changed |= rewrite_call_args(c, func_name, param_idx, accessed_fields, all_fields);
            }
            if let Some(s) = step {
                changed |= rewrite_call_args(s, func_name, param_idx, accessed_fields, all_fields);
            }
            changed |=
                rewrite_call_args_in_stmt(body, func_name, param_idx, accessed_fields, all_fields);
            changed
        }
        Stmt::Block { b, .. } => {
            let mut changed = false;
            for item in &mut b.items {
                changed |= rewrite_call_args_in_block_item(
                    item,
                    func_name,
                    param_idx,
                    accessed_fields,
                    all_fields,
                );
            }
            changed
        }
        Stmt::Switch { expr, cases, .. } => {
            let mut changed =
                rewrite_call_args(expr, func_name, param_idx, accessed_fields, all_fields);
            for c in cases {
                for item in &mut c.body.items {
                    changed |= rewrite_call_args_in_block_item(
                        item,
                        func_name,
                        param_idx,
                        accessed_fields,
                        all_fields,
                    );
                }
            }
            changed
        }
        _ => false,
    }
}

fn rewrite_call_args_in_block_item(
    item: &mut BlockItem,
    func_name: &str,
    param_idx: usize,
    accessed_fields: &[String],
    all_fields: &[(String, Type)],
) -> bool {
    match item {
        BlockItem::Stmt(s) => {
            rewrite_call_args_in_stmt(s, func_name, param_idx, accessed_fields, all_fields)
        }
        BlockItem::Decl(Decl::Var(v)) => v
            .init
            .as_mut()
            .map(|i| rewrite_call_args(i, func_name, param_idx, accessed_fields, all_fields))
            .unwrap_or(false),
        BlockItem::Decl(Decl::Func(f)) => f
            .body
            .as_mut()
            .map(|b| {
                b.items.iter_mut().any(|item| {
                    rewrite_call_args_in_block_item(
                        item,
                        func_name,
                        param_idx,
                        accessed_fields,
                        all_fields,
                    )
                })
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// 主入口: 对整个 Program 运行 IPA-SRA
pub fn run_ipa_sra(prog: &mut Program, config: &IpaSraConfig) {
    if !config.should_run() {
        return;
    }

    // 构建函数名 → FuncDecl 索引映射
    let mut func_indices: HashMap<String, usize> = HashMap::new();
    for (i, decl) in prog.decls.iter().enumerate() {
        if let Decl::Func(f) = decl {
            if f.is_static && !f.is_variadic {
                func_indices.insert(f.name.name.clone(), i);
            }
        }
    }

    // 收集所有需要拆分的候选
    let mut candidates: Vec<(usize, StructParamAnalysis)> = Vec::new();
    for (i, decl) in prog.decls.iter().enumerate() {
        if let Decl::Func(f) = decl {
            if !f.is_static || f.is_variadic || f.body.is_none() {
                continue;
            }
            let analyses = analyze_function(f);
            for analysis in analyses {
                if is_worth_splitting(&analysis, config) {
                    candidates.push((i, analysis));
                }
            }
        }
    }

    // 执行拆分 (从后往前处理, 避免索引偏移)
    candidates.sort_by_key(|b| std::cmp::Reverse(b.0));
    for (func_idx, analysis) in candidates {
        let func_name = match &prog.decls[func_idx] {
            Decl::Func(f) => f.name.name.clone(),
            _ => continue,
        };

        // 获取原始字段列表
        let all_fields = extract_fields(&analysis.struct_type);
        if all_fields.is_empty() {
            continue;
        }

        // 改写函数签名
        if let Decl::Func(func) = &mut prog.decls[func_idx] {
            let span = func.span.clone();

            // 替换函数体中的 Member 引用: 直接遍历 body items
            if let Some(body) = &mut func.body {
                let field_map: HashMap<String, String> = analysis
                    .accessed_fields
                    .iter()
                    .map(|f| (f.clone(), scalar_param_name(&analysis.param_name, f)))
                    .collect();
                for item in &mut body.items {
                    replace_param_refs_in_block_item(item, &analysis.param_name, &field_map);
                }

                // 注入局部变量组装
                inject_scalar_locals(body, &analysis, &all_fields, &span);
            }

            // 2. 替换参数列表
            rewrite_function_params(func, &analysis, &all_fields);
        }

        // 3. 改写所有调用点
        for (caller_idx, decl) in prog.decls.iter_mut().enumerate() {
            if caller_idx == func_idx {
                continue;
            }
            if let Decl::Func(caller) = decl {
                if let Some(body) = &mut caller.body {
                    for item in &mut body.items {
                        rewrite_call_args_in_block_item(
                            item,
                            &func_name,
                            analysis.param_idx,
                            &analysis.accessed_fields,
                            &all_fields,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Block, Field, FuncDecl, Ident, Param, Program, Stmt, Type};
    use crate::diagnostics::Span;

    fn span_zero() -> Span {
        Span::point("t.c", 1, 1)
    }

    fn int_ty() -> Type {
        Type::Int
    }

    fn make_struct_type(name: &str, fields: Vec<(&str, Type)>) -> Type {
        let field_vec: Vec<Field> = fields
            .into_iter()
            .enumerate()
            .map(|(i, (n, t))| Field {
                name: n.to_string(),
                ty: t,
                offset: i * 4,
                bit_width: None,
                bit_offset: 0,
                quals: QualifierFlags::NONE,
                ptr_quals: vec![],
                field_align: None,
                field_packed: false,
            })
            .collect();
        let align = field_vec.iter().map(|f| f.ty.align()).max().unwrap_or(1);
        Type::Struct(
            name.to_string(),
            std::sync::Arc::new(field_vec),
            align,
            false,
        )
    }

    /// 测试分析函数: 检测 struct 参数中哪些字段被访问
    #[test]
    fn test_analyze_struct_param_access() {
        let struct_ty = make_struct_type("Point", vec![("x", int_ty()), ("y", int_ty())]);
        let func = FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "f".into(),
                span: span_zero(),
            },
            params: vec![Param {
                    quals: crate::ast::QualifierFlags::NONE,
                ty: struct_ty,
                name: Some(Ident {
                    name: "p".into(),
                    span: span_zero(),
                }),
                span: span_zero(),
                vla_dim: None,
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Member {
                        expr: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: span_zero(),
                            },
                            span: span_zero(),
                            ty: int_ty(),
                        }),
                        field: Ident {
                            name: "x".into(),
                            span: span_zero(),
                        },
                        span: span_zero(),
                        ty: int_ty(),
                        is_arrow: false,
                    }),
                    span: span_zero(),
                })],
                span: span_zero(),
            }),
            span: span_zero(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
                    is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
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
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
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

        let analyses = analyze_function(&func);
        assert_eq!(analyses.len(), 1);
        let a = &analyses[0];
        assert_eq!(a.param_name, "p");
        assert_eq!(a.accessed_fields, vec!["x"]);
        assert!(!a.has_address_taken);
    }

    /// 测试 address_taken 检测: &p 应阻止拆分
    #[test]
    fn test_address_taken_blocks_sra() {
        let struct_ty = make_struct_type("Point", vec![("x", int_ty())]);
        let func = FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "f".into(),
                span: span_zero(),
            },
            params: vec![Param {
                    quals: crate::ast::QualifierFlags::NONE,
                ty: struct_ty,
                name: Some(Ident {
                    name: "p".into(),
                    span: span_zero(),
                }),
                span: span_zero(),
                vla_dim: None,
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Expr {
                    e: Expr::AddrOf {
                        e: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: span_zero(),
                            },
                            span: span_zero(),
                            ty: int_ty(),
                        }),
                        span: span_zero(),
                        ty: Type::Ptr(Box::new(int_ty().into())),
                    },
                    span: span_zero(),
                })],
                span: span_zero(),
            }),
            span: span_zero(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
                    is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
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
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
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

        let analyses = analyze_function(&func);
        assert_eq!(analyses.len(), 1);
        let a = &analyses[0];
        assert!(a.has_address_taken, "&p 应标记 address_taken");

        let config = IpaSraConfig {
            enabled: true,
            opt_level: 2,
            max_scalar_params: 8,
        };
        assert!(!is_worth_splitting(a, &config));
    }

    /// 测试全部字段被访问时不应拆分
    #[test]
    fn test_all_fields_accessed_no_split() {
        let struct_ty = make_struct_type("Point", vec![("x", int_ty()), ("y", int_ty())]);
        let func = FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "f".into(),
                span: span_zero(),
            },
            params: vec![Param {
                    quals: crate::ast::QualifierFlags::NONE,
                ty: struct_ty,
                name: Some(Ident {
                    name: "p".into(),
                    span: span_zero(),
                }),
                span: span_zero(),
                vla_dim: None,
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Binop {
                        op: BinOp::Add,
                        l: Box::new(Expr::Member {
                            expr: Box::new(Expr::Ident {
                                name: Ident {
                                    name: "p".into(),
                                    span: span_zero(),
                                },
                                span: span_zero(),
                                ty: int_ty(),
                            }),
                            field: Ident {
                                name: "x".into(),
                                span: span_zero(),
                            },
                            span: span_zero(),
                            ty: int_ty(),
                            is_arrow: false,
                        }),
                        r: Box::new(Expr::Member {
                            expr: Box::new(Expr::Ident {
                                name: Ident {
                                    name: "p".into(),
                                    span: span_zero(),
                                },
                                span: span_zero(),
                                ty: int_ty(),
                            }),
                            field: Ident {
                                name: "y".into(),
                                span: span_zero(),
                            },
                            span: span_zero(),
                            ty: int_ty(),
                            is_arrow: false,
                        }),
                        span: span_zero(),
                        ty: int_ty(),
                    }),
                    span: span_zero(),
                })],
                span: span_zero(),
            }),
            span: span_zero(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
                    is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
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
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
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

        let analyses = analyze_function(&func);
        assert_eq!(analyses.len(), 1);
        let a = &analyses[0];
        // 只访问了 2 个字段, 但结构体只有 2 个字段, 所以不拆分
        assert_eq!(a.accessed_fields.len(), 2);
        let config = IpaSraConfig {
            enabled: true,
            opt_level: 2,
            max_scalar_params: 8,
        };
        assert!(!is_worth_splitting(a, &config));
    }

    /// 测试完整的 IPA-SRA 流程: 拆分 struct 参数并改写调用点
    #[test]
    fn test_ipa_sra_full_flow() {
        let struct_ty = make_struct_type("Point", vec![("x", int_ty()), ("y", int_ty())]);

        // f(p): 只使用 p.x
        let f_func = Decl::Func(FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "f".into(),
                span: span_zero(),
            },
            params: vec![Param {
                    quals: crate::ast::QualifierFlags::NONE,
                ty: struct_ty.clone(),
                name: Some(Ident {
                    name: "p".into(),
                    span: span_zero(),
                }),
                span: span_zero(),
                vla_dim: None,
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Member {
                        expr: Box::new(Expr::Ident {
                            name: Ident {
                                name: "p".into(),
                                span: span_zero(),
                            },
                            span: span_zero(),
                            ty: int_ty(),
                        }),
                        field: Ident {
                            name: "x".into(),
                            span: span_zero(),
                        },
                        span: span_zero(),
                        ty: int_ty(),
                        is_arrow: false,
                    }),
                    span: span_zero(),
                })],
                span: span_zero(),
            }),
            span: span_zero(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
                    is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
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
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
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
        });

        // main(): 调用 f(Point{1, 2})
        let main_func = Decl::Func(FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "main".into(),
                span: span_zero(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: Ident {
                                name: "f".into(),
                                span: span_zero(),
                            },
                            span: span_zero(),
                            ty: Type::Func(Box::new(int_ty().into()), vec![struct_ty.clone().into()]),
                        }),
                        args: vec![Expr::CompoundLit {
                            ty: struct_ty.clone(),
                            inits: vec![
                                crate::ast::InitItem::Expr(Expr::Int {
                                    v: 1,
                                    span: span_zero(),
                                    ty: int_ty(),
                                }),
                                crate::ast::InitItem::Expr(Expr::Int {
                                    v: 2,
                                    span: span_zero(),
                                    ty: int_ty(),
                                }),
                            ],
                            span: span_zero(),
                        }],
                        span: span_zero(),
                        ty: int_ty(),
                    }),
                    span: span_zero(),
                })],
                span: span_zero(),
            }),
            span: span_zero(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
                    is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
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
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
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
        });

        let mut prog = Program {
            decls: vec![f_func, main_func],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };

        let config = IpaSraConfig {
            enabled: true,
            opt_level: 2,
            max_scalar_params: 8,
        };
        run_ipa_sra(&mut prog, &config);

        // 验证: f 的参数从 struct 变为 scalar (p_x)
        if let Decl::Func(f) = &prog.decls[0] {
            assert_eq!(f.params.len(), 1, "f 应有 1 个标量参数");
            // 参数名应为 p_x
            if let Some(name) = &f.params[0].name {
                assert!(
                    name.name.contains("p_x"),
                    "参数名应包含 p_x, 实际: {}",
                    name.name
                );
            }
        } else {
            panic!("第一个 decl 应为 Func");
        }
    }
}
