// src/link/relocate.rs - 重定位引擎
//
// 支持所有架构的完整重定位类型:
//   x86-64:  NONE/64/PC32/PLT32/GLOB_DAT/JUMP_SLOT/RELATIVE/GOTPCREL/
//            32/32S/16/PC16/8/PC8/PC64/TPOFF64/TPOFF32/GOTTPOFF/
//            TLSGD/TLSLD/DTPOFF32/DTPOFF64/DTPMOD64/
//            GOT64/GOTOFF64/GOTPC32/GOTPC64/GOTPCREL64/GOTPLT64/
//            PLTOFF64/SIZE32/SIZE64/GOTPC32_TLSDESC/IRELATIVE
//   AArch64: ABS64/ABS32/ABS16/PREL64/PREL32/PREL16/CALL26/JUMP26/
//            ADR_PREL_LO21/ADR_PREL_PG_HI21(_NC)/ADD_ABS_LO12_NC/
//            LDST{8,16,32,64,128}_ABS_LO12_NC/MOVW_UABS_G0..G3/
//            MOVW_SABS_G0..G2/TLSDESC_*/TLSIE_*/TLSLE_*
//   RISC-V:  64/32/32_PCREL/JAL/BRANCH/CALL(_PLT)/PCREL_HI20/PCREL_LO12_{I,S}/
//            HI20/LO12_{I,S}/TPREL64/TPOFF64/TPREL_HI20/TPREL_LO12_{I,S}/
//            ALIGN/RELAX/RVC_BRANCH/RVC_JUMP/SET{6,8,16,32}/SUB{6,8,16,32}
//   MIPS:    NONE/16/26/HI16/LO16/32/64/REL32/GPREL16/GPREL32/
//            GOT16/GOT_HI16/GOT_LO16/CALL16/CALL_HI16/CALL_LO16/
//            PC16/HIGHER/HIGHEST/TLS_{GD,LDM,DTPREL,GOTTPREL}
//   ARM:     NONE/ABS32/ABS16/ABS8/PREL32/PC24/CALL/JUMP24/
//            THM_CALL/THM_JUMP24/THM_JUMP19/MOVW_ABS_NC/MOVT_ABS/
//            MOVW_PREL_NC/MOVT_PREL/THM_MOVW_ABS_NC/THM_MOVT_ABS/
//            THM_MOVW_PREL_NC/THM_MOVT_PREL

use super::layout::{Layout, OutputRela, OutputSection};
use crate::codegen::elf::{
    R_AARCH64_ABS16, R_AARCH64_ABS32, R_AARCH64_ABS64,
    R_AARCH64_ADD_ABS_LO12_NC, R_AARCH64_ADR_PREL_LO21,
    R_AARCH64_ADR_PREL_PG_HI21, R_AARCH64_ADR_PREL_PG_HI21_NC,
    R_AARCH64_CALL26, R_AARCH64_JUMP26, R_AARCH64_LDST128_ABS_LO12_NC,
    R_AARCH64_LDST16_ABS_LO12_NC, R_AARCH64_LDST32_ABS_LO12_NC,
    R_AARCH64_LDST64_ABS_LO12_NC, R_AARCH64_LDST8_ABS_LO12_NC,
    R_AARCH64_MOVW_SABS_G0, R_AARCH64_MOVW_SABS_G1, R_AARCH64_MOVW_SABS_G2,
    R_AARCH64_MOVW_UABS_G0, R_AARCH64_MOVW_UABS_G1, R_AARCH64_MOVW_UABS_G2,
    R_AARCH64_MOVW_UABS_G3, R_AARCH64_PREL16, R_AARCH64_PREL32, R_AARCH64_PREL64,
    R_AARCH64_TLSDESC_ADD, R_AARCH64_TLSDESC_ADD_LO12,
    R_AARCH64_TLSDESC_ADR_PAGE21, R_AARCH64_TLSDESC_CALL,
    R_AARCH64_TLSDESC_LD64_LO12, R_AARCH64_TLSDESC_LDR,
    R_AARCH64_TLSDESC_OFF_G0_NC, R_AARCH64_TLSDESC_OFF_G1,
    R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21, R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC,
    R_AARCH64_TLSLE_ADD_TPREL_HI12, R_AARCH64_TLSLE_ADD_TPREL_LO12,
    R_AARCH64_TLSLE_ADD_TPREL_LO12_NC, R_AARCH64_TLSLE_MOVW_TPREL_G0,
    R_AARCH64_TLSLE_MOVW_TPREL_G0_NC, R_AARCH64_TLSLE_MOVW_TPREL_G1,
    R_AARCH64_TLSLE_MOVW_TPREL_G1_NC, R_AARCH64_TLSLE_MOVW_TPREL_G2,
    R_ARM_ABS16, R_ARM_ABS32, R_ARM_ABS8, R_ARM_CALL, R_ARM_JUMP24,
    R_ARM_MOVT_ABS, R_ARM_MOVT_PREL, R_ARM_MOVW_ABS_NC, R_ARM_MOVW_PREL_NC,
    R_ARM_NONE, R_ARM_PC24, R_ARM_PREL32, R_ARM_THM_CALL, R_ARM_THM_JUMP19,
    R_ARM_THM_JUMP24, R_ARM_THM_MOVT_ABS, R_ARM_THM_MOVT_PREL,
    R_ARM_THM_MOVW_ABS_NC, R_ARM_THM_MOVW_PREL_NC,
    R_MIPS_16, R_MIPS_26, R_MIPS_32, R_MIPS_64, R_MIPS_CALL16,
    R_MIPS_CALL_HI16, R_MIPS_CALL_LO16, R_MIPS_GOT16, R_MIPS_GOT_HI16,
    R_MIPS_GOT_LO16, R_MIPS_GPREL16, R_MIPS_GPREL32, R_MIPS_HI16,
    R_MIPS_HIGHER, R_MIPS_HIGHEST, R_MIPS_LO16, R_MIPS_NONE, R_MIPS_PC16,
    R_MIPS_REL32, R_MIPS_TLS_DTPREL_HI16, R_MIPS_TLS_DTPREL_LO16,
    R_MIPS_TLS_GD, R_MIPS_TLS_GOTTPREL, R_MIPS_TLS_LDM,
    R_RISCV_32, R_RISCV_32_PCREL, R_RISCV_64, R_RISCV_ALIGN,
    R_RISCV_BRANCH, R_RISCV_CALL, R_RISCV_CALL_PLT, R_RISCV_HI20,
    R_RISCV_JAL, R_RISCV_LO12_I, R_RISCV_LO12_S, R_RISCV_PCREL_HI20,
    R_RISCV_PCREL_LO12_I, R_RISCV_PCREL_LO12_S, R_RISCV_RELAX,
    R_RISCV_RVC_BRANCH, R_RISCV_RVC_JUMP, R_RISCV_SET16, R_RISCV_SET32,
    R_RISCV_SET6, R_RISCV_SET8, R_RISCV_SUB16, R_RISCV_SUB32,
    R_RISCV_SUB6, R_RISCV_SUB8, R_RISCV_TPOFF64, R_RISCV_TPREL64,
    R_RISCV_TPREL_HI20, R_RISCV_TPREL_LO12_I, R_RISCV_TPREL_LO12_S,
    R_X86_64_16, R_X86_64_32, R_X86_64_32S, R_X86_64_64, R_X86_64_8,
    R_X86_64_DTPMOD64, R_X86_64_DTPOFF32, R_X86_64_DTPOFF64,
    R_X86_64_GLOB_DAT, R_X86_64_GOT64, R_X86_64_GOTOFF64,
    R_X86_64_GOTPC32, R_X86_64_GOTPC32_TLSDESC, R_X86_64_GOTPC64,
    R_X86_64_GOTPCREL, R_X86_64_GOTPCREL64, R_X86_64_GOTPLT64,
    R_X86_64_GOTTPOFF, R_X86_64_IRELATIVE, R_X86_64_JUMP_SLOT,
    R_X86_64_NONE, R_X86_64_PC16, R_X86_64_PC32, R_X86_64_PC64,
    R_X86_64_PC8, R_X86_64_PLT32, R_X86_64_PLTOFF64, R_X86_64_RELATIVE,
    R_X86_64_SIZE32, R_X86_64_SIZE64, R_X86_64_TLSGD, R_X86_64_TLSLD,
    R_X86_64_TPOFF32, R_X86_64_TPOFF64, SHF_WRITE,
};
use crate::debug::ArchKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocError {
    OutOfRange,
    UnsupportedType(u32),
    UndefinedSymbol,
    BadAddend,
}

impl std::fmt::Display for RelocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelocError::OutOfRange => write!(f, "relocation out of range"),
            RelocError::UnsupportedType(t) => write!(f, "unsupported relocation type {}", t),
            RelocError::UndefinedSymbol => write!(f, "undefined symbol"),
            RelocError::BadAddend => write!(f, "bad addend"),
        }
    }
}

impl std::error::Error for RelocError {}

/// PIE / Shared 输出: 数据段 (可写节) 中的 R_X86_64_64 绝对重定位不能在链接时
/// 静态应用 —— 写入的是未加 load bias 的链接时地址, PIE 加载基址随机化后
/// 立即失效 (典型崩溃: 全局指针 char *s = "hello" 的初始化, 运行时读到
/// 0x2000 级别的非法地址)。把这些条目从 output_relas 取出并转为
/// R_X86_64_RELATIVE 动态重定位 (由 ld.so 加载时写入 load_bias + addend,
/// addend 在 fill_plt_got 最终 vaddr 确定后填写), 其余条目保留给
/// apply_relocations 静态应用。
pub fn collect_pie_dynamic_relas(layout: &mut Layout) -> Vec<OutputRela> {
    if !(layout.is_pie || layout.is_shared) {
        return Vec::new();
    }
    let mut kept = Vec::new();
    let mut dyns = Vec::new();
    for r in std::mem::take(&mut layout.output_relas) {
        // 重定位所在节必须可写 (数据段); 代码段中的绝对引用保持原样
        let is_writable = layout
            .sections
            .get(r.section as usize)
            .map(|s| s.flags & SHF_WRITE != 0)
            .unwrap_or(false);
        // 符号必须已定义 (vaddr != 0), 否则 addend 无法确定, 保持原样
        let sym_defined =
            (r.sym as usize) < layout.symbols.len() && layout.symbols[r.sym as usize].vaddr != 0;
        if r.rel_type == R_X86_64_64 && is_writable && sym_defined {
            dyns.push(OutputRela {
                rel_type: R_X86_64_RELATIVE,
                ..r
            });
        } else {
            kept.push(r);
        }
    }
    layout.output_relas = kept;
    dyns
}

/// 把 layout 中的输出节内容 + 输出重定位 + 输出符号表转换成最终字节。
/// 同时把需要写入 .rela.dyn / .rela.plt 的动态重定位分离出来。
pub fn apply_relocations(layout: &mut Layout) -> Result<DynamicRelocs, RelocError> {
    let mut dyn_relas: Vec<OutputRela> = Vec::new();
    let mut dyn_plt_relas: Vec<OutputRela> = Vec::new();

    // 复制出 relocations 以避免借用冲突
    let relas = std::mem::take(&mut layout.output_relas);

    // 按 section 索引分组
    use std::collections::HashMap;
    let mut by_section: HashMap<u32, Vec<OutputRela>> = HashMap::new();
    for r in relas {
        by_section.entry(r.section).or_default().push(r);
    }

    for (sec_idx, rs) in by_section {
        for r in rs {
            // 区分静态 / 动态重定位
            if !is_supported_reloc_for_arch(layout.arch, r.rel_type) {
                return Err(RelocError::UnsupportedType(r.rel_type));
            }
            if needs_dyn_rela(layout.arch, r.rel_type) {
                if r.rel_type == R_X86_64_JUMP_SLOT {
                    dyn_plt_relas.push(r);
                } else {
                    dyn_relas.push(r);
                }
                continue;
            }
            // 取出节（持短借用），再交给 apply_one（持 &mut layout）
            let sec_data_len = {
                let sec = &layout.sections[sec_idx as usize];
                sec.data.len()
            };
            // 重新取 mut 借用
            let s_idx = sec_idx as usize;
            let r_offset = r.offset as usize;
            let r_sym = r.sym;
            let r_addend = r.addend;
            let r_type = r.rel_type;
            // 计算 s_vaddr 后释放借用再 mut borrow
            let s_vaddr = {
                let sec = &layout.sections[s_idx];
                sec.vaddr
            };
            let sym = &layout.symbols[r_sym as usize];
            // sym.section 是 0-based 输出节索引, 0 对应 .text (有效), 不是 UNDEF.
            // 只有第一个空符号 (name="" + value=0 + size=0) 才是真正的 UNDEF.
            let s_sym_vaddr = if sym.name.is_empty() && sym.value == 0 && sym.size == 0 {
                0 // null entry
            } else {
                let sec_idx = sym.section as usize;
                if sec_idx < layout.sections.len() {
                    let sym_sec = &layout.sections[sec_idx];
                    sym_sec.vaddr.wrapping_add(sym.value)
                } else {
                    0
                }
            };
            let sec = &mut layout.sections[s_idx];
            // 64-bit 重定位和指令对重定位需要更宽的修补窗口。
            let patch_width = relocation_patch_width(layout.arch, r_type);
            if r_offset + patch_width > sec_data_len {
                return Err(RelocError::OutOfRange);
            }
            // 1. x86-64 通用重定位
            // 不同架构的 relocation type 数值可能撞车，所有分支都按
            // layout.arch 做命名空间过滤，未枚举类型保持 UnsupportedType。
            #[allow(clippy::match_overlapping_arm)]
            match r_type {
                R_X86_64_64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_PC32 | R_X86_64_PLT32 | R_X86_64_GOTPCREL
                    if layout.arch == ArchKind::X86_64 =>
                {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        ((s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_32 if layout.arch == ArchKind::X86_64 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_32S if layout.arch == ArchKind::X86_64 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as i64 as i32 as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOT64 if layout.arch == ArchKind::X86_64 => {
                    // 64-bit GOT entry address (S + A)
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOTOFF64 if layout.arch == ArchKind::X86_64 => {
                    // 64-bit GOT-relative: S + A - GOT base
                    // 这里假设 GOT base = s_sym_vaddr 已经被设置 (在 plt_got 中分配)
                    // 简化实现：与 GOT64 相同 — 由调用方负责 GOT base
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_TPOFF64 if layout.arch == ArchKind::X86_64 => {
                    // TLS thread-pointer 偏移: S + A - FS base
                    // 静态链接: 我们用 S + A 作为 thread-local 偏移
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_TPOFF32 if layout.arch == ArchKind::X86_64 => {
                    // TLS thread-pointer 32-bit 偏移 (sign-extended)
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as i64 as i32 as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE | R_X86_64_IRELATIVE
                    if layout.arch == ArchKind::X86_64 => {}
                R_X86_64_NONE if layout.arch == ArchKind::X86_64 => {
                    // no-op
                }
                R_X86_64_16 if layout.arch == ArchKind::X86_64 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u16;
                    sec.data[r_offset..r_offset + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_PC16 if layout.arch == ArchKind::X86_64 => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u16;
                    sec.data[r_offset..r_offset + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_8 if layout.arch == ArchKind::X86_64 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u8;
                    sec.data[r_offset] = value;
                }
                R_X86_64_PC8 if layout.arch == ArchKind::X86_64 => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u8;
                    sec.data[r_offset] = value;
                }
                R_X86_64_PC64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let p = s_vaddr + r_offset as u64;
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    sec.data[r_offset..r_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOTTPOFF if layout.arch == ArchKind::X86_64 => {
                    // TLS IE: GOT[S] + A - P
                    // Static: S is TP offset, GOT entry = S, so result = S + A - P
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        ((s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_TLSGD | R_X86_64_TLSLD if layout.arch == ArchKind::X86_64 => {
                    // TLS GD/LD: for static linking, resolve to DTP offset
                    // GOT entry address + A - P, where GOT entry = DTP offset
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        ((s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_DTPOFF32 if layout.arch == ArchKind::X86_64 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_DTPOFF64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_DTPMOD64 if layout.arch == ArchKind::X86_64 => {
                    // Module ID: static linking => 0 (self)
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&0u64.to_le_bytes());
                }
                R_X86_64_GOTPC32 | R_X86_64_GOTPC32_TLSDESC if layout.arch == ArchKind::X86_64 => {
                    // GOT + A - P: for static, use symbol address as proxy
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        ((s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOTPC64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let p = s_vaddr + r_offset as u64;
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    sec.data[r_offset..r_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOTPCREL64 | R_X86_64_GOTPLT64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_PLTOFF64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let p = s_vaddr + r_offset as u64;
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    sec.data[r_offset..r_offset + 8]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_SIZE32 if layout.arch == ArchKind::X86_64 => {
                    let value = sym.size as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_SIZE64 if layout.arch == ArchKind::X86_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    sec.data[r_offset..r_offset + 8]
                        .copy_from_slice(&sym.size.to_le_bytes());
                }
                // 2. AArch64 重定位
                R_AARCH64_ABS64 => {
                    // 64-bit 绝对
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_AARCH64_PREL64 => {
                    // 64-bit PC 相对
                    let p = s_vaddr + r_offset as u64;
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_AARCH64_CALL26 | R_AARCH64_JUMP26 => {
                    // 26-bit PC 相对, 4 字节指令, imm26 = (S+A-P) >> 2
                    let p = s_vaddr + r_offset as u64;
                    let offset = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    let imm26 = (offset >> 2) & 0x3ffffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0x3ffffff) | (imm26 as u32);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_ADR_PREL_LO21 => {
                    // ADR 21-bit 立即数 (S+A-P), 范围 ±1MB
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    // 21-bit = immlo[1:0] + immhi[18:0]
                    let immlo = (offset & 0x3) as u32;
                    let immhi = ((offset >> 2) & 0x7ffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0x60ffffe0) | (immhi << 5) | (immlo << 29);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_ADR_PREL_PG_HI21 => {
                    // ADR page: ((S+A) >> 12) - (P >> 12), 21-bit
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let p = s_vaddr + r_offset as u64;
                    let offset = ((s & !0xfff).wrapping_sub(p & !0xfff)) as i64 >> 12;
                    let immlo = (offset & 0x3) as u32;
                    let immhi = ((offset >> 2) & 0x7ffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0x60ffffe0) | (immhi << 5) | (immlo << 29);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_ADD_ABS_LO12_NC => {
                    // ADD 12-bit unsigned
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = (s & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_LDST64_ABS_LO12_NC
                | R_AARCH64_LDST32_ABS_LO12_NC
                | R_AARCH64_LDST16_ABS_LO12_NC
                | R_AARCH64_LDST8_ABS_LO12_NC => {
                    // load/store 12-bit unsigned, scale by element size
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let scale: u64 = match r_type {
                        R_AARCH64_LDST8_ABS_LO12_NC => 1,
                        R_AARCH64_LDST16_ABS_LO12_NC => 2,
                        R_AARCH64_LDST32_ABS_LO12_NC => 4,
                        R_AARCH64_LDST64_ABS_LO12_NC => 8,
                        _ => 1,
                    };
                    let imm12 = ((s / scale) & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_UABS_G0 => {
                    // 16-bit chunk 0: bits[15:0]
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = (s & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_UABS_G1 => {
                    // 16-bit chunk 1: bits[31:16]
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_UABS_G2 => {
                    // 16-bit chunk 2: bits[47:32]
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 32) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_UABS_G3 => {
                    // 16-bit chunk 3: bits[63:48]
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 48) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                // AArch64 TLS 重定位
                R_AARCH64_TLSDESC_ADR_PAGE21 => {
                    // TLSDESC adrp: page(S+A) - page(P), 21-bit
                    // 静态链接: 简化为 ADR_PREL_PG_HI21 语义
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let p = s_vaddr + r_offset as u64;
                    let offset = ((s & !0xfff).wrapping_sub(p & !0xfff)) as i64 >> 12;
                    let immlo = (offset & 0x3) as u32;
                    let immhi = ((offset >> 2) & 0x7ffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0x60ffffe0) | (immhi << 5) | (immlo << 29);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSDESC_LD64_LO12 => {
                    // TLSDESC ldr: lo12(S+A), 12-bit unsigned scaled by 8
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = ((s >> 3) & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSDESC_ADD_LO12 => {
                    // TLSDESC add: lo12(S+A), 12-bit unsigned
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = (s & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSDESC_OFF_G1 => {
                    // TLSDESC MOVW: bits[31:16] of TLS offset
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSDESC_OFF_G0_NC => {
                    // TLSDESC MOVK: bits[15:0] of TLS offset (no carry)
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = (s & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSDESC_LDR | R_AARCH64_TLSDESC_ADD | R_AARCH64_TLSDESC_CALL => {
                    // Marker relocations: ld.so 识别用, 静态链接无需修改指令
                }
                R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21 => {
                    // TLS IE adrp: page(GOTTPREL(S+A)) - page(P)
                    // 静态链接: 简化为直接 TP 偏移的页计算
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let p = s_vaddr + r_offset as u64;
                    let offset = ((s & !0xfff).wrapping_sub(p & !0xfff)) as i64 >> 12;
                    let immlo = (offset & 0x3) as u32;
                    let immhi = ((offset >> 2) & 0x7ffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0x60ffffe0) | (immhi << 5) | (immlo << 29);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC => {
                    // TLS IE ldr: lo12 of GOTTPREL, scaled by 8
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = ((s >> 3) & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_MOVW_TPREL_G1 => {
                    // TLS LE MOVW: bits[31:16] of TP offset
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_MOVW_TPREL_G0_NC => {
                    // TLS LE MOVK: bits[15:0] of TP offset (no carry)
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = (s & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_ABS32 => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let value = s as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_AARCH64_ABS16 => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let value = s as u16;
                    sec.data[r_offset..r_offset + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_AARCH64_PREL32 => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_AARCH64_PREL16 => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u16;
                    sec.data[r_offset..r_offset + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
                R_AARCH64_ADR_PREL_PG_HI21_NC => {
                    // Same as ADR_PREL_PG_HI21 but no overflow check
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let p = s_vaddr + r_offset as u64;
                    let offset = ((s & !0xfff).wrapping_sub(p & !0xfff)) as i64 >> 12;
                    let immlo = (offset & 0x3) as u32;
                    let immhi = ((offset >> 2) & 0x7ffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0x60ffffe0) | (immhi << 5) | (immlo << 29);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_LDST128_ABS_LO12_NC => {
                    // 128-bit load/store: 12-bit unsigned, scale by 16
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = ((s / 16) & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_SABS_G0 => {
                    // Signed 16-bit chunk 0: bits[15:0], MOVSF (alias of MOVZ)
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64) as i64;
                    let imm16 = (s & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_SABS_G1 => {
                    // Signed 16-bit chunk 1: bits[31:16], MOVKS
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64) as i64;
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_MOVW_SABS_G2 => {
                    // Signed 16-bit chunk 2: bits[47:32], MOVKS
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64) as i64;
                    let imm16 = ((s >> 32) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_MOVW_TPREL_G2 => {
                    // TLS LE MOVW: bits[47:32] of TP offset
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 32) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_MOVW_TPREL_G1_NC => {
                    // TLS LE MOVK: bits[31:16] of TP offset (no carry)
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_MOVW_TPREL_G0 => {
                    // TLS LE MOVW: bits[15:0] of TP offset (with carry check)
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = (s & 0xffff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xffff) | imm16;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_ADD_TPREL_HI12 => {
                    // TLS LE ADD: bits[23:12] of TP offset
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = ((s >> 12) & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_AARCH64_TLSLE_ADD_TPREL_LO12
                | R_AARCH64_TLSLE_ADD_TPREL_LO12_NC => {
                    // TLS LE ADD: bits[11:0] of TP offset
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm12 = (s & 0xfff) as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                // 3. RISC-V 重定位
                R_RISCV_64 if layout.arch == ArchKind::RiscV64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_32 if layout.arch == ArchKind::RiscV64 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_32_PCREL if layout.arch == ArchKind::RiscV64 => {
                    let p = s_vaddr + r_offset as u64;
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_CALL | R_RISCV_CALL_PLT if layout.arch == ArchKind::RiscV64 => {
                    // auipc + jalr pair: 8 字节, imm = (S+A-P)
                    // 高 20 位写入 auipc 的 imm, 低 12 位写入 jalr 的 imm
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm_hi = ((offset + 0x800) >> 12) & 0xfffff;
                    let imm_lo = offset & 0xfff;
                    // auipc (first 4 bytes)
                    let inst0 =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst0 = (inst0 & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst0.to_le_bytes());
                    // jalr (second 4 bytes)
                    let inst1 = u32::from_le_bytes(
                        sec.data[r_offset + 4..r_offset + 8].try_into().unwrap(),
                    );
                    let new_inst1 = (inst1 & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset + 4..r_offset + 8].copy_from_slice(&new_inst1.to_le_bytes());
                }
                R_RISCV_PCREL_HI20 if layout.arch == ArchKind::RiscV64 => {
                    // auipc: imm[31:12] = ((S+A-P) + 0x800) >> 12
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm_hi = ((offset + 0x800) >> 12) & 0xfffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_PCREL_LO12_I if layout.arch == ArchKind::RiscV64 => {
                    // I-type: imm[11:0] = (S+A-P) & 0xfff
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm_lo = offset & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_PCREL_LO12_S if layout.arch == ArchKind::RiscV64 => {
                    // S-type: imm[11:0] split as imm[11:5] @ bits[31:25] and imm[4:0] @ bits[11:7]
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm_lo = offset & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfe000f80)
                        | (((imm_lo as u32) >> 5) << 25)
                        | (((imm_lo as u32) & 0x1f) << 7);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_HI20 if layout.arch == ArchKind::RiscV64 => {
                    // 绝对高 20 位
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s + 0x800) >> 12) & 0xfffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_LO12_I if layout.arch == ArchKind::RiscV64 => {
                    // 绝对低 12 位 I-type
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = s & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_LO12_S if layout.arch == ArchKind::RiscV64 => {
                    // 绝对低 12 位 S-type
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = s & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfe000f80)
                        | (((imm_lo as u32) >> 5) << 25)
                        | (((imm_lo as u32) & 0x1f) << 7);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_TPOFF64 if layout.arch == ArchKind::RiscV64 => {
                    // TLS: 64-bit thread pointer offset
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_TPREL_HI20 if layout.arch == ArchKind::RiscV64 => {
                    // TLS: 高 20 位
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s + 0x800) >> 12) & 0xfffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_TPREL_LO12_I if layout.arch == ArchKind::RiscV64 => {
                    // TLS: 低 12 位 I-type
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = s & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_TPREL_LO12_S if layout.arch == ArchKind::RiscV64 => {
                    // TLS: 低 12 位 S-type
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = s & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfe000f80)
                        | (((imm_lo as u32) >> 5) << 25)
                        | (((imm_lo as u32) & 0x1f) << 7);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_TPREL64 if layout.arch == ArchKind::RiscV64 => {
                    // TLS: 64-bit TP-relative offset
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_JAL if layout.arch == ArchKind::RiscV64 => {
                    // J-type: imm[20|10:1|11|19:12] encoded in instruction
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as i64;
                    let imm = offset as u32;
                    // J-imm encoding: inst[31]=imm[20], inst[30:21]=imm[10:1],
                    //   inst[20]=imm[11], inst[19:12]=imm[19:12]
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000)
                        | (((imm >> 20) & 1) << 31)  // imm[20]
                        | (((imm >> 1) & 0x3ff) << 21) // imm[10:1]
                        | (((imm >> 11) & 1) << 20)   // imm[11]
                        | (((imm >> 12) & 0xff) << 12); // imm[19:12]
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_BRANCH if layout.arch == ArchKind::RiscV64 => {
                    // B-type: imm[12|10:5] in inst[31:25], imm[4:1|11] in inst[11:7]
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as i64;
                    let imm = offset as u32;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfe000f80)
                        | (((imm >> 12) & 1) << 31)   // imm[12]
                        | (((imm >> 5) & 0x3f) << 25)  // imm[10:5]
                        | (((imm >> 1) & 0xf) << 8)    // imm[4:1]
                        | (((imm >> 11) & 1) << 7);    // imm[11]
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_RVC_BRANCH if layout.arch == ArchKind::RiscV64 => {
                    // Compressed branch: CB-type, 9-bit signed offset
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as i64;
                    let imm = offset as u16;
                    let inst =
                        u16::from_le_bytes(sec.data[r_offset..r_offset + 2].try_into().unwrap());
                    // CB offset encoding: inst[12]=imm[8], inst[11:10]=imm[4:3],
                    //   inst[6:5]=imm[7:6], inst[4:3]=imm[2:1], inst[2]=imm[5]
                    let new_inst = (inst & !0x1c79)
                        | (((imm as u16 >> 8) & 1) << 12)
                        | (((imm as u16 >> 3) & 3) << 10)
                        | (((imm as u16 >> 6) & 3) << 5)
                        | (((imm as u16 >> 1) & 3) << 3)
                        | (((imm as u16 >> 5) & 1) << 2);
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_RVC_JUMP if layout.arch == ArchKind::RiscV64 => {
                    // Compressed jump: CJ-type, 12-bit signed offset
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as i64;
                    let imm = offset as u16;
                    let inst =
                        u16::from_le_bytes(sec.data[r_offset..r_offset + 2].try_into().unwrap());
                    // CJ offset encoding: complex bit scatter
                    let new_inst = (inst & !0x1fff)
                        | (((imm as u16 >> 11) & 1) << 12)
                        | (((imm as u16 >> 4) & 1) << 11)
                        | (((imm as u16 >> 8) & 1) << 10)
                        | (((imm as u16 >> 10) & 1) << 9)
                        | (((imm as u16 >> 6) & 1) << 8)
                        | (((imm as u16 >> 7) & 1) << 7)
                        | (((imm as u16 >> 1) & 1) << 6)
                        | (((imm as u16 >> 5) & 1) << 5)
                        | (((imm as u16 >> 3) & 1) << 4)
                        | (((imm as u16 >> 2) & 1) << 3)
                        | (((imm as u16 >> 9) & 1) << 2);
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_ALIGN | R_RISCV_RELAX if layout.arch == ArchKind::RiscV64 => {
                    // Linker hint relocations: no patching needed for static link
                }
                R_RISCV_SET6 | R_RISCV_SUB6 if layout.arch == ArchKind::RiscV64 => {
                    // 6-bit field patching
                    let val = sec.data[r_offset];
                    let sym_val = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u8;
                    let result = if r_type == R_RISCV_SET6 {
                        sym_val & 0x3f
                    } else {
                        val.wrapping_sub(sym_val) & 0x3f
                    };
                    sec.data[r_offset] = result;
                }
                R_RISCV_SET8 | R_RISCV_SUB8 if layout.arch == ArchKind::RiscV64 => {
                    let val = sec.data[r_offset];
                    let sym_val = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u8;
                    let result = if r_type == R_RISCV_SET8 {
                        sym_val
                    } else {
                        val.wrapping_sub(sym_val)
                    };
                    sec.data[r_offset] = result;
                }
                R_RISCV_SET16 | R_RISCV_SUB16 if layout.arch == ArchKind::RiscV64 => {
                    let val = u16::from_le_bytes(
                        sec.data[r_offset..r_offset + 2].try_into().unwrap(),
                    );
                    let sym_val = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u16;
                    let result = if r_type == R_RISCV_SET16 {
                        sym_val
                    } else {
                        val.wrapping_sub(sym_val)
                    };
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&result.to_le_bytes());
                }
                R_RISCV_SET32 | R_RISCV_SUB32 if layout.arch == ArchKind::RiscV64 => {
                    let val = u32::from_le_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let sym_val = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    let result = if r_type == R_RISCV_SET32 {
                        sym_val
                    } else {
                        val.wrapping_sub(sym_val)
                    };
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&result.to_le_bytes());
                }
                // 4. MIPS 重定位
                R_MIPS_26 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // J-type: target = (S + A) >> 2, 26-bit
                    let target = (s_sym_vaddr.wrapping_add(r_addend as u64)) >> 2;
                    let inst =
                        u32::from_be_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFC000000) | ((target as u32) & 0x03FFFFFF);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_HI16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // 高 16 位: (S + A) >> 16
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_be_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFFF0000) | imm_hi;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_LO16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // 低 16 位: (S + A) & 0xffff
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = (s & 0xffff) as u32;
                    let inst =
                        u32::from_be_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFFF0000) | imm_lo;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_32 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // 32-bit 绝对
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_be_bytes());
                }
                R_MIPS_64 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // 64-bit 绝对
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_be_bytes());
                }
                R_MIPS_NONE if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // no-op
                }
                R_MIPS_16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm = (s & 0xffff) as u16;
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&imm.to_be_bytes());
                }
                R_MIPS_REL32 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_be_bytes());
                }
                R_MIPS_GPREL16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // GP-relative 16-bit: S + A - GP
                    // For static linking, GP = 0 (simplified)
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm = (s & 0xffff) as u16;
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&imm.to_be_bytes());
                }
                R_MIPS_GPREL32 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let value = s as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_be_bytes());
                }
                R_MIPS_GOT16 | R_MIPS_GOT_HI16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // GOT 16-bit: for static linking, treat as HI16
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s >> 16) & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm_hi;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_GOT_LO16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = (s & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm_lo;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_CALL16 | R_MIPS_CALL_HI16
                    if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) =>
                {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s >> 16) & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm_hi;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_CALL_LO16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = (s & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm_lo;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_PC16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&offset.to_be_bytes());
                }
                R_MIPS_HIGHER if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm = ((s >> 32) & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_HIGHEST if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm = ((s >> 48) & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_TLS_GD | R_MIPS_TLS_LDM | R_MIPS_TLS_GOTTPREL
                | R_MIPS_TLS_DTPREL_HI16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    // TLS: for static linking, treat as HI16 of DTP/TP offset
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s >> 16) & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm_hi;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_TLS_DTPREL_LO16 if matches!(layout.arch, ArchKind::Mips | ArchKind::Mips64) => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = (s & 0xffff) as u32;
                    let inst = u32::from_be_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst = (inst & 0xFFFF0000) | imm_lo;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                // 5. ARM 重定位
                R_ARM_ABS32 if layout.arch == ArchKind::Arm => {
                    // 32-bit 绝对
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_ARM_MOVW_ABS_NC if layout.arch == ArchKind::Arm => {
                    // 16-bit 低 16 位: imm16 = (S + A) & 0xffff
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = (s & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFF0F000) | (imm4 << 16) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_ARM_MOVT_ABS if layout.arch == ArchKind::Arm => {
                    // 16-bit 高 16 位: imm16 = (S + A) >> 16
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFF0F000) | (imm4 << 16) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_ARM_NONE if layout.arch == ArchKind::Arm => {
                    // no-op
                }
                R_ARM_ABS16 if layout.arch == ArchKind::Arm => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let value = (s & 0xffff) as u16;
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&value.to_le_bytes());
                }
                R_ARM_ABS8 if layout.arch == ArchKind::Arm => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset] = (s & 0xff) as u8;
                }
                R_ARM_PREL32 if layout.arch == ArchKind::Arm => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        (s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_ARM_PC24 | R_ARM_CALL | R_ARM_JUMP24 if layout.arch == ArchKind::Arm => {
                    // Thumb BL: 21-bit signed offset (halfword aligned)
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm = (offset >> 1) as u32;
                    let inst_lo = u16::from_le_bytes(
                        sec.data[r_offset..r_offset + 2].try_into().unwrap(),
                    );
                    let inst_hi = u16::from_le_bytes(
                        sec.data[r_offset + 2..r_offset + 4].try_into().unwrap(),
                    );
                    // BL encoding: imm[21] in inst_lo[10], imm[20:11] in inst_lo[9:0],
                    //   imm[10:1] in inst_hi[10:0]
                    let new_lo = (inst_lo & !0x07FF)
                        | (((imm >> 11) & 0x3FF) as u16)
                        | (((imm >> 21) & 1) << 10) as u16;
                    let new_hi = (inst_hi & !0x07FF)
                        | ((imm >> 1) & 0x7FF) as u16;
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&new_lo.to_le_bytes());
                    sec.data[r_offset + 2..r_offset + 4].copy_from_slice(&new_hi.to_le_bytes());
                }
                R_ARM_THM_JUMP24 if layout.arch == ArchKind::Arm => {
                    // Thumb B: 21-bit signed offset (same encoding as THM_CALL but different opcode)
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm = (offset >> 1) as u32;
                    let inst_lo = u16::from_le_bytes(
                        sec.data[r_offset..r_offset + 2].try_into().unwrap(),
                    );
                    let inst_hi = u16::from_le_bytes(
                        sec.data[r_offset + 2..r_offset + 4].try_into().unwrap(),
                    );
                    let new_lo = (inst_lo & !0x07FF)
                        | (((imm >> 11) & 0x3FF) as u16)
                        | (((imm >> 21) & 1) << 10) as u16;
                    let new_hi = (inst_hi & !0x07FF)
                        | ((imm >> 1) & 0x7FF) as u16;
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&new_lo.to_le_bytes());
                    sec.data[r_offset + 2..r_offset + 4].copy_from_slice(&new_hi.to_le_bytes());
                }
                R_ARM_THM_JUMP19 if layout.arch == ArchKind::Arm => {
                    // Thumb conditional branch: 20-bit signed offset
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm = (offset >> 1) as u32;
                    let inst_lo = u16::from_le_bytes(
                        sec.data[r_offset..r_offset + 2].try_into().unwrap(),
                    );
                    let inst_hi = u16::from_le_bytes(
                        sec.data[r_offset + 2..r_offset + 4].try_into().unwrap(),
                    );
                    // T2 encoding: imm[19] in inst_lo[10], imm[18:13] in inst_lo[5:0],
                    //   imm[12:11] in inst_hi[13:12], imm[10:1] in inst_hi[10:0]
                    let new_lo = (inst_lo & !0x0FC0)
                        | (((imm >> 19) & 1) << 10) as u16
                        | ((imm >> 13) & 0x3F) as u16;
                    let new_hi = (inst_hi & !0x37FF)
                        | (((imm >> 11) & 3) << 12) as u16
                        | ((imm >> 1) & 0x7FF) as u16;
                    sec.data[r_offset..r_offset + 2].copy_from_slice(&new_lo.to_le_bytes());
                    sec.data[r_offset + 2..r_offset + 4].copy_from_slice(&new_hi.to_le_bytes());
                }
                R_ARM_MOVW_PREL_NC if layout.arch == ArchKind::Arm => {
                    // PC-relative MOVW: imm16 = (S + A - P) & 0xffff
                    let p = s_vaddr + r_offset as u64;
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    let imm16 = (s & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFF0F000) | (imm4 << 16) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_ARM_MOVT_PREL if layout.arch == ArchKind::Arm => {
                    let p = s_vaddr + r_offset as u64;
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFF0F000) | (imm4 << 16) | imm12;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_ARM_THM_MOVW_ABS_NC if layout.arch == ArchKind::Arm => {
                    // Thumb MOVW: imm16 split as imm4 @ bits[19:16], imm12 @ bits[11:0]
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = (s & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    // Re-encode as 32-bit Thumb instruction
                    let inst32 = u32::from_le_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst32 = (inst32 & !0x040F0FFF)
                        | ((imm4 & 0xF) << 16)
                        | (imm12 & 0xFFF);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst32.to_le_bytes());
                }
                R_ARM_THM_MOVT_ABS if layout.arch == ArchKind::Arm => {
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst32 = u32::from_le_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst32 = (inst32 & !0x040F0FFF)
                        | ((imm4 & 0xF) << 16)
                        | (imm12 & 0xFFF);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst32.to_le_bytes());
                }
                R_ARM_THM_MOVW_PREL_NC if layout.arch == ArchKind::Arm => {
                    let p = s_vaddr + r_offset as u64;
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    let imm16 = (s & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst32 = u32::from_le_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst32 = (inst32 & !0x040F0FFF)
                        | ((imm4 & 0xF) << 16)
                        | (imm12 & 0xFFF);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst32.to_le_bytes());
                }
                R_ARM_THM_MOVT_PREL if layout.arch == ArchKind::Arm => {
                    let p = s_vaddr + r_offset as u64;
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64).wrapping_sub(p);
                    let imm16 = ((s >> 16) & 0xffff) as u32;
                    let imm4 = (imm16 >> 12) & 0xf;
                    let imm12 = imm16 & 0xfff;
                    let inst32 = u32::from_le_bytes(
                        sec.data[r_offset..r_offset + 4].try_into().unwrap(),
                    );
                    let new_inst32 = (inst32 & !0x040F0FFF)
                        | ((imm4 & 0xF) << 16)
                        | (imm12 & 0xFFF);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst32.to_le_bytes());
                }
                _ => return Err(RelocError::UnsupportedType(r_type)),
            }
        }
    }

    Ok(DynamicRelocs {
        rela_dyn: dyn_relas,
        rela_plt: dyn_plt_relas,
    })
}

#[derive(Debug)]
pub struct DynamicRelocs {
    pub rela_dyn: Vec<OutputRela>,
    pub rela_plt: Vec<OutputRela>,
}

fn needs_dyn_rela(arch: ArchKind, rel_type: u32) -> bool {
    arch == ArchKind::X86_64
        && matches!(
            rel_type,
            R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE | R_X86_64_IRELATIVE
        )
}

fn is_supported_reloc_for_arch(arch: ArchKind, rel_type: u32) -> bool {
    match arch {
        ArchKind::X86_64 => matches!(
            rel_type,
            R_X86_64_NONE
                | R_X86_64_64
                | R_X86_64_PC32
                | R_X86_64_PLT32
                | R_X86_64_GLOB_DAT
                | R_X86_64_JUMP_SLOT
                | R_X86_64_RELATIVE
                | R_X86_64_GOTPCREL
                | R_X86_64_32
                | R_X86_64_32S
                | R_X86_64_16
                | R_X86_64_PC16
                | R_X86_64_8
                | R_X86_64_PC8
                | R_X86_64_PC64
                | R_X86_64_TPOFF64
                | R_X86_64_TPOFF32
                | R_X86_64_GOTTPOFF
                | R_X86_64_TLSGD
                | R_X86_64_TLSLD
                | R_X86_64_DTPOFF32
                | R_X86_64_DTPOFF64
                | R_X86_64_DTPMOD64
                | R_X86_64_GOT64
                | R_X86_64_GOTOFF64
                | R_X86_64_GOTPC32
                | R_X86_64_GOTPC64
                | R_X86_64_GOTPCREL64
                | R_X86_64_GOTPLT64
                | R_X86_64_PLTOFF64
                | R_X86_64_SIZE32
                | R_X86_64_SIZE64
                | R_X86_64_GOTPC32_TLSDESC
                | R_X86_64_IRELATIVE
        ),
        ArchKind::AArch64 => matches!(
            rel_type,
            R_AARCH64_ABS64
                | R_AARCH64_ABS32
                | R_AARCH64_ABS16
                | R_AARCH64_PREL64
                | R_AARCH64_PREL32
                | R_AARCH64_PREL16
                | R_AARCH64_CALL26
                | R_AARCH64_JUMP26
                | R_AARCH64_ADR_PREL_LO21
                | R_AARCH64_ADR_PREL_PG_HI21
                | R_AARCH64_ADR_PREL_PG_HI21_NC
                | R_AARCH64_ADD_ABS_LO12_NC
                | R_AARCH64_LDST64_ABS_LO12_NC
                | R_AARCH64_LDST32_ABS_LO12_NC
                | R_AARCH64_LDST16_ABS_LO12_NC
                | R_AARCH64_LDST8_ABS_LO12_NC
                | R_AARCH64_LDST128_ABS_LO12_NC
                | R_AARCH64_MOVW_UABS_G0
                | R_AARCH64_MOVW_UABS_G1
                | R_AARCH64_MOVW_UABS_G2
                | R_AARCH64_MOVW_UABS_G3
                | R_AARCH64_MOVW_SABS_G0
                | R_AARCH64_MOVW_SABS_G1
                | R_AARCH64_MOVW_SABS_G2
                | R_AARCH64_TLSDESC_ADR_PAGE21
                | R_AARCH64_TLSDESC_LD64_LO12
                | R_AARCH64_TLSDESC_ADD_LO12
                | R_AARCH64_TLSDESC_OFF_G1
                | R_AARCH64_TLSDESC_OFF_G0_NC
                | R_AARCH64_TLSDESC_LDR
                | R_AARCH64_TLSDESC_ADD
                | R_AARCH64_TLSDESC_CALL
                | R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21
                | R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC
                | R_AARCH64_TLSLE_MOVW_TPREL_G2
                | R_AARCH64_TLSLE_MOVW_TPREL_G1
                | R_AARCH64_TLSLE_MOVW_TPREL_G1_NC
                | R_AARCH64_TLSLE_MOVW_TPREL_G0
                | R_AARCH64_TLSLE_MOVW_TPREL_G0_NC
                | R_AARCH64_TLSLE_ADD_TPREL_HI12
                | R_AARCH64_TLSLE_ADD_TPREL_LO12
                | R_AARCH64_TLSLE_ADD_TPREL_LO12_NC
        ),
        ArchKind::RiscV64 => matches!(
            rel_type,
            R_RISCV_64
                | R_RISCV_32
                | R_RISCV_32_PCREL
                | R_RISCV_JAL
                | R_RISCV_BRANCH
                | R_RISCV_CALL
                | R_RISCV_CALL_PLT
                | R_RISCV_PCREL_HI20
                | R_RISCV_PCREL_LO12_I
                | R_RISCV_PCREL_LO12_S
                | R_RISCV_HI20
                | R_RISCV_LO12_I
                | R_RISCV_LO12_S
                | R_RISCV_TPOFF64
                | R_RISCV_TPREL64
                | R_RISCV_TPREL_HI20
                | R_RISCV_TPREL_LO12_I
                | R_RISCV_TPREL_LO12_S
                | R_RISCV_ALIGN
                | R_RISCV_RELAX
                | R_RISCV_RVC_BRANCH
                | R_RISCV_RVC_JUMP
                | R_RISCV_SET6
                | R_RISCV_SUB6
                | R_RISCV_SET8
                | R_RISCV_SUB8
                | R_RISCV_SET16
                | R_RISCV_SUB16
                | R_RISCV_SET32
                | R_RISCV_SUB32
        ),
        ArchKind::Mips | ArchKind::Mips64 => matches!(
            rel_type,
            R_MIPS_NONE
                | R_MIPS_16
                | R_MIPS_26
                | R_MIPS_HI16
                | R_MIPS_LO16
                | R_MIPS_32
                | R_MIPS_64
                | R_MIPS_REL32
                | R_MIPS_GPREL16
                | R_MIPS_GPREL32
                | R_MIPS_GOT16
                | R_MIPS_GOT_HI16
                | R_MIPS_GOT_LO16
                | R_MIPS_CALL16
                | R_MIPS_CALL_HI16
                | R_MIPS_CALL_LO16
                | R_MIPS_PC16
                | R_MIPS_HIGHER
                | R_MIPS_HIGHEST
                | R_MIPS_TLS_GD
                | R_MIPS_TLS_LDM
                | R_MIPS_TLS_DTPREL_HI16
                | R_MIPS_TLS_DTPREL_LO16
                | R_MIPS_TLS_GOTTPREL
        ),
        ArchKind::Arm => matches!(
            rel_type,
            R_ARM_NONE
                | R_ARM_ABS32
                | R_ARM_ABS16
                | R_ARM_ABS8
                | R_ARM_PREL32
                | R_ARM_PC24
                | R_ARM_CALL
                | R_ARM_JUMP24
                | R_ARM_THM_CALL
                | R_ARM_THM_JUMP24
                | R_ARM_THM_JUMP19
                | R_ARM_MOVW_ABS_NC
                | R_ARM_MOVT_ABS
                | R_ARM_MOVW_PREL_NC
                | R_ARM_MOVT_PREL
                | R_ARM_THM_MOVW_ABS_NC
                | R_ARM_THM_MOVT_ABS
                | R_ARM_THM_MOVW_PREL_NC
                | R_ARM_THM_MOVT_PREL
        ),
    }
}

fn relocation_patch_width(arch: ArchKind, rel_type: u32) -> usize {
    match arch {
        ArchKind::X86_64 => match rel_type {
            R_X86_64_64 | R_X86_64_GOT64 | R_X86_64_GOTOFF64
            | R_X86_64_TPOFF64 | R_X86_64_PC64 | R_X86_64_GOTPC64
            | R_X86_64_GOTPCREL64 | R_X86_64_GOTPLT64 | R_X86_64_PLTOFF64
            | R_X86_64_DTPOFF64 | R_X86_64_SIZE64 => 8,
            R_X86_64_16 | R_X86_64_PC16 => 2,
            R_X86_64_8 | R_X86_64_PC8 => 1,
            _ => 4,
        },
        ArchKind::AArch64 => match rel_type {
            R_AARCH64_ABS64 | R_AARCH64_PREL64 => 8,
            R_AARCH64_ABS16 | R_AARCH64_PREL16 => 2,
            R_AARCH64_ABS32 | R_AARCH64_PREL32 => 4,
            _ => 4,
        },
        ArchKind::RiscV64 => match rel_type {
            R_RISCV_64 | R_RISCV_TPOFF64 | R_RISCV_TPREL64
            | R_RISCV_CALL | R_RISCV_CALL_PLT => 8,
            R_RISCV_SET6 | R_RISCV_SUB6 => 1,
            R_RISCV_SET8 | R_RISCV_SUB8 => 1,
            R_RISCV_SET16 | R_RISCV_SUB16 => 2,
            R_RISCV_SET32 | R_RISCV_SUB32 => 4,
            _ => 4,
        },
        ArchKind::Mips | ArchKind::Mips64 => match rel_type {
            R_MIPS_64 => 8,
            R_MIPS_16 | R_MIPS_GPREL16 | R_MIPS_TLS_DTPREL_HI16
            | R_MIPS_TLS_DTPREL_LO16 | R_MIPS_TLS_GOTTPREL => 4,
            _ => 4,
        },
        ArchKind::Arm => 4,
    }
}

fn apply_one(sec: &mut OutputSection, r: OutputRela, layout: &Layout) -> Result<(), RelocError> {
    let sym = &layout.symbols[r.sym as usize];
    // u16::MAX 是未定义符号的哨兵值 (SHN_UNDEF)
    let s_vaddr = if sym.section != 0 && sym.section != u16::MAX {
        let sec_idx = sym.section as usize;
        if sec_idx < layout.sections.len() {
            let s_sec = &layout.sections[sec_idx];
            s_sec.vaddr.wrapping_add(sym.value)
        } else {
            0
        }
    } else {
        0
    };
    let place = r.offset as usize;
    let a = r.addend;

    match r.rel_type {
        R_X86_64_64 => {
            // S + A
            let value = s_vaddr.wrapping_add(a as u64);
            write_u64(sec, place, value)?;
        }
        R_X86_64_PC32 | R_X86_64_PLT32 => {
            // S + A - P
            let p = sec.vaddr + place as u64;
            let value = ((s_vaddr.wrapping_add(a as u64)).wrapping_sub(p)) as u32;
            write_u32(sec, place, value)?;
        }
        R_X86_64_GOTPCREL => {
            // GOT + A - P（需要 GOT 项；PIE 链接暂用占位修正）
            // 简化实现：要求 GOT 已在 layout 中分配，我们用 PLT/GOT 槽
            // 这里假设 caller 已经在 caller 节中预留了 4 字节，需要回填成
            // (GOT_entry_vaddr + A - P)
            // 此处为简化，假定 A = -4（标准 RIP-relative）
            let p = sec.vaddr + place as u64;
            // 我们无法在这里找到 GOT 项 — 改为直接回填 S+A-P
            // 调用方需要在生成 .o 时已经按 RIP-relative 设置过 A=-4
            let value = ((s_vaddr.wrapping_add(a as u64)).wrapping_sub(p)) as u32;
            write_u32(sec, place, value)?;
        }
        R_X86_64_32 => {
            let value = (s_vaddr.wrapping_add(a as u64)) as u32;
            write_u32(sec, place, value)?;
        }
        R_X86_64_32S => {
            let value = (s_vaddr.wrapping_add(a as u64)) as i64 as i32 as u32;
            write_u32(sec, place, value)?;
        }
        R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE => {
            // 已在 apply_relocations 阶段分离
        }
        other => return Err(RelocError::UnsupportedType(other)),
    }
    Ok(())
}

fn write_u64(sec: &mut OutputSection, off: usize, value: u64) -> Result<(), RelocError> {
    if off + 8 > sec.data.len() {
        return Err(RelocError::OutOfRange);
    }
    sec.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u32(sec: &mut OutputSection, off: usize, value: u32) -> Result<(), RelocError> {
    if off + 4 > sec.data.len() {
        return Err(RelocError::OutOfRange);
    }
    sec.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf::{SHF_ALLOC, SHF_EXECINSTR, SHT_PROGBITS, STB_GLOBAL, STT_FUNC};
    use crate::link::layout::{OutputSection, OutputSectionKind, OutputSymbol};

    #[allow(clippy::too_many_arguments)]
    fn mk_layout(
        text_vaddr: u64,
        text: Vec<u8>,
        text_off: u64,
        data_vaddr: u64,
        data: Vec<u8>,
        data_off: u64,
        relas: Vec<OutputRela>,
        sym_value: u64,
        sym_section: u16,
    ) -> Layout {
        let mut layout = Layout {
            is_pie: false,
            is_shared: false,
            base_addr: 0x400000,
            file_base: 0,
            sections: vec![
                // index 0: SHN_UNDEF 占位（真实节从 1 开始，与 ELF 约定一致）
                OutputSection {
                    kind: OutputSectionKind::Text,
                    name: "".into(),
                    sh_name_off: 0,
                    ty: SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                    data: vec![],
                    size: 0,
                    entsize: 0,
                    output_index: 0,
                    load_segment: 0,
                    vaddr: 0,
                    file_off: 0,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
                // index 1: .text
                OutputSection {
                    kind: OutputSectionKind::Text,
                    name: ".text".into(),
                    sh_name_off: 0,
                    ty: SHT_PROGBITS,
                    flags: SHF_ALLOC | SHF_EXECINSTR,
                    align: 16,
                    data: text,
                    size: 6,
                    entsize: 0,
                    output_index: 1,
                    load_segment: 0,
                    vaddr: text_vaddr,
                    file_off: text_off,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
                // index 2: .data
                OutputSection {
                    kind: OutputSectionKind::Data,
                    name: ".data".into(),
                    sh_name_off: 0,
                    ty: SHT_PROGBITS,
                    flags: SHF_ALLOC,
                    align: 8,
                    data,
                    size: 8,
                    entsize: 0,
                    output_index: 2,
                    load_segment: 1,
                    vaddr: data_vaddr,
                    file_off: data_off,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
            ],
            segments: vec![],
            symbols: vec![
                OutputSymbol {
                    name: "".into(),
                    value: 0,
                    vaddr: 0,
                    size: 0,
                    bind: 0,
                    ty: 0,
                    kind: 0,
                    visibility: 0,
                    section: 0,
                    shndx_if_section: 0,
                },
                OutputSymbol {
                    name: "callee".into(),
                    value: sym_value,
                    vaddr: sym_value,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: STT_FUNC,
                    kind: STT_FUNC,
                    visibility: 0,
                    section: sym_section,
                    shndx_if_section: 0,
                },
            ],
            entry: text_vaddr,
            has_symtab: true,
            has_dynamic: false,
            n_locals: 1,
            output_relas: relas,
            needed: vec![],
            dynamic_linker: None,
            arch: crate::debug::ArchKind::X86_64,
        };
        apply_relocations(&mut layout).expect("apply");
        layout
    }

    fn apply_one_test_reloc(arch: ArchKind, rel_type: u32) -> Result<DynamicRelocs, RelocError> {
        let mut layout = Layout {
            is_pie: false,
            is_shared: false,
            base_addr: 0x400000,
            file_base: 0,
            sections: vec![
                OutputSection {
                    kind: OutputSectionKind::Text,
                    name: "".into(),
                    sh_name_off: 0,
                    ty: SHT_PROGBITS,
                    flags: 0,
                    align: 1,
                    data: vec![],
                    size: 0,
                    entsize: 0,
                    output_index: 0,
                    load_segment: 0,
                    vaddr: 0,
                    file_off: 0,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
                OutputSection {
                    kind: OutputSectionKind::Text,
                    name: ".text".into(),
                    sh_name_off: 0,
                    ty: SHT_PROGBITS,
                    flags: SHF_ALLOC | SHF_EXECINSTR,
                    align: 16,
                    data: vec![0; 8],
                    size: 8,
                    entsize: 0,
                    output_index: 1,
                    load_segment: 0,
                    vaddr: 0x401000,
                    file_off: 0x120,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
            ],
            segments: vec![],
            symbols: vec![
                OutputSymbol {
                    name: "".into(),
                    value: 0,
                    vaddr: 0,
                    size: 0,
                    bind: 0,
                    ty: 0,
                    kind: 0,
                    visibility: 0,
                    section: 0,
                    shndx_if_section: 0,
                },
                OutputSymbol {
                    name: "target".into(),
                    value: 0,
                    vaddr: 0x401004,
                    size: 0,
                    bind: STB_GLOBAL,
                    ty: STT_FUNC,
                    kind: STT_FUNC,
                    visibility: 0,
                    section: 1,
                    shndx_if_section: 0,
                },
            ],
            entry: 0x401000,
            has_symtab: true,
            has_dynamic: false,
            n_locals: 1,
            output_relas: vec![OutputRela {
                section: 1,
                offset: 0,
                sym: 1,
                rel_type,
                addend: 0,
                _name: String::new(),
            }],
            needed: vec![],
            dynamic_linker: None,
            arch,
        };
        apply_relocations(&mut layout)
    }

    #[test]
    fn unsupported_relocation_types_return_unsupported_type() {
        let err = apply_one_test_reloc(ArchKind::X86_64, R_AARCH64_CALL26).unwrap_err();
        assert_eq!(err, RelocError::UnsupportedType(R_AARCH64_CALL26));

        let err = apply_one_test_reloc(ArchKind::AArch64, R_X86_64_PC32).unwrap_err();
        assert_eq!(err, RelocError::UnsupportedType(R_X86_64_PC32));
    }

    #[test]
    fn pc32_within_text() {
        // text @ 0x401000, call at +1, A = -4, target sym @ 0x401006
        let relas = vec![OutputRela {
            section: 1,
            offset: 1,
            sym: 1,
            rel_type: R_X86_64_PC32,
            addend: -4,
            _name: String::new(),
        }];
        let layout = mk_layout(
            0x401000,
            vec![0xe8, 0, 0, 0, 0, 0xc3],
            0x400120,
            0x402000,
            vec![0; 8],
            0x401008,
            relas,
            6,
            1,
        );
        // S + A - P = (0x401000 + 6) + (-4) - (0x401000 + 1) = 1
        let got = u32::from_le_bytes(layout.sections[1].data[1..5].try_into().unwrap());
        assert_eq!(got, 1, "PC32 should be relative 1");
    }

    #[test]
    fn plt32_within_text() {
        let relas = vec![OutputRela {
            section: 1,
            offset: 1,
            sym: 1,
            rel_type: R_X86_64_PLT32,
            addend: -4,
            _name: String::new(),
        }];
        let layout = mk_layout(
            0x401000,
            vec![0xe8, 0, 0, 0, 0, 0xc3],
            0x400120,
            0x402000,
            vec![0; 8],
            0x401008,
            relas,
            6,
            1,
        );
        let got = u32::from_le_bytes(layout.sections[1].data[1..5].try_into().unwrap());
        assert_eq!(got, 1);
    }

    #[test]
    fn abs64_in_data() {
        // symbol "callee" in .data at offset 0x234
        // => final vaddr = 0x402000 + 0x234 = 0x402234
        let relas = vec![OutputRela {
            section: 2,
            offset: 0,
            sym: 1,
            rel_type: R_X86_64_64,
            addend: 0,
            _name: String::new(),
        }];
        let layout = mk_layout(
            0x401000,
            vec![0; 6],
            0x400120,
            0x402000,
            vec![0; 8],
            0x401008,
            relas,
            0x234,
            2,
        );
        let got = u64::from_le_bytes(layout.sections[2].data[0..8].try_into().unwrap());
        assert_eq!(got, 0x402234);
    }
}
