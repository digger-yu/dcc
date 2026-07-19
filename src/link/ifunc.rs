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

use super::layout::{OutputRela, Layout};
use crate::codegen::elf::R_X86_64_IRELATIVE;

/// IFUNC 解析表：符号名 -> 解析器地址 (vaddr)
#[derive(Debug, Default, Clone)]
pub struct IfuncTable {
    /// 符号名 -> resolver 的输出 vaddr
    pub resolvers: HashMap<String, u64>,
    /// 符号名 -> IFUNC 自身 GLOB_DAT 重定位 (用于 .rela.dyn)
    pub glob_dats: Vec<(String, u64)>,  // (name, offset in .got)
}

impl IfuncTable {
    pub fn new() -> Self { Self::default() }

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
        if sym.kind == 10 /* STT_GNU_IFUNC */ {
            // 找到 resolver (与 sym 同名, kind = STT_FUNC)
            // 简化: 用 sym 的 vaddr 作为 resolver
            let vaddr = sym.vaddr;
            table.register(&sym.name, vaddr);
        }
    }
    table
}

/// 为 IFUNC 生成 .rela.dyn 记录 (R_X86_64_IRELATIVE)
/// 动态链接器会在加载时调用 resolver 并把返回值写入对应 GOT 项
pub fn gen_irelative_relas(table: &IfuncTable) -> Vec<OutputRela> {
    let mut out = Vec::new();
    for (name, &resolver_vaddr) in &table.resolvers {
        out.push(OutputRela::new(
            0,
            0,
            0,
            R_X86_64_IRELATIVE,
            resolver_vaddr as i64,
        ));
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
}
