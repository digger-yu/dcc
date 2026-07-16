// src/lto.rs - 跨翻译单元 LTO 摘要容器
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

/// 从函数列表构建 TU 摘要。
pub fn build_unit(path: &str, functions: &[(&str, u64, bool)]) -> LtoUnit {
    let mut unit = LtoUnit { path: path.to_string(), ..Default::default() };
    for (name, size, defined) in functions {
        unit.functions.insert(
            (*name).to_string(),
            FunctionSummary {
                name: (*name).to_string(),
                defined: *defined,
                referenced: !defined,
                size_hint: *size,
            },
        );
    }
    unit
}

/// 解析/合并 LTO 摘要。
pub fn merge_units(mut units: Vec<LtoUnit>) -> LtoUnit {
    let mut merged = LtoUnit::default();
    for unit in units.drain(..) {
        for (name, summary) in unit.functions {
            merged.functions
                .entry(name)
                .and_modify(|existing| {
                    existing.defined |= summary.defined;
                    existing.referenced |= summary.referenced;
                    if summary.size_hint > existing.size_hint {
                        existing.size_hint = summary.size_hint;
                    }
                })
                .or_insert(summary);
        }
    }
    merged
}

pub fn is_lto_enabled(args: &[String]) -> bool {
    args.iter().any(|a| a == "-flto" || a.starts_with("-flto="))
}

/// 将 LTO 摘要序列化为链接器可消费的节载荷（文本 JSON 子集）。
pub fn serialize_summary(unit: &LtoUnit) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"DCC_LTO\0");
    out.extend_from_slice(unit.path.as_bytes());
    out.push(0);
    for (name, s) in &unit.functions {
        let line = format!("{}:{}:{}\n", name, s.size_hint, s.defined as u8);
        out.extend_from_slice(line.as_bytes());
    }
    out
}

/// 从 .llvm.lto / .dcc.lto 节载荷解析摘要（供链接器探测）。
pub fn parse_summary(data: &[u8]) -> Option<LtoUnit> {
    if !data.starts_with(b"DCC_LTO\0") {
        return None;
    }
    let rest = &data[8..];
    let nul = rest.iter().position(|&b| b == 0)?;
    let path = String::from_utf8_lossy(&rest[..nul]).to_string();
    let mut unit = LtoUnit { path, ..Default::default() };
    let body = std::str::from_utf8(&rest[nul + 1..]).ok()?;
    for line in body.lines() {
        if line.is_empty() { continue; }
        let mut parts = line.split(':');
        let name = parts.next()?.to_string();
        let size: u64 = parts.next()?.parse().ok()?;
        let defined: u8 = parts.next()?.parse().ok()?;
        unit.functions.insert(name.clone(), FunctionSummary {
            name,
            defined: defined != 0,
            referenced: defined == 0,
            size_hint: size,
        });
    }
    Some(unit)
}

/// 从单个 ET_REL 目标文件提取 `.llvm.lto` 摘要。
pub fn extract_from_object(elf: &[u8]) -> Option<LtoUnit> {
    let data = crate::codegen::elf::read_section_data(elf, ".llvm.lto")?;
    parse_summary(&data)
}

/// 合并多个目标文件的 LTO 摘要（链接器入口）。
pub fn collect_from_objects(objects: &[&[u8]]) -> LtoUnit {
    let units: Vec<LtoUnit> = objects.iter().filter_map(|obj| extract_from_object(obj)).collect();
    merge_units(units)
}

/// 在合并摘要上标记跨 TU 引用，返回可安全丢弃的本地定义符号。
pub fn dead_local_functions(merged: &LtoUnit) -> Vec<String> {
    merged.functions.values()
        .filter(|s| s.defined && !s.referenced && s.size_hint > 0)
        .map(|s| s.name.clone())
        .collect()
}

/// 根据输入目标文件的重定位表，将引用的符号标记为 referenced。
pub fn mark_references_from_objects(merged: &mut LtoUnit, objects: &[&[u8]]) {
    for obj in objects {
        for name in crate::codegen::elf::referenced_symbol_names(obj) {
            if let Some(summary) = merged.functions.get_mut(&name) {
                summary.referenced = true;
            } else {
                merged.functions.insert(name.clone(), FunctionSummary {
                    name: name.clone(),
                    defined: false,
                    referenced: true,
                    size_hint: 0,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_and_parse_roundtrip() {
        let unit = build_unit("foo.c", &[("bar", 64, true), ("baz", 0, false)]);
        let data = serialize_summary(&unit);
        let parsed = parse_summary(&data).expect("parse LTO summary");
        assert_eq!(parsed.path, "foo.c");
        assert!(parsed.functions["bar"].defined);
        assert!(!parsed.functions["baz"].defined);
    }

    #[test]
    fn merge_units_unions_functions() {
        let a = build_unit("a.c", &[("f", 10, true)]);
        let b = build_unit("b.c", &[("g", 20, true)]);
        let merged = merge_units(vec![a, b]);
        assert_eq!(merged.functions.len(), 2);
        assert!(merged.functions.contains_key("f"));
        assert!(merged.functions.contains_key("g"));
    }

    #[test]
    fn extract_from_elf_section() {
        use crate::codegen::elf::{self, ElfSection, SHT_PROGBITS};
        let unit = build_unit("unit.c", &[("entry", 48, true)]);
        let payload = serialize_summary(&unit);
        let obj = elf::build_elf_rel_x86_64(
            &[0xc3], &[], &[], 0, 1, 1, 1, &[], &[],
            &[ElfSection {
                name: ".llvm.lto".to_string(),
                data: payload,
                ty: SHT_PROGBITS,
                flags: 0,
                align: 1,
            }],
            &[], &[],
        );
        let parsed = extract_from_object(&obj).expect("extract LTO from ELF");
        assert_eq!(parsed.path, "unit.c");
        assert_eq!(parsed.functions["entry"].size_hint, 48);
    }

    #[test]
    fn dead_local_excludes_referenced() {
        let mut merged = build_unit("a.c", &[("used", 10, true), ("unused", 20, true)]);
        merged.functions.get_mut("used").unwrap().referenced = true;
        let dead = dead_local_functions(&merged);
        assert_eq!(dead, vec!["unused".to_string()]);
    }
}
