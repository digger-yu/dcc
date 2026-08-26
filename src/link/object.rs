// src/link/object.rs - ELF/COFF 目标文件读取器
//
// 解析 dcc / gcc 生成的 .o 文件,提取节、符号表、重定位表。
// 完整支持 ELF64 x86-64 / aarch64 / riscv64 和 ELF32 mips / arm ET_REL 文件。
// 支持 little-endian 和 big-endian (MIPS) 两种字节序。
// 同时支持 COFF/PE AMD64 目标文件 (Windows MSVC codegen 输出)。

#![allow(dead_code)]

use std::collections::HashMap;

use crate::codegen::coff::{
    IMAGE_SCN_CNT_CODE, IMAGE_SCN_CNT_INITIALIZED_DATA, IMAGE_SCN_CNT_UNINITIALIZED_DATA,
    IMAGE_SCN_MEM_WRITE,
};
use crate::codegen::elf::{
    ELFCLASS32, ELFCLASS64, ELFDATA2LSB, ELFDATA2MSB, ELFMAG, EM_AARCH64, EM_ARM, EM_MIPS,
    EM_RISCV, EM_X86_64, SHN_UNDEF, SHT_GROUP, SHT_NOBITS, SHT_NULL, SHT_PROGBITS, SHT_REL,
    SHT_RELA, SHT_STRTAB, SHT_SYMTAB,
};

/// 从 .o 文件节头表中读出的原始字段:
/// (sh_name, sh_type, sh_flags, sh_addr, sh_offset, sh_size, sh_addralign, sh_entsize, sh_info)
type RawShdr = (u32, u32, u64, u64, u64, u64, u64, u64, u32);

/// 从 .o 文件读出的节描述。
#[derive(Debug, Clone)]
pub struct InputSection {
    pub name: String,
    pub ty: u32, // SHT_PROGBITS / SHT_NOBITS / ...
    pub flags: u32,
    pub align: u64,
    pub data: Vec<u8>, // NOBITS 时为空
    pub size: u64,
    /// 该节在所有 .o 输入中的索引（链接后会重新编号）
    pub input_index: u32,
    /// 原始 .o 内的节索引（用于解析重定位）
    pub orig_index: u32,
}

/// 来自 .o 文件的符号。
#[derive(Debug, Clone)]
pub struct InputSymbol {
    pub name: String,
    pub value: u64,
    pub size: u64,
    pub bind: u8, // STB_LOCAL / STB_GLOBAL / STB_WEAK
    pub ty: u8,   // STT_NOTYPE / STT_FUNC / STT_OBJECT / STT_SECTION / STT_TLS
    pub visibility: u8,
    pub section: u16,     // 所在 .o 节索引（SHN_UNDEF=0 表示外部引用）
    pub input_obj: usize, // 来自第几个 .o
    pub orig_index: u32,  // 在该 .o 的 symtab 中的原始索引
}

/// 来自 .o 文件的重定位条目。
#[derive(Debug, Clone)]
pub struct InputRela {
    pub offset: u64,   // 修补位置（在节内偏移）
    pub sym: u32,      // 符号表中的索引（输入对象内部索引）
    pub rel_type: u32, // R_X86_64_PC32 / PLT32 / ...
    pub addend: i64,
    pub section: u32, // 所在 .o 节索引
    pub input_obj: usize,
}

/// COMDAT 组描述。
///
/// SHT_GROUP 节的第一个 4 字节是 flag word (通常为 GRP_COMDAT=1),
/// 后续每个 4 字节是一个节索引 (shndx), 表示该组的成员节。
/// 组签名 (signature) 来自该 SHT_GROUP 节在符号表中关联的符号名,
/// 链接器按签名去重: 相同签名的 COMDAT 组只保留一份。
#[derive(Debug, Clone)]
pub struct ComdatGroup {
    /// 组签名 (来自关联符号名)
    pub signature: String,
    /// 组标志 (GRP_COMDAT = 1)
    pub flags: u32,
    /// 组成员节在本 .o 的 section header 索引列表
    pub member_sec_indices: Vec<u32>,
    /// 来自第几个 .o
    pub input_obj: usize,
}

/// 解析后的 .o 文件。
#[derive(Debug)]
pub struct InputObject {
    pub machine: u16,
    pub sections: Vec<InputSection>,
    pub symbols: Vec<InputSymbol>,
    pub relocations: Vec<InputRela>,
    pub name: String,
    /// COMDAT 组列表 (来自 SHT_GROUP 节)
    pub comdat_groups: Vec<ComdatGroup>,
    /// 每个 section 所属的 COMDAT 组索引 (在 comdat_groups 中), None 表示不属于任何组
    pub section_to_group: Vec<Option<usize>>,
}

impl InputObject {
    pub fn section_data(&self, idx: u32) -> Option<&[u8]> {
        if idx == 0 {
            return None;
        }
        let s = self.sections.get(idx as usize)?;
        if s.ty == SHT_NOBITS {
            return None;
        }
        Some(&s.data)
    }
}

/// 解析失败时的诊断信息。
#[derive(Debug)]
pub struct ObjectError {
    pub msg: String,
}

impl std::fmt::Display for ObjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for ObjectError {}

/// 解析一个 ET_REL 目标文件 (ELF 或 COFF)。
///
/// 自动检测文件格式: ELF (magic 0x7f 'E' 'L' 'F') 或 COFF
/// (machine type 0x8664 for AMD64)。两种格式均解析为统一的
/// `InputObject` 结构，供后续链接使用。
pub fn parse_object(data: &[u8], name: &str) -> Result<InputObject, ObjectError> {
    // Detect format by magic bytes
    if data.len() >= 4 && data[0..4] == ELFMAG {
        parse_elf_object(data, name)
    } else if data.len() >= 20 {
        // COFF: first 2 bytes are machine type
        let machine = u16::from_le_bytes([data[0], data[1]]);
        if machine == 0x8664 {
            // AMD64 COFF
            parse_coff_object(data, name)
        } else {
            Err(ObjectError {
                msg: format!(
                    "{}: unknown object format (not ELF, not COFF AMD64)",
                    name
                ),
            })
        }
    } else {
        Err(ObjectError {
            msg: format!("{}: too short for object header", name),
        })
    }
}

/// 解析 COFF (PE/COFF) AMD64 目标文件。
///
/// COFF 文件头 20 字节, 后跟节头表 (每节 40 字节),
/// 节数据, 重定位, 符号表 (每符号 18 字节), 字符串表。
fn parse_coff_object(data: &[u8], name: &str) -> Result<InputObject, ObjectError> {
    if data.len() < 20 {
        return Err(ObjectError {
            msg: format!("{}: COFF header truncated", name),
        });
    }
    let ru16 = |o: usize| -> u16 {
        u16::from_le_bytes(data[o..o + 2].try_into().unwrap())
    };
    let ru32 = |o: usize| -> u32 {
        u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
    };

    let machine_raw = ru16(0);
    let num_sections = ru16(2) as usize;
    let _timestamp = ru32(4);
    let symtab_ptr = ru32(8) as usize;
    let num_symbols = ru32(12) as usize;
    let opt_hdr_size = ru16(16) as usize;

    if machine_raw != 0x8664 {
        return Err(ObjectError {
            msg: format!("{}: unsupported COFF machine={}", name, machine_raw),
        });
    }

    let section_headers_off = 20 + opt_hdr_size;
    if data.len() < section_headers_off + num_sections * 40 {
        return Err(ObjectError {
            msg: format!("{}: COFF section headers truncated", name),
        });
    }

    // Parse section headers
    let mut sections = Vec::with_capacity(num_sections);
    for i in 0..num_sections {
        let off = section_headers_off + i * 40;
        // Name (8 bytes)
        let name_raw = &data[off..off + 8];
        let sec_name = if name_raw[0] == b'/' {
            // Long name: "/" + decimal offset into string table
            let s = std::str::from_utf8(&name_raw[1..8]).unwrap_or("");
            let end = s.find('\0').unwrap_or(s.len());
            let strtab_off: usize = s[..end].parse().unwrap_or(0);
            // Will resolve after string table is available
            format!("__coff_long_name_{}", strtab_off)
        } else {
            let end = name_raw.iter().position(|&b| b == 0).unwrap_or(8);
            std::str::from_utf8(&name_raw[..end]).unwrap_or("").to_string()
        };

        let _virtual_size = ru32(off + 8);
        let _virtual_addr = ru32(off + 12);
        let raw_data_size = ru32(off + 16) as usize;
        let raw_data_ptr = ru32(off + 20) as usize;
        let reloc_ptr = ru32(off + 24) as usize;
        let _linenum_ptr = ru32(off + 28);
        let num_relocs = ru16(off + 32) as usize;
        let _num_linenums = ru16(off + 34);
        let characteristics = ru32(off + 36);

        let mut sec_data = Vec::new();
        if raw_data_size > 0 && raw_data_ptr + raw_data_size <= data.len() {
            sec_data.extend_from_slice(&data[raw_data_ptr..raw_data_ptr + raw_data_size]);
        }

        // Map COFF characteristics to ELF-like flags
        let (elf_ty, elf_flags) = coff_chars_to_elf(characteristics);

        sections.push(InputSection {
            name: sec_name,
            ty: elf_ty,
            flags: elf_flags,
            align: 4,
            size: raw_data_size as u64,
            data: sec_data,
            input_index: 0,
            orig_index: i as u32,
        });

        // Parse relocations for this section
        let _ = reloc_ptr;
        let _ = num_relocs;
    }

    // Parse symbol table
    let strtab_ptr = symtab_ptr + num_symbols * 18;
    let mut symbols = Vec::with_capacity(num_symbols);
    if symtab_ptr + num_symbols * 18 <= data.len() {
        for i in 0..num_symbols {
            let base = symtab_ptr + i * 18;
            if base + 18 > data.len() {
                break;
            }
            // Name (8 bytes)
            let name_bytes = &data[base..base + 8];
            let sym_name = if u32::from_le_bytes(name_bytes[0..4].try_into().unwrap()) == 0 {
                // Long name: offset in string table
                let strtab_off =
                    u32::from_le_bytes(name_bytes[4..8].try_into().unwrap()) as usize;
                if strtab_ptr + strtab_off < data.len() {
                    read_cstr(data, strtab_ptr + strtab_off)
                        .unwrap_or("")
                        .to_string()
                } else {
                    String::new()
                }
            } else {
                let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(8);
                std::str::from_utf8(&name_bytes[..end]).unwrap_or("").to_string()
            };

            let value = ru32(base + 8) as u64;
            let section_number = i16::from_le_bytes([data[base + 12], data[base + 13]]);
            let coff_type = ru16(base + 14);
            let storage_class = data[base + 16];
            let _num_aux = data[base + 17];

            // Map COFF storage class to ELF bind/type
            let (bind, ty) = coff_storage_to_elf(storage_class, coff_type);

            let elf_section = if section_number <= 0 {
                0u16 // undefined or special
            } else {
                section_number as u16
            };

            symbols.push(InputSymbol {
                name: sym_name,
                value,
                size: 0,
                bind,
                ty,
                visibility: 0,
                section: elf_section,
                input_obj: 0,
                orig_index: i as u32,
            });
        }
    }

    // Parse relocations from all sections
    let mut relocations = Vec::new();
    for i in 0..num_sections {
        let off = section_headers_off + i * 40;
        let reloc_ptr = ru32(off + 24) as usize;
        let num_relocs = ru16(off + 32) as usize;
        let target_sec = i as u32;

        for j in 0..num_relocs {
            let base = reloc_ptr + j * 10;
            if base + 10 > data.len() {
                break;
            }
            let r_offset = ru32(base) as u64;
            let r_sym = ru32(base + 4);
            let r_type = ru16(base + 8);

            // Map COFF relocation type to ELF relocation type
            let elf_rel_type = coff_reloc_to_elf(r_type);

            relocations.push(InputRela {
                offset: r_offset,
                sym: r_sym,
                rel_type: elf_rel_type,
                addend: -4, // COFF REL32 has implicit addend of -4 for PC-relative
                section: target_sec,
                input_obj: 0,
            });
        }
    }

    // Resolve long section names from string table
    if strtab_ptr < data.len() {
        let strtab_len = ru32(strtab_ptr) as usize;
        let strtab_end = (strtab_ptr + strtab_len).min(data.len());
        for sec in &mut sections {
            if sec.name.starts_with("__coff_long_name_") {
                let off_str = &sec.name["__coff_long_name_".len()..];
                if let Ok(strtab_off) = off_str.parse::<usize>() {
                    let abs_off = strtab_ptr + strtab_off;
                    if abs_off < strtab_end {
                        sec.name = read_cstr(data, abs_off).unwrap_or("").to_string();
                    }
                }
            }
        }
    }

    Ok(InputObject {
        machine: 62, // EM_X86_64 — normalize to ELF machine number
        sections,
        symbols,
        relocations,
        name: name.to_string(),
        comdat_groups: Vec::new(),
        section_to_group: Vec::new(),
    })
}

/// Map COFF section characteristics to ELF-like type and flags.
fn coff_chars_to_elf(chars: u32) -> (u32, u32) {
    use crate::codegen::elf::*;
    let mut flags = 0u32;
    let mut ty = SHT_PROGBITS;

    if chars & IMAGE_SCN_CNT_CODE != 0 {
        flags |= SHF_ALLOC | SHF_EXECINSTR;
    } else if chars & IMAGE_SCN_CNT_INITIALIZED_DATA != 0 {
        flags |= SHF_ALLOC;
        if chars & IMAGE_SCN_MEM_WRITE != 0 {
            flags |= SHF_WRITE;
        }
    } else if chars & IMAGE_SCN_CNT_UNINITIALIZED_DATA != 0 {
        flags |= SHF_ALLOC;
        if chars & IMAGE_SCN_MEM_WRITE != 0 {
            flags |= SHF_WRITE;
        }
        ty = SHT_NOBITS;
    }

    (ty, flags)
}

/// Map COFF storage class to ELF bind/type.
fn coff_storage_to_elf(storage_class: u8, coff_type: u16) -> (u8, u8) {
    use crate::codegen::elf::*;
    let bind = match storage_class {
        2 => STB_GLOBAL, // IMAGE_SYM_CLASS_EXTERNAL
        3 => STB_LOCAL,  // IMAGE_SYM_CLASS_STATIC
        _ => STB_GLOBAL,
    };
    let ty = if coff_type == 0x0020 {
        STT_FUNC
    } else {
        STT_NOTYPE
    };
    (bind, ty)
}

/// Map COFF AMD64 relocation type to ELF x86_64 relocation type.
fn coff_reloc_to_elf(coff_type: u16) -> u32 {
    use crate::codegen::elf::*;
    match coff_type {
        0x0001 => R_X86_64_64,       // IMAGE_REL_AMD64_ADDR64
        0x0002 => R_X86_64_32,       // IMAGE_REL_AMD64_ADDR32
        0x0003 => R_X86_64_32S,      // IMAGE_REL_AMD64_ADDR32NB
        0x0004 => R_X86_64_PC32,     // IMAGE_REL_AMD64_REL32
        0x0005 => R_X86_64_PC32,     // IMAGE_REL_AMD64_REL32_1
        0x0006 => R_X86_64_PC32,     // IMAGE_REL_AMD64_REL32_2
        0x0007 => R_X86_64_PC32,     // IMAGE_REL_AMD64_REL32_3
        0x0008 => R_X86_64_PC32,     // IMAGE_REL_AMD64_REL32_4
        0x0009 => R_X86_64_PC32,     // IMAGE_REL_AMD64_REL32_5
        _ => R_X86_64_NONE,
    }
}

/// 解析 ELF ET_REL 目标文件。
fn parse_elf_object(data: &[u8], name: &str) -> Result<InputObject, ObjectError> {
    if data.len() < 52 {
        return Err(ObjectError {
            msg: format!("{}: too short for ELF header", name),
        });
    }
    if data[0..4] != ELFMAG {
        return Err(ObjectError {
            msg: format!("{}: bad ELF magic", name),
        });
    }
    let ei_class = data[4];
    if ei_class != ELFCLASS64 && ei_class != ELFCLASS32 {
        return Err(ObjectError {
            msg: format!("{}: unsupported ELF class={}", name, ei_class),
        });
    }
    let is_64 = ei_class == ELFCLASS64;
    let ei_data = data[5];
    if ei_data != ELFDATA2LSB && ei_data != ELFDATA2MSB {
        return Err(ObjectError {
            msg: format!("{}: unsupported ELF data encoding={}", name, ei_data),
        });
    }
    let is_be = ei_data == ELFDATA2MSB;
    let ru16 = |o: usize| -> u16 {
        let b: [u8; 2] = data[o..o + 2].try_into().unwrap();
        if is_be { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
    };
    let ru32 = |o: usize| -> u32 {
        let b: [u8; 4] = data[o..o + 4].try_into().unwrap();
        if is_be { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    };
    let ru64 = |o: usize| -> u64 {
        let b: [u8; 8] = data[o..o + 8].try_into().unwrap();
        if is_be { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) }
    };
    let machine = ru16(18);
    if machine != EM_X86_64
        && machine != EM_AARCH64
        && machine != EM_RISCV
        && machine != EM_MIPS
        && machine != EM_ARM
    {
        return Err(ObjectError {
            msg: format!("{}: unsupported e_machine={}", name, machine),
        });
    }
    let e_type = ru16(16);
    if e_type != 1 {
        return Err(ObjectError {
            msg: format!("{}: expected ET_REL (1), got {}", name, e_type),
        });
    }
    let (shoff, shentsize, shnum, shstrndx) = if is_64 {
        if data.len() < 64 {
            return Err(ObjectError {
                msg: format!("{}: ELF64 header truncated", name),
            });
        }
        (
            ru64(40) as usize,
            ru16(58) as usize,
            ru16(60) as usize,
            ru16(62) as usize,
        )
    } else {
        if data.len() < 52 {
            return Err(ObjectError {
                msg: format!("{}: ELF32 header truncated", name),
            });
        }
        (
            ru32(32) as usize,
            ru16(46) as usize,
            ru16(48) as usize,
            ru16(50) as usize,
        )
    };
    let min_shentsize = if is_64 { 64 } else { 40 };
    if shentsize < min_shentsize {
        return Err(ObjectError {
            msg: format!("{}: shentsize too small", name),
        });
    }
    if shnum == 0 || shstrndx >= shnum {
        return Err(ObjectError {
            msg: format!("{}: invalid shnum/shstrndx", name),
        });
    }
    if shoff + shnum * shentsize > data.len() {
        return Err(ObjectError {
            msg: format!("{}: section header table out of bounds", name),
        });
    }

    // 读取节头表 (ELF32: 40 bytes/entry, ELF64: 64 bytes/entry)
    let mut raw_sections: Vec<RawShdr> = Vec::with_capacity(shnum);
    for i in 0..shnum {
        let off = shoff + i * shentsize;
        if is_64 {
            let sh_name = ru32(off);
            let sh_type = ru32(off + 4);
            let sh_flags = ru64(off + 8);
            let sh_addr = ru64(off + 16);
            let sh_offset = ru64(off + 24) as usize;
            let sh_size = ru64(off + 32) as usize;
            let _sh_link = ru32(off + 40);
            let sh_info = ru32(off + 44);
            let sh_addralign = ru64(off + 48);
            let sh_entsize = ru64(off + 56);
            raw_sections.push((
                sh_name, sh_type, sh_flags, sh_addr,
                sh_offset as u64, sh_size as u64,
                sh_addralign, sh_entsize, sh_info,
            ));
        } else {
            let sh_name = ru32(off);
            let sh_type = ru32(off + 4);
            let sh_flags = ru32(off + 8) as u64;
            let sh_addr = ru32(off + 12) as u64;
            let sh_offset = ru32(off + 16) as usize;
            let sh_size = ru32(off + 20) as usize;
            let _sh_link = ru32(off + 24);
            let sh_info = ru32(off + 28);
            let sh_addralign = ru32(off + 32) as u64;
            let sh_entsize = ru32(off + 36) as u64;
            raw_sections.push((
                sh_name, sh_type, sh_flags, sh_addr,
                sh_offset as u64, sh_size as u64,
                sh_addralign, sh_entsize, sh_info,
            ));
        }
    }

    // 读取 shstrtab
    let shstr_hdr = &raw_sections[shstrndx];
    let shstrtab = if shstr_hdr.4 + shstr_hdr.5 <= data.len() as u64 {
        &data[shstr_hdr.4 as usize..(shstr_hdr.4 + shstr_hdr.5) as usize]
    } else {
        return Err(ObjectError {
            msg: format!("{}: shstrtab out of bounds", name),
        });
    };

    // 收集节名与 symtab/strtab
    let mut sections = Vec::with_capacity(shnum);
    let mut symtab_idx: Option<u32> = None;
    let mut strtab_idx: Option<u32> = None;
    for (i, s) in raw_sections.iter().enumerate() {
        let sec_name = read_cstr(shstrtab, s.0 as usize).unwrap_or("").to_string();
        if sec_name == ".symtab" && symtab_idx.is_none() {
            symtab_idx = Some(i as u32);
        } else if sec_name == ".strtab" && strtab_idx.is_none() {
            strtab_idx = Some(i as u32);
        }
        let mut data_vec = Vec::new();
        if s.1 != SHT_NOBITS && s.4 + s.5 <= data.len() as u64 {
            data_vec.extend_from_slice(&data[s.4 as usize..(s.4 + s.5) as usize]);
        }
        sections.push(InputSection {
            name: sec_name,
            ty: s.1,
            flags: s.2 as u32,
            align: if s.6 == 0 { 1 } else { s.6 },
            size: s.5,
            data: data_vec,
            input_index: 0,
            orig_index: i as u32,
        });
    }

    // 读取符号表 (ELF32: 16 bytes/entry, ELF64: 24 bytes/entry)
    let mut symbols = Vec::new();
    if let (Some(sym_i), Some(str_i)) = (symtab_idx, strtab_idx) {
        let sym_hdr = &raw_sections[sym_i as usize];
        let str_hdr = &raw_sections[str_i as usize];
        let default_entsize: u64 = if is_64 { 24 } else { 16 };
        let entsize = if sym_hdr.7 > 0 { sym_hdr.7 } else { default_entsize };
        let sym_off = sym_hdr.4 as usize;
        let sym_size = sym_hdr.5 as usize;
        let str_off = str_hdr.4 as usize;
        let str_size = str_hdr.5 as usize;
        if str_off + str_size <= data.len() {
            let strtab = &data[str_off..str_off + str_size];
            let count = sym_size / entsize as usize;
            for i in 0..count {
                let base = sym_off + i * entsize as usize;
                let min_sz = if is_64 { 24 } else { 16 };
                if base + min_sz > data.len() {
                    break;
                }
                let (st_name, st_info, st_other, st_shndx, st_value, st_size) = if is_64 {
                    let n = ru32(base);
                    let info = data[base + 4];
                    let other = data[base + 5];
                    let shndx = ru16(base + 6);
                    let val = ru64(base + 8);
                    let sz = ru64(base + 16);
                    (n, info, other, shndx, val, sz)
                } else {
                    let n = ru32(base);
                    let info = data[base + 4];
                    let other = data[base + 5];
                    let shndx = ru16(base + 6);
                    let val = ru32(base + 8) as u64;
                    let sz = ru32(base + 12) as u64;
                    (n, info, other, shndx, val, sz)
                };
                let bind = st_info >> 4;
                let ty = st_info & 0xf;
                let sym_name = read_cstr(strtab, st_name as usize)
                    .unwrap_or("")
                    .to_string();
                symbols.push(InputSymbol {
                    name: sym_name,
                    value: st_value,
                    size: st_size,
                    bind,
                    ty,
                    visibility: st_other & 0x3,
                    section: st_shndx,
                    input_obj: 0,
                    orig_index: i as u32,
                });
            }
        }
    }

    // 读取所有 .rela.* 和 .rel.* 节的重定位
    let mut relocations = Vec::new();
    for (i, s) in raw_sections.iter().enumerate() {
        let is_rela = s.1 == SHT_RELA;
        let is_rel = s.1 == SHT_REL;
        if !is_rela && !is_rel {
            continue;
        }
        let sec_name = &sections[i].name;
        if !sec_name.starts_with(".rel") {
            continue;
        }
        let default_entsize: u64 = if is_rela {
            if is_64 { 24 } else { 12 }
        } else {
            if is_64 { 16 } else { 8 }
        };
        let entsize = if s.7 > 0 { s.7 } else { default_entsize };
        let off = s.4 as usize;
        let size = s.5 as usize;
        let count = size / entsize as usize;
        // sh_info 是被重定位的目标节索引
        let target_sec = s.8;
        for j in 0..count {
            let base = off + j * entsize as usize;
            if is_64 {
                if base + entsize as usize > data.len() {
                    break;
                }
                let r_offset = ru64(base);
                let r_info = ru64(base + 8);
                let r_sym = (r_info >> 32) as u32;
                let r_type = (r_info & 0xffffffff) as u32;
                let r_addend = if is_rela {
                    i64::from_le_bytes(
                        data[base + 16..base + 24].try_into().unwrap(),
                    )
                } else {
                    0i64
                };
                relocations.push(InputRela {
                    offset: r_offset,
                    sym: r_sym,
                    rel_type: r_type,
                    addend: r_addend,
                    section: target_sec,
                    input_obj: 0,
                });
            } else {
                if base + entsize as usize > data.len() {
                    break;
                }
                let r_offset = ru32(base) as u64;
                let r_info = ru32(base + 4);
                // ELF32: sym = info >> 8, type = info & 0xff
                let r_sym = r_info >> 8;
                let r_type = r_info & 0xff;
                let r_addend = if is_rela {
                    i64::from_le_bytes(
                        data[base + 8..base + 12].try_into().unwrap(),
                    )
                } else {
                    0i64
                };
                relocations.push(InputRela {
                    offset: r_offset,
                    sym: r_sym,
                    rel_type: r_type,
                    addend: r_addend,
                    section: target_sec,
                    input_obj: 0,
                });
            }
        }
    }

    // 抑制未使用导入的告警
    let _ = SHT_NULL;
    let _ = SHT_PROGBITS;
    let _ = SHT_SYMTAB;
    let _ = SHT_STRTAB;
    let _ = SHN_UNDEF;

    // 解析 SHT_GROUP 节 (COMDAT 组)
    let mut comdat_groups: Vec<ComdatGroup> = Vec::new();
    // section_to_group: 每个 InputSection 对应的 comdat_groups 索引
    let mut section_to_group: Vec<Option<usize>> = vec![None; sections.len()];

    for (grp_sec_idx, raw) in raw_sections.iter().enumerate() {
        if raw.1 != SHT_GROUP {
            continue;
        }
        // raw = (sh_name, sh_type, sh_flags, sh_addr, sh_offset, sh_size,
        //        sh_addralign, sh_entsize, sh_info)
        let grp_offset = raw.4 as usize;
        let grp_size = raw.5 as usize;
        let sh_info = raw.8; // signature symbol index in symtab
        let _sh_link = raw.4; // we use the already-found symtab/strtab

        if grp_offset + grp_size > data.len() || grp_size < 4 {
            continue; // truncated or empty group
        }

        // 读取 flag word (第一个 4 字节)
        let flags = ru32(grp_offset);

        // 读取成员节索引 (后续每 4 字节一个)
        let mut member_sec_indices = Vec::new();
        let mut pos = grp_offset + 4;
        while pos + 4 <= grp_offset + grp_size {
            let member_idx = ru32(pos);
            member_sec_indices.push(member_idx);
            pos += 4;
        }

        // 查找签名符号名
        let signature = if let (Some(sym_i), Some(str_i)) = (symtab_idx, strtab_idx) {
            let sym_hdr = &raw_sections[sym_i as usize];
            let str_hdr = &raw_sections[str_i as usize];
            let default_entsize: u64 = if is_64 { 24 } else { 16 };
            let entsize = if sym_hdr.7 > 0 { sym_hdr.7 } else { default_entsize };
            let sym_off = sym_hdr.4 as usize;
            let str_off = str_hdr.4 as usize;
            let str_size = str_hdr.5 as usize;
            let sym_base = sym_off + sh_info as usize * entsize as usize;
            let min_sz = if is_64 { 24 } else { 16 };
            if sym_base + min_sz <= data.len() && str_off + str_size <= data.len() {
                let st_name = ru32(sym_base);
                let strtab = &data[str_off..str_off + str_size];
                read_cstr(strtab, st_name as usize)
                    .unwrap_or("")
                    .to_string()
            } else {
                format!(".group.{}", grp_sec_idx)
            }
        } else {
            format!(".group.{}", grp_sec_idx)
        };

        let grp_idx = comdat_groups.len();

        // 标记成员节所属的组 (在 move 之前)
        for &member_idx in &member_sec_indices {
            if let Some(stg) = section_to_group.get_mut(member_idx as usize) {
                *stg = Some(grp_idx);
            }
        }

        comdat_groups.push(ComdatGroup {
            signature,
            flags,
            member_sec_indices,
            input_obj: 0, // will be set by InputBundle
        });
    }

    Ok(InputObject {
        machine,
        sections,
        symbols,
        relocations,
        name: name.to_string(),
        comdat_groups,
        section_to_group,
    })
}

/// 将 NUL 结尾的 C 字符串从 `data` 的 `off` 偏移处读取。
pub fn read_cstr(data: &[u8], off: usize) -> Option<&str> {
    let slice = data.get(off..)?;
    let end = slice.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&slice[..end]).ok()
}

/// 多 .o 输入合并容器。
#[derive(Debug)]
pub struct InputBundle {
    pub objects: Vec<InputObject>,
    /// 全部符号的扁平列表
    pub symbols: Vec<FlatSymbol>,
    /// 全部重定位的扁平列表
    pub relocations: Vec<FlatRela>,
    /// 全部节的扁平列表
    pub sections: Vec<FlatSection>,
    pub machine: u16,
    /// 扁平化的 COMDAT 组列表
    pub comdat_groups: Vec<FlatComdatGroup>,
}

/// 跨多个 .o 的符号条目。
#[derive(Debug, Clone)]
pub struct FlatSymbol {
    pub name: String,
    pub value: u64,
    pub size: u64,
    pub bind: u8,
    pub ty: u8,
    pub visibility: u8,
    pub section: u32, // 在 InputBundle.sections 中的索引
    pub input_obj: usize,
    pub orig_index: u32,
    pub defined: bool, // 在某个节中定义（section != 0）
    /// 是否为 COMMON 符号 (st_shndx == SHN_COMMON).
    /// COMMON 符号是未初始化的 tentative definition,
    /// 链接时合并到 .bss 并取最大 size.
    pub is_common: bool,
}

/// 跨多个 .o 的重定位条目。
#[derive(Debug, Clone)]
pub struct FlatRela {
    pub offset: u64,
    pub sym: u32, // 跨输入的符号索引
    pub rel_type: u32,
    pub addend: i64,
    pub section: u32, // 目标节（sections 索引）
    pub input_obj: usize,
}

/// 跨多个 .o 的节条目。
#[derive(Debug, Clone)]
pub struct FlatSection {
    pub name: String,
    pub ty: u32,
    pub flags: u32,
    pub align: u64,
    pub data: Vec<u8>,
    pub size: u64,
    pub input_obj: usize,
    pub orig_index: u32,
    /// COMDAT 组签名 (如果该节属于 COMDAT 组)
    pub comdat_signature: Option<String>,
}

/// 扁平化的 COMDAT 组条目。
#[derive(Debug, Clone)]
pub struct FlatComdatGroup {
    /// 组签名
    pub signature: String,
    /// 组标志 (GRP_COMDAT = 1)
    pub flags: u32,
    /// 组成员在 FlatSection 中的索引列表
    pub member_indices: Vec<u32>,
    /// 来自第几个 .o
    pub input_obj: usize,
}

impl InputBundle {
    /// 从多个 .o 文件路径构建统一的输入 bundle。
    pub fn from_files(paths: &[std::path::PathBuf]) -> Result<Self, ObjectError> {
        let mut objects = Vec::with_capacity(paths.len());
        for p in paths {
            let bytes = std::fs::read(p).map_err(|e| ObjectError {
                msg: format!("cannot read {}: {}", p.display(), e),
            })?;
            let name = p.display().to_string();
            let mut obj = parse_object(&bytes, &name)?;
            obj.name = name;
            objects.push(obj);
        }
        if objects.is_empty() {
            return Err(ObjectError {
                msg: "no input objects".into(),
            });
        }
        let machine = objects[0].machine;
        for o in &objects[1..] {
            if o.machine != machine {
                return Err(ObjectError {
                    msg: format!("machine mismatch: {} vs {}", objects[0].name, o.name),
                });
            }
        }

        // 扁平化
        let mut sections = Vec::new();
        let mut symbols = Vec::new();
        let mut relocations = Vec::new();
        let mut comdat_groups_flat: Vec<FlatComdatGroup> = Vec::new();
        let mut sym_index_map: HashMap<(usize, u32), u32> = HashMap::new();

        for (oi, obj) in objects.iter().enumerate() {
            for (si, s) in obj.sections.iter().enumerate() {
                // 查找该节所属的 COMDAT 组签名
                let comdat_signature = obj
                    .section_to_group
                    .get(si)
                    .and_then(|g| *g)
                    .and_then(|gidx| obj.comdat_groups.get(gidx))
                    .map(|g| g.signature.clone());
                sections.push(FlatSection {
                    name: s.name.clone(),
                    ty: s.ty,
                    flags: s.flags,
                    align: s.align,
                    data: s.data.clone(),
                    size: s.size,
                    input_obj: oi,
                    orig_index: s.orig_index,
                    comdat_signature,
                });
            }
        }
        // 扁平化 COMDAT 组: 将成员节索引从原始 .o 索引转换为 FlatSection 索引
        for (oi, obj) in objects.iter().enumerate() {
            for grp in &obj.comdat_groups {
                let mut member_indices = Vec::new();
                for &member_sec_idx in &grp.member_sec_indices {
                    if let Some(flat_idx) =
                        find_section_in_bundle(&sections, oi, member_sec_idx)
                    {
                        member_indices.push(flat_idx);
                    }
                }
                comdat_groups_flat.push(FlatComdatGroup {
                    signature: grp.signature.clone(),
                    flags: grp.flags,
                    member_indices,
                    input_obj: oi,
                });
            }
        }
        for (oi, obj) in objects.iter().enumerate() {
            for sym in &obj.symbols {
                let is_common = sym.section == crate::codegen::elf::SHN_COMMON;
                let section = if sym.section == 0 || is_common {
                    0u32
                } else {
                    // 把 (input_obj, orig_index) 映射到扁平 sections 索引
                    find_section_in_bundle(&sections, oi, sym.section as u32).unwrap_or(0)
                };
                let global_idx = symbols.len() as u32;
                sym_index_map.insert((oi, sym.orig_index), global_idx);
                symbols.push(FlatSymbol {
                    name: sym.name.clone(),
                    value: sym.value,
                    size: sym.size,
                    bind: sym.bind,
                    ty: sym.ty,
                    visibility: sym.visibility,
                    section,
                    input_obj: oi,
                    orig_index: sym.orig_index,
                    // COMMON 符号不在真实节中, 链接时合并到 .bss
                    defined: sym.section != 0
                        && sym.section != 0xffff
                        && !is_common,
                    is_common,
                });
            }
        }
        for (oi, obj) in objects.iter().enumerate() {
            for r in &obj.relocations {
                let target_section = find_section_in_bundle(&sections, oi, r.section).unwrap_or(0);
                let sym_global = *sym_index_map.get(&(oi, r.sym)).unwrap_or(&0);
                relocations.push(FlatRela {
                    offset: r.offset,
                    sym: sym_global,
                    rel_type: r.rel_type,
                    addend: r.addend,
                    section: target_section,
                    input_obj: oi,
                });
            }
        }

        Ok(InputBundle {
            objects,
            symbols,
            relocations,
            sections,
            machine,
            comdat_groups: comdat_groups_flat,
        })
    }
}

fn find_section_in_bundle(
    sections: &[FlatSection],
    input_obj: usize,
    orig_index: u32,
) -> Option<u32> {
    for (i, s) in sections.iter().enumerate() {
        if s.input_obj == input_obj && s.orig_index == orig_index {
            return Some(i as u32);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf::{
        self, ElfRela, ElfSym, SHT_RELA, SHT_STRTAB, SHT_SYMTAB, STB_GLOBAL, STT_FUNC, STT_NOTYPE,
        STV_DEFAULT,
    };

    #[test]
    fn parse_minimal_rel_object() {
        let obj = elf::build_elf_rel_x86_64(
            &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3], // mov eax, 42; ret
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &[ElfSym {
                name: "answer".to_string(),
                section: 1,
                value: 0,
                size: 6,
                bind: STB_GLOBAL,
                ty: STT_FUNC,
                visibility: STV_DEFAULT,
            }],
            &[],
            &[],
            &[],
            &[],
        );
        let parsed = parse_object(&obj, "min.o").expect("parse");
        assert_eq!(parsed.machine, 62);
        assert_eq!(parsed.sections.len(), 11); // 1 null + 6 standard + 1 rela.text + symtab + strtab + shstrtab
        let answer = parsed
            .symbols
            .iter()
            .find(|s| s.name == "answer")
            .expect("answer");
        assert_eq!(answer.value, 0);
        assert_eq!(answer.size, 6);
        assert_eq!(answer.bind, STB_GLOBAL);
        // 抑制未使用
        let _ = SHT_RELA;
        let _ = SHT_SYMTAB;
        let _ = SHT_STRTAB;
        // 测试无符号的边界
        let empty = elf::build_elf_rel_x86_64(
            &[],
            &[],
            &[],
            0,
            &[],
            0,
            1,
            1,
            1,
            1,
            1,
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let p2 = parse_object(&empty, "empty.o").expect("parse empty");
        assert!(p2.symbols.is_empty());
    }

    #[test]
    fn accepts_all_supported_object_classes_and_machines() {
        // ELF64 little-endian targets
        let aarch64 = elf::build_elf_rel_aarch64(&[0], &[], &[], &[], &[], &[]);
        assert!(parse_object(&aarch64, "aarch64.o").is_ok());

        let riscv64 = elf::build_elf_rel_riscv64(&[0], &[], &[], &[], &[], &[]);
        assert!(parse_object(&riscv64, "riscv64.o").is_ok());

        let x86_64 = elf::build_elf_rel_x86_64(
            &[0], &[], &[], 0, &[], 0, 1, 1, 1, 1, 1,
            &[], &[], &[], &[], &[],
        );
        assert!(parse_object(&x86_64, "x86_64.o").is_ok());

        // ELF32 little-endian: ARM
        let arm32 = elf::build_elf32_arm(&[0], &[], &[], &[], &[], &[]);
        let parsed_arm = parse_object(&arm32, "arm32.o").expect("parse arm32");
        assert_eq!(parsed_arm.machine, elf::EM_ARM);

        // ELF32 "big-endian" (dcc writer uses LE data with MSB flag): MIPS
        let mips32 = elf::build_elf32_mips(&[0], &[], &[], &[], &[], &[]);
        let parsed_mips = parse_object(&mips32, "mips32.o").expect("parse mips32");
        assert_eq!(parsed_mips.machine, elf::EM_MIPS);
    }

    #[test]
    fn rejects_truly_unsupported_machines() {
        // Craft an ELF64 with an unknown e_machine (e.g. 999)
        let mut bad = elf::build_elf_rel_x86_64(
            &[0], &[], &[], 0, &[], 0, 1, 1, 1, 1, 1,
            &[], &[], &[], &[], &[],
        );
        bad[18..20].copy_from_slice(&999u16.to_le_bytes());
        let err = parse_object(&bad, "bad.o").unwrap_err();
        assert!(err.to_string().contains("unsupported e_machine=999"));
    }

    #[test]
    fn parse_rela_collects_relocations() {
        let obj = elf::build_elf_rel_x86_64(
            &[0xe8, 0, 0, 0, 0, 0xc3],
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &[ElfSym {
                name: "callee".to_string(),
                section: 0,
                value: 0,
                size: 0,
                bind: STB_GLOBAL,
                ty: STT_FUNC,
                visibility: STV_DEFAULT,
            }],
            &[ElfRela {
                offset: 1,
                sym_index: 1,
                rel_type: 4,
                addend: -4,
            }],
            &[],
            &[],
            &[],
        );
        let parsed = parse_object(&obj, "rela.o").expect("parse");
        assert!(!parsed.relocations.is_empty());
        assert_eq!(parsed.relocations[0].rel_type, 4);
        let _ = STT_NOTYPE;
    }
}
