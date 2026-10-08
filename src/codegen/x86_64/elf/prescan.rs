// src/codegen/x86_64/elf/prescan.rs
// Prescan helpers: nested function collection and signature registration.

use crate::ast::*;
use crate::codegen::x86_64::Codegen;
use std::collections::HashMap;

impl Codegen {
    pub fn collect_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            if let BlockItem::Decl(Decl::Func(f)) = item {
                if f.body.is_some() {
                    out.push(f.clone());
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, out);
                    }
                }
            }
            // 也扫描嵌套块 (if/for/while 等)
            if let BlockItem::Stmt(s) = item {
                Self::collect_nested_funcs_stmt(s, out);
            }
        }
    }
    fn collect_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                if let Stmt::Block { b, .. } = then_.as_ref() {
                    Self::collect_nested_funcs(&b.items, out);
                }
                if let Some(e) = else_ {
                    if let Stmt::Block { b, .. } = e.as_ref() {
                        Self::collect_nested_funcs(&b.items, out);
                    }
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                if let Stmt::Block { b, .. } = body.as_ref() {
                    Self::collect_nested_funcs(&b.items, out);
                }
            },
            _ => {},
        }
    }
    pub fn register_func_sig(f: &FuncDecl, map: &mut HashMap<String, Vec<Type>>) {
        let params: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
        map.insert(f.name.name.clone(), params);
    }

    /// 预分配**全部**嵌套函数的唯一符号名（发射期只查不分配）。
    ///
    /// 为什么必须在并行发射之前一次定完：唯一名里带一个全局递增计数
    /// `nested_func_rename_counter`。旧实现把它放在 `compile_func` 里递增，而每个
    /// worker 都从同一份快照起号 ⇒ 第二个含嵌套函数的顶层函数在并行下拿到 `.0`、
    /// 串行下拿到 `.1`（两份产物因此永远不一致），并且主 Codegen 收不回 worker
    /// 侧新登记的符号事实 ⇒ 嵌套函数的局部符号整个消失、调用点悬空成
    /// `U inner_parent.0`（§11.13 的 N1）。
    ///
    /// 递归形状必须与发射期的嵌套处理逐字一致：先按源码序给本层**全部直接**
    /// 嵌套函数编号，再依次下探（用的就是发射期同一个遍历器
    /// `collect_direct_nested_funcs`，下探时父名换成刚分配的唯一名 —— 与
    /// `compile_func` 把 `nf_decl.name.name` 换成唯一名后再递归同一形状）。
    pub(in crate::codegen::x86_64) fn prescan_nested_names(
        &mut self,
        items: &[BlockItem],
        parent_name: &str,
    ) {
        let mut directs: Vec<FuncDecl> = Vec::new();
        Self::collect_direct_nested_funcs(items, &mut directs);
        // 先给本层全部编号，再下探：两个列表分开存，避开 `directs` 与 `&mut self` 的
        // 借用冲突，也避开「下探过程中新分配的姓名抢了本层序号」的阶差。
        let mut descended: Vec<(String, Block)> = Vec::new();
        for nf in &directs {
            let orig_name = nf.name.name.clone();
            let unique_name = format!(
                "{}_{}.{}",
                orig_name, parent_name, self.nested_func_rename_counter
            );
            self.nested_func_rename_counter += 1;
            self.nested_func_renames
                .entry(parent_name.to_string())
                .or_default()
                .insert(orig_name, unique_name.clone());
            // 唯一名 → 定义父函数名 (静态链上溯与符号发射都靠这两张表反查)
            self.nested_func_parent
                .insert(unique_name.clone(), parent_name.to_string());
            self.nested_funcs
                .insert(unique_name.clone(), nf.upvars.clone());
            if let Some(body) = &nf.body {
                descended.push((unique_name, body.clone()));
            }
        }
        for (uniq, body) in &descended {
            self.prescan_nested_names(&body.items, uniq);
        }
    }
}
