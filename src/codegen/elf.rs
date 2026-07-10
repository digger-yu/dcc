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

// ===== 数据结构 =====

/// 符号表条目 (供 .o 使用)
#[derive(Clone, Debug)]
pub struct ElfSym {
    pub name: String,
    pub section: u16,   // 节索引 (0=SHN_UNDEF)
    pub value: u64,     // 节内偏移
    pub size: u64,
    pub bind: u8,       // STB_LOCAL / STB_GLOBAL
    pub ty: u8,         // STT_NOTYPE / STT_FUNC / STT_OBJECT
}

/// 重定位条目 (供 .rela.text 使用)
#[derive(Clone, Debug)]
pub struct ElfRela {
    pub offset: u64,    // .text 内修补位置
    pub sym_index: u32, // .symtab 中的符号索引
    pub rel_type: u32,  // R_X86_64_PC32 等
    pub addend: i64,
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
    out.extend_from_slice(&5u32.to_le_bytes());     // p_flags
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

// ===== ET_REL 构建 (.o 文件) =====

/// 构建可重定位 ELF (x86-64)
///
/// 布局:
///   ELF Header (64B)
///   .text
///   .rodata (对齐 8)
///   .data (对齐 8)
///   .rela.text (对齐 8)
///   .symtab (对齐 8)
///   .strtab
///   .shstrtab
///   节头表 (对齐 8)
pub fn build_elf_rel_x86_64(
    text: &[u8],
    rodata: &[u8],
    data: &[u8],
    bss_size: u64,
    symbols: &[ElfSym],
    relocations: &[ElfRela],
) -> Vec<u8> {
    // 节索引:
    // [0] SHT_NULL
    // [1] .text
    // [2] .rodata
    // [3] .data
    // [4] .bss
    // [5] .rela.text
    // [6] .symtab
    // [7] .strtab
    // [8] .shstrtab
    const SH_TEXT: u16 = 1;
    const SH_RODATA: u16 = 2;
    const SH_DATA: u16 = 3;
    const SH_BSS: u16 = 4;
    const SH_RELA: u16 = 5;
    const SH_SYMTAB: u16 = 6;
    const SH_STRTAB: u16 = 7;
    const SH_SHSTRTAB: u16 = 8;
    const SHNUM: u16 = 9;

    // 构建 .shstrtab (节名字符串表)
    let shstrtab = build_shstrtab();

    // 构建 .strtab (符号名字符串表)
    let (strtab, sym_name_offsets) = build_strtab(symbols);

    // 对符号排序: 先 LOCAL 再 GLOBAL (ELF 要求 symtab 中 local 在前)
    // 同时构建 .symtab 二进制
    let symtab_data = build_symtab(symbols, &sym_name_offsets);

    // 构建 .rela.text
    let rela_data = build_rela(relocations);

    // 计算各节偏移
    let mut offset: u64 = 64; // ELF header 后

    let text_off = offset;
    offset += text.len() as u64;

    // 对齐到 8
    offset = align(offset, 8);
    let rodata_off = offset;
    offset += rodata.len() as u64;

    offset = align(offset, 8);
    let data_off = offset;
    offset += data.len() as u64;

    // .bss 不占文件空间
    let bss_off = offset;

    offset = align(offset, 8);
    let rela_off = offset;
    offset += rela_data.len() as u64;

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
    let mut out = Vec::with_capacity(offset as usize + SHNUM as usize * 64);

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
    out.extend_from_slice(&(SHNUM as u16).to_le_bytes()); // e_shnum
    out.extend_from_slice(&(SH_SHSTRTAB as u16).to_le_bytes()); // e_shstrndx

    // ===== .text =====
    pad_to(&mut out, text_off as usize);
    out.extend_from_slice(text);

    // ===== .rodata =====
    pad_to(&mut out, rodata_off as usize);
    out.extend_from_slice(rodata);

    // ===== .data =====
    pad_to(&mut out, data_off as usize);
    out.extend_from_slice(data);

    // ===== .rela.text =====
    pad_to(&mut out, rela_off as usize);
    out.extend_from_slice(&rela_data);

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
        0, text_off, text.len() as u64, 0, 0, 16, 0,
    );

    // [2] .rodata
    write_section_header(&mut out, 7, SHT_PROGBITS, SHF_ALLOC,
        0, rodata_off, rodata.len() as u64, 0, 0, 8, 0,
    );

    // [3] .data
    write_section_header(&mut out, 15, SHT_PROGBITS, SHF_ALLOC | SHF_WRITE,
        0, data_off, data.len() as u64, 0, 0, 8, 0,
    );

    // [4] .bss
    write_section_header(&mut out, 21, SHT_NOBITS, SHF_ALLOC | SHF_WRITE,
        0, bss_off, bss_size, 0, 0, 8, 0,
    );

    // [5] .rela.text
    write_section_header(&mut out, 26, SHT_RELA, SHF_INFO_LINK,
        0, rela_off, rela_data.len() as u64,
        SH_SYMTAB as u32, SH_TEXT as u32, 8, 24,
    );

    // [6] .symtab
    write_section_header(&mut out, 37, SHT_SYMTAB, 0,
        0, symtab_off, symtab_data.len() as u64,
        SH_STRTAB as u32, n_locals, 8, 24,
    );

    // [7] .strtab
    write_section_header(&mut out, 45, SHT_STRTAB, 0,
        0, strtab_off, strtab.len() as u64, 0, 0, 1, 0,
    );

    // [8] .shstrtab
    write_section_header(&mut out, 53, SHT_STRTAB, 0,
        0, shstrtab_off, shstrtab.len() as u64, 0, 0, 1, 0,
    );

    out
}

// ===== 辅助函数 =====

fn align(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

fn pad_to(out: &mut Vec<u8>, target: usize) {
    while out.len() < target { out.push(0); }
}

/// 构建 .shstrtab: 所有节名的字符串表
fn build_shstrtab() -> Vec<u8> {
    // 布局: \0 .text \0 .rodata \0 .data \0 .bss \0 .rela.text \0 .symtab \0 .strtab \0 .shstrtab \0
    // 偏移: 0  1     7      15    21   26         37      45      53
    let mut s = vec![0u8]; // 初始 \0
    for name in &[
        ".text", ".rodata", ".data", ".bss",
        ".rela.text", ".symtab", ".strtab", ".shstrtab",
    ] {
        s.extend_from_slice(name.as_bytes());
        s.push(0);
    }
    s
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
        let st_other = 0u8;
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
