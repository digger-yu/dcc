// src/link/ifunc.rs - IFUNC (间接函数) 解析
//
// IFUNC 是 STT_GNU_IFUNC 类型的符号, 实际值在运行时由解析器 (resolver) 决定.
// 编译期:  引用 IFUNC 符号的代码使用一个 IRELATIVE 重定位, 指向 resolver 地址
// 链接期:  把 IFUNC 符号本身放入 .got (Irel_relatif), 记录 resolver 在 .rela.dyn
// 加载期:  动态链接器 (ld-linux) 调用 resolver, 把返回值写入 .got
//          之后所有引用此符号的代码通过 PLT/GOT 跳转
//
// dcc 实现简化方案 (静态可执行):
//   1. 链接期记录所有 STT_GNU_IFUNC 符号
//   2. 为每个 IFUNC 在 .rela.dyn 写一条 R_X86_64_IRELATIVE 重定位
//   3. 共享库加载时, 动态链接器执行 resolver 把最终函数地址写入 GOT 项
//   4. 引用方按普通函数调用处理 (PLT/GOT), 在静态可执行下会直接走 resolver
//
// 静态可执行模式下, dcc 不执行 resolver, 只记录解析关系。

#![allow(dead_code)]

use std::collections::HashMap;

use super::layout::{Layout, OutputRela};
use crate::codegen::shared::elf::R_X86_64_IRELATIVE;

/// IFUNC 解析表：符号名 -> 解析器地址 (vaddr)
#[derive(Debug, Default, Clone)]
pub struct IfuncTable {
    /// 符号名 -> resolver 的输出 vaddr
    pub resolvers: HashMap<String, u64>,
    /// 符号名 -> IFUNC 自身 GLOB_DAT 重定位 (用于 .rela.dyn)
    pub glob_dats: Vec<(String, u64)>, // (name, offset in .got)
}

impl IfuncTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册 IFUNC：name 是 IFUNC 符号名，resolver_vaddr 是 resolver 函数地址
    pub fn register(&mut self, name: &str, resolver_vaddr: u64) {
        self.resolvers.insert(name.to_string(), resolver_vaddr);
    }

    /// 检查符号是否被注册为 IFUNC
    pub fn is_ifunc(&self, name: &str) -> bool {
        self.resolvers.contains_key(name)
    }
}

/// 从 Layout 中生成 IFUNC 解析表
/// 扫描所有 STT_GNU_IFUNC 类型 (kind=10) 符号
pub fn collect_ifuncs(layout: &Layout) -> IfuncTable {
    let mut table = IfuncTable::new();
    for sym in &layout.symbols {
        if sym.kind == 10
        /* STT_GNU_IFUNC */
        {
            // 找到 resolver (与 sym 同名, kind = STT_FUNC)
            // 简化: 用 sym 的 vaddr 作为 resolver
            let vaddr = sym.vaddr;
            table.register(&sym.name, vaddr);
        }
    }
    table
}

/// 从 .dcc.ifunc 节二进制载荷解析 ifunc 记录.
/// 格式: 每条记录 = u16 name_len + name bytes + u16 resolver_len + resolver bytes
/// 返回 Vec<(ifunc_name, resolver_name)>
pub fn parse_dcc_ifunc_section(data: &[u8]) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 2 <= data.len() {
        let name_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + name_len > data.len() {
            break;
        }
        let name = std::str::from_utf8(&data[pos..pos + name_len])
            .unwrap_or("")
            .to_string();
        pos += name_len;
        if pos + 2 > data.len() {
            break;
        }
        let res_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + res_len > data.len() {
            break;
        }
        let resolver = std::str::from_utf8(&data[pos..pos + res_len])
            .unwrap_or("")
            .to_string();
        pos += res_len;
        if !name.is_empty() && !resolver.is_empty() {
            entries.push((name, resolver));
        }
    }
    entries
}

/// 从 InputBundle 解析 .dcc.ifunc 节获取 (ifunc_name, resolver_name) 映射,
/// 再从 Layout 查找 resolver 的最终 vaddr, 注册到 IfuncTable.
/// 返回 IfuncTable 和新增的 IRELATIVE 重定位.
pub fn collect_from_bundle(
    bundle: &super::InputBundle,
    layout: &super::layout::Layout,
) -> (IfuncTable, Vec<super::layout::OutputRela>) {
    let mut table = IfuncTable::new();
    // 1. 从 bundle 的 .dcc.ifunc 节解析 (ifunc_name, resolver_name) 对
    let mut mappings: Vec<(String, String)> = Vec::new();
    for obj in &bundle.objects {
        for sec in &obj.sections {
            if sec.name == ".dcc.ifunc" {
                mappings.extend(parse_dcc_ifunc_section(&sec.data));
            }
        }
    }
    // 2. 对每个映射, 在 layout.symbols 查找 resolver 函数的 vaddr
    for (ifunc_name, resolver_name) in &mappings {
        let resolver_vaddr = layout
            .symbols
            .iter()
            .find(|s| s.name == *resolver_name)
            .map(|s| s.vaddr);
        if let Some(vaddr) = resolver_vaddr {
            table.register(ifunc_name, vaddr);
        }
    }
    let relas = gen_irelative_relas(&table);
    (table, relas)
}

/// 为 IFUNC 生成 .rela.dyn 记录 (R_X86_64_IRELATIVE)
/// 动态链接器会在加载时调用 resolver 并把返回值写入对应 GOT 项
pub fn gen_irelative_relas(table: &IfuncTable) -> Vec<OutputRela> {
    let mut out = Vec::new();
    for (name, &resolver_vaddr) in &table.resolvers {
        out.push(OutputRela::new(0, 0, 0, R_X86_64_IRELATIVE, resolver_vaddr as i64));
        if let Some(last) = out.last_mut() {
            last._name = format!("__ifunc_reloc_{}", name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let mut t = IfuncTable::new();
        t.register("memcpy_ifunc", 0x1000);
        assert!(t.is_ifunc("memcpy_ifunc"));
        assert!(!t.is_ifunc("memcpy"));
        assert_eq!(t.resolvers.get("memcpy_ifunc"), Some(&0x1000));
    }

    #[test]
    fn test_irelative_relas_count() {
        let mut t = IfuncTable::new();
        t.register("a", 0x1000);
        t.register("b", 0x2000);
        let relas = gen_irelative_relas(&t);
        assert_eq!(relas.len(), 2);
        for r in &relas {
            assert_eq!(r.rel_type, R_X86_64_IRELATIVE);
        }
    }

    /// 编码辅助: 按 .dcc.ifunc 节格式序列化 (name, resolver) 对
    fn encode_ifunc_section(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut data = Vec::new();
        for (name, resolver) in entries {
            let n = name.as_bytes();
            let r = resolver.as_bytes();
            data.extend_from_slice(&(n.len() as u16).to_le_bytes());
            data.extend_from_slice(n);
            data.extend_from_slice(&(r.len() as u16).to_le_bytes());
            data.extend_from_slice(r);
        }
        data
    }

    #[test]
    fn test_parse_dcc_ifunc_section_roundtrip() {
        let entries = vec![("memcpy", "resolve_memcpy"), ("strlen", "resolve_strlen")];
        let data = encode_ifunc_section(&entries);
        let parsed = parse_dcc_ifunc_section(&data);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("memcpy".to_string(), "resolve_memcpy".to_string()));
        assert_eq!(parsed[1], ("strlen".to_string(), "resolve_strlen".to_string()));
    }

    #[test]
    fn test_parse_dcc_ifunc_section_empty() {
        let parsed = parse_dcc_ifunc_section(&[]);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_dcc_ifunc_section_truncated() {
        // name_len 声明 100 字节但数据不足, 应安全截断
        let data = [100u8, 0, b'a', b'b'];
        let parsed = parse_dcc_ifunc_section(&data);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_dcc_ifunc_section_single() {
        let data = encode_ifunc_section(&[("foo", "bar")]);
        let parsed = parse_dcc_ifunc_section(&data);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], ("foo".to_string(), "bar".to_string()));
    }
}
