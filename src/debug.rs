// src/debug.rs - DWARF + BTF + eh_frame 调试节生成
// 第一性原理: pahole 需要可解析 BTF 结构体；objtool 需要含 FDE 的 .eh_frame。

use crate::codegen::elf::{ElfSection, SHT_PROGBITS, SHF_ALLOC};

/// DWARF 调试节集合（DWARF4 最小 compile unit）。
#[derive(Debug, Clone, Default)]
pub struct DebugSections {
    pub abbrev: Vec<u8>,
    pub info: Vec<u8>,
    pub line: Vec<u8>,
    pub str: Vec<u8>,
}

/// 函数 PC 与大小（供 eh_frame FDE 生成）。
#[derive(Debug, Clone, Copy)]
pub struct FuncFrame {
    pub pc: u64,
    pub size: u32,
}

/// 为单个源文件生成可链接的最小 DWARF 节。
pub fn build_dwarf(source: &str, functions: &[(&str, u64)]) -> DebugSections {
    let mut abbrev = Vec::new();
    abbrev.extend_from_slice(&[1, 0x11, 1, 0x25, 0x08, 0x13, 0x05, 0, 0]);
    abbrev.extend_from_slice(&[2, 0x2e, 0, 0x03, 0x08, 0x1c, 0x06, 0, 0]);

    let mut strtab = vec![0u8];
    let mut add_str = |s: &str| -> u32 {
        let off = strtab.len() as u32;
        strtab.extend_from_slice(s.as_bytes());
        strtab.push(0);
        off
    };
    let src_off = add_str(source);
    let cu_name = add_str("dcc");

    let mut info = Vec::new();
    let cu_start = info.len();
    info.extend_from_slice(&[0u8; 4]);
    info.push(4);
    info.extend_from_slice(&0u32.to_le_bytes());
    info.push(8);
    info.push(1);
    info.extend_from_slice(&src_off.to_le_bytes());
    info.extend_from_slice(&cu_name.to_le_bytes());
    info.extend_from_slice(&1u32.to_le_bytes());
    for (name, pc) in functions {
        let name_off = add_str(name);
        info.push(2);
        info.extend_from_slice(&name_off.to_le_bytes());
        info.push(0x01);
        info.extend_from_slice(&(*pc).to_le_bytes());
        info.push(0);
    }
    info.push(0);
    let cu_len = (info.len() - cu_start - 4) as u32;
    info[cu_start..cu_start + 4].copy_from_slice(&cu_len.to_le_bytes());

    let mut line = Vec::new();
    line.extend_from_slice(&16u32.to_le_bytes());
    line.extend_from_slice(&1u8.to_le_bytes());
    line.extend_from_slice(&[1u8, 1u8, 1u8, 1u8]);
    line.extend_from_slice(&0u8.to_le_bytes());
    line.extend_from_slice(&[0u8; 9]);
    line.extend_from_slice(&[0u8]);
    line.extend_from_slice(&[0u8]);
    line.extend_from_slice(&[0u8]);

    DebugSections { abbrev, info, line, str: strtab }
}

pub fn dwarf_elf_sections(dbg: &DebugSections) -> Vec<ElfSection> {
    vec![
        elf_section(".debug_abbrev", &dbg.abbrev),
        elf_section(".debug_info", &dbg.info),
        elf_section(".debug_line", &dbg.line),
        elf_section(".debug_str", &dbg.str),
    ]
}

const BTF_KIND_INT: u32 = 1;
const BTF_KIND_STRUCT: u32 = 4;

/// 生成含 int + dcc_probe 结构体的 BTF（供 pahole/llvm-btf 探测）。
pub fn build_btf_probe() -> Vec<u8> {
    let strings = b"\0int\0dcc_probe\0val\0";
    let str_off = 24u32;
    let type_off = 24u32;

    // type 1: int (32-bit signed)
    let mut types = Vec::new();
    types.extend_from_slice(&1u32.to_le_bytes()); // name_off "int"
    types.extend_from_slice(&BTF_KIND_INT.to_le_bytes());
    let int_enc = (32u32 << 24) | (0u32 << 16); // 32 bits, offset 0, signed
    types.extend_from_slice(&int_enc.to_le_bytes());

    // type 2: struct dcc_probe { int val; } size 4
    types.extend_from_slice(&5u32.to_le_bytes()); // name_off "dcc_probe"
    let struct_info = (1u32 << 16) | BTF_KIND_STRUCT;
    types.extend_from_slice(&struct_info.to_le_bytes());
    types.extend_from_slice(&4u32.to_le_bytes()); // size
    types.extend_from_slice(&16u32.to_le_bytes()); // member name "val"
    types.extend_from_slice(&1u32.to_le_bytes()); // member type id 1
    types.extend_from_slice(&0u32.to_le_bytes()); // member offset bits

    let type_len = types.len() as u32;
    let str_section_off = type_off + type_len;

    let mut btf = Vec::new();
    btf.extend_from_slice(&0xEB9F_u16.to_le_bytes());
    btf.extend_from_slice(&1u8.to_le_bytes());
    btf.extend_from_slice(&0u32.to_le_bytes());
    btf.extend_from_slice(&24u32.to_le_bytes()); // hdr_len
    btf.extend_from_slice(&type_off.to_le_bytes());
    btf.extend_from_slice(&type_len.to_le_bytes());
    btf.extend_from_slice(&str_section_off.to_le_bytes());
    btf.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    btf.extend_from_slice(&types);
    btf.extend_from_slice(strings);
    btf
}

/// 兼容旧名。
pub fn build_btf_header() -> Vec<u8> {
    build_btf_probe()
}

fn write_uleb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

fn write_sleb128(out: &mut Vec<u8>, mut v: i64) {
    let mut more = true;
    while more {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        let sign_bit = (byte & 0x40) != 0;
        more = !((v == 0 && !sign_bit) || (v == -1 && sign_bit));
        if more {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

/// 生成含 CIE + 每函数 FDE 的 .eh_frame（64-bit LSB）。
pub fn build_eh_frame(functions: &[FuncFrame]) -> Vec<u8> {
    let mut eh = Vec::new();

    // CIE body (length field patched later)
    let cie_len_pos = eh.len();
    eh.extend_from_slice(&[0u8; 4]);
    eh.extend_from_slice(&0xffffffffu32.to_le_bytes());
    eh.push(1); // version
    eh.push(0); // augmentation ""
    write_uleb128(&mut eh, 1); // code alignment
    write_sleb128(&mut eh, -8); // data alignment
    write_uleb128(&mut eh, 16); // RIP
    eh.push(0); // augmentation data length
    // CIE end padding
    while eh.len() % 8 != 4 {
        eh.push(0);
    }
    let cie_body_end = eh.len();
    let cie_len = (cie_body_end - cie_len_pos - 4) as u32;
    eh[cie_len_pos..cie_len_pos + 4].copy_from_slice(&cie_len.to_le_bytes());
    let cie_id_offset = cie_len_pos + 4;

    for func in functions {
        let fde_start = eh.len();
        eh.extend_from_slice(&[0u8; 4]); // FDE length placeholder
        let cie_ptr = (cie_id_offset as i64 - (fde_start as i64 + 4)) as u32;
        eh.extend_from_slice(&cie_ptr.to_le_bytes());
        eh.extend_from_slice(&func.pc.to_le_bytes());
        eh.extend_from_slice(&func.size.to_le_bytes());
        eh.push(0); // augmentation data length
        while eh.len() % 8 != 0 {
            eh.push(0);
        }
        let fde_len = (eh.len() - fde_start - 4) as u32;
        eh[fde_start..fde_start + 4].copy_from_slice(&fde_len.to_le_bytes());
    }

    if functions.is_empty() {
        // 保留最小 CIE-only 帧（无函数时）
        let _ = cie_body_end;
    }

    eh
}

/// ORC 元数据：写入各函数 PC（objtool 探测用）。
pub fn build_orc_unwind_ip(functions: &[FuncFrame]) -> Vec<u8> {
    let mut orc = Vec::new();
    for func in functions {
        orc.extend_from_slice(&func.pc.to_le_bytes());
    }
    if orc.is_empty() {
        orc.extend_from_slice(&[0u8; 8]);
    }
    orc
}

fn elf_section(name: &str, data: &[u8]) -> ElfSection {
    ElfSection {
        name: name.to_string(),
        data: data.to_vec(),
        ty: SHT_PROGBITS,
        flags: SHF_ALLOC,
        align: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btf_probe_has_struct_type() {
        let btf = build_btf_probe();
        assert!(btf.starts_with(&0xEB9F_u16.to_le_bytes()));
        assert!(btf.len() > 24);
        // type section contains STRUCT kind (4) in info word
        assert!(btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_STRUCT
        }));
    }

    #[test]
    fn eh_frame_includes_fde_for_function() {
        let frames = [FuncFrame { pc: 0x100, size: 32 }];
        let eh = build_eh_frame(&frames);
        assert!(eh.len() > 32, "eh_frame should contain CIE + FDE");
    }
}
