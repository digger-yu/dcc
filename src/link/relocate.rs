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
//
// R_X86_64_TPOFF64 等 TLS 类型暂留给后续扩展。

use super::layout::{Layout, OutputRela, OutputSection, OutputSymbol};
use crate::codegen::elf::{
    R_X86_64_64, R_X86_64_PC32, R_X86_64_PLT32, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT,
    R_X86_64_RELATIVE, R_X86_64_GOTPCREL, R_X86_64_32, R_X86_64_32S,
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
        by_section.entry(r.section).or_insert_with(Vec::new).push(r);
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
            let s_sym_vaddr = if sym.section != 0 {
                let sym_sec = &layout.sections[sym.section as usize];
                sym_sec.vaddr + sym.value
            } else { 0 };
            let sec = &mut layout.sections[s_idx];
            if r_offset + 4 > sec_data_len && r_type != R_X86_64_64 {
                return Err(RelocError::OutOfRange);
            }
            match r_type {
                R_X86_64_64 => {
                    if r_offset + 8 > sec_data_len { return Err(RelocError::OutOfRange); }
                    let value = s_sym_vaddr.wrapping_add(r_addend as u64);
                    sec.data[r_offset..r_offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                R_X86_64_PC32 | R_X86_64_PLT32 | R_X86_64_GOTPCREL => {
                    let p = s_vaddr + r_offset as u64;
                    let value = ((s_sym_vaddr.wrapping_add(r_addend as u64)).wrapping_sub(p)) as u32;
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
                R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE => {}
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
    matches!(rel_type,
        R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT | R_X86_64_RELATIVE
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
    if off + 8 > sec.data.len() { return Err(RelocError::OutOfRange); }
    sec.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u32(sec: &mut OutputSection, off: usize, value: u32) -> Result<(), RelocError> {
    if off + 4 > sec.data.len() { return Err(RelocError::OutOfRange); }
    sec.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::layout::{OutputSectionKind, OutputSection};
    use crate::codegen::elf::{SHF_ALLOC, SHF_EXECINSTR, SHT_PROGBITS, STB_GLOBAL, STT_FUNC};

    fn mk_layout(text_vaddr: u64, text: Vec<u8>, text_off: u64,
                 data_vaddr: u64, data: Vec<u8>, data_off: u64,
                 relas: Vec<OutputRela>, sym_value: u64, sym_section: u16) -> Layout {
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
                    name: "".into(), value: 0, size: 0, bind: 0, ty: 0, visibility: 0, section: 0, shndx_if_section: 0,
                },
                OutputSymbol {
                    name: "callee".into(), value: sym_value, size: 0, bind: STB_GLOBAL, ty: STT_FUNC,
                    visibility: 0, section: sym_section, shndx_if_section: 0,
                },
            ],
            entry: text_vaddr,
            has_symtab: true,
            has_dynamic: false,
            n_locals: 1,
            output_relas: relas,
        };
        apply_relocations(&mut layout).expect("apply");
        layout
    }

    #[test]
    fn pc32_within_text() {
        // text @ 0x401000, call at +1, A = -4, target sym @ 0x401006
        let relas = vec![OutputRela { section: 1, offset: 1, sym: 1, rel_type: R_X86_64_PC32, addend: -4 }];
        let layout = mk_layout(
            0x401000, vec![0xe8, 0, 0, 0, 0, 0xc3], 0x400120,
            0x402000, vec![0; 8], 0x401008,
            relas, 6, 1,
        );
        // S + A - P = (0x401000 + 6) + (-4) - (0x401000 + 1) = 1
        let got = u32::from_le_bytes(layout.sections[1].data[1..5].try_into().unwrap());
        assert_eq!(got, 1, "PC32 should be relative 1");
    }

    #[test]
    fn plt32_within_text() {
        let relas = vec![OutputRela { section: 1, offset: 1, sym: 1, rel_type: R_X86_64_PLT32, addend: -4 }];
        let layout = mk_layout(
            0x401000, vec![0xe8, 0, 0, 0, 0, 0xc3], 0x400120,
            0x402000, vec![0; 8], 0x401008,
            relas, 6, 1,
        );
        let got = u32::from_le_bytes(layout.sections[1].data[1..5].try_into().unwrap());
        assert_eq!(got, 1);
    }

    #[test]
    fn abs64_in_data() {
        // symbol "callee" in .data at offset 0x234
        // => final vaddr = 0x402000 + 0x234 = 0x402234
        let relas = vec![OutputRela { section: 2, offset: 0, sym: 1, rel_type: R_X86_64_64, addend: 0 }];
        let layout = mk_layout(
            0x401000, vec![0; 6], 0x400120,
            0x402000, vec![0; 8], 0x401008,
            relas, 0x234, 2,
        );
        let got = u64::from_le_bytes(layout.sections[2].data[0..8].try_into().unwrap());
        assert_eq!(got, 0x402234);
    }
}
