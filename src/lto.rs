// src/lto.rs - 跨翻译单元 LTO 摘要容器（初版）
// 第一性原理: LTO 在链接期读取可重定位 IR 摘要，而非依赖外部 bitcode。

use std::collections::HashMap;

/// 单个函数在 LTO 阶段的摘要。
#[derive(Debug, Clone, Default)]
pub struct FunctionSummary {
    pub name: String,
    pub defined: bool,
    pub referenced: bool,
    pub size_hint: u64,
}

/// 单个 TU 的 LTO 摘要块。
#[derive(Debug, Clone, Default)]
pub struct LtoUnit {
    pub path: String,
    pub functions: HashMap<String, FunctionSummary>,
}

/// 解析/合并 LTO 摘要（当前为占位实现，供 Kbuild 探测）。
pub fn merge_units(mut units: Vec<LtoUnit>) -> LtoUnit {
    let mut merged = LtoUnit::default();
    for unit in units.drain(..) {
        for (name, summary) in unit.functions {
            merged.functions.entry(name).or_insert(summary);
        }
    }
    merged
}

pub fn is_lto_enabled(args: &[String]) -> bool {
    args.iter().any(|a| a == "-flto" || a.starts_with("-flto="))
}
