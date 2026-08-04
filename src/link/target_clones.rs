// src/link/target_clones.rs - __attribute__((target_clones(...))) IFUNC resolver
//
// `target_clones` 让函数生成多版本变体, 运行时通过 IFUNC resolver 选择最优实现.
//
// 编译器视角:
//   1. 对每个 `__attribute__((target_clones("sse4.2,default")))` 函数 `foo`,
//      生成变体 `foo.sse4.2` 和 `foo.default` (名字 mangle 不变, 但有属性)
//   2. 生成 resolver 函数 `foo` (STT_GNU_IFUNC), 运行时选择:
//
// 第一性原理: 静态可执行无 CPU feature 检测, 我们使用最简 resolver:
//   直接返回 default 变体地址. 这是 dcc 简化策略 (与 GCC -march=native 同).
//
// 参考 codegen: 在 frontend 把 target_clones 解析为 FuncDecl.target_clones.
// 后端在 codegen 时为每个变体生成带 `.ifunc` 标记的代码段.
// 链接阶段:
//   1. 收集所有 STT_GNU_IFUNC 符号
//   2. 对每个生成 R_X86_64_IRELATIVE 重定位
//   3. resolver 在 .text 中, 引用 .default 变体

use std::collections::HashMap;

use super::layout::{Layout, OutputRela, OutputSectionKind, OutputSymbol};
use crate::codegen::elf::{R_X86_64_IRELATIVE, STB_GLOBAL, STT_GNU_IFUNC, STV_DEFAULT};

/// 单个 target_clones 函数的所有变体
#[derive(Debug, Clone)]
pub struct TargetClonesFunc {
    /// resolver 名 (= 函数名)
    pub resolver_name: String,
    /// 变体名 (e.g. "foo.sse4.2", "foo.default")
    pub variants: Vec<TargetVariant>,
    /// default 变体名 (resolver 默认选择)
    pub default_variant: String,
}

#[derive(Debug, Clone)]
pub struct TargetVariant {
    /// 变体符号名 (e.g. "foo.sse4.2")
    pub name: String,
    /// 架构字符串 (e.g. "sse4.2", "default", "avx2")
    pub arch: String,
    /// 变体的 vaddr (link 后填回)
    pub vaddr: u64,
}

/// target_clones 收集表
#[derive(Debug, Default, Clone)]
pub struct TargetClonesTable {
    pub funcs: HashMap<String, TargetClonesFunc>,
}

impl TargetClonesTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解析 target_clones 字符串: "sse4.2,avx2,default"
    pub fn parse(&mut self, func_name: &str, attr: &str) {
        let variants: Vec<String> = attr
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if variants.is_empty() {
            return;
        }
        let mut vlist = Vec::new();
        for v in &variants {
            vlist.push(TargetVariant {
                name: format!("{}.{}", func_name, v),
                arch: v.clone(),
                vaddr: 0,
            });
        }
        // default 变体: 第一个或显式 "default"
        let default = variants
            .iter()
            .find(|s| *s == "default")
            .cloned()
            .unwrap_or_else(|| variants[0].clone());
        let default_variant = format!("{}.{}", func_name, default);
        self.funcs.insert(
            func_name.to_string(),
            TargetClonesFunc {
                resolver_name: func_name.to_string(),
                variants: vlist,
                default_variant,
            },
        );
    }

    /// 在 layout 中查找所有 target_clones resolver, 并生成 IRELATIVE 重定位.
    /// 返回 (新增的 STT_GNU_IFUNC 符号条目, 新增的 R_X86_64_IRELATIVE 重定位)
    pub fn gen_irelative_relas(&self, layout: &Layout) -> (Vec<OutputSymbol>, Vec<OutputRela>) {
        let mut new_syms = Vec::new();
        let mut relas = Vec::new();
        // 找 .rela.dyn 节
        let rel_dyn_idx = layout
            .sections
            .iter()
            .position(|s| s.kind == OutputSectionKind::RelDyn)
            .unwrap_or(0) as u32;
        for tcf in self.funcs.values() {
            // 找 default 变体的 vaddr
            let mut default_vaddr = 0u64;
            for v in &tcf.variants {
                if v.name == tcf.default_variant {
                    default_vaddr = v.vaddr;
                }
            }
            if default_vaddr == 0 {
                // 兜底: 用 resolver 名
                if let Some(s) = layout.symbols.iter().find(|s| s.name == tcf.resolver_name) {
                    default_vaddr = s.vaddr;
                }
            }
            // 添加 IFUNC 符号
            new_syms.push(OutputSymbol {
                name: tcf.resolver_name.clone(),
                value: 0,
                vaddr: default_vaddr,
                size: 0,
                bind: STB_GLOBAL,
                ty: STT_GNU_IFUNC,
                kind: STT_GNU_IFUNC,
                visibility: STV_DEFAULT,
                section: 0,
                shndx_if_section: 0,
            });
            // IRELATIVE 重定位: addend = resolver 地址
            relas.push(OutputRela::new(
                rel_dyn_idx,
                0, // offset: GOT 槽 (由 plt_got 填)
                0, // sym: 由 plt_got 填 (symidx)
                R_X86_64_IRELATIVE,
                default_vaddr as i64,
            ));
        }
        (new_syms, relas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_arch() {
        let mut t = TargetClonesTable::new();
        t.parse("memcpy", "sse4.2");
        let f = &t.funcs["memcpy"];
        assert_eq!(f.variants.len(), 1);
        assert_eq!(f.variants[0].arch, "sse4.2");
        assert_eq!(f.default_variant, "memcpy.sse4.2");
    }

    #[test]
    fn parse_multi_arch_default() {
        let mut t = TargetClonesTable::new();
        t.parse("foo", "sse4.2,avx2,default");
        let f = &t.funcs["foo"];
        assert_eq!(f.variants.len(), 3);
        assert_eq!(f.default_variant, "foo.default");
    }

    #[test]
    fn parse_uses_first_as_default() {
        let mut t = TargetClonesTable::new();
        t.parse("bar", "sse2,avx");
        let f = &t.funcs["bar"];
        assert_eq!(f.default_variant, "bar.sse2");
    }
}
