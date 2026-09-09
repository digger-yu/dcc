// src/static_locals.rs - 静态局部变量分析 pass
//
// 独立于后端代码生成的分析阶段: 扫描 Program 中所有函数体,
// 收集 static local 变量信息, 输出结构化的 StaticLocalMap.
// 后端消费此结果进行代码生成, 不再自行扫描 AST.

use crate::ast::*;
use std::collections::HashMap;

/// 单个 static local 的完整信息 (分析 pass 输出).
///
/// `needs_runtime_init` 字段由后端在注册阶段 (register_static_global)
/// 根据实际初始化结果回填, 分析 pass 本身不做编译期求值判断.
#[derive(Debug, Clone)]
pub struct StaticLocalInfo {
    /// 源码名称 (仅用于诊断和 rename map 查找)
    pub source_name: String,
    /// 唯一 mangled 名 (链接用, 格式: .Lsl<N>.<func>.<name>)
    pub mangled_name: String,
    /// 变量类型
    pub ty: Type,
    /// 初始化式 (AST 节点, 后端用于生成初始化代码)
    pub init: Option<Expr>,
    /// 对齐要求
    pub alignment: usize,
    /// 大小 (字节)
    pub size: usize,
    /// 所属函数名
    pub owner_func: String,
    /// 是否需要运行时初始化 (由后端注册阶段回填)
    pub needs_runtime_init: bool,
}

/// 所有 static local 的分析结果.
///
/// 提供按函数名、按 mangled 名、按源码名的多种查找方式,
/// 替代后端原先的 registered_static_locals / static_local_rename_map 等分散字段.
#[derive(Debug, Clone, Default)]
pub struct StaticLocalMap {
    /// 按函数名索引: func_name -> [StaticLocalInfo]
    pub by_function: HashMap<String, Vec<StaticLocalInfo>>,
    /// 快速查找: mangled_name -> StaticLocalInfo
    pub by_mangled: HashMap<String, StaticLocalInfo>,
    /// 源码名 -> mangled 名 (每函数一个条目, 有序, 后注册的优先)
    rename_map: HashMap<String, Vec<(String, String)>>,
    /// 需要运行时初始化的 static local: func_name -> [source_name]
    /// 由后端注册阶段回填.
    runtime_init: HashMap<String, Vec<String>>,
}

impl StaticLocalMap {
    /// 查询变量的 mangled 名 (从后向前扫描, 内层作用域优先).
    /// 如果 name 不是 static local, 返回 None.
    pub fn resolve(&self, func_name: &str, source_name: &str) -> Option<&str> {
        self.rename_map.get(func_name).and_then(|entries| {
            entries
                .iter()
                .rev()
                .find(|(src, _)| src == source_name)
                .map(|(_, mangled)| mangled.as_str())
        })
    }

    /// 判断某源码名在某函数中是否是 static local.
    pub fn is_static_local(&self, func_name: &str, source_name: &str) -> bool {
        self.resolve(func_name, source_name).is_some()
    }

    /// 判断某 static local 是否需要运行时初始化.
    pub fn needs_runtime_init(&self, func_name: &str, source_name: &str) -> bool {
        self.runtime_init
            .get(func_name)
            .map_or(false, |names| names.iter().any(|n| n == source_name))
    }

    /// 获取某函数的所有 static local 信息.
    pub fn func_static_locals(&self, func_name: &str) -> &[StaticLocalInfo] {
        self.by_function
            .get(func_name)
            .map_or(&[], |v| v.as_slice())
    }

    /// 标记某 static local 需要运行时初始化 (由后端注册阶段调用).
    pub fn mark_runtime_init(&mut self, func_name: &str, source_name: &str) {
        // 更新 StaticLocalInfo
        if let Some(info) = self.by_mangled.get_mut(
            &self.resolve(func_name, source_name)
                .unwrap_or("")
                .to_string()
        ) {
            info.needs_runtime_init = true;
        }
        // 更新 runtime_init 索引
        self.runtime_init
            .entry(func_name.to_string())
            .or_default()
            .push(source_name.to_string());
    }

    /// 迭代所有 static local (跨函数).
    pub fn iter_all(&self) -> impl Iterator<Item = &StaticLocalInfo> {
        self.by_function.values().flat_map(|v| v.iter())
    }
}

/// 分析 pass 入口: 扫描 Program 中所有函数, 收集 static local 信息.
///
/// 遍历顺序与后端 compile_exec/compile_rel 处理全局变量的顺序一致:
/// 先处理文件级全局变量 (由后端完成), 再调用此函数分析函数内 static local.
/// 分析结果传递给后端, 后端据此预注册 static local 到 data/bss 段.
pub fn analyze_static_locals(prog: &Program) -> StaticLocalMap {
    let mut map = StaticLocalMap::default();
    let mut counter: u32 = 0;

    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &f.body {
                collect_from_block(&mut map, &f.name.name, &body.items, &mut counter);
            }
        }
    }
    map
}

/// 递归扫描块中的声明和语句, 收集 static local.
fn collect_from_block(
    map: &mut StaticLocalMap,
    func_name: &str,
    items: &[BlockItem],
    counter: &mut u32,
) {
    for item in items {
        match item {
            BlockItem::Decl(Decl::Var(v)) if v.is_static => {
                let mangled = format!(".Lsl{}.{}.{}", *counter, func_name, v.name.name);
                *counter += 1;
                let sz = v.ty.size().max(1);
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                // needs_runtime_init 由后端注册阶段根据实际初始化结果回填,
                // 分析 pass 保守设为 false (假设可以静态初始化).
                let info = StaticLocalInfo {
                    source_name: v.name.name.clone(),
                    mangled_name: mangled.clone(),
                    ty: v.ty.clone(),
                    init: v.init.clone(),
                    alignment: align,
                    size: sz,
                    owner_func: func_name.to_string(),
                    needs_runtime_init: false,
                };
                map.by_function
                    .entry(func_name.to_string())
                    .or_default()
                    .push(info.clone());
                map.by_mangled.insert(mangled.clone(), info);
                map.rename_map
                    .entry(func_name.to_string())
                    .or_default()
                    .push((v.name.name.clone(), mangled));
            }
            BlockItem::Decl(_) => {}
            BlockItem::Stmt(s) => collect_from_stmt(map, func_name, s, counter),
        }
    }
}

/// 递归扫描语句, 收集 static local.
fn collect_from_stmt(
    map: &mut StaticLocalMap,
    func_name: &str,
    s: &Stmt,
    counter: &mut u32,
) {
    match s {
        Stmt::Block { b, .. } => collect_from_block(map, func_name, &b.items, counter),
        Stmt::If { then_, else_, .. } => {
            collect_from_stmt(map, func_name, then_, counter);
            if let Some(e) = else_ {
                collect_from_stmt(map, func_name, e, counter);
            }
        }
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            collect_from_stmt(map, func_name, body, counter);
        }
        Stmt::For { init, body, .. } => {
            if let Some(init_stmt) = init {
                collect_from_stmt(map, func_name, init_stmt, counter);
            }
            collect_from_stmt(map, func_name, body, counter);
        }
        Stmt::Switch { cases, .. } => {
            for case in cases {
                collect_from_block(map, func_name, &case.body.items, counter);
            }
        }
        Stmt::Label { stmt, .. } => collect_from_stmt(map, func_name, stmt, counter),
        _ => {}
    }
}
