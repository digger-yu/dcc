// src/codegen/elf.rs - 最小 ELF 生成器 (suckless 风格)
// 第一性原理: Linux 内核可直接 execve 一个静态 ELF,只要入口正确即可运行。
// 我们生成 ET_EXEC (非 PIE) ELF64,入口 _start,程序段只有 PT_LOAD 一段。
// suckless 准则: 手写 ELF 头,200 行内搞定。
//
// 不需要 ld,不依赖 glibc,不依赖 musl——直接 mmap-friendly。

#![allow(dead_code)]

pub const ELFMAG:    [u8; 4] = [0x7f, b'E', b'L', b'F'];
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;       // little-endian
pub const EV_CURRENT: u8 = 1;
pub const ET_EXEC:    u16 = 2;
pub const EM_X86_64:  u16 = 62;
pub const EM_AARCH64: u16 = 183;
pub const EM_RISCV:   u16 = 243;

pub const PT_LOAD: u32 = 1;
pub const PF_X: u32 = 1; pub const PF_W: u32 = 2; pub const PF_R: u32 = 4;

pub const SHT_NULL: u32 = 0;
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_NOBITS: u32 = 8;

pub const SHF_ALLOC: u32 = 2; pub const SHF_EXECINSTR: u32 = 4; pub const SHF_WRITE: u32 = 1;

pub fn build_elf_x86_64(code: &[u8], entry: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(code.len() + 256);
    // ===== ELF Header (64 bytes) =====
    out.extend_from_slice(&ELFMAG);
    out.push(ELFCLASS64);     // ei_class
    out.push(ELFDATA2LSB);    // ei_data
    out.push(EV_CURRENT);     // ei_version
    out.push(0);              // ei_osabi (SYSV)
    out.extend_from_slice(&[0u8; 8]); // ei_pad
    out.extend_from_slice(&2u16.to_le_bytes());   // e_type = ET_EXEC
    out.extend_from_slice(&EM_X86_64.to_le_bytes()); // e_machine
    out.extend_from_slice(&1u32.to_le_bytes());   // e_version
    out.extend_from_slice(&entry.to_le_bytes());  // e_entry
    out.extend_from_slice(&64u64.to_le_bytes());  // e_phoff (program header starts right after elf header)
    out.extend_from_slice(&0u64.to_le_bytes());   // e_shoff (no section headers in M0)
    out.extend_from_slice(&0u32.to_le_bytes());   // e_flags
    out.extend_from_slice(&64u16.to_le_bytes());  // e_ehsize
    out.extend_from_slice(&56u16.to_le_bytes());  // e_phentsize
    out.extend_from_slice(&1u16.to_le_bytes());   // e_phnum
    out.extend_from_slice(&0u16.to_le_bytes());   // e_shentsize
    out.extend_from_slice(&0u16.to_le_bytes());   // e_shnum
    out.extend_from_slice(&0u16.to_le_bytes());   // e_shstrndx

    // ===== Program Header (56 bytes) - PT_LOAD covering whole file =====
    let p_offset = 0u64;
    let p_vaddr  = 0x400000u64;       // _start 加载到 0x400000
    let p_paddr  = p_vaddr;
    let p_filesz = (64 + 56) as u64 + code.len() as u64;
    let p_memsz  = p_filesz;
    out.extend_from_slice(&PT_LOAD.to_le_bytes());  // p_type
    out.extend_from_slice(&5u32.to_le_bytes());     // p_flags = PF_R|PF_X|PF_W (RWX 简化)
    out.extend_from_slice(&p_offset.to_le_bytes()); // p_offset
    out.extend_from_slice(&p_vaddr.to_le_bytes());  // p_vaddr
    out.extend_from_slice(&p_paddr.to_le_bytes());  // p_paddr
    out.extend_from_slice(&p_filesz.to_le_bytes()); // p_filesz
    out.extend_from_slice(&p_memsz.to_le_bytes());  // p_memsz
    out.extend_from_slice(&0x1000u64.to_le_bytes());// p_align

    // ===== Code =====
    out.extend_from_slice(code);
    out
}
