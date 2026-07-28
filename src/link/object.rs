// src/link/object.rs - ELF ET_REL 目标文件读取器
//
// 解析 dcc / gcc 生成的 .o 文件,提取节、符号表、重定位表。
// 完整支持 ELF64 x86-64 / aarch64 / riscv64 ET_REL 文件。

#![allow(dead_code)]

use std::collections::HashMap;

use crate::codegen::elf::{
    SHN_UNDEF, SHT_NULL, SHT_PROGBITS, SHT_SYMTAB, SHT_STRTAB, SHT_RELA, SHT_NOBITS,
    ELFCLASS64, ELFMAG, EM_X86_64, EM_AARCH64, EM_RISCV,
};

/// 从 .o 文件节头表中读出的原始字段:
/// (sh_name, sh_type, sh_flags, sh_addr, sh_offset, sh_size, sh_addralign, sh_entsize, sh_info)
type RawShdr = (u32, u32, u64, u64, u64, u64, u64, u64, u32);

/// 从 .o 文件读出的节描述。
#[derive(Debug, Clone)]
pub struct InputSection {
    pub name: String,
    pub ty: u32,           // SHT_PROGBITS / SHT_NOBITS / ...
    pub flags: u32,
    pub align: u64,
    pub data: Vec<u8>,     // NOBITS 时为空
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
    pub bind: u8,          // STB_LOCAL / STB_GLOBAL / STB_WEAK
    pub ty: u8,            // STT_NOTYPE / STT_FUNC / STT_OBJECT / STT_SECTION / STT_TLS
    pub visibility: u8,
    pub section: u16,      // 所在 .o 节索引（SHN_UNDEF=0 表示外部引用）
    pub input_obj: usize,  // 来自第几个 .o
    pub orig_index: u32,   // 在该 .o 的 symtab 中的原始索引
}

/// 来自 .o 文件的重定位条目。
#[derive(Debug, Clone)]
pub struct InputRela {
    pub offset: u64,        // 修补位置（在节内偏移）
    pub sym: u32,           // 符号表中的索引（输入对象内部索引）
    pub rel_type: u32,      // R_X86_64_PC32 / PLT32 / ...
    pub addend: i64,
    pub section: u32,       // 所在 .o 节索引
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
}

impl InputObject {
    pub fn section_data(&self, idx: u32) -> Option<&[u8]> {
        if idx == 0 { return None; }
        let s = self.sections.get(idx as usize)?;
        if s.ty == SHT_NOBITS { return None; }
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

/// 解析一个 ET_REL 目标文件。
pub fn parse_object(data: &[u8], name: &str) -> Result<InputObject, ObjectError> {
    if data.len() < 64 {
        return Err(ObjectError { msg: format!("{}: too short for ELF header", name) });
    }
    if data[0..4] != ELFMAG {
        return Err(ObjectError { msg: format!("{}: bad ELF magic", name) });
    }
    if data[4] != ELFCLASS64 {
        return Err(ObjectError { msg: format!("{}: only ELF64 supported", name) });
    }
    let machine = u16::from_le_bytes(data[18..20].try_into().unwrap());
    if machine != EM_X86_64 && machine != EM_AARCH64 && machine != EM_RISCV {
        return Err(ObjectError { msg: format!("{}: unsupported e_machine={}", name, machine) });
    }
    let e_type = u16::from_le_bytes(data[16..18].try_into().unwrap());
    if e_type != 1 {
        return Err(ObjectError { msg: format!("{}: expected ET_REL (1), got {}", name, e_type) });
    }
    let shoff = u64::from_le_bytes(data[40..48].try_into().unwrap()) as usize;
    let shentsize = u16::from_le_bytes(data[58..60].try_into().unwrap()) as usize;
    let shnum = u16::from_le_bytes(data[60..62].try_into().unwrap()) as usize;
    let shstrndx = u16::from_le_bytes(data[62..64].try_into().unwrap()) as usize;

    if shentsize < 64 {
        return Err(ObjectError { msg: format!("{}: shentsize too small", name) });
    }
    if shnum == 0 || shstrndx >= shnum {
        return Err(ObjectError { msg: format!("{}: invalid shnum/shstrndx", name) });
    }
    if shoff + shnum * shentsize > data.len() {
        return Err(ObjectError { msg: format!("{}: section header table out of bounds", name) });
    }

    // 读取节头表
    let mut raw_sections: Vec<RawShdr> = Vec::with_capacity(shnum);
    for i in 0..shnum {
        let off = shoff + i * shentsize;
        let sh_name = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        let sh_type = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap());
        let sh_flags = u64::from_le_bytes(data[off + 8..off + 16].try_into().unwrap());
        let sh_addr = u64::from_le_bytes(data[off + 16..off + 24].try_into().unwrap());
        let sh_offset = u64::from_le_bytes(data[off + 24..off + 32].try_into().unwrap()) as usize;
        let sh_size = u64::from_le_bytes(data[off + 32..off + 40].try_into().unwrap()) as usize;
        let _sh_link = u32::from_le_bytes(data[off + 40..off + 44].try_into().unwrap());
        let sh_info = u32::from_le_bytes(data[off + 44..off + 48].try_into().unwrap());
        let sh_addralign = u64::from_le_bytes(data[off + 48..off + 56].try_into().unwrap());
        let sh_entsize = u64::from_le_bytes(data[off + 56..off + 64].try_into().unwrap());
        raw_sections.push((sh_name, sh_type, sh_flags, sh_addr, sh_offset as u64, sh_size as u64, sh_addralign, sh_entsize, sh_info));
    }

    // 读取 shstrtab
    let shstr_hdr = &raw_sections[shstrndx];
    let shstrtab = if shstr_hdr.4 + shstr_hdr.5 <= data.len() as u64 {
        &data[shstr_hdr.4 as usize..(shstr_hdr.4 + shstr_hdr.5) as usize]
    } else {
        return Err(ObjectError { msg: format!("{}: shstrtab out of bounds", name) });
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

    // 读取符号表
    let mut symbols = Vec::new();
    if let (Some(sym_i), Some(str_i)) = (symtab_idx, strtab_idx) {
        let sym_hdr = &raw_sections[sym_i as usize];
        let str_hdr = &raw_sections[str_i as usize];
        let entsize = if sym_hdr.7 > 0 { sym_hdr.7 } else { 24 };
        let sym_off = sym_hdr.4 as usize;
        let sym_size = sym_hdr.5 as usize;
        let str_off = str_hdr.4 as usize;
        let str_size = str_hdr.5 as usize;
        if str_off + str_size <= data.len() {
            let strtab = &data[str_off..str_off + str_size];
            let count = sym_size / entsize as usize;
            for i in 0..count {
                let base = sym_off + i * entsize as usize;
                if base + 24 > data.len() { break; }
                let st_name = u32::from_le_bytes(data[base..base + 4].try_into().unwrap());
                let st_info = data[base + 4];
                let st_other = data[base + 5];
                let st_shndx = u16::from_le_bytes(data[base + 6..base + 8].try_into().unwrap());
                let st_value = u64::from_le_bytes(data[base + 8..base + 16].try_into().unwrap());
                let st_size = u64::from_le_bytes(data[base + 16..base + 24].try_into().unwrap());
                let bind = st_info >> 4;
                let ty = st_info & 0xf;
                let sym_name = read_cstr(strtab, st_name as usize).unwrap_or("").to_string();
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

    // 读取所有 .rela.* 节的重定位
    let mut relocations = Vec::new();
    for (i, s) in raw_sections.iter().enumerate() {
        if s.1 != SHT_RELA { continue; }
        let sec_name = &sections[i].name;
        if !sec_name.starts_with(".rela") { continue; }
        let entsize = if s.7 > 0 { s.7 } else { 24 };
        let off = s.4 as usize;
        let size = s.5 as usize;
        let count = size / entsize as usize;
        // sh_info 是被重定位的目标节索引
        let target_sec = s.8;
        for j in 0..count {
            let base = off + j * entsize as usize;
            if base + 24 > data.len() { break; }
            let r_offset = u64::from_le_bytes(data[base..base + 8].try_into().unwrap());
            let r_info = u64::from_le_bytes(data[base + 8..base + 16].try_into().unwrap());
            let r_addend = i64::from_le_bytes(data[base + 16..base + 24].try_into().unwrap());
            let r_sym = (r_info >> 32) as u32;
            let r_type = (r_info & 0xffffffff) as u32;
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

    // 抑制未使用导入的告警
    let _ = SHT_NULL;
    let _ = SHT_PROGBITS;
    let _ = SHT_SYMTAB;
    let _ = SHT_STRTAB;
    let _ = SHN_UNDEF;

    Ok(InputObject {
        machine,
        sections,
        symbols,
        relocations,
        name: name.to_string(),
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
    pub section: u32,        // 在 InputBundle.sections 中的索引
    pub input_obj: usize,
    pub orig_index: u32,
    pub defined: bool,       // 在某个节中定义（section != 0）
}

/// 跨多个 .o 的重定位条目。
#[derive(Debug, Clone)]
pub struct FlatRela {
    pub offset: u64,
    pub sym: u32,            // 跨输入的符号索引
    pub rel_type: u32,
    pub addend: i64,
    pub section: u32,        // 目标节（sections 索引）
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
            return Err(ObjectError { msg: "no input objects".into() });
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
        let mut sym_index_map: HashMap<(usize, u32), u32> = HashMap::new();

        for (oi, obj) in objects.iter().enumerate() {
            for s in &obj.sections {
                sections.push(FlatSection {
                    name: s.name.clone(),
                    ty: s.ty,
                    flags: s.flags,
                    align: s.align,
                    data: s.data.clone(),
                    size: s.size,
                    input_obj: oi,
                    orig_index: s.orig_index,
                });
            }
        }
        for (oi, obj) in objects.iter().enumerate() {
            for sym in &obj.symbols {
                let section = if sym.section == 0 {
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
                    defined: sym.section != 0 && sym.section != 0xffff,
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

        Ok(InputBundle { objects, symbols, relocations, sections, machine })
    }
}

fn find_section_in_bundle(sections: &[FlatSection], input_obj: usize, orig_index: u32) -> Option<u32> {
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
    use crate::codegen::elf::{self, ElfSym, ElfRela, SHT_RELA, SHT_SYMTAB, SHT_STRTAB, STB_GLOBAL, STT_FUNC, STT_NOTYPE, STV_DEFAULT};

    #[test]
    fn parse_minimal_rel_object() {
        let obj = elf::build_elf_rel_x86_64(
            &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3], // mov eax, 42; ret
            &[], &[], 0, &[], 0, 16, 16, 16, 16, 16,
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
        assert_eq!(parsed.sections.len(), 12); // 1 null + 6 standard + 3 rela + symtab + strtab + shstrtab
        let answer = parsed.symbols.iter().find(|s| s.name == "answer").expect("answer");
        assert_eq!(answer.value, 0);
        assert_eq!(answer.size, 6);
        assert_eq!(answer.bind, STB_GLOBAL);
        // 抑制未使用
        let _ = SHT_RELA;
        let _ = SHT_SYMTAB;
        let _ = SHT_STRTAB;
        // 测试无符号的边界
        let empty = elf::build_elf_rel_x86_64(
            &[], &[], &[], 0, &[], 0, 1, 1, 1, 1, 1,
            &[], &[], &[], &[], &[],
        );
        let p2 = parse_object(&empty, "empty.o").expect("parse empty");
        assert!(p2.symbols.is_empty());
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
            16, 16, 16, 16, 16,
            &[ElfSym {
                name: "callee".to_string(),
                section: 0,
                value: 0,
                size: 0,
                bind: STB_GLOBAL,
                ty: STT_FUNC,
                visibility: STV_DEFAULT,
            }],
            &[ElfRela { offset: 1, sym_index: 1, rel_type: 4, addend: -4 }],
            &[], &[], &[],
        );
        let parsed = parse_object(&obj, "rela.o").expect("parse");
        assert!(!parsed.relocations.is_empty());
        assert_eq!(parsed.relocations[0].rel_type, 4);
        let _ = STT_NOTYPE;
    }
}
