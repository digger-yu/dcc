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
pub const ET_REL:     u16 = 1;
pub const ET_EXEC:    u16 = 2;
pub const ET_DYN:     u16 = 3;
pub const EM_X86_64:  u16 = 62;
pub const EM_AARCH64: u16 = 183;
pub const EM_RISCV:   u16 = 243;

pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_STACK: u32 = 0x6474e551;
pub const PT_GNU_RELRO: u32 = 0x6474e552;
pub const PF_X: u32 = 1; pub const PF_W: u32 = 2; pub const PF_R: u32 = 4;

/// Section header flag: section is retained by --gc-sections even if no
/// explicit reference exists. Used by `__attribute__((retain))` and various
/// metadata sections (`.init`, `.fini`, `.init_array`, `.fini_array`, ...).
/// Defined by binutils since 2017; mirrors the constant in <elf.h>.
pub const SHF_GNU_RETAIN: u32 = 0x200000;

// 节类型
pub const SHT_NULL: u32 = 0;
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_RELA: u32 = 4;
pub const SHT_HASH: u32 = 5;
pub const SHT_DYNAMIC: u32 = 6;
pub const SHT_NOTE: u32 = 7;
pub const SHT_NOBITS: u32 = 8;
pub const SHT_DYNSYM: u32 = 11;

// 节标志
pub const SHF_ALLOC: u32 = 2;
pub const SHF_EXECINSTR: u32 = 4;
pub const SHF_WRITE: u32 = 1;
pub const SHF_TLS: u32 = 0x400;
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
pub const STT_TLS: u8 = 6;

// 特殊节索引
pub const SHN_UNDEF: u16 = 0;

// x86-64 重定位类型
pub const R_X86_64_64:        u32 = 1;
pub const R_X86_64_PC32:      u32 = 2;
pub const R_X86_64_PLT32:     u32 = 4;  // 用于函数调用 (PIE 兼容)
pub const R_X86_64_GLOB_DAT:  u32 = 6;  // GOT 数据（动态库符号）
pub const R_X86_64_JUMP_SLOT: u32 = 7;  // PLT 槽
pub const R_X86_64_RELATIVE:  u32 = 8;  // base + offset
pub const R_X86_64_GOTPCREL:  u32 = 9;
pub const R_X86_64_32:        u32 = 10;
pub const R_X86_64_32S:       u32 = 11;
pub const R_X86_64_TPOFF64:   u32 = 18; // TLS: thread pointer offset (64-bit)
pub const R_X86_64_TPOFF32:   u32 = 23; // TLS: thread pointer offset (32-bit, sign-extend)
pub const R_X86_64_GOT64:     u32 = 19; // 64-bit GOT entry
pub const R_X86_64_GOTOFF64:  u32 = 25; // 64-bit GOT-relative
pub const R_X86_64_IRELATIVE: u32 = 37; // IFUNC resolver
pub const R_X86_64_TPOFF:     u32 = 17; // TLS: traditional thread-pointer offset
pub const R_X86_64_DTPOFF64:  u32 = 21; // TLS: dynamic thread pointer offset
pub const R_X86_64_DTPMOD64:  u32 = 20; // TLS: module id

// AArch64 重定位类型
pub const R_AARCH64_CALL26: u32 = 283;
pub const R_AARCH64_JUMP26: u32 = 282;
pub const R_AARCH64_ADR_PREL_LO21: u32 = 274;
pub const R_AARCH64_ADR_PREL_PG_HI21: u32 = 275;
pub const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
pub const R_AARCH64_LDST64_ABS_LO12_NC: u32 = 286;
pub const R_AARCH64_LDST8_ABS_LO12_NC: u32 = 278;
pub const R_AARCH64_LDST16_ABS_LO12_NC: u32 = 284;
pub const R_AARCH64_LDST32_ABS_LO12_NC: u32 = 285;
pub const R_AARCH64_LDST128_ABS_LO12_NC: u32 = 299;
pub const R_AARCH64_MOVW_UABS_G0: u32 = 287;
pub const R_AARCH64_MOVW_UABS_G1: u32 = 288;
pub const R_AARCH64_MOVW_UABS_G2: u32 = 289;
pub const R_AARCH64_MOVW_UABS_G3: u32 = 290;
pub const R_AARCH64_ABS64: u32 = 257;
pub const R_AARCH64_PREL64: u32 = 260;
pub const R_AARCH64_GOTREL64: u32 = 311;
pub const R_AARCH64_GOT_LD_PREL19: u32 = 309;

// RISC-V 重定位类型
pub const R_RISCV_JAL: u32 = 5;
pub const R_RISCV_CALL: u32 = 18;
pub const R_RISCV_CALL_PLT: u32 = 19;
pub const R_RISCV_PCREL_HI20: u32 = 20;
pub const R_RISCV_PCREL_LO12_I: u32 = 21;
pub const R_RISCV_PCREL_LO12_S: u32 = 22;
pub const R_RISCV_HI20: u32 = 23;
pub const R_RISCV_LO12_I: u32 = 24;
pub const R_RISCV_LO12_S: u32 = 25;
pub const R_RISCV_RELAX: u32 = 51;
pub const R_RISCV_64: u32 = 2;
pub const R_RISCV_32: u32 = 1;
pub const R_RISCV_32_PCREL: u32 = 3;
pub const R_RISCV_TPOFF64: u32 = 7;
pub const R_RISCV_TPREL_HI20: u32 = 29;
pub const R_RISCV_TPREL_LO12_I: u32 = 32;
pub const R_RISCV_TPREL_LO12_S: u32 = 33;

// 符号类型 (扩展, 用于 STT_GNU_IFUNC)
pub const STT_GNU_IFUNC: u8 = 10;

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

/// ET_EXEC + 节头表 / 符号表（供 readelf -s 等工具使用）
pub fn build_elf_exec_x86_64(
    image: &[u8],
    entry: u64,
    text_size: u64,
    rodata_size: u64,
    tdata_size: u64,
    tbss_size: u64,
    symbols: &[ElfSym],
) -> Vec<u8> {
    let hdr = 64 + 56;
    let p_vaddr = 0x400000u64;

    let mut shstr = vec![0u8];
    let mut names: Vec<&str> = vec![".text"];
    if rodata_size > 0 {
        names.push(".rodata");
    }
    if tdata_size > 0 {
        names.push(".tdata");
    }
    if tbss_size > 0 {
        names.push(".tbss");
    }
    names.extend_from_slice(&[".symtab", ".strtab", ".shstrtab"]);
    let mut name_off = vec![0u32];
    for n in &names {
        name_off.push(shstr.len() as u32);
        shstr.extend_from_slice(n.as_bytes());
        shstr.push(0);
    }

    let (strtab, sym_name_offsets) = build_strtab(symbols);
    let symtab_data = build_symtab(symbols, &sym_name_offsets);
    let n_locals = symbols.iter().filter(|s| s.bind == STB_LOCAL).count() as u32;

    let sh_symtab = {
        let mut idx = 2u16;
        if rodata_size > 0 { idx += 1; }
        if tdata_size > 0 { idx += 1; }
        if tbss_size > 0 { idx += 1; }
        idx
    };
    let sh_strtab = sh_symtab + 1;
    let sh_shstrtab = sh_strtab + 1;
    let shnum = sh_shstrtab + 1;

    let mut out = Vec::new();
    // 占位 ELF 头 + Program Header
    out.resize(hdr, 0);

    let image_off = hdr as u64;
    out.extend_from_slice(image);

    let symtab_off = out.len() as u64;
    out.extend_from_slice(&symtab_data);
    let strtab_off = out.len() as u64;
    out.extend_from_slice(&strtab);
    let shstrtab_off = out.len() as u64;
    out.extend_from_slice(&shstr);

    let shoff = align(out.len() as u64, 8);
    pad_to(&mut out, shoff as usize);
    write_sh_null(&mut out);

    let mut vaddr = p_vaddr + hdr as u64;
    let mut file_off = image_off;
    write_section_header(
        &mut out, name_off[1], SHT_PROGBITS, SHF_ALLOC | SHF_EXECINSTR,
        vaddr, file_off, text_size, 0, 0, 16, 0,
    );
    vaddr += text_size;
    file_off += text_size;

    let mut ni = 2usize;
    if rodata_size > 0 {
        write_section_header(
            &mut out, name_off[ni], SHT_PROGBITS, SHF_ALLOC,
            vaddr, file_off, rodata_size, 0, 0, 8, 0,
        );
        vaddr += rodata_size;
        file_off += rodata_size;
        ni += 1;
    }
    if tdata_size > 0 {
        write_section_header(
            &mut out, name_off[ni], SHT_PROGBITS, SHF_ALLOC | SHF_WRITE | SHF_TLS,
            vaddr, file_off, tdata_size, 0, 0, 8, 0,
        );
        vaddr += tdata_size;
        // file_off 在每个分支各自更新后已隐式累加；此处避免 dead-store 警告。
        ni += 1;
    }
    if tbss_size > 0 {
        write_section_header(
            &mut out, name_off[ni], SHT_NOBITS, SHF_ALLOC | SHF_WRITE | SHF_TLS,
            vaddr, 0, tbss_size, 0, 0, 8, 0,
        );
        ni += 1;
    }

    write_section_header(
        &mut out, name_off[ni], SHT_SYMTAB, 0,
        0, symtab_off, symtab_data.len() as u64, sh_strtab as u32, n_locals, 8, 24,
    );
    ni += 1;
    write_section_header(
        &mut out, name_off[ni], SHT_STRTAB, 0,
        0, strtab_off, strtab.len() as u64, 0, 0, 1, 0,
    );
    ni += 1;
    write_section_header(
        &mut out, name_off[ni], SHT_STRTAB, 0,
        0, shstrtab_off, shstr.len() as u64, 0, 0, 1, 0,
    );

    // 回填 ELF 头与 Program Header
    out[0..4].copy_from_slice(&ELFMAG);
    out[4] = ELFCLASS64;
    out[5] = ELFDATA2LSB;
    out[6] = EV_CURRENT;
    out[0x10..0x12].copy_from_slice(&ET_EXEC.to_le_bytes());
    out[0x12..0x14].copy_from_slice(&EM_X86_64.to_le_bytes());
    out[0x14..0x18].copy_from_slice(&1u32.to_le_bytes());
    out[0x18..0x20].copy_from_slice(&entry.to_le_bytes());
    out[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    out[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
    out[0x34..0x36].copy_from_slice(&64u16.to_le_bytes());
    out[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
    out[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes());
    out[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
    out[0x3c..0x3e].copy_from_slice(&(shnum as u16).to_le_bytes());
    out[0x3e..0x40].copy_from_slice(&(sh_shstrtab as u16).to_le_bytes());

    let p_filesz = hdr as u64 + image.len() as u64;
    let p_memsz = p_filesz + tbss_size;
    out[0x40..0x44].copy_from_slice(&PT_LOAD.to_le_bytes());
    out[0x44..0x48].copy_from_slice(&7u32.to_le_bytes());
    out[0x48..0x50].copy_from_slice(&0u64.to_le_bytes());
    out[0x50..0x58].copy_from_slice(&p_vaddr.to_le_bytes());
    out[0x58..0x60].copy_from_slice(&p_vaddr.to_le_bytes());
    out[0x60..0x68].copy_from_slice(&p_filesz.to_le_bytes());
    out[0x68..0x70].copy_from_slice(&p_memsz.to_le_bytes());
    out[0x70..0x78].copy_from_slice(&0x1000u64.to_le_bytes());

    out
}

/// AArch64 ET_EXEC（静态可执行，.text + 可选 .rodata 同段）
pub fn build_elf_aarch64(code: &[u8], rodata: &[u8], entry: u64) -> Vec<u8> {
    let rodata_off = align_up(code.len(), 8);
    let mut combined = Vec::with_capacity(rodata_off + rodata.len());
    combined.extend_from_slice(code);
    pad_to(&mut combined, rodata_off);
    combined.extend_from_slice(rodata);
    let code = &combined;
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

/// AArch64 ET_REL .o（.text + 可选 .rodata + symtab + rela）
pub fn build_elf_rel_aarch64(
    text: &[u8],
    rodata: &[u8],
    symbols: &[ElfSym],
    relocations: &[ElfRela],
    rodata_relocations: &[ElfRela],
    extra_sections: &[ElfSection],
) -> Vec<u8> {
    build_elf_rel_generic(
        EM_AARCH64,
        text,
        rodata,
        symbols,
        relocations,
        rodata_relocations,
        extra_sections,
    )
}

/// RISC-V ET_EXEC
pub fn build_elf_riscv64(code: &[u8], rodata: &[u8], entry: u64) -> Vec<u8> {
    let rodata_off = align_up(code.len(), 8);
    let mut combined = Vec::with_capacity(rodata_off + rodata.len());
    combined.extend_from_slice(code);
    pad_to(&mut combined, rodata_off);
    combined.extend_from_slice(rodata);
    let code = &combined;
    let mut out = Vec::with_capacity(code.len() + 256);
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);
    out.push(ELFDATA2LSB);
    out.push(EV_CURRENT);
    out.push(0);
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&ET_EXEC.to_le_bytes());
    out.extend_from_slice(&EM_RISCV.to_le_bytes());
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

/// RISC-V ET_REL .o
pub fn build_elf_rel_riscv64(
    text: &[u8],
    rodata: &[u8],
    symbols: &[ElfSym],
    relocations: &[ElfRela],
    rodata_relocations: &[ElfRela],
) -> Vec<u8> {
    build_elf_rel_generic(
        EM_RISCV,
        text,
        rodata,
        symbols,
        relocations,
        rodata_relocations,
        &[],
    )
}

/// 通用 ET_REL 构建（AArch64 / RISC-V）
fn build_elf_rel_generic(
    e_machine: u16,
    text: &[u8],
    rodata: &[u8],
    symbols: &[ElfSym],
    relocations: &[ElfRela],
    rodata_relocations: &[ElfRela],
    extra_sections: &[ElfSection],
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
    let rodata_name = if rodata.is_empty() {
        0
    } else {
        sec_name_off(&mut shstr, ".rodata")
    };
    let symtab_name = sec_name_off(&mut shstr, ".symtab");
    let strtab_name = sec_name_off(&mut shstr, ".strtab");
    let shstrtab_name = sec_name_off(&mut shstr, ".shstrtab");
    let rela_text_name = sec_name_off(&mut shstr, ".rela.text");
    let rela_rodata_name = if rodata_relocations.is_empty() {
        0
    } else {
        sec_name_off(&mut shstr, ".rela.rodata")
    };
    let extra_names: Vec<u32> = extra_sections
        .iter()
        .map(|s| sec_name_off(&mut shstr, &s.name))
        .collect();

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

    let mut rela_text = Vec::new();
    for rel in relocations {
        rela_text.extend_from_slice(&rel.offset.to_le_bytes());
        let info = ((rel.sym_index as u64) << 32) | (rel.rel_type as u64);
        rela_text.extend_from_slice(&info.to_le_bytes());
        rela_text.extend_from_slice(&rel.addend.to_le_bytes());
    }
    let mut rela_rodata = Vec::new();
    for rel in rodata_relocations {
        rela_rodata.extend_from_slice(&rel.offset.to_le_bytes());
        let info = ((rel.sym_index as u64) << 32) | (rel.rel_type as u64);
        rela_rodata.extend_from_slice(&info.to_le_bytes());
        rela_rodata.extend_from_slice(&rel.addend.to_le_bytes());
    }

    let header_size = 64usize;
    let text_off = header_size;
    let rodata_off = align_up(text_off + text.len(), 8);
    let mut cursor = if rodata.is_empty() {
        rodata_off
    } else {
        rodata_off + rodata.len()
    };
    let symtab_off = align_up(cursor, 8);
    cursor = symtab_off + symtab.len();
    let strtab_off = cursor;
    cursor = strtab_off + strtab.len();
    let shstrtab_off = cursor;
    cursor = shstrtab_off + shstr.len();
    let rela_text_off = align_up(cursor, 8);
    cursor = rela_text_off + rela_text.len();
    let rela_rodata_off = if rela_rodata.is_empty() {
        cursor
    } else {
        let off = align_up(cursor, 8);
        cursor = off + rela_rodata.len();
        off
    };
    let mut extra_layout: Vec<(u32, u64, &ElfSection)> = Vec::new();
    for sec in extra_sections {
        let off = align_up(cursor, sec.align.max(1) as usize);
        let name = extra_names[extra_layout.len()];
        extra_layout.push((name, off as u64, sec));
        cursor = off + sec.data.len();
    }
    let shoff = align_up(cursor, 8);

    let n_sections = 1 + 1
        + if rodata.is_empty() { 0 } else { 1 }
        + 3
        + 1
        + if rela_rodata.is_empty() { 0 } else { 1 }
        + extra_sections.len();

    let symtab_idx = if rodata.is_empty() { 2u32 } else { 3 };
    let strtab_idx = symtab_idx + 1;
    let shstrtab_idx = strtab_idx + 1;

    let mut out = Vec::with_capacity(shoff + n_sections * 64);
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);
    out.push(ELFDATA2LSB);
    out.push(EV_CURRENT);
    out.push(0);
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&ET_REL.to_le_bytes());
    out.extend_from_slice(&e_machine.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(shoff as u64).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&(n_sections as u16).to_le_bytes());
    out.extend_from_slice(&(shstrtab_idx as u16).to_le_bytes());

    pad_to(&mut out, text_off);
    out.extend_from_slice(text);
    if !rodata.is_empty() {
        pad_to(&mut out, rodata_off);
        out.extend_from_slice(rodata);
    }
    pad_to(&mut out, symtab_off);
    out.extend_from_slice(&symtab);
    pad_to(&mut out, strtab_off);
    out.extend_from_slice(&strtab);
    pad_to(&mut out, shstrtab_off);
    out.extend_from_slice(&shstr);
    pad_to(&mut out, rela_text_off);
    out.extend_from_slice(&rela_text);
    if !rela_rodata.is_empty() {
        pad_to(&mut out, rela_rodata_off);
        out.extend_from_slice(&rela_rodata);
    }
    for (_, off, sec) in &extra_layout {
        pad_to(&mut out, *off as usize);
        out.extend_from_slice(&sec.data);
    }
    pad_to(&mut out, shoff);

    let rela_text_idx = shstrtab_idx + 1;
    let rodata_idx = if rodata.is_empty() { 0 } else { 2 };
    let _rela_rodata_idx = if rela_rodata.is_empty() {
        0
    } else {
        rela_text_idx + 1
    };

    write_section_header(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    write_section_header(&mut out, text_name, SHT_PROGBITS, SHF_ALLOC | SHF_EXECINSTR,
        0, text_off as u64, text.len() as u64, 0, 0, 4, 0);
    if !rodata.is_empty() {
        write_section_header(&mut out, rodata_name, SHT_PROGBITS, SHF_ALLOC,
            0, rodata_off as u64, rodata.len() as u64, 0, 0, 8, 0);
    }
    write_section_header(&mut out, symtab_name, SHT_SYMTAB, 0,
        0, symtab_off as u64, symtab.len() as u64, strtab_idx as u32, 1, 8, 24);
    write_section_header(&mut out, strtab_name, SHT_STRTAB, 0,
        0, strtab_off as u64, strtab.len() as u64, 0, 0, 1, 0);
    write_section_header(&mut out, shstrtab_name, SHT_STRTAB, 0,
        0, shstrtab_off as u64, shstr.len() as u64, 0, 0, 1, 0);
    write_section_header(&mut out, rela_text_name, SHT_RELA, SHF_INFO_LINK,
        0, rela_text_off as u64, rela_text.len() as u64, symtab_idx as u32, 1, 8, 24);
    if !rela_rodata.is_empty() {
        write_section_header(&mut out, rela_rodata_name, SHT_RELA, SHF_INFO_LINK,
            0, rela_rodata_off as u64, rela_rodata.len() as u64, symtab_idx, rodata_idx as u32, 8, 24);
    }
    for (name, off, sec) in &extra_layout {
        write_section_header(
            &mut out,
            *name,
            sec.ty,
            sec.flags,
            0,
            *off,
            sec.data.len() as u64,
            0,
            0,
            sec.align.max(1),
            0,
        );
    }
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
    tdata: &[u8],
    tbss_size: u64,
    text_align: u64,
    data_align: u64,
    bss_align: u64,
    tdata_align: u64,
    tbss_align: u64,
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
    // [5] .tdata
    // [6] .tbss
    // [7..7+N] extra sections
    // [7+N..7+N+M] extra rela sections
    // [7+N+M] .rela.text
    // [8+N+M] .rela.data
    // [9+N+M] .symtab
    // [10+N+M] .strtab
    // [11+N+M] .shstrtab
    let n_extra = extra_sections.len() as u16;
    let n_extra_rela = extra_rela_sections.len() as u16;
    let _sh_rela: u16 = 7 + n_extra + n_extra_rela;
    let _sh_rela_data: u16 = 8 + n_extra + n_extra_rela;
    let sh_symtab: u16 = 9 + n_extra + n_extra_rela;
    let sh_strtab: u16 = 10 + n_extra + n_extra_rela;
    let sh_shstrtab: u16 = 11 + n_extra + n_extra_rela;
    let shnum: u16 = 12 + n_extra + n_extra_rela;

    // 构建 .shstrtab (节名字符串表)
    let shstrtab = build_shstrtab_with_tls(extra_sections, extra_rela_sections);

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
    offset = align(offset, bss_align.max(1));
    let bss_off = offset;
    offset += bss_size;

    offset = align(offset, tdata_align.max(1));
    let tdata_off = offset;
    offset += tdata.len() as u64;

    offset = align(offset, tbss_align.max(1));
    let tbss_off = offset;
    offset += tbss_size;

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

    // ===== .tdata =====
    pad_to(&mut out, tdata_off as usize);
    out.extend_from_slice(tdata);

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

    // [5] .tdata
    write_section_header(&mut out, 26, SHT_PROGBITS, SHF_ALLOC | SHF_WRITE | SHF_TLS,
        0, tdata_off, tdata.len() as u64, 0, 0, tdata_align.max(1), 0,
    );

    // [6] .tbss
    write_section_header(&mut out, 33, SHT_NOBITS, SHF_ALLOC | SHF_WRITE | SHF_TLS,
        0, tbss_off, tbss_size, 0, 0, tbss_align.max(1), 0,
    );

    // [7..7+N] 自定义节
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
    let base_off: u32 = 39 + extra_names_len + extra_rela_names_len;
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

/// 构建 .shstrtab（含 .tdata / .tbss）
fn build_shstrtab_with_tls(extra_sections: &[ElfSection], extra_rela_sections: &[ExtraRelaSection]) -> Vec<u8> {
    let mut s = vec![0u8];
    for name in &[".text", ".rodata", ".data", ".bss", ".tdata", ".tbss"] {
        s.extend_from_slice(name.as_bytes());
        s.push(0);
    }
    for sec in extra_sections {
        s.extend_from_slice(sec.name.as_bytes());
        s.push(0);
    }
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

/// 构建 .shstrtab: 所有节名的字符串表
fn build_shstrtab(extra_sections: &[ElfSection], extra_rela_sections: &[ExtraRelaSection]) -> Vec<u8> {
    build_shstrtab_with_tls(extra_sections, extra_rela_sections)
}

/// 计算自定义节名在 .shstrtab 中的偏移
fn shstrtab_name_offset(extra_sections: &[ElfSection], idx: usize) -> u32 {
    // \0(1) + .text\0(6) + .rodata\0(8) + .data\0(6) + .bss\0(5) + .tdata\0(7) + .tbss\0(6) = 39
    let mut off = 39u32;
    for i in 0..idx {
        off += extra_sections[i].name.len() as u32 + 1;
    }
    off
}

/// 计算额外 .rela 节名在 .shstrtab 中的偏移
fn shstrtab_extra_rela_name_offset(extra_sections: &[ElfSection], extra_rela_sections: &[ExtraRelaSection], idx: usize) -> u32 {
    let mut off = 39u32;
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

/// 从 ET_REL 目标文件的 .symtab 收集 STB_GLOBAL / STB_WEAK 符号名。
///
/// 供 `-r --gc-sections` 通路用 `-u <name>` 把每个全局符号都定为
/// root, 让宿主 ld 不会因为"无 root"而直接拒绝.
pub fn global_symbol_names(elf: &[u8]) -> Vec<String> {
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

    let mut symtab: Option<(usize, usize, u64)> = None;
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
        if name == ".symtab" && sh_type == SHT_SYMTAB {
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
    let mut seen = std::collections::HashSet::new();
    for i in 0..sym_count {
        let base = i * sym_entsize as usize;
        if base + 24 > symtab_data.len() {
            break;
        }
        let st_name = u32::from_le_bytes(symtab_data[base..base + 4].try_into().unwrap()) as usize;
        let st_info = symtab_data[base + 4];
        let st_shndx = u16::from_le_bytes(symtab_data[base + 6..base + 8].try_into().unwrap());
        let st_value = u64::from_le_bytes(symtab_data[base + 8..base + 16].try_into().unwrap());
        let st_size = u64::from_le_bytes(symtab_data[base + 16..base + 24].try_into().unwrap());
        let bind = st_info >> 4;
        // STB_GLOBAL=1 / STB_WEAK=2
        if bind != 1 && bind != 2 {
            continue;
        }
        // 跳过 UNDEF 且 size=0 且 value=0 的"声明"空符号
        if st_shndx == 0 && st_value == 0 && st_size == 0 {
            continue;
        }
        let name = read_cstr(strtab_data, st_name).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}
