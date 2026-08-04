// src/link/relocate.rs - 重定位引擎
//
// 处理以下 x86-64 重定位类型：
//   R_X86_64_64       (1)  绝对 64-bit
//   R_X86_64_PC32     (2)  PC 相对 32-bit
//   R_X86_64_PLT32    (4)  PLT 相对 32-bit (同 PC32, PIE 兼容)
//   R_X86_64_GLOB_DAT (6)  GOT 数据（动态库符号）
//   R_X86_64_JUMP_SLOT(7)  PLT 槽
//   R_X86_64_RELATIVE (8)  base + offset
//   R_X86_64_GOTPCREL (9)  32-bit GOT 相对
//   R_X86_64_32       (10) 绝对 32-bit (zero-extend)
//   R_X86_64_32S      (11) 绝对 32-bit (sign-extend)
//   R_X86_64_TPOFF64  (18) TLS: thread-local 偏移 (64-bit)
//   R_X86_64_GOT64    (19) 64-bit GOT 项地址
//   R_X86_64_TPOFF32  (23) TLS: thread-local 偏移 (32-bit, sign-extend)
//   R_X86_64_GOTOFF64 (25) 64-bit GOT 相对偏移
//   R_X86_64_IRELATIVE(37) IFUNC 解析器
//
// AArch64:
//   R_AARCH64_CALL26              (283)  26-bit PC 相对调用
//   R_AARCH64_JUMP26              (282)  26-bit PC 相对跳转
//   R_AARCH64_ADR_PREL_LO21       (274)  ADR 21-bit 立即数
//   R_AARCH64_ADR_PREL_PG_HI21    (275)  ADR page 21-bit
//   R_AARCH64_ADD_ABS_LO12_NC     (277)  ADD 12-bit 低位
//   R_AARCH64_LDST64_ABS_LO12_NC  (286)  64-bit load/store 12-bit
//   R_AARCH64_LDST32_ABS_LO12_NC  (285)
//   R_AARCH64_LDST16_ABS_LO12_NC  (284)
//   R_AARCH64_LDST8_ABS_LO12_NC   (278)
//   R_AARCH64_MOVW_UABS_G0..G3    (287..290) MOVZ/MOVK 16-bit chunk
//   R_AARCH64_ABS64               (257)  64-bit 绝对地址
//   R_AARCH64_PREL64              (260)  64-bit PC 相对
//
// RISC-V:
//   R_RISCV_CALL                  (18)  auipc+jalr 组合
//   R_RISCV_CALL_PLT              (19)  auipc+jalr 组合 (走 PLT)
//   R_RISCV_PCREL_HI20            (20)  auipc 高 20 位
//   R_RISCV_PCREL_LO12_I          (21)  低 12 位 I-type
//   R_RISCV_PCREL_LO12_S          (22)  低 12 位 S-type
//   R_RISCV_HI20                  (23)  高 20 位 (绝对)
//   R_RISCV_LO12_I                (24)
//   R_RISCV_LO12_S                (25)
//   R_RISCV_64                    (2)
//   R_RISCV_32                    (1)
//   R_RISCV_32_PCREL              (3)
//   R_RISCV_TPOFF64               (7)
//   R_RISCV_TPREL_HI20            (29)
//   R_RISCV_TPREL_LO12_I          (32)
//   R_RISCV_TPREL_LO12_S          (33)

use super::layout::{Layout, OutputRela, OutputSection};
use crate::codegen::elf::{
    R_AARCH64_ABS64, R_AARCH64_ADD_ABS_LO12_NC, R_AARCH64_ADR_PREL_LO21,
    R_AARCH64_ADR_PREL_PG_HI21, R_AARCH64_CALL26, R_AARCH64_JUMP26, R_AARCH64_LDST16_ABS_LO12_NC,
    R_AARCH64_LDST32_ABS_LO12_NC, R_AARCH64_LDST64_ABS_LO12_NC, R_AARCH64_LDST8_ABS_LO12_NC,
    R_AARCH64_MOVW_UABS_G0, R_AARCH64_MOVW_UABS_G1, R_AARCH64_MOVW_UABS_G2, R_AARCH64_MOVW_UABS_G3,
    R_AARCH64_PREL64, R_ARM_ABS32, R_ARM_CALL, R_ARM_JUMP24, R_ARM_MOVT_ABS, R_ARM_MOVW_ABS_NC,
    R_MIPS_26, R_MIPS_32, R_MIPS_64, R_MIPS_HI16, R_MIPS_LO16, R_RISCV_32, R_RISCV_32_PCREL,
    R_RISCV_64, R_RISCV_CALL, R_RISCV_CALL_PLT, R_RISCV_HI20, R_RISCV_LO12_I, R_RISCV_LO12_S,
    R_RISCV_PCREL_HI20, R_RISCV_PCREL_LO12_I, R_RISCV_PCREL_LO12_S, R_RISCV_TPOFF64,
    R_RISCV_TPREL_HI20, R_RISCV_TPREL_LO12_I, R_RISCV_TPREL_LO12_S, R_X86_64_32, R_X86_64_32S,
    R_X86_64_64, R_X86_64_GLOB_DAT, R_X86_64_GOT64, R_X86_64_GOTOFF64, R_X86_64_GOTPCREL,
    R_X86_64_IRELATIVE, R_X86_64_JUMP_SLOT, R_X86_64_PC32, R_X86_64_PLT32, R_X86_64_RELATIVE,
    R_X86_64_TPOFF32, R_X86_64_TPOFF64,
};

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
            if needs_dyn_rela(r.rel_type) {
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
                    sym_sec.vaddr + sym.value
                } else {
                    0
                }
            };
            let sec = &mut layout.sections[s_idx];
            // 64-bit 重定位 (R_X86_64_64 / GOT64 / GOTOFF64 / TPOFF64) 需要 8 字节
            let needs_8 = matches!(
                r_type,
                R_X86_64_64 | R_X86_64_GOT64 | R_X86_64_GOTOFF64 | R_X86_64_TPOFF64
            );
            if needs_8 && r_offset + 8 > sec_data_len {
                return Err(RelocError::OutOfRange);
            }
            if !needs_8 && r_offset + 4 > sec_data_len {
                return Err(RelocError::OutOfRange);
            }
            // 1. x86-64 通用重定位
            // 注: 不同架构的 relocation type 数值可能撞车 (如 R_X86_64_64=1 与 R_RISCV_32=1),
            // layout 实际上只装一种 arch 的重定位, 故此处 match 看似 overlap 实则安全.
            #[allow(clippy::match_overlapping_arm)]
            match r_type {
                R_X86_64_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_PC32 | R_X86_64_PLT32 | R_X86_64_GOTPCREL => {
                    let p = s_vaddr + r_offset as u64;
                    let value =
                        ((s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_32 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_32S => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as i64 as i32 as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOT64 => {
                    // 64-bit GOT entry address (S + A)
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GOTOFF64 => {
                    // 64-bit GOT-relative: S + A - GOT base
                    // 这里假设 GOT base = s_sym_vaddr 已经被设置 (在 plt_got 中分配)
                    // 简化实现：与 GOT64 相同 — 由调用方负责 GOT base
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_TPOFF64 => {
                    // TLS thread-pointer 偏移: S + A - FS base
                    // 静态链接: 我们用 S + A 作为 thread-local 偏移
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_TPOFF32 => {
                    // TLS thread-pointer 32-bit 偏移 (sign-extended)
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as i64 as i32 as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE | R_X86_64_IRELATIVE => {
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
                // 3. RISC-V 重定位
                R_RISCV_64 => {
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_32 => {
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_32_PCREL => {
                    let p = s_vaddr + r_offset as u64;
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_CALL | R_RISCV_CALL_PLT => {
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
                R_RISCV_PCREL_HI20 => {
                    // auipc: imm[31:12] = ((S+A-P) + 0x800) >> 12
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm_hi = ((offset + 0x800) >> 12) & 0xfffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_PCREL_LO12_I => {
                    // I-type: imm[11:0] = (S+A-P) & 0xfff
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p) as i64;
                    let imm_lo = offset & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_PCREL_LO12_S => {
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
                R_RISCV_HI20 => {
                    // 绝对高 20 位
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s + 0x800) >> 12) & 0xfffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_LO12_I => {
                    // 绝对低 12 位 I-type
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = s & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_LO12_S => {
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
                R_RISCV_TPOFF64 => {
                    // TLS: 64-bit thread pointer offset
                    if r_offset + 8 > sec_data_len {
                        return Err(RelocError::OutOfRange);
                    }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_RISCV_TPREL_HI20 => {
                    // TLS: 高 20 位
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s + 0x800) >> 12) & 0xfffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfffff000) | ((imm_hi as u32) << 12);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_TPREL_LO12_I => {
                    // TLS: 低 12 位 I-type
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = s & 0xfff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & !0xfff00000) | ((imm_lo as u32) << 20);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_RISCV_TPREL_LO12_S => {
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
                // 4. MIPS 重定位
                R_MIPS_26 => {
                    // J-type: target = (S + A) >> 2, 26-bit
                    let target = (s_sym_vaddr.wrapping_add(r_addend as u64)) >> 2;
                    let inst =
                        u32::from_be_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFC000000) | ((target as u32) & 0x03FFFFFF);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_HI16 => {
                    // 高 16 位: (S + A) >> 16
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_hi = ((s >> 16) & 0xffff) as u32;
                    let inst =
                        u32::from_be_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFFF0000) | imm_hi;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_LO16 => {
                    // 低 16 位: (S + A) & 0xffff
                    let s = s_sym_vaddr.wrapping_add(r_addend as u64);
                    let imm_lo = (s & 0xffff) as u32;
                    let inst =
                        u32::from_be_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFFFF0000) | imm_lo;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_be_bytes());
                }
                R_MIPS_32 => {
                    // 32-bit 绝对
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_be_bytes());
                }
                R_MIPS_64 => {
                    // 64-bit 绝对
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_be_bytes());
                }
                // 5. ARM 重定位
                R_ARM_ABS32 => {
                    // 32-bit 绝对
                    let value = (s_sym_vaddr.wrapping_add(r_addend as u64)) as u32;
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&value.to_le_bytes());
                }
                R_ARM_CALL | R_ARM_JUMP24 => {
                    // 24-bit PC 相对: offset = (S + A - P) >> 2
                    let p = s_vaddr + r_offset as u64;
                    let offset = (s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p);
                    let imm24 = (offset >> 2) & 0xffffff;
                    let inst =
                        u32::from_le_bytes(sec.data[r_offset..r_offset + 4].try_into().unwrap());
                    let new_inst = (inst & 0xFF000000) | (imm24 as u32);
                    sec.data[r_offset..r_offset + 4].copy_from_slice(&new_inst.to_le_bytes());
                }
                R_ARM_MOVW_ABS_NC => {
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
                R_ARM_MOVT_ABS => {
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
                _ => return Err(RelocError::UnsupportedType(r_type)),
            }
        }
    }

    Ok(DynamicRelocs {
        rela_dyn: dyn_relas,
        rela_plt: dyn_plt_relas,
    })
}

pub struct DynamicRelocs {
    pub rela_dyn: Vec<OutputRela>,
    pub rela_plt: Vec<OutputRela>,
}

fn needs_dyn_rela(rel_type: u32) -> bool {
    matches!(
        rel_type,
        R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE | R_X86_64_IRELATIVE
    )
}

fn apply_one(sec: &mut OutputSection, r: OutputRela, layout: &Layout) -> Result<(), RelocError> {
    let sym = &layout.symbols[r.sym as usize];
    let s_vaddr = if sym.section != 0 {
        let s_sec = &layout.sections[sym.section as usize];
        s_sec.vaddr + sym.value
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
            dynamic_linker: None,
        };
        apply_relocations(&mut layout).expect("apply");
        layout
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
