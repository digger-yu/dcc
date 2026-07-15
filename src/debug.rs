// src/debug.rs - 最小 DWARF + BTF 调试节生成
// 第一性原理: 内核/pahole 需要 .debug_* 与 .BTF 节存在且可被 readelf 识别。

use crate::codegen::elf::{ElfSection, SHT_PROGBITS, SHF_ALLOC};

/// DWARF 调试节集合（DWARF4 最小 compile unit）。
#[derive(Debug, Clone, Default)]
pub struct DebugSections {
    pub abbrev: Vec<u8>,
    pub info: Vec<u8>,
    pub line: Vec<u8>,
    pub str: Vec<u8>,
}

/// 为单个源文件生成可链接的最小 DWARF 节。
pub fn build_dwarf(source: &str, functions: &[(&str, u64)]) -> DebugSections {
    let mut abbrev = Vec::new();
    // Abbrev #1: compile_unit
    abbrev.extend_from_slice(&[1, 0x11, 1, 0x25, 0x08, 0x13, 0x05, 0, 0]);
    // Abbrev #2: subprogram
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
    // CU length placeholder
    let cu_start = info.len();
    info.extend_from_slice(&[0u8; 4]);
    info.push(4); // DWARF version 4
    info.extend_from_slice(&0u32.to_le_bytes()); // debug_abbrev offset
    info.push(8); // address size
    info.push(1); // abbrev #1 compile_unit
    info.extend_from_slice(&src_off.to_le_bytes());
    info.extend_from_slice(&cu_name.to_le_bytes());
    info.extend_from_slice(&1u32.to_le_bytes()); // line base
    for (name, pc) in functions {
        let name_off = add_str(name);
        info.push(2); // abbrev #2 subprogram
        info.extend_from_slice(&name_off.to_le_bytes());
        info.push(0x01); // DW_AT_low_pc
        info.extend_from_slice(&(*pc).to_le_bytes());
        info.push(0); // children none
    }
    info.push(0); // end CU children
    let cu_len = (info.len() - cu_start - 4) as u32;
    info[cu_start..cu_start + 4].copy_from_slice(&cu_len.to_le_bytes());

    // 最小 line number program（单行源文件映射）
    let mut line = Vec::new();
    line.extend_from_slice(&16u32.to_le_bytes()); // prologue length
    line.extend_from_slice(&1u8.to_le_bytes()); // min insn length
    line.extend_from_slice(&[1u8, 1u8, 1u8, 1u8]); // default/max/line_base/line_range
    line.extend_from_slice(&0u8.to_le_bytes()); // opcode_base
    line.extend_from_slice(&[0u8; 9]); // std opcode lengths
    line.extend_from_slice(&[0u8]); // include dirs
    line.extend_from_slice(&[0u8]); // files
    line.extend_from_slice(&[0u8]); // end sequence

    DebugSections { abbrev, info, line, str: strtab }
}

/// 将 DWARF 节转为 ELF 自定义节列表。
pub fn dwarf_elf_sections(dbg: &DebugSections) -> Vec<ElfSection> {
    vec![
        elf_section(".debug_abbrev", &dbg.abbrev),
        elf_section(".debug_info", &dbg.info),
        elf_section(".debug_line", &dbg.line),
        elf_section(".debug_str", &dbg.str),
    ]
}

/// 生成最小 BTF 头（magic 0xeB9F），供 pahole/llvm-objcopy 探测。
pub fn build_btf_header() -> Vec<u8> {
    let mut btf = Vec::new();
    btf.extend_from_slice(&0xEB9F_u16.to_le_bytes()); // BTF_MAGIC
    btf.extend_from_slice(&1u8.to_le_bytes()); // BTF_VERSION
    btf.extend_from_slice(&0u32.to_le_bytes()); // flags
    btf.extend_from_slice(&24u32.to_le_bytes()); // hdr_len
    btf.extend_from_slice(&24u32.to_le_bytes()); // type_off
    btf.extend_from_slice(&24u32.to_le_bytes()); // type_len
    btf.extend_from_slice(&24u32.to_le_bytes()); // str_off
    btf.extend_from_slice(&1u32.to_le_bytes()); // str_len
    btf.push(0); // empty strtab
    btf
}

/// 生成 .eh_frame 最小 CFI 字节（end-of-CIE 标记）。
pub fn build_eh_frame() -> Vec<u8> {
    // CIE length=4, CIE id=0xffffffff, version=1, augmentation="", code_align=1, data_align=-4, ra=16
    let mut eh = Vec::new();
    eh.extend_from_slice(&16u32.to_le_bytes());
    eh.extend_from_slice(&0xffffffffu32.to_le_bytes());
    eh.push(1);
    eh.push(0); // augmentation ""
    eh.extend_from_slice(&1u8.to_le_bytes()); // code alignment
    eh.extend_from_slice(&((-4i32) as u32).to_le_bytes());
    eh.push(16); // return address column
    eh.push(0); // augmentation data
    eh.extend_from_slice(&0u32.to_le_bytes()); // CIE end
    eh
}

/// ORC 元数据占位节（objtool 探测用）。
pub fn build_orc_unwind_ip() -> Vec<u8> {
    vec![0u8; 8]
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
