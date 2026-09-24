// src/link/target_clones.rs - __attribute__((target_clones(...))) IFUNC resolver
//
// `target_clones` 让函数生成多版本变体, 运行时通过 IFUNC resolver 选择最优实现.
//
// 编译器视角:
//   1. 对每个 `__attribute__((target_clones("sse4.2,default")))` 函数 `foo`,
//      生成变体 `foo.sse4.2` 和 `foo.default` (名字 mangle 不变, 但有属性)
//   2. 生成 resolver 函数 `foo` (STT_GNU_IFUNC), 运行时选择:
//
// 链接期实现:
//   对每个 target_clones 函数, 在 `.text.ifunc` 节生成 x86_64 机器码 resolver.
//   resolver 使用 CPUID 指令检测 CPU 特性, 按优先级从高到低选择最优变体.
//   IRELATIVE 重定位的 addend 指向 resolver 函数地址 (而非 default 变体).
//   动态链接器 (或静态 loader) 调用 resolver, 把返回值写入 GOT 项.
//
// 参考 codegen: 在 frontend 把 target_clones 解析为 FuncDecl.target_clones.
// 后端在 codegen 时为每个变体生成带 `.ifunc` 标记的代码段.

use std::collections::HashMap;

use super::layout::{Layout, OutputRela, OutputSectionKind, OutputSymbol};
use crate::codegen::shared::elf::{R_X86_64_IRELATIVE, STB_GLOBAL, STT_GNU_IFUNC, STV_DEFAULT};

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

// ===== CPUID feature mapping =====

/// CPUID 检测所需的寄存器位.
struct CpuidBit {
    /// CPUID leaf (EAX input)
    leaf: u32,
    /// 输出寄存器: 0=EAX, 1=EBX, 2=ECX, 3=EDX
    reg: u8,
    /// 位索引
    bit: u8,
}

/// 将 GCC target 架构字符串映射到所需 CPUID 位.
///
/// 支持: "sse4.2", "avx", "avx2", "avx512f", "default".
/// 未知架构返回空列表 (视为纯 default).
fn arch_to_cpuid_bits(arch: &str) -> Vec<CpuidBit> {
    match arch {
        "sse4.2" => vec![CpuidBit {
            leaf: 1,
            reg: 2,
            bit: 20,
        }],
        "avx" => vec![CpuidBit {
            leaf: 1,
            reg: 2,
            bit: 28,
        }],
        "avx2" => vec![
            CpuidBit {
                leaf: 1,
                reg: 2,
                bit: 28,
            }, // AVX
            CpuidBit {
                leaf: 7,
                reg: 1,
                bit: 5,
            }, // AVX2
        ],
        "avx512f" => vec![
            CpuidBit {
                leaf: 1,
                reg: 2,
                bit: 28,
            }, // AVX
            CpuidBit {
                leaf: 7,
                reg: 1,
                bit: 16,
            }, // AVX512F
        ],
        _ => vec![], // "default" or unknown
    }
}

/// 架构是否需要 XCR0 (xgetbv) 检测 OS 已启用扩展寄存器.
/// AVX/AVX2/AVX512 需要; SSE4.2 不需要 (XMM 始终可用).
fn arch_needs_xsave(arch: &str) -> bool {
    matches!(arch, "avx" | "avx2" | "avx512f")
}

// ===== x86_64 code emission helpers =====

/// 发射 CPUID 指令 (如果当前 leaf 与要求不同).
fn emit_cpuid_leaf(code: &mut Vec<u8>, current_leaf: &mut Option<u32>, leaf: u32) {
    if *current_leaf == Some(leaf) {
        return;
    }
    // mov eax, leaf
    code.push(0xB8);
    code.extend_from_slice(&leaf.to_le_bytes());
    // xor ecx, ecx (sub-leaf = 0)
    code.extend_from_slice(&[0x31, 0xC9]);
    // cpuid
    code.extend_from_slice(&[0x0F, 0xA2]);
    *current_leaf = Some(leaf);
}

/// 发射 TEST reg, reg 指令 (32-bit).
fn emit_test_reg_reg(code: &mut Vec<u8>, reg: u8) {
    // test reg, reg: 0x85 ModRM
    let modrm = match reg {
        0 => 0xC0, // EAX
        1 => 0xC3, // EBX
        2 => 0xC1, // ECX
        3 => 0xC2, // EDX
        _ => 0xC0,
    };
    code.push(0x85);
    code.push(modrm);
}

/// 发射 TEST reg, imm32 指令.
fn emit_test_reg_imm32(code: &mut Vec<u8>, reg: u8, imm: u32) {
    let modrm = match reg {
        0 => 0xC0,
        1 => 0xC3,
        2 => 0xC1,
        3 => 0xC2,
        _ => 0xC0,
    };
    code.push(0xF7);
    code.push(modrm);
    code.extend_from_slice(&imm.to_le_bytes());
}

/// 发射 JE rel32 (jump if zero flag set, i.e. test result == 0).
/// 返回 jmp offset 字段的偏移 (用于后续 patch).
fn emit_je_rel32(code: &mut Vec<u8>) -> usize {
    code.push(0x0F);
    code.push(0x84);
    let pos = code.len();
    code.extend_from_slice(&[0u8; 4]); // placeholder
    pos
}

/// 发射 JNE rel32 (jump if zero flag clear).
/// 返回 jmp offset 字段的偏移.
fn emit_jne_rel32(code: &mut Vec<u8>) -> usize {
    code.push(0x0F);
    code.push(0x85);
    let pos = code.len();
    code.extend_from_slice(&[0u8; 4]); // placeholder
    pos
}

/// 发射 MOV RAX, imm64 指令.
/// 返回 imm64 字段的偏移 (用于后续 patch).
fn emit_mov_rax_imm64(code: &mut Vec<u8>) -> usize {
    code.push(0x48); // REX.W
    code.push(0xB8);
    let pos = code.len();
    code.extend_from_slice(&[0u8; 8]); // placeholder
    pos
}

/// 在 code 的 jmp_pos 处写入相对偏移, 目标为 target_pos.
/// 相对偏移 = target_pos - (jmp_pos + 4), 因为 RIP 指向 jmp 指令之后.
fn patch_jmp(code: &mut [u8], jmp_pos: usize, target_pos: usize) {
    let offset = target_pos as i32 - (jmp_pos as i32 + 4);
    code[jmp_pos..jmp_pos + 4].copy_from_slice(&offset.to_le_bytes());
}

// ===== Resolver code generation =====

/// 为单个 target_clones 函数生成 x86_64 resolver 机器码.
///
/// 生成策略:
///   1. 按所需特性数量降序排列非 default 变体 (最多特性 = 最高优先级)
///   2. 对每个变体: 检测所有所需 CPUID 位, 任一失败则跳到下一个变体
///   3. 所有检测通过: MOV RAX, variant_vaddr; RET
///   4. 所有变体都不匹配: MOV RAX, default_vaddr; RET
///
/// 返回: (code, Vec<(variant_index, patch_offset)>)
/// patch_offset 指向 MOV RAX, imm64 中 imm64 的位置.
pub fn generate_resolver_code(tcf: &TargetClonesFunc) -> (Vec<u8>, Vec<(usize, usize)>) {
    let mut code = Vec::with_capacity(128);
    let mut patches: Vec<(usize, usize)> = Vec::new();

    // 收集非 default 变体, 按特性数量降序排列
    let mut dispatch_variants: Vec<(usize, &TargetVariant)> = tcf
        .variants
        .iter()
        .filter(|v| v.arch != "default" && v.name != tcf.default_variant)
        .enumerate()
        .collect();
    dispatch_variants.sort_by(|a, b| {
        let na = arch_to_cpuid_bits(&b.1.arch).len();
        let nb = arch_to_cpuid_bits(&a.1.arch).len();
        na.cmp(&nb)
    });

    let default_idx = tcf
        .variants
        .iter()
        .position(|v| v.name == tcf.default_variant)
        .unwrap_or(0);

    let mut current_leaf: Option<u32> = None;

    for (variant_idx, variant) in &dispatch_variants {
        let bits = arch_to_cpuid_bits(&variant.arch);
        if bits.is_empty() {
            continue;
        }

        // 收集本变体的失败跳转位置 (需要 patch 到 "skip" 位置)
        let mut fail_jmps: Vec<usize> = Vec::new();

        // XSAVE/OSXSAVE 检测 (AVX/AVX2/AVX512 需要)
        if arch_needs_xsave(&variant.arch) {
            // 检查 OSXSAVE: CPUID leaf 1, ECX bit 27
            emit_cpuid_leaf(&mut code, &mut current_leaf, 1);
            // test ecx, (1 << 27)
            emit_test_reg_imm32(&mut code, 2, 1u32 << 27);
            // jne .skip_variant (OSXSAVE not set)
            fail_jmps.push(emit_jne_rel32(&mut code));

            // xgetbv(ecx=0) 检查 XCR0
            code.extend_from_slice(&[0x0F, 0x01, 0xD0]); // xgetbv
                                                         // and eax, 0x6 (XCR0[2:1])
            code.extend_from_slice(&[0x83, 0xE0, 0x06]);
            // cmp eax, 0x6
            code.extend_from_slice(&[0x83, 0xF8, 0x06]);
            // jne .skip_variant (OS hasn't enabled AVX state)
            fail_jmps.push(emit_jne_rel32(&mut code));

            // 检测剩余 CPUID 位 (跳过已在上面检测的 leaf 1 ECX bit 28)
            let remaining_bits: Vec<&CpuidBit> = bits
                .iter()
                .filter(|b| !(b.leaf == 1 && b.reg == 2 && b.bit == 28))
                .collect();

            for cpuid_bit in &remaining_bits {
                emit_cpuid_leaf(&mut code, &mut current_leaf, cpuid_bit.leaf);
                // test reg, (1 << bit)
                emit_test_reg_imm32(&mut code, cpuid_bit.reg, 1u32 << cpuid_bit.bit);
                // je .skip_variant (feature not present)
                fail_jmps.push(emit_je_rel32(&mut code));
            }
        } else {
            // 非 XSAVE 变体 (e.g. SSE4.2): 直接 CPUID 检测
            for cpuid_bit in &bits {
                emit_cpuid_leaf(&mut code, &mut current_leaf, cpuid_bit.leaf);
                // test reg, (1 << bit)
                emit_test_reg_imm32(&mut code, cpuid_bit.reg, 1u32 << cpuid_bit.bit);
                // je .skip_variant (feature not present)
                fail_jmps.push(emit_je_rel32(&mut code));
            }
        }

        // 所有检测通过: 返回此变体
        let patch_off = emit_mov_rax_imm64(&mut code);
        patches.push((*variant_idx, patch_off));
        code.push(0xC3); // ret

        // "skip" 位置: patch 所有失败跳转到这里
        let skip_target = code.len();
        for jmp_pos in &fail_jmps {
            patch_jmp(&mut code, *jmp_pos, skip_target);
        }
    }

    // Fallback: 返回 default 变体地址
    let patch_off = emit_mov_rax_imm64(&mut code);
    patches.push((default_idx, patch_off));
    code.push(0xC3); // ret

    (code, patches)
}

/// 为所有 target_clones 函数生成 resolver 代码 blob.
///
/// 返回: (combined_code, Vec<(resolver_name, offset, size)>)
pub fn generate_resolver_blob(table: &TargetClonesTable) -> (Vec<u8>, Vec<(String, usize, usize)>) {
    let mut blob = Vec::new();
    let mut entries = Vec::new();

    // 按名称排序确保确定性
    let mut sorted_keys: Vec<&String> = table.funcs.keys().collect();
    sorted_keys.sort();

    for key in sorted_keys {
        let tcf = &table.funcs[key];
        let offset = blob.len();
        let (code, _patches) = generate_resolver_code(tcf);
        let size = code.len();
        blob.extend_from_slice(&code);
        // 对齐到 16 字节 (INT3 padding)
        let align = (16 - (blob.len() % 16)) % 16;
        blob.extend(std::iter::repeat_n(0xCC, align));
        entries.push((tcf.resolver_name.clone(), offset, size));
    }

    (blob, entries)
}

/// 回填所有 resolver 代码中的变体 vaddr 占位符.
pub fn patch_all_resolvers(
    blob: &mut [u8],
    table: &TargetClonesTable,
    resolver_base_vaddr: u64,
    entries: &[(String, usize, usize)],
) {
    for (resolver_name, offset, _size) in entries {
        if let Some(tcf) = table.funcs.get(resolver_name) {
            let resolver_vaddr = resolver_base_vaddr + *offset as u64;
            let code_slice = &mut blob[*offset..];
            // 重新生成 patch positions (与 generate_resolver_code 同逻辑)
            let (_code, patches) = generate_resolver_code(tcf);
            for (variant_idx, patch_off) in &patches {
                if *variant_idx < tcf.variants.len() {
                    let vaddr = tcf.variants[*variant_idx].vaddr;
                    code_slice[*patch_off..*patch_off + 8].copy_from_slice(&vaddr.to_le_bytes());
                }
            }
            let _ = resolver_vaddr;
        }
    }
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

    /// 生成 IRELATIVE 重定位和 IFUNC 符号.
    ///
    /// `resolver_vaddrs`: resolver 函数名 -> resolver 在 .text.ifunc 中的 vaddr
    /// 如果某函数不在 resolver_vaddrs 中 (如仅 default 变体), 使用 default 变体地址.
    pub fn gen_irelative_relas(
        &self,
        layout: &Layout,
        resolver_vaddrs: &HashMap<String, u64>,
    ) -> (Vec<OutputSymbol>, Vec<OutputRela>) {
        let mut new_syms = Vec::new();
        let mut relas = Vec::new();
        // 找 .rela.dyn 节
        let rel_dyn_idx = layout
            .sections
            .iter()
            .position(|s| s.kind == OutputSectionKind::RelDyn)
            .unwrap_or(0) as u32;

        // 按名称排序确保确定性
        let mut sorted_keys: Vec<&String> = self.funcs.keys().collect();
        sorted_keys.sort();

        for key in sorted_keys {
            let tcf = &self.funcs[key];
            // 找 default 变体的 vaddr
            let mut default_vaddr = 0u64;
            for v in &tcf.variants {
                if v.name == tcf.default_variant {
                    default_vaddr = v.vaddr;
                }
            }
            if default_vaddr == 0 {
                if let Some(s) = layout.symbols.iter().find(|s| s.name == tcf.resolver_name) {
                    default_vaddr = s.vaddr;
                }
            }
            // 使用 CPUID resolver 地址 (如果有), 否则回退到 default 变体
            let resolver_vaddr = resolver_vaddrs
                .get(&tcf.resolver_name)
                .copied()
                .unwrap_or(default_vaddr);

            // 添加 IFUNC 符号
            new_syms.push(OutputSymbol {
                name: tcf.resolver_name.clone(),
                value: resolver_vaddr,
                vaddr: resolver_vaddr,
                size: 0,
                bind: STB_GLOBAL,
                ty: STT_GNU_IFUNC,
                kind: STT_GNU_IFUNC,
                visibility: STV_DEFAULT,
                section: 0,
                shndx_if_section: 0,
            });
            // IRELATIVE 重定位: addend = resolver 函数地址
            relas.push(OutputRela::new(
                rel_dyn_idx,
                0, // offset: GOT 槽 (由 plt_got 填)
                0, // sym: 由 plt_got 填 (symidx)
                R_X86_64_IRELATIVE,
                resolver_vaddr as i64,
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

    #[test]
    fn arch_to_features_sse42() {
        let bits = arch_to_cpuid_bits("sse4.2");
        assert_eq!(bits.len(), 1);
        assert_eq!(bits[0].leaf, 1);
        assert_eq!(bits[0].reg, 2); // ECX
        assert_eq!(bits[0].bit, 20);
    }

    #[test]
    fn arch_to_features_avx2() {
        let bits = arch_to_cpuid_bits("avx2");
        assert_eq!(bits.len(), 2);
        assert_eq!(bits[0].leaf, 1);
        assert_eq!(bits[0].bit, 28); // AVX
        assert_eq!(bits[1].leaf, 7);
        assert_eq!(bits[1].bit, 5); // AVX2
    }

    #[test]
    fn arch_to_features_default_empty() {
        let bits = arch_to_cpuid_bits("default");
        assert!(bits.is_empty());
    }

    #[test]
    fn arch_to_features_unknown_empty() {
        let bits = arch_to_cpuid_bits("bmi2");
        assert!(bits.is_empty());
    }

    #[test]
    fn xsave_requirement() {
        assert!(!arch_needs_xsave("sse4.2"));
        assert!(arch_needs_xsave("avx"));
        assert!(arch_needs_xsave("avx2"));
        assert!(arch_needs_xsave("avx512f"));
        assert!(!arch_needs_xsave("default"));
    }

    #[test]
    fn generate_resolver_default_only() {
        let tcf = TargetClonesFunc {
            resolver_name: "foo".into(),
            variants: vec![TargetVariant {
                name: "foo.default".into(),
                arch: "default".into(),
                vaddr: 0x1000,
            }],
            default_variant: "foo.default".into(),
        };
        let (code, patches) = generate_resolver_code(&tcf);
        // Should be: MOV RAX, 0x1000; RET = 10 + 1 = 11 bytes
        assert_eq!(code.len(), 11);
        assert_eq!(patches.len(), 1);
        // Check REX.W + MOV RAX prefix
        assert_eq!(code[0], 0x48);
        assert_eq!(code[1], 0xB8);
        // Check vaddr in immediate (before patching it's 0)
        let vaddr_before = u64::from_le_bytes(code[2..10].try_into().unwrap());
        assert_eq!(vaddr_before, 0); // not yet patched
                                     // Check RET
        assert_eq!(code[10], 0xC3);
    }

    #[test]
    fn generate_resolver_sse42_and_default() {
        let tcf = TargetClonesFunc {
            resolver_name: "foo".into(),
            variants: vec![
                TargetVariant {
                    name: "foo.sse4.2".into(),
                    arch: "sse4.2".into(),
                    vaddr: 0x2000,
                },
                TargetVariant {
                    name: "foo.default".into(),
                    arch: "default".into(),
                    vaddr: 0x1000,
                },
            ],
            default_variant: "foo.default".into(),
        };
        let (code, patches) = generate_resolver_code(&tcf);
        // Should have 2 patches: sse4.2 variant + default fallback
        assert_eq!(patches.len(), 2);
        // Should contain CPUID instructions
        assert!(code.windows(2).any(|w| w == [0x0F, 0xA2]));
        // Should end with RET
        assert_eq!(*code.last().unwrap(), 0xC3);
        // Code should be non-trivial (> just MOV + RET)
        assert!(code.len() > 11);
    }

    #[test]
    fn generate_resolver_blob_deterministic() {
        let mut table = TargetClonesTable::new();
        table.parse("foo", "sse4.2,default");
        table.parse("bar", "avx2,default");

        let (blob1, entries1) = generate_resolver_blob(&table);
        let (blob2, entries2) = generate_resolver_blob(&table);
        assert_eq!(blob1, blob2);
        assert_eq!(entries1.len(), entries2.len());
    }

    #[test]
    fn resolver_code_contains_cpuid_for_avx2() {
        let tcf = TargetClonesFunc {
            resolver_name: "fn1".into(),
            variants: vec![
                TargetVariant {
                    name: "fn1.avx2".into(),
                    arch: "avx2".into(),
                    vaddr: 0x3000,
                },
                TargetVariant {
                    name: "fn1.default".into(),
                    arch: "default".into(),
                    vaddr: 0x1000,
                },
            ],
            default_variant: "fn1.default".into(),
        };
        let (code, _patches) = generate_resolver_code(&tcf);
        // Should have at least 2 CPUID instructions (leaf 1 + leaf 7)
        let cpuid_count = code.windows(2).filter(|w| *w == [0x0F, 0xA2]).count();
        assert!(cpuid_count >= 2, "expected >= 2 CPUID, got {}", cpuid_count);
        // Should have XGETBV for AVX OS check
        let xgetbv_count = code.windows(3).filter(|w| *w == [0x0F, 0x01, 0xD0]).count();
        assert!(xgetbv_count >= 1, "expected XGETBV");
    }

    #[test]
    fn resolver_patch_vaddrs() {
        let tcf = TargetClonesFunc {
            resolver_name: "foo".into(),
            variants: vec![
                TargetVariant {
                    name: "foo.sse4.2".into(),
                    arch: "sse4.2".into(),
                    vaddr: 0xAAAA_BBBB_CCCC_DDDD,
                },
                TargetVariant {
                    name: "foo.default".into(),
                    arch: "default".into(),
                    vaddr: 0x1111_2222_3333_4444,
                },
            ],
            default_variant: "foo.default".into(),
        };
        let (mut code, patches) = generate_resolver_code(&tcf);
        // Patch vaddrs
        for (variant_idx, patch_off) in &patches {
            let vaddr = tcf.variants[*variant_idx].vaddr;
            code[*patch_off..*patch_off + 8].copy_from_slice(&vaddr.to_le_bytes());
        }
        // Verify sse4.2 vaddr is in the code
        let sse42_bytes = 0xAAAA_BBBB_CCCC_DDDD_u64.to_le_bytes();
        assert!(
            code.windows(8).any(|w| w == sse42_bytes),
            "sse4.2 vaddr not found in resolver code"
        );
        // Verify default vaddr is in the code
        let default_bytes = 0x1111_2222_3333_4444_u64.to_le_bytes();
        assert!(
            code.windows(8).any(|w| w == default_bytes),
            "default vaddr not found in resolver code"
        );
    }
}
