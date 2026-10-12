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
//
// ## 本模块与 codegen 侧 resolver 的分工（为什么两条路径都要留着）
//
// `src/codegen/x86_64/ifunc.rs` 现在会**在 `.o` 里合成真正的 resolver 体**
// （IFUNC 符号的 `st_value` 指向它，CPUID 检测走一个同 `.o` 自包含的
// `__dcc_cpu_indicator_init`），因此宿主 `cc`/`ld` 能原生处理 target_clones 产物，
// 走宿主链接出动态可执行文件。本模块的「链接器现场拼 blob」路径**并不因此作废**，
// 理由是它服务的输入形状不同：
//
// 1. **输入里根本没有 resolver 体**。`collect_link_directives` 会从符号表反推
//    target_clones 函数（含 `.dcc.ifunc` 显式指令那条路径、以及外部提供的、
//    只带变体符号不带 resolver 的 `.o`）。这种输入没有「对象内的 init 函数」可调，
//    只能在链接期凭变体列表现拼一个。
// 2. **它必须发绝对地址而不是重定位**。链接期拼出的 resolver 落在 `.text.ifunc`，
//    其 `MOV RAX, imm64` 里的立即量是**最终镜像 vaddr**（`patch_all_resolvers` 回填），
//    因为这条路径产出的是内置链接器的静态镜像，没有 `.rela.text` 可依赖；
//    codegen 侧的 resolver 用的是 `R_X86_64_PC32` 重定位，交给宿主 `ld` 解析。
//    两种定位方式不能互换，所以两段体各自发射。
// 3. **规格只有一份**：leaf/bit、XCR0 需求、变体优先级全部来自
//    `crate::semantics::cpu_isa`（见该模块头注释）。本模块**不再自带位表** ——
//    否则「内置链接器选的变体」与「宿主链接器选的变体」会因两张表漂移而不同，
//    那正是本项目禁止的分阶段缺口。

use std::collections::HashMap;

use super::layout::{Layout, OutputRela, OutputSectionKind, OutputSymbol};
use crate::codegen::shared::elf::{R_X86_64_IRELATIVE, STB_GLOBAL, STT_GNU_IFUNC, STV_DEFAULT};
use crate::semantics::cpu_isa::{self, CpuidBit};

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
//
// 位表本身在 `crate::semantics::cpu_isa`（单一真源）。这里只保留两个薄适配器，
// 让既有调用点与单元测试继续可用；它们**不得**再各自列 leaf/bit 常量。

/// 将 GCC target 架构字符串映射到所需 CPUID 位（转发自 `cpu_isa`）。
///
/// "default" 与未知架构返回空列表（视为纯 default）；未知名字在
/// `cpu_isa::lookup` 里为 `None`，调用方若需区分「default」与「拼错的 ISA 名」
/// 必须自己查 `cpu_isa::lookup`，不得静默按 default 处理。
fn arch_to_cpuid_bits(arch: &str) -> Vec<CpuidBit> {
    cpu_isa::cpuid_bits(arch).to_vec()
}

/// 架构是否需要 XCR0（xgetbv）门控；真值即所需 XCR0 低 32 位掩码。
///
/// 不再是「硬编码三个名字」：`avx512f` 需要 0xE6（SSE+YMM+opmask+ZMM_hi16），
/// `avx`/`avx2`/`fma` 需要 0x6，SSE 系为 0（XMM 始终可用）。
/// 本函数是全模块唯一的 XSAVE 判据入口（旧版另有一份 `matches!(arch, "avx" | ...)`
/// 硬编码名单，会让 `fma`/`avx512vl` 这类名字漏掉 XCR0 门控）。
fn arch_xcr0_mask(arch: &str) -> u32 {
    cpu_isa::xcr0_mask(arch)
}

// ===== x86_64 code emission helpers =====

/// 发射 CPUID 指令 (如果当前 (leaf, sub-leaf) 与要求不同).
///
/// 缓存键是**二元组**而不是只看 leaf：`CpuidBit` 自带 `subleaf`（来自
/// `cpu_isa`，单一真源），只比 leaf 会把「(7,0) 已查过」当成「(7,1) 也查过」，
/// 于是拿子叶 0 的寄存器去判子叶 1 的位 —— 那是静默错答。
/// `subleaf == 0` 仍发 `xor ecx, ecx`（短、且与 gcc 一致）；非零时发
/// `mov ecx, imm32`。
///
/// **本子叶可用性门控在本路径上不发**，理由：CPUID 的子叶越界行为是
/// 「返回子叶 0 的内容」，所以读 (7,1) 必须先用 (7,0):EAX（最大子叶号）门控，
/// 否则会把 EBX 的位误读成 EAX。codegen 侧的 `cpu_isa::PROBES` 已经把这条门控
/// 填进缓存槽（见 `SLOT_LEAF7_MAX`）。本路径今天**不可能**遇到 `subleaf != 0` 的
/// 位：`cpu_isa` 里所有 `cloneable: true` 的特性都在 subleaf 0（唯一用到子叶 1 的
/// `avx_vnni` 与 `amx-*` 系都不可克隆）。这个前提由单元测试
/// `linker_path_cloneable_bits_have_no_subleaf` 钉住 —— 有人加了带子叶的可克隆
/// 特性时该测试变红，提醒在这里补门控，而不是默默发一条读错寄存器的检测。
fn emit_cpuid_leaf(code: &mut Vec<u8>, current: &mut Option<(u32, u32)>, leaf: u32, subleaf: u32) {
    if *current == Some((leaf, subleaf)) {
        return;
    }
    // mov eax, leaf
    code.push(0xB8);
    code.extend_from_slice(&leaf.to_le_bytes());
    if subleaf == 0 {
        // xor ecx, ecx (sub-leaf = 0)
        code.extend_from_slice(&[0x31, 0xC9]);
    } else {
        // mov ecx, sub-leaf
        code.push(0xB9);
        code.extend_from_slice(&subleaf.to_le_bytes());
    }
    // cpuid
    code.extend_from_slice(&[0x0F, 0xA2]);
    *current = Some((leaf, subleaf));
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

/// 发射 AND EAX, imm32（`25 id`）。
///
/// 不用 `83 E0 ib`（imm8）：`avx512f` 的需求掩码是 0xE6，超出有符号 imm8 的表示范围。
fn emit_and_eax_imm32(code: &mut Vec<u8>, imm: u32) {
    code.push(0x25);
    code.extend_from_slice(&imm.to_le_bytes());
}

/// 发射 CMP EAX, imm32（`3D id`）。
fn emit_cmp_eax_imm32(code: &mut Vec<u8>, imm: u32) {
    code.push(0x3D);
    code.extend_from_slice(&imm.to_le_bytes());
}

/// 发射 XGETBV（读 `ECX` 索引的控制寄存器到 `EDX:EAX`）。
fn emit_xgetbv(code: &mut Vec<u8>) {
    code.extend_from_slice(&[0x0F, 0x01, 0xD0]);
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
///   1. 非 default 变体按 `cpu_isa::dispatch_priority` **降序**排列（与 codegen 侧
///      resolver 、与 gcc 13 实测判定链共用同一份规格；声明序不参与决策）
///   2. 对每个变体: 先按 `cpu_isa::xcr0_mask` 做 OSXSAVE + XCR0 门控（需要时），
///      再检测该特性的**全部** CPUID 位, 任一失败则跳到下一个变体
///   3. 所有检测通过: MOV RAX, variant_vaddr; RET
///   4. 所有变体都不匹配: MOV RAX, default_vaddr; RET
///
/// 返回: (code, Vec<(variant_index, patch_offset)>)，`variant_index` 是
/// `tcf.variants` 里的**原始下标**（`patch_all_resolvers` 按它取 vaddr）。
/// patch_offset 指向 MOV RAX, imm64 中 imm64 的位置.
pub fn generate_resolver_code(tcf: &TargetClonesFunc) -> (Vec<u8>, Vec<(usize, usize)>) {
    let mut code = Vec::with_capacity(128);
    let mut patches: Vec<(usize, usize)> = Vec::new();

    // 收集非 default 变体, 按 cpu_isa 优先级降序。**不能**用 filter().enumerate()
    // 的下标当 patch 索引：那会把「过滤后序号」当「全表下标」用，
    // `("default","avx")` 这种书写序会把 default 的地址补到 avx 分支上。
    let mut dispatch_variants: Vec<(usize, &TargetVariant)> = tcf
        .variants
        .iter()
        .enumerate()
        .filter(|(_, v)| v.arch != "default" && v.name != tcf.default_variant)
        .collect();
    dispatch_variants.sort_by(|a, b| {
        cpu_isa::dispatch_priority(&b.1.arch)
            .cmp(&cpu_isa::dispatch_priority(&a.1.arch))
            .then(a.0.cmp(&b.0)) // 同优先级保持原序（稳定）
    });

    let default_idx = tcf
        .variants
        .iter()
        .position(|v| v.name == tcf.default_variant)
        .unwrap_or(0);

    let mut current_leaf: Option<(u32, u32)> = None;

    for (variant_idx, variant) in &dispatch_variants {
        let bits = arch_to_cpuid_bits(&variant.arch);
        if bits.is_empty() {
            // 没有可检测的条件（`default` 或 `cpu_isa` 不认识的 ISA 名）→ 不参与判定。
            // 「ISA 名拼错」的用户面诊断在 codegen 侧（E1125，直接让编译失败），
            // 因此不认识的名单不会从合法输入走到这条路径上。
            continue;
        }

        // 收集本变体的失败跳转位置 (需要 patch 到 "skip" 位置)
        let mut fail_jmps: Vec<usize> = Vec::new();
        let xcr0_mask = arch_xcr0_mask(&variant.arch);

        if xcr0_mask != 0 {
            // 检查 OSXSAVE：位号不在此处重写，由 `cpu_isa::osxsave_bit()` 从
            // `FEATURES["osxsave"]` 反查（旧版在这里硬写 `1 << 27`，位表就有两份）。
            let (osx_leaf, osx_subleaf, osx_reg, osx_mask) =
                cpu_isa::osxsave_bit().unwrap_or_else(|| {
                    panic!(
                        "internal bug: cpu_isa::FEATURES 里没有 \"osxsave\" 位，链接期 resolver \
                     无法做 XSAVE 门控（不能静默退成「不门控」：那会在不支持 xgetbv 的机器上 \
                     #UD）。补回该位或改走 codegen 侧 resolver。"
                    );
                });
            emit_cpuid_leaf(&mut code, &mut current_leaf, osx_leaf, osx_subleaf);
            // test reg, mask
            emit_test_reg_imm32(&mut code, osx_reg, osx_mask);
            // je .skip_variant —— OSXSAVE 为 0 时 xgetbv 会 #UD，必须跳过本变体。
            // （旧实现在这里是 `jne`，方向反了：在 OSXSAVE 置位的真机上
            //  avx/avx2/avx512f 变体永远选不到。）
            fail_jmps.push(emit_je_rel32(&mut code));

            // xgetbv(ecx=0) 检查 XCR0
            code.extend_from_slice(&[0x31, 0xC9]); // xor ecx, ecx —— 显式置索引 0
            emit_xgetbv(&mut code);
            // and eax, mask; cmp eax, mask; jne .skip_variant（OS 未使能该状态）
            emit_and_eax_imm32(&mut code, xcr0_mask);
            emit_cmp_eax_imm32(&mut code, xcr0_mask);
            fail_jmps.push(emit_jne_rel32(&mut code));
            // xgetbv 写 EDX:EAX，leaf 1 的结果寄存器已被覆盖：不能让
            // `emit_cpuid_leaf` 的「同一 leaf 只查一次」缓存误以为它们仍然有效。
            current_leaf = None;
        }

        // 检测该特性的**全部** CPUID 位。旧版为了避开自己手写的 leaf1/ECX#28
        // 而滤掉那一位，会让 `avx` 只剩 XCR0 判据 —— XCR0 只能证明 OS 使能了
        // 状态，不能证明 CPU 支持该指令。
        for cpuid_bit in &bits {
            emit_cpuid_leaf(&mut code, &mut current_leaf, cpuid_bit.leaf, cpuid_bit.subleaf);
            // test reg, (1 << bit)
            emit_test_reg_imm32(&mut code, cpuid_bit.reg, 1u32 << cpuid_bit.bit);
            // je .skip_variant (feature not present)
            fail_jmps.push(emit_je_rel32(&mut code));
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
        // 真正不存在的 ISA 名（bmi2 现在是真的特性，见 xsave_requirement）。
        assert!(cpu_isa::lookup("not_a_real_isa").is_none());
        let bits = arch_to_cpuid_bits("not_a_real_isa");
        assert!(bits.is_empty());
    }

    /// 单一真源钉字：本模块的位表必须是 `cpu_isa` 的薄转发，不得自列常量。
    /// （旧版只认 sse4.2/avx/avx2/avx512f 四个名字，其余全当 default；
    ///  那会让「内置链接器选的变体」与「宿主链接器选的变体」不一致。）
    #[test]
    fn bit_table_is_the_shared_cpu_isa_source() {
        for name in [
            "sse4.2", "avx", "avx2", "avx512f", "fma", "bmi2", "popcnt", "sse3",
        ] {
            let forwarded = arch_to_cpuid_bits(name);
            let truth: Vec<CpuidBit> = cpu_isa::cpuid_bits(name).to_vec();
            assert_eq!(forwarded.len(), truth.len(), "{name} 不是薄转发");
            for (a, b) in forwarded.iter().zip(truth.iter()) {
                assert_eq!(
                    (a.leaf, a.subleaf, a.reg, a.bit),
                    (b.leaf, b.subleaf, b.reg, b.bit),
                    "{name}"
                );
            }
            assert_eq!(arch_xcr0_mask(name), cpu_isa::xcr0_mask(name), "{name}");
        }
        // 旧版遗漏的 avx 依赖位（sse4.2/avx 只有 1 位，fma/avx2 跨 leaf）与
        // XSAVE 名单（fma 需要 XCR0，旧版的硬编码名单里没有它）。
        assert_eq!(arch_to_cpuid_bits("fma").len(), 2);
        assert_eq!(arch_xcr0_mask("fma"), 0x6);
        assert_eq!(arch_xcr0_mask("avx512f"), 0xE6);
        assert_eq!(arch_xcr0_mask("sse4.2"), 0);
    }

    /// 链接期路径的**子叶前提**：本模块 `emit_cpuid_leaf` 不发「(7,0):EAX 最大子叶号」
    /// 门控，所以它只能服务「所有可克隆特性的位都在 subleaf 0」这个前提。
    ///
    /// 为什么这个前提不是巧合而是事实：`cpu_isa` 里带子叶的特性是 `avx_vnni`(7,1)
    /// 与 `amx_*` 系，而 gcc 的 `target_clones` 不接受这些名字（`cloneable: false`）。
    /// 如果有人新加一条 `cloneable: true` 且 `subleaf != 0` 的特性，本测试变红，
    /// 提醒在 `emit_cpuid_leaf` 那条路径上补子叶门控 —— 不补的后果是拿子叶 0 的
    /// EAX/EBX 去判子叶 1 的位（子叶越界返回子叶 0 的内容），得到一个**静默错答**。
    #[test]
    fn linker_path_cloneable_bits_have_no_subleaf() {
        let mut offenders: Vec<String> = Vec::new();
        for f in cpu_isa::all() {
            if !f.cloneable {
                continue;
            }
            for b in f.bits {
                if b.subleaf != 0 {
                    offenders.push(format!("{}: ({:#x},{})", f.name, b.leaf, b.subleaf));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "cloneable features with a non-zero sub-leaf need an explicit sub-leaf \
             availability gate in link::target_clones::emit_cpuid_leaf; found {offenders:?}"
        );
        // 反面：表里确实有带子叶的特性（否则本测试只是在验证空气）。
        let with_sub = cpu_isa::all()
            .flat_map(|f| f.bits.iter())
            .filter(|b| b.subleaf != 0)
            .count();
        assert!(
            with_sub > 0,
            "cpu_isa has no sub-leaf bits at all: the sub-leaf modelling would be vacuous"
        );
    }

    /// `emit_cpuid_leaf` 的缓存键必须是 `(leaf, subleaf)` 二元组，且非零子叶要真发
    /// 到 `ecx`（只比 leaf 会把「(7,0) 已查」当成「(7,1) 已查」而完全不发第二条
    /// CPUID —— 那是「看起来检了、其实没检」的形状）。
    #[test]
    fn emit_cpuid_leaf_caches_by_leaf_and_subleaf() {
        let mut code = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        emit_cpuid_leaf(&mut code, &mut cur, 7, 0);
        let first = code.len();
        assert_eq!(cur, Some((7, 0)));
        assert_eq!(&code[5..7], &[0x31, 0xC9], "sub-leaf 0 应用 xor ecx,ecx");
        assert_eq!(&code[first - 2..first], &[0x0F, 0xA2]);
        // 同 leaf + 同子叶：不重发。
        emit_cpuid_leaf(&mut code, &mut cur, 7, 0);
        assert_eq!(code.len(), first, "(leaf,sub-leaf) 命中时不应重发");
        // 同 leaf + 不同子叶：必须重发，且 `ecx` 里是新子叶号。
        emit_cpuid_leaf(&mut code, &mut cur, 7, 1);
        assert_eq!(cur, Some((7, 1)));
        assert!(code.len() > first, "子叶变化必须重发一条 CPUID");
        let start = first;
        assert_eq!(code[start], 0xB8, "mov eax, imm32");
        assert_eq!(&code[start + 5..start + 6], &[0xB9], "mov ecx, imm32");
        assert_eq!(
            u32::from_le_bytes(code[start + 6..start + 10].try_into().unwrap()),
            1,
            "ecx 里必须是新子叶号"
        );
        assert_eq!(&code[start + 10..start + 12], &[0x0F, 0xA2]);
    }

    #[test]
    fn xsave_requirement() {
        assert_eq!(arch_xcr0_mask("sse4.2"), 0);
        assert_eq!(arch_xcr0_mask("avx"), 0x6);
        assert_eq!(arch_xcr0_mask("avx2"), 0x6);
        assert_eq!(arch_xcr0_mask("avx512f"), 0xE6);
        assert_eq!(arch_xcr0_mask("default"), 0);
    }

    /// 变体判定顺序由 `cpu_isa` 优先级决定，与声明序无关（与 gcc 一致）。
    /// `("sse4.2","avx")` 里 avx 优先级(65) 高于 sse4.2(45)，所以第一条
    /// `MOV RAX` 补丁必须落在 avx 变体上。
    #[test]
    fn dispatch_order_follows_cpu_isa_priority_not_declaration_order() {
        let tcf = TargetClonesFunc {
            resolver_name: "foo".into(),
            variants: vec![
                TargetVariant {
                    name: "foo.sse4.2".into(),
                    arch: "sse4.2".into(),
                    vaddr: 0x2000,
                },
                TargetVariant {
                    name: "foo.avx".into(),
                    arch: "avx".into(),
                    vaddr: 0x3000,
                },
                TargetVariant {
                    name: "foo.default".into(),
                    arch: "default".into(),
                    vaddr: 0x1000,
                },
            ],
            default_variant: "foo.default".into(),
        };
        let (_code, patches) = generate_resolver_code(&tcf);
        assert_eq!(
            patches.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![1, 0, 2],
            "avx(variants[1]) 应先于 sse4.2(variants[0]) 被测试, 最后兜底 default(variants[2])"
        );
    }

    /// OSXSAVE 门控的方向：未置位时跳过本变体（`je`），而不是反过来。
    #[test]
    fn osxsave_gate_jumps_away_when_clear() {
        let tcf = TargetClonesFunc {
            resolver_name: "foo".into(),
            variants: vec![
                TargetVariant {
                    name: "foo.avx".into(),
                    arch: "avx".into(),
                    vaddr: 0x3000,
                },
                TargetVariant {
                    name: "foo.default".into(),
                    arch: "default".into(),
                    vaddr: 0x1000,
                },
            ],
            default_variant: "foo.default".into(),
        };
        let (code, _) = generate_resolver_code(&tcf);
        // test ecx, 1<<27 后的下一条条件跳转必须是 0F 84 (je)，不能是 0F 85 (jne)
        let test_at = code
            .windows(6)
            .position(|w| w == [0xF7, 0xC1, 0x00, 0x00, 0x00, 0x08])
            .expect("test ecx, 1<<27");
        let jcc = &code[test_at + 6..test_at + 8];
        assert_eq!(jcc, &[0x0F, 0x84], "OSXSAVE 门控方反了");
        // avx 自身位（leaf1/ECX#28）必须真的被测试，不能只靠 XCR0
        assert!(
            code.windows(6)
                .any(|w| w == [0xF7, 0xC1, 0x00, 0x00, 0x00, 0x10]),
            "没测 leaf1/ECX#28（AVX 本身），旧实现把它滤掉了"
        );
        // XCR0 需求值必须从 cpu_isa 来（avx = 0x6），并且以 imm32 形式发射
        assert!(code.windows(5).any(|w| w == [0x25, 0x06, 0x00, 0x00, 0x00]));
        assert!(code.windows(5).any(|w| w == [0x3D, 0x06, 0x00, 0x00, 0x00]));
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
