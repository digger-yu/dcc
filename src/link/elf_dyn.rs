// src/link/elf_dyn.rs - ET_DYN 输出生成（PIE 可执行 / .so 共享库）
//
// 与 ET_EXEC 共享写出逻辑，但段基址为 0、类型为 ET_DYN，并强制包含 .dynamic。
// 共享库的 entry 字段为 0；PIE 可执行的 entry 与 ET_EXEC 相同。

use super::elf_exec::write_elf_internal;
use super::layout::Layout;
use crate::codegen::elf::{ET_DYN, EM_X86_64};

pub fn write_elf_pie_x86_64(layout: &Layout) -> Vec<u8> {
    let mut out = Vec::new();
    // 构造 shstrtab / strtab / symtab
    let shstrtab = build_shstrtab(layout);
    let (strtab, strtab_offsets) = build_strtab(layout);
    let symtab = build_symtab(layout, &strtab_offsets);
    let gnu_hash = build_gnu_hash(layout);
    write_elf_internal(layout, EM_X86_64, ET_DYN, &mut out, &shstrtab, &strtab, &symtab, &gnu_hash);
    out
}

pub fn write_elf_shared_x86_64(layout: &Layout) -> Vec<u8> {
    // 共享库无 entry
    let mut layout_clone = layout.clone();
    layout_clone.entry = 0;
    let mut out = Vec::new();
    let shstrtab = build_shstrtab(&layout_clone);
    let (strtab, strtab_offsets) = build_strtab(&layout_clone);
    let symtab = build_symtab(&layout_clone, &strtab_offsets);
    let gnu_hash = build_gnu_hash(&layout_clone);
    write_elf_internal(&layout_clone, EM_X86_64, ET_DYN, &mut out, &shstrtab, &strtab, &symtab, &gnu_hash);
    out
}

fn build_shstrtab(layout: &Layout) -> Vec<u8> {
    let mut s = vec![0u8];
    for sec in &layout.sections {
        s.extend_from_slice(sec.name.as_bytes());
        s.push(0);
    }
    s.extend_from_slice(b".symtab\0.strtab\0.shstrtab\0");
    s
}

fn build_strtab(layout: &Layout) -> (Vec<u8>, Vec<u32>) {
    let mut strtab = vec![0u8];
    let mut offsets = vec![0u32; layout.symbols.len()];
    for (i, sym) in layout.symbols.iter().enumerate() {
        if sym.name.is_empty() { continue; }
        offsets[i] = strtab.len() as u32;
        strtab.extend_from_slice(sym.name.as_bytes());
        strtab.push(0);
    }
    (strtab, offsets)
}

fn build_symtab(layout: &Layout, name_offsets: &[u32]) -> Vec<u8> {
    let mut symtab = Vec::with_capacity(layout.symbols.len() * 24);
    for (i, sym) in layout.symbols.iter().enumerate() {
        let st_name = name_offsets[i];
        let st_info = (sym.bind << 4) | sym.ty;
        let st_other = sym.visibility;
        let st_shndx = sym.section;
        symtab.extend_from_slice(&st_name.to_le_bytes());
        symtab.push(st_info);
        symtab.push(st_other);
        symtab.extend_from_slice(&st_shndx.to_le_bytes());
        symtab.extend_from_slice(&sym.value.to_le_bytes());
        symtab.extend_from_slice(&sym.size.to_le_bytes());
    }
    symtab
}

fn build_gnu_hash(layout: &Layout) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&1u32.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&0u64.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    let _ = layout;
    h
}
