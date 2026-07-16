// src/codegen/elf.rs - ELF 生成器 (suckless 风格)
// 第一性原理:
//   ET_EXEC: 直接可执行的静态 ELF, 不需要 ld/glibc
//   ET_REL:  可重定位目标文件 (.o), 供 ld/gcc 链接
// suckless 准则: 手写 ELF 头,最小必要代码。

#![allow(dead_code)]


// ===== ELF 常量 =====
pub const ELFMAG:    [u8; 4] = [0x7f, b'E', b'L', b'F'];
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const EV_CURRENT: u8 = 1;
pub const ET_EXEC:    u16 = 2;
pub const ET_REL:     u16 = 1;
pub const EM_X86_64:  u16 = 62;
pub const EM_AARCH64: u16 = 183;
pub const EM_RISCV:   u16 = 243;

pub const PT_LOAD: u32 = 1;
pub const PF_X: u32 = 1; pub const PF_W: u32 = 2; pub const PF_R: u32 = 4;

// 节类型
pub const SHT_NULL: u32 = 0;
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_RELA: u32 = 4;
pub const SHT_NOBITS: u32 = 8;

// 节标志
pub const SHF_ALLOC: u32 = 2;
pub const SHF_EXECINSTR: u32 = 4;
pub const SHF_WRITE: u32 = 1;
pub const SHF_INFO_LINK: u32 = 0x40;

// 符号绑定
pub const STB_LOCAL: u8 = 0;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;

// 符号 visibility
pub const STV_DEFAULT: u8 = 0;
pub const STV_INTERNAL: u8 = 1;
pub const STV_HIDDEN: u8 = 2;
pub const STV_PROTECTED: u8 = 3;

// 符号类型
pub const STT_NOTYPE: u8 = 0;
pub const STT_FUNC: u8 = 2;
pub const STT_SECTION: u8 = 3;
pub const STT_OBJECT: u8 = 1;

// 特殊节索引
pub const SHN_UNDEF: u16 = 0;

// x86-64 重定位类型
pub const R_X86_64_PC32: u32 = 2;
pub const R_X86_64_PLT32: u32 = 4;  // 用于函数调用 (PIE 兼容)
pub const R_X86_64_32: u32 = 10;
pub const R_X86_64_32S: u32 = 11;
pub const R_X86_64_64: u32 = 1;
pub const R_X86_64_GOTPCREL: u32 = 9;

// AArch64 重定位类型
pub const R_AARCH64_CALL26: u32 = 283;
pub const R_AARCH64_JUMP26: u32 = 282;

// ===== 数据结构 =====

/// 符号表条目 (供 .o 使用)
#[derive(Clone, Debug)]
pub struct ElfSym {
    pub name: String,
    pub section: u16,   // 节索引 (0=SHN_UNDEF)
    pub value: u64,     // 节内偏移
    pub size: u64,
    pub bind: u8,       // STB_LOCAL / STB_GLOBAL / STB_WEAK
    pub ty: u8,         // STT_NOTYPE / STT_FUNC / STT_OBJECT
    pub visibility: u8, // STV_DEFAULT / STV_HIDDEN / STV_PROTECTED
}

/// 重定位条目 (供 .rela.text 使用)
#[derive(Clone, Debug)]
pub struct ElfRela {
    pub offset: u64,    // .text 内修补位置
    pub sym_index: u32, // .symtab 中的符号索引
    pub rel_type: u32,  // R_X86_64_PC32 等
    pub addend: i64,
}

/// 自定义 ELF 节 (用于 section 属性)
#[derive(Clone, Debug)]
pub struct ElfSection {
    pub name: String,
    pub data: Vec<u8>,
    pub ty: u32,      // SHT_PROGBITS / SHT_NOBITS
    pub flags: u32,   // SHF_ALLOC | SHF_WRITE | SHF_EXECINSTR
    pub align: u64,
}

/// 额外 .rela 节 (用于 .init_array 等自定义节的重定位)
#[derive(Clone, Debug)]
pub struct ExtraRelaSection {
    pub name: String,         // e.g. ".rela.init_array"
    pub relocations: Vec<ElfRela>,
    pub target_sec_index: u32, // sh_info: 目标节索引
}

// ===== ET_EXEC 构建 (现有逻辑) =====

pub fn build_elf_x86_64(code: &[u8], entry: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(code.len() + 256);
    // ===== ELF Header (64 bytes) =====
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);
    out.push(ELFDATA2LSB);
    out.push(EV_CURRENT);
    out.push(0);              // ei_osabi
    out.extend_from_slice(&[0u8; 8]); // ei_pad
    out.extend_from_slice(&ET_EXEC.to_le_bytes());
    out.extend_from_slice(&EM_X86_64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());   // e_version
    out.extend_from_slice(&entry.to_le_bytes());
    out.extend_from_slice(&64u64.to_le_bytes());  // e_phoff
    out.extend_from_slice(&0u64.to_le_bytes());   // e_shoff
    out.extend_from_slice(&0u32.to_le_bytes());   // e_flags
    out.extend_from_slice(&64u16.to_le_bytes());  // e_ehsize
    out.extend_from_slice(&56u16.to_le_bytes());  // e_phentsize
    out.extend_from_slice(&1u16.to_le_bytes());   // e_phnum
    out.extend_from_slice(&0u16.to_le_bytes());   // e_shentsize
    out.extend_from_slice(&0u16.to_le_bytes());   // e_shnum
    out.extend_from_slice(&0u16.to_le_bytes());   // e_shstrndx

    // ===== Program Header =====
    let p_vaddr  = 0x400000u64;
    let p_filesz = (64 + 56) as u64 + code.len() as u64;
    out.extend_from_slice(&PT_LOAD.to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());     // p_flags (PF_R|PF_W|PF_X)
    out.extend_from_slice(&0u64.to_le_bytes());     // p_offset
    out.extend_from_slice(&p_vaddr.to_le_bytes());
    out.extend_from_slice(&p_vaddr.to_le_bytes());  // p_paddr
    out.extend_from_slice(&p_filesz.to_le_bytes());
    out.extend_from_slice(&p_filesz.to_le_bytes()); // p_memsz
    out.extend_from_slice(&0x1000u64.to_le_bytes());

    // ===== Code =====
    out.extend_from_slice(code);
    out
}

/// AArch64 ET_EXEC（M7 hello world）
pub fn build_elf_aarch64(code: &[u8], entry: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(code.len() + 256);
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);
    out.push(ELFDATA2LSB);
    out.push(EV_CURRENT);
    out.push(0);
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&ET_EXEC.to_le_bytes());
    out.extend_from_slice(&EM_AARCH64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&entry.to_le_bytes());
    out.extend_from_slice(&64u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&56u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    let p_vaddr = 0x400000u64;
    let p_filesz = (64 + 56) as u64 + code.len() as u64;
    out.extend_from_slice(&PT_LOAD.to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&p_vaddr.to_le_bytes());
    out.extend_from_slice(&p_vaddr.to_le_bytes());
    out.extend_from_slice(&p_filesz.to_le_bytes());
    out.extend_from_slice(&p_filesz.to_le_bytes());
    out.extend_from_slice(&0x1000u64.to_le_bytes());
    out.extend_from_slice(code);
    out
}

/// AArch64 ET_REL 最小 .o（仅 .text + symtab）
pub fn build_elf_rel_aarch64(
    text: &[u8],
    symbols: &[ElfSym],
    relocations: &[ElfRela],
) -> Vec<u8> {
    let mut shstr = vec![0u8];
    let mut strtab = vec![0u8];
    let sec_name_off = |buf: &mut Vec<u8>, name: &str| -> u32 {
        let off = buf.len() as u32;
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
        off
    };
    let text_name = sec_name_off(&mut shstr, ".text");
    let symtab_name = sec_name_off(&mut shstr, ".symtab");
    let strtab_name = sec_name_off(&mut shstr, ".strtab");
    let shstrtab_name = sec_name_off(&mut shstr, ".shstrtab");
    let rela_name = sec_name_off(&mut shstr, ".rela.text");

    let mut sym_name_off = vec![0u32];
    for sym in symbols {
        sym_name_off.push(strtab.len() as u32);
        strtab.extend_from_slice(sym.name.as_bytes());
        strtab.push(0);
    }

    let mut symtab = Vec::new();
    symtab.extend_from_slice(&0u32.to_le_bytes());
    symtab.extend_from_slice(&[0u8; 13]);
    symtab.extend_from_slice(&0u64.to_le_bytes());
    for (i, sym) in symbols.iter().enumerate() {
        symtab.extend_from_slice(&sym_name_off[i + 1].to_le_bytes());
        symtab.push(sym.ty);
        symtab.push(sym.bind | (sym.visibility << 4));
        symtab.extend_from_slice(&sym.section.to_le_bytes());
        symtab.extend_from_slice(&sym.value.to_le_bytes());
        symtab.extend_from_slice(&sym.size.to_le_bytes());
    }

    let mut rela = Vec::new();
    for rel in relocations {
        rela.extend_from_slice(&rel.offset.to_le_bytes());
        let info = ((rel.sym_index as u64) << 32) | (rel.rel_type as u64);
        rela.extend_from_slice(&info.to_le_bytes());
        rela.extend_from_slice(&rel.addend.to_le_bytes());
    }

    let header_size = 64usize;
    let text_off = header_size;
    let symtab_off = align_up(text_off + text.len(), 8);
    let strtab_off = symtab_off + symtab.len();
    let shstrtab_off = strtab_off + strtab.len();
    let rela_off = align_up(shstrtab_off + shstr.len(), 8);
    let shoff = align_up(rela_off + rela.len(), 8);

    let mut out = Vec::with_capacity(shoff + 6 * 64);
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);
    out.push(ELFDATA2LSB);
    out.push(EV_CURRENT);
    out.push(0);
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&ET_REL.to_le_bytes());
    out.extend_from_slice(&EM_AARCH64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(shoff as u64).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&6u16.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());

    pad_to(&mut out, text_off);
    out.extend_from_slice(text);
    pad_to(&mut out, symtab_off);
    out.extend_from_slice(&symtab);
    pad_to(&mut out, strtab_off);
    out.extend_from_slice(&strtab);
    pad_to(&mut out, shstrtab_off);
    out.extend_from_slice(&shstr);
    pad_to(&mut out, rela_off);
    out.extend_from_slice(&rela);
    pad_to(&mut out, shoff);

    write_section_header(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    write_section_header(&mut out, text_name, SHT_PROGBITS, SHF_ALLOC | SHF_EXECINSTR,
        0, text_off as u64, text.len() as u64, 0, 0, 4, 0);
    write_section_header(&mut out, symtab_name, SHT_SYMTAB, 0,
        0, symtab_off as u64, symtab.len() as u64, 3, 1, 8, 24);
    write_section_header(&mut out, strtab_name, SHT_STRTAB, 0,
        0, strtab_off as u64, strtab.len() as u64, 0, 0, 1, 0);
    write_section_header(&mut out, shstrtab_name, SHT_STRTAB, 0,
        0, shstrtab_off as u64, shstr.len() as u64, 0, 0, 1, 0);
    write_section_header(&mut out, rela_name, SHT_RELA, SHF_INFO_LINK,
        0, rela_off as u64, rela.len() as u64, 2, 1, 8, 24);
    out
}

fn align_up(v: usize, align: usize) -> usize {
    (v + align - 1) & !(align - 1)
}

fn pad_to(buf: &mut Vec<u8>, target: usize) {
    while buf.len() < target {
        buf.push(0);
    }
}

// ===== ET_REL 构建 (.o 文件) =====

/// 构建可重定位 ELF (x86-64)
///
/// 布局:
///   ELF Header (64B)
///   .text
///   .rodata (对齐 8)
///   .data (对齐 8)
///   .rela.text (对齐 8)
///   .rela.data (对齐 8)
///   .symtab (对齐 8)
///   .strtab
///   .shstrtab
///   节头表 (对齐 8)
pub fn build_elf_rel_x86_64(
    text: &[u8],
    rodata: &[u8],
    data: &[u8],
    bss_size: u64,
    text_align: u64,
    data_align: u64,
    bss_align: u64,
    symbols: &[ElfSym],
    relocations: &[ElfRela],
    extra_sections: &[ElfSection],
    data_relocations: &[ElfRela],
    extra_rela_sections: &[ExtraRelaSection],
) -> Vec<u8> {
    // 节索引:
    // [0] SHT_NULL
    // [1] .text
    // [2] .rodata
    // [3] .data
    // [4] .bss
    // [5..5+N] extra sections
    // [5+N..5+N+M] extra rela sections (.rela.init_array etc.)
    // [5+N+M] .rela.text
    // [6+N+M] .rela.data
    // [7+N+M] .symtab
    // [8+N+M] .strtab
    // [9+N+M] .shstrtab
    let n_extra = extra_sections.len() as u16;
    let n_extra_rela = extra_rela_sections.len() as u16;
    let _sh_rela: u16 = 5 + n_extra + n_extra_rela;
    let _sh_rela_data: u16 = 6 + n_extra + n_extra_rela;
    let sh_symtab: u16 = 7 + n_extra + n_extra_rela;
    let sh_strtab: u16 = 8 + n_extra + n_extra_rela;
    let sh_shstrtab: u16 = 9 + n_extra + n_extra_rela;
    let shnum: u16 = 10 + n_extra + n_extra_rela;

    // 构建 .shstrtab (节名字符串表)
    let shstrtab = build_shstrtab(extra_sections, extra_rela_sections);

    // 构建 .strtab (符号名字符串表)
    let (strtab, sym_name_offsets) = build_strtab(symbols);

    // 对符号排序: 先 LOCAL 再 GLOBAL (ELF 要求 symtab 中 local 在前)
    // 同时构建 .symtab 二进制
    let symtab_data = build_symtab(symbols, &sym_name_offsets);

    // 构建 .rela.text
    let rela_data = build_rela(relocations);
    // 构建 .rela.data
    let rela_data_data = build_rela(data_relocations);
    // 构建额外 .rela 节
    let extra_rela_data: Vec<Vec<u8>> = extra_rela_sections.iter()
        .map(|ers| build_rela(&ers.relocations)).collect();

    // 计算各节偏移
    let mut offset: u64 = 64; // ELF header 后

    offset = align(offset, text_align.max(1));
    let text_off = offset;
    offset += text.len() as u64;

    // 对齐到 8
    offset = align(offset, 8);
    let rodata_off = offset;
    offset += rodata.len() as u64;

    offset = align(offset, data_align.max(1));
    let data_off = offset;
    offset += data.len() as u64;

    // .bss 不占文件空间
    let bss_off = align(offset, bss_align.max(1));

    // 自定义节
    let mut extra_offsets = Vec::with_capacity(extra_sections.len());
    for sec in extra_sections {
        offset = align(offset, sec.align.max(1) as u64);
        extra_offsets.push(offset);
        if sec.ty != SHT_NOBITS {
            offset += sec.data.len() as u64;
        }
    }

    // 额外 .rela 节
    let mut extra_rela_offsets = Vec::with_capacity(extra_rela_sections.len());
    for (i, _ers) in extra_rela_sections.iter().enumerate() {
        offset = align(offset, 8);
        extra_rela_offsets.push(offset);
        offset += extra_rela_data[i].len() as u64;
    }

    offset = align(offset, 8);
    let rela_off = offset;
    offset += rela_data.len() as u64;

    offset = align(offset, 8);
    let rela_data_off = offset;
    offset += rela_data_data.len() as u64;

    offset = align(offset, 8);
    let symtab_off = offset;
    offset += symtab_data.len() as u64;

    let strtab_off = offset;
    offset += strtab.len() as u64;

    let shstrtab_off = offset;
    offset += shstrtab.len() as u64;

    // 节头表
    offset = align(offset, 8);
    let shoff = offset;

    // 计算 local 符号数 (用于 symtab 的 sh_info)
    let n_locals = symbols.iter().filter(|s| s.bind == STB_LOCAL).count() as u32;

    // 开始组装输出
    let mut out = Vec::with_capacity(offset as usize + shnum as usize * 64);

    // ===== ELF Header (64 bytes) =====
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);     // ei_class
    out.push(ELFDATA2LSB);    // ei_data
    out.push(EV_CURRENT);     // ei_version
    out.push(0);              // ei_osabi (SYSV)
    out.extend_from_slice(&[0u8; 8]); // ei_pad
    out.extend_from_slice(&ET_REL.to_le_bytes());
    out.extend_from_slice(&EM_X86_64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());   // e_version
    out.extend_from_slice(&0u64.to_le_bytes());   // e_entry (无入口)
    out.extend_from_slice(&0u64.to_le_bytes());   // e_phoff (无程序头)
    out.extend_from_slice(&shoff.to_le_bytes());  // e_shoff
    out.extend_from_slice(&0u32.to_le_bytes());   // e_flags
    out.extend_from_slice(&64u16.to_le_bytes());  // e_ehsize
    out.extend_from_slice(&0u16.to_le_bytes());   // e_phentsize
    out.extend_from_slice(&0u16.to_le_bytes());   // e_phnum
    out.extend_from_slice(&64u16.to_le_bytes());  // e_shentsize
    out.extend_from_slice(&(shnum as u16).to_le_bytes()); // e_shnum
    out.extend_from_slice(&(sh_shstrtab as u16).to_le_bytes()); // e_shstrndx

    // ===== .text =====
    pad_to(&mut out, text_off as usize);
    out.extend_from_slice(text);

    // ===== .rodata =====
    pad_to(&mut out, rodata_off as usize);
    out.extend_from_slice(rodata);

    // ===== .data =====
    pad_to(&mut out, data_off as usize);
    out.extend_from_slice(data);

    // ===== 自定义节 =====
    for (i, sec) in extra_sections.iter().enumerate() {
        if sec.ty == SHT_NOBITS { continue; }
        pad_to(&mut out, extra_offsets[i] as usize);
        out.extend_from_slice(&sec.data);
    }

    // ===== 额外 .rela 节 =====
    for (i, _ers) in extra_rela_sections.iter().enumerate() {
        pad_to(&mut out, extra_rela_offsets[i] as usize);
        out.extend_from_slice(&extra_rela_data[i]);
    }

    // ===== .rela.text =====
    pad_to(&mut out, rela_off as usize);
    out.extend_from_slice(&rela_data);

    // ===== .rela.data =====
    pad_to(&mut out, rela_data_off as usize);
    out.extend_from_slice(&rela_data_data);

    // ===== .symtab =====
    pad_to(&mut out, symtab_off as usize);
    out.extend_from_slice(&symtab_data);

    // ===== .strtab =====
    out.extend_from_slice(&strtab);

    // ===== .shstrtab =====
    out.extend_from_slice(&shstrtab);

    // ===== 节头表 =====
    pad_to(&mut out, shoff as usize);

    // [0] SHT_NULL
    write_sh_null(&mut out);

    // [1] .text
    write_section_header(&mut out,
        1, SHT_PROGBITS, SHF_ALLOC | SHF_EXECINSTR,
        0, text_off, text.len() as u64, 0, 0, text_align.max(1), 0,
    );

    // [2] .rodata
    write_section_header(&mut out, 7, SHT_PROGBITS, SHF_ALLOC,
        0, rodata_off, rodata.len() as u64, 0, 0, 8, 0,
    );

    // [3] .data
    write_section_header(&mut out, 15, SHT_PROGBITS, SHF_ALLOC | SHF_WRITE,
        0, data_off, data.len() as u64, 0, 0, data_align.max(1), 0,
    );

    // [4] .bss
    write_section_header(&mut out, 21, SHT_NOBITS, SHF_ALLOC | SHF_WRITE,
        0, bss_off, bss_size, 0, 0, bss_align.max(1), 0,
    );

    // [5..5+N] 自定义节
    for (i, sec) in extra_sections.iter().enumerate() {
        let name_off = shstrtab_name_offset(extra_sections, i);
        write_section_header(&mut out, name_off, sec.ty, sec.flags,
            0, extra_offsets[i], sec.data.len() as u64, 0, 0, sec.align, 0,
        );
    }

    // [5+N..5+N+M] 额外 .rela 节
    for (i, ers) in extra_rela_sections.iter().enumerate() {
        let name_off = shstrtab_extra_rela_name_offset(extra_sections, extra_rela_sections, i);
        write_section_header(&mut out, name_off, SHT_RELA, SHF_INFO_LINK,
            0, extra_rela_offsets[i], extra_rela_data[i].len() as u64,
            sh_symtab as u32, ers.target_sec_index, 8, 24,
        );
    }

    // [.rela.text]
    let extra_names_len: u32 = extra_sections.iter().map(|s| s.name.len() as u32 + 1).sum();
    let extra_rela_names_len: u32 = extra_rela_sections.iter().map(|s| s.name.len() as u32 + 1).sum();
    let base_off: u32 = 26 + extra_names_len + extra_rela_names_len;
    let rela_text_name_off = base_off;
    let rela_data_name_off = rela_text_name_off + ".rela.text".len() as u32 + 1;
    let symtab_name_off = rela_data_name_off + ".rela.data".len() as u32 + 1;
    let strtab_name_off = symtab_name_off + ".symtab".len() as u32 + 1;
    let shstrtab_name_off_val = strtab_name_off + ".strtab".len() as u32 + 1;
    write_section_header(&mut out, rela_text_name_off, SHT_RELA, SHF_INFO_LINK,
        0, rela_off, rela_data.len() as u64,
        sh_symtab as u32, 1, 8, 24,
    );

    // [.rela.data]
    write_section_header(&mut out, rela_data_name_off, SHT_RELA, SHF_INFO_LINK,
        0, rela_data_off, rela_data_data.len() as u64,
        sh_symtab as u32, 3, 8, 24,  // sh_info=3 → .data 节索引
    );

    // [.symtab]
    write_section_header(&mut out, symtab_name_off, SHT_SYMTAB, 0,
        0, symtab_off, symtab_data.len() as u64,
        sh_strtab as u32, n_locals, 8, 24,
    );

    // [.strtab]
    write_section_header(&mut out, strtab_name_off, SHT_STRTAB, 0,
        0, strtab_off, strtab.len() as u64, 0, 0, 1, 0,
    );

    // [.shstrtab]
    write_section_header(&mut out, shstrtab_name_off_val, SHT_STRTAB, 0,
        0, shstrtab_off, shstrtab.len() as u64, 0, 0, 1, 0,
    );

    out
}

// ===== 辅助函数 =====

fn align(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

/// 构建 .shstrtab: 所有节名的字符串表
fn build_shstrtab(extra_sections: &[ElfSection], extra_rela_sections: &[ExtraRelaSection]) -> Vec<u8> {
    // 布局: \0 .text \0 .rodata \0 .data \0 .bss \0 [extra names...] [extra rela names...] .rela.text \0 .rela.data \0 .symtab \0 .strtab \0 .shstrtab \0
    let mut s = vec![0u8]; // 初始 \0
    for name in &[".text", ".rodata", ".data", ".bss"] {
        s.extend_from_slice(name.as_bytes());
        s.push(0);
    }
    // 自定义节名
    for sec in extra_sections {
        s.extend_from_slice(sec.name.as_bytes());
        s.push(0);
    }
    // 额外 .rela 节名 (放在标准 .rela 名之前)
    for sec in extra_rela_sections {
        s.extend_from_slice(sec.name.as_bytes());
        s.push(0);
    }
    for name in &[".rela.text", ".rela.data", ".symtab", ".strtab", ".shstrtab"] {
        s.extend_from_slice(name.as_bytes());
        s.push(0);
    }
    s
}

/// 计算自定义节名在 .shstrtab 中的偏移
fn shstrtab_name_offset(extra_sections: &[ElfSection], idx: usize) -> u32 {
    // 偏移计算: \0(1) + .text\0(6) + .rodata\0(8) + .data\0(6) + .bss\0(5) = 26
    let mut off = 26u32;
    for i in 0..idx {
        off += extra_sections[i].name.len() as u32 + 1;
    }
    off
}

/// 计算额外 .rela 节名在 .shstrtab 中的偏移
fn shstrtab_extra_rela_name_offset(extra_sections: &[ElfSection], extra_rela_sections: &[ExtraRelaSection], idx: usize) -> u32 {
    // 偏移: 26 (base) + extra section names + preceding extra rela names
    let mut off = 26u32;
    for sec in extra_sections {
        off += sec.name.len() as u32 + 1;
    }
    for i in 0..idx {
        off += extra_rela_sections[i].name.len() as u32 + 1;
    }
    off
}

/// 构建 .strtab (符号名字符串表)
/// 返回 (strtab 数据, 每个符号名在 strtab 中的偏移)
fn build_strtab(symbols: &[ElfSym]) -> (Vec<u8>, Vec<u32>) {
    let mut strtab = vec![0u8]; // 初始 \0
    let mut offsets = Vec::with_capacity(symbols.len());
    for sym in symbols {
        if sym.name.is_empty() {
            offsets.push(0);
        } else {
            offsets.push(strtab.len() as u32);
            strtab.extend_from_slice(sym.name.as_bytes());
            strtab.push(0);
        }
    }
    (strtab, offsets)
}

/// 构建 .symtab 二进制数据
/// 每个 Elf64_Sym 24 字节:
///   u32 st_name, u8 st_info, u8 st_other, u16 st_shndx, u64 st_value, u64 st_size
fn build_symtab(symbols: &[ElfSym], name_offsets: &[u32]) -> Vec<u8> {
    let mut data = Vec::with_capacity(symbols.len() * 24);
    for (i, sym) in symbols.iter().enumerate() {
        let st_name = name_offsets[i];
        let st_info = (sym.bind << 4) | sym.ty;
        let st_other = sym.visibility;
        let st_shndx = sym.section;
        data.extend_from_slice(&st_name.to_le_bytes());
        data.push(st_info);
        data.push(st_other);
        data.extend_from_slice(&st_shndx.to_le_bytes());
        data.extend_from_slice(&sym.value.to_le_bytes());
        data.extend_from_slice(&sym.size.to_le_bytes());
    }
    data
}

/// 构建 .rela.text 二进制数据
/// 每个 Elf64_Rela 24 字节:
///   u64 r_offset, u64 r_info (sym<<32 | type), i64 r_addend
fn build_rela(relocations: &[ElfRela]) -> Vec<u8> {
    let mut data = Vec::with_capacity(relocations.len() * 24);
    for rel in relocations {
        data.extend_from_slice(&rel.offset.to_le_bytes());
        let r_info = ((rel.sym_index as u64) << 32) | (rel.rel_type as u64);
        data.extend_from_slice(&r_info.to_le_bytes());
        data.extend_from_slice(&rel.addend.to_le_bytes());
    }
    data
}

fn write_sh_null(out: &mut Vec<u8>) {
    // 64 bytes of zeros
    out.extend_from_slice(&[0u8; 64]);
}

fn write_section_header(
    out: &mut Vec<u8>,
    sh_name: u32,
    sh_type: u32,
    sh_flags: u32,
    sh_addr: u64,
    sh_offset: u64,
    sh_size: u64,
    sh_link: u32,
    sh_info: u32,
    sh_addralign: u64,
    sh_entsize: u64,
) {
    out.extend_from_slice(&sh_name.to_le_bytes());       // 4
    out.extend_from_slice(&sh_type.to_le_bytes());        // 4
    out.extend_from_slice(&(sh_flags as u64).to_le_bytes()); // 8 — ELF64 sh_flags is Xword
    out.extend_from_slice(&sh_addr.to_le_bytes());        // 8
    out.extend_from_slice(&sh_offset.to_le_bytes());      // 8
    out.extend_from_slice(&sh_size.to_le_bytes());        // 8
    out.extend_from_slice(&sh_link.to_le_bytes());        // 4
    out.extend_from_slice(&sh_info.to_le_bytes());        // 4
    out.extend_from_slice(&sh_addralign.to_le_bytes());   // 8
    out.extend_from_slice(&sh_entsize.to_le_bytes());     // 8
    // Total: 4+4+8+8+8+8+4+4+8+8 = 64 bytes
}

/// 从 ET_REL ELF64 目标文件中按节名读取载荷（供 LTO 摘要提取等）。
pub fn read_section_data(elf: &[u8], section_name: &str) -> Option<Vec<u8>> {
    if elf.len() < 64 || elf.get(0..4)? != &ELFMAG || elf[4] != ELFCLASS64 {
        return None;
    }
    let shoff = u64::from_le_bytes(elf.get(40..48)?.try_into().ok()?);
    let shentsize = u16::from_le_bytes(elf.get(58..60)?.try_into().ok()?) as u64;
    let shnum = u16::from_le_bytes(elf.get(60..62)?.try_into().ok()?) as usize;
    let shstrndx = u16::from_le_bytes(elf.get(62..64)?.try_into().ok()?) as usize;
    if shnum == 0 || shstrndx >= shnum || shentsize < 64 {
        return None;
    }
    let shstr_hdr = shoff as usize + shstrndx * shentsize as usize;
    if shstr_hdr + 64 > elf.len() {
        return None;
    }
    let shstr_off = u64::from_le_bytes(elf[shstr_hdr + 24..shstr_hdr + 32].try_into().ok()?) as usize;
    let shstr_size = u64::from_le_bytes(elf[shstr_hdr + 32..shstr_hdr + 40].try_into().ok()?) as usize;
    if shstr_off + shstr_size > elf.len() {
        return None;
    }
    let shstrtab = &elf[shstr_off..shstr_off + shstr_size];

    for i in 0..shnum {
        let base = shoff as usize + i * shentsize as usize;
        if base + 64 > elf.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(elf[base..base + 4].try_into().ok()?) as usize;
        let name = read_cstr(shstrtab, name_idx)?;
        if name != section_name {
            continue;
        }
        let data_off = u64::from_le_bytes(elf[base + 24..base + 32].try_into().ok()?) as usize;
        let data_size = u64::from_le_bytes(elf[base + 32..base + 40].try_into().ok()?) as usize;
        if data_off + data_size > elf.len() {
            return None;
        }
        return Some(elf[data_off..data_off + data_size].to_vec());
    }
    None
}

/// 从 ET_REL 目标文件的 .rela.* 节收集被引用的符号名（供 LTO 引用标记）。
pub fn referenced_symbol_names(elf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if elf.len() < 64 || elf.get(0..4) != Some(&ELFMAG) || elf[4] != ELFCLASS64 {
        return out;
    }
    let shoff = match u64::from_le_bytes(elf.get(40..48).and_then(|s| s.try_into().ok()).unwrap_or([0; 8])) {
        v if v > 0 => v as usize,
        _ => return out,
    };
    let shentsize = u16::from_le_bytes(elf.get(58..60).and_then(|s| s.try_into().ok()).unwrap_or([0; 2])) as usize;
    let shnum = u16::from_le_bytes(elf.get(60..62).and_then(|s| s.try_into().ok()).unwrap_or([0; 2])) as usize;
    let shstrndx = u16::from_le_bytes(elf.get(62..64).and_then(|s| s.try_into().ok()).unwrap_or([0; 2])) as usize;
    if shnum == 0 || shentsize < 64 || shstrndx >= shnum {
        return out;
    }

    let mut sections: Vec<(String, u32, u64, u64, u64)> = Vec::new(); // name, type, offset, size, entsize
    let shstr_hdr = shoff + shstrndx * shentsize;
    if shstr_hdr + 64 > elf.len() {
        return out;
    }
    let shstr_off = u64::from_le_bytes(elf[shstr_hdr + 24..shstr_hdr + 32].try_into().unwrap()) as usize;
    let shstr_size = u64::from_le_bytes(elf[shstr_hdr + 32..shstr_hdr + 40].try_into().unwrap()) as usize;
    if shstr_off + shstr_size > elf.len() {
        return out;
    }
    let shstrtab = &elf[shstr_off..shstr_off + shstr_size];

    let mut symtab: Option<(usize, usize, u64)> = None; // offset, size, entsize
    let mut strtab: Option<(usize, usize)> = None;

    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > elf.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(elf[base..base + 4].try_into().unwrap()) as usize;
        let sh_type = u32::from_le_bytes(elf[base + 4..base + 8].try_into().unwrap());
        let data_off = u64::from_le_bytes(elf[base + 24..base + 32].try_into().unwrap()) as usize;
        let data_size = u64::from_le_bytes(elf[base + 32..base + 40].try_into().unwrap()) as usize;
        let entsize = u64::from_le_bytes(elf[base + 56..base + 64].try_into().unwrap());
        let name = read_cstr(shstrtab, name_idx).unwrap_or("").to_string();
        sections.push((name.clone(), sh_type, data_off as u64, data_size as u64, entsize));
        if name == ".symtab" {
            symtab = Some((data_off, data_size, entsize));
        } else if name == ".strtab" {
            strtab = Some((data_off, data_size));
        }
    }

    let (sym_off, sym_size, sym_entsize) = match symtab {
        Some(v) if v.2 >= 24 => v,
        _ => return out,
    };
    let (str_off, str_size) = match strtab {
        Some(v) => v,
        _ => return out,
    };
    if sym_off + sym_size > elf.len() || str_off + str_size > elf.len() {
        return out;
    }
    let symtab_data = &elf[sym_off..sym_off + sym_size];
    let strtab_data = &elf[str_off..str_off + str_size];
    let sym_count = sym_size / sym_entsize as usize;

    let mut sym_names: Vec<String> = vec![String::new(); sym_count];
    for i in 0..sym_count {
        let base = i * sym_entsize as usize;
        if base + 24 > symtab_data.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(symtab_data[base..base + 4].try_into().unwrap()) as usize;
        sym_names[i] = read_cstr(strtab_data, name_idx).unwrap_or("").to_string();
    }

    let mut seen = std::collections::HashSet::new();
    for (name, sh_type, data_off, data_size, entsize) in &sections {
        if *sh_type != SHT_RELA || !name.starts_with(".rela") || *entsize < 24 {
            continue;
        }
        let off = *data_off as usize;
        let end = off + *data_size as usize;
        if end > elf.len() {
            continue;
        }
        let mut pos = off;
        while pos + 24 <= end {
            let info = u64::from_le_bytes(elf[pos + 8..pos + 16].try_into().unwrap());
            let sym_idx = (info >> 32) as usize;
            if sym_idx > 0 && sym_idx < sym_names.len() {
                let sym_name = &sym_names[sym_idx];
                if !sym_name.is_empty() && seen.insert(sym_name.clone()) {
                    out.push(sym_name.clone());
                }
            }
            pos += *entsize as usize;
        }
    }
    out
}

fn read_cstr(data: &[u8], off: usize) -> Option<&str> {
    let end = data.get(off..)?.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&data[off..off + end]).ok()
}
