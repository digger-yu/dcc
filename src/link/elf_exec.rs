// src/link/elf_exec.rs - ET_EXEC 输出生成
//
// 把 Layout 序列化进 ELF64 ET_EXEC 文件。
// 段布局：
//   PHDR[0]  PT_LOAD  R+X  .text + .rodata + .init + .fini
//   PHDR[1]  PT_LOAD  R+W  .data + .bss + .tdata + .tbss
//   PHDR[2]  PT_GNU_STACK  RW  (无 X)

// ELF PHDR/SHDR 写入函数需传入多个 p_* / sh_* 字段, 允许超过 7
#![allow(clippy::too_many_arguments)]

use super::layout::{Layout, OutputSectionKind, OutputSymbol};
use super::link_opts;
use crate::codegen::shared::elf::{
    ELFCLASS64, ELFDATA2LSB, ELFMAG, EM_X86_64, ET_EXEC, EV_CURRENT, PF_R, PF_W, PF_X,
    PT_GNU_RELRO, PT_GNU_STACK, PT_LOAD, PT_PHDR, SHT_GROUP, SHT_NOBITS, SHT_PROGBITS,
    SHT_STRTAB, SHT_SYMTAB,
};

pub fn write_elf_exec(layout: &Layout, machine: u16) -> Vec<u8> {
    let mut shstrtab = vec![0u8];
    for sec in &layout.sections {
        for byte in sec.name.as_bytes() {
            shstrtab.push(*byte);
        }
        shstrtab.push(0);
    }
    shstrtab.extend_from_slice(b".symtab\0.strtab\0.shstrtab\0");
    // COMDAT group section names (".group.<signature>")
    let mut comdat_group_shnames: Vec<(u32, u32, Vec<u32>)> = Vec::new();
    for grp in &layout.comdat_groups {
        let name = format!(".group.{}\0", grp.signature);
        let name_off = shstrtab.len() as u32;
        shstrtab.extend_from_slice(name.as_bytes());
        comdat_group_shnames.push((name_off, grp.flags, grp.member_sec_indices.clone()));
    }

    let mut strtab = vec![0u8];
    let mut strtab_offsets = vec![0u32; layout.symbols.len()];
    for (i, sym) in layout.symbols.iter().enumerate() {
        if sym.name.is_empty() {
            continue;
        }
        strtab_offsets[i] = strtab.len() as u32;
        for byte in sym.name.bytes() {
            strtab.push(byte);
        }
        strtab.push(0);
    }

    let mut symtab = Vec::with_capacity(layout.symbols.len() * 24);
    for (i, sym) in layout.symbols.iter().enumerate() {
        let st_name = strtab_offsets[i];
        let st_info = (sym.bind << 4) | sym.ty;
        let st_other = sym.visibility;
        // section 是 0-based 内部索引。ELF 节 0 是 NULL 节，所以 st_shndx = section + 1。
        // i==0 的符号条目总是 UNDEF（st_shndx=0）。
        // 未定义符号使用 u16::MAX 作为哨兵值，映射到 SHN_UNDEF (0)。
        let st_shndx = if i == 0 || sym.section == u16::MAX {
            0 // SHN_UNDEF
        } else {
            sym.section + 1
        };
        // 对于 ET_EXEC（可执行文件），st_value 是绝对虚拟地址
        symtab.extend_from_slice(&st_name.to_le_bytes());
        symtab.push(st_info);
        symtab.push(st_other);
        symtab.extend_from_slice(&st_shndx.to_le_bytes());
        symtab.extend_from_slice(&sym.vaddr.to_le_bytes());
        symtab.extend_from_slice(&sym.size.to_le_bytes());
    }

    let gnu_hash = build_gnu_hash(&layout.symbols);

    let mut out = Vec::new();
    write_elf_internal(
        layout,
        machine,
        ET_EXEC,
        &mut out,
        &shstrtab,
        &strtab,
        &symtab,
        &gnu_hash,
        &comdat_group_shnames,
    );
    out
}

fn segment_range_actual(
    layout: &Layout,
    seg: u8,
    section_offsets: &[u64],
    _data_base: u64,
) -> (u64, u64, u64, u64) {
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    let mut fo = u64::MAX;
    let mut fend = 0u64;
    for (i, sec) in layout.sections.iter().enumerate() {
        if sec.load_segment != seg {
            continue;
        }
        if sec.ty == SHT_NOBITS {
            continue;
        }
        if sec.size == 0 {
            continue;
        }
        lo = lo.min(sec.vaddr);
        hi = hi.max(sec.vaddr + sec.size);
        let actual_off = section_offsets.get(i).copied().unwrap_or(sec.file_off);
        fo = fo.min(actual_off);
        fend = fend.max(actual_off + sec.size);
    }
    if lo == u64::MAX {
        return (0, 0, 0, 0);
    }
    (lo, hi, fo, fend - fo)
}

fn build_gnu_hash(symbols: &[OutputSymbol]) -> Vec<u8> {
    // 简化 GNU hash 表：仅头 + bucket count = 0
    let mut h = Vec::new();
    h.extend_from_slice(&1u32.to_le_bytes()); // nbuckets
    h.extend_from_slice(&0u32.to_le_bytes()); // symoffset
    h.extend_from_slice(&0u32.to_le_bytes()); // bloom_size
    h.extend_from_slice(&0u32.to_le_bytes()); // bloom_shift
    h.extend_from_slice(&0u64.to_le_bytes()); // bloom[0]
    h.extend_from_slice(&0u32.to_le_bytes()); // buckets[0]
    h.extend_from_slice(&0u32.to_le_bytes()); // chain[0]
    let _ = symbols;
    h
}

pub fn write_elf_internal(
    layout: &Layout,
    machine: u16,
    e_type: u16,
    out: &mut Vec<u8>,
    shstrtab: &[u8],
    strtab: &[u8],
    symtab: &[u8],
    gnu_hash: &[u8],
    comdat_groups: &[(u32, u32, Vec<u32>)], // (sh_name, flags, member_sec_indices)
) {
    // 1. ELF 头占位
    let _ehdr_off = 0u64;
    out.resize(64, 0);
    // 2. PHDR
    // 计算 nphdr: PT_PHDR(1) + PT_LOAD(2) + PT_GNU_STACK(1) + 可选 PT_DYNAMIC(1) + 可选 PT_GNU_RELRO(1) + 可选 PT_INTERP(1)
    // PT_PHDR 必须有: 动态链接器用 l_addr = AT_PHDR - p_vaddr 计算主程序 load
    // bias, 缺失时 l_addr=0, 所有 D_PTR 解析出未加 bias 的地址 (运行段错误)。
    let has_relro = link_opts::compute_relro_range(layout).is_some();
    let has_interp = layout.dynamic_linker.is_some();
    let nphdr = 1
        + if layout.has_dynamic { 4u16 } else { 3u16 }
        + if has_relro { 1u16 } else { 0u16 }
        + if has_interp { 1u16 } else { 0u16 };
    let phdr_off = 64u64;
    out.resize(64 + nphdr as usize * 56, 0);

    // 3. 计算节数据偏移
    let data_base: u64 = (64 + nphdr as usize * 56) as u64;
    // ET_EXEC/ET_DYN: 节文件偏移起点由 layout 阶段确定 (file_base, 在 ELF 头 + phdr
    // 最大数量之后), 节 vaddr 起点 = base_addr + file_base, 保证
    // file_off ≡ vaddr (mod page), 且首个 PT_LOAD 段可从文件偏移 0 开始
    // (覆盖 ELF 头 + phdr, 动态链接器 AT_PHDR 需要)。
    // ET_REL (file_base=0): 无 LOAD 段, 节紧跟 phdr 区域, 对齐到 16 即可。
    let data_aligned = if layout.file_base != 0 {
        layout.file_base
    } else {
        (data_base + 15) & !15
    };
    // 节文件偏移 = assign_section_addrs 分配的 file_off + 起点补偿
    let section_offsets: Vec<u64> = layout
        .sections
        .iter()
        .map(|s| s.file_off + (data_aligned - layout.file_base))
        .collect();
    // 加上 .gnu.hash .symtab .strtab .shstrtab (接在最后一个节数据之后)
    let mut cur = layout
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| section_offsets[i] + if s.ty == SHT_NOBITS { 0 } else { s.size })
        .max()
        .unwrap_or(data_aligned);
    // COMDAT group data (SHT_GROUP contents: flag word + member section indices)
    // Each group's data is placed after the user sections and before .gnu.hash.
    let mut comdat_data_offsets: Vec<u64> = Vec::with_capacity(comdat_groups.len());
    for (_sh_name, _flags, member_indices) in comdat_groups {
        comdat_data_offsets.push(cur);
        // 4 bytes flag word + 4 bytes per member section index
        cur += (4 + member_indices.len() * 4) as u64;
    }
    let gnu_hash_off = cur;
    cur += gnu_hash.len() as u64;
    let symtab_off = cur;
    cur += symtab.len() as u64;
    let strtab_off = cur;
    cur += strtab.len() as u64;
    let shstrtab_off = cur;
    cur += shstrtab.len() as u64;
    // shdr
    let shdr_off = (cur + 7) & !7;

    // 4. 写入各节数据
    // 节偏移按 load_segment 分组分配, 索引顺序与偏移顺序不一致
    // (段 0 末尾节 .interp 的偏移可能小于段 1 节): 先扩展文件到总长度,
    // 再按偏移覆盖写入, 避免 resize 截断已写数据 / extend 写错位置。
    let mut total_len: usize = 0;
    for (i, sec) in layout.sections.iter().enumerate() {
        let end = section_offsets[i] as usize + sec.data.len();
        total_len = total_len.max(end);
    }
    out.resize(total_len, 0);
    for (i, sec) in layout.sections.iter().enumerate() {
        let off = section_offsets[i] as usize;
        if sec.ty != SHT_NOBITS && !sec.data.is_empty() {
            out[off..off + sec.data.len()].copy_from_slice(&sec.data);
        }
    }
    // COMDAT group data (SHT_GROUP contents)
    for (i, (_sh_name, flags, member_indices)) in comdat_groups.iter().enumerate() {
        let off = comdat_data_offsets[i] as usize;
        out.resize(off + 4 + member_indices.len() * 4, 0);
        out[off..off + 4].copy_from_slice(&flags.to_le_bytes());
        for (j, &member_idx) in member_indices.iter().enumerate() {
            // ELF section indices are 1-based (slot 0 is SHT_NULL)
            let elf_idx = member_idx + 1;
            let pos = off + 4 + j * 4;
            out[pos..pos + 4].copy_from_slice(&elf_idx.to_le_bytes());
        }
    }
    // .gnu.hash
    out.resize(gnu_hash_off as usize, 0);
    out.extend_from_slice(gnu_hash);
    out.resize(symtab_off as usize, 0);
    out.extend_from_slice(symtab);
    out.resize(strtab_off as usize, 0);
    out.extend_from_slice(strtab);
    out.resize(shstrtab_off as usize, 0);
    out.extend_from_slice(shstrtab);
    out.resize(shdr_off as usize, 0);

    // 5. 段 (PHDR) 范围, 使用实际 section_offsets 计算文件偏移
    let (s0_lo, s0_hi, s0_fo, s0_fsz) =
        segment_range_actual(layout, 0, &section_offsets, data_base);
    let (s1_lo, s1_hi, s1_fo, s1_fsz) =
        segment_range_actual(layout, 1, &section_offsets, data_base);
    let s0_memsz = s0_hi - s0_lo;
    let s1_memsz = s1_hi - s1_lo;

    // 6. 写 PHDR
    let mut phdr_cursor = phdr_off as usize;
    // PT_PHDR: 指向本程序头表自身。p_vaddr = base_addr + 64 (ELF 头后),
    // 使 ld.so 的 l_addr = AT_PHDR - p_vaddr 等于实际 load bias。
    write_phdr(
        out,
        &mut phdr_cursor,
        PT_PHDR,
        PF_R,
        64,
        layout.base_addr + 64,
        layout.base_addr + 64,
        nphdr as u64 * 56,
        nphdr as u64 * 56,
        8,
    );
    // PT_LOAD #1: 从文件偏移 0 开始, 覆盖 ELF 头 + phdr + 段 0 节数据;
    // p_vaddr = base_addr (与 p_offset=0 同余), 这样 AT_PHDR = base + e_phoff
    // 指向可访问内存, 动态链接器才能解析主程序映射。
    write_phdr(
        out,
        &mut phdr_cursor,
        PT_LOAD,
        PF_R | PF_X,
        0,
        layout.base_addr,
        layout.base_addr,
        s0_fsz + s0_fo,
        s0_memsz + s0_fo,
        0x1000,
    );
    // PT_LOAD #2
    write_phdr(
        out,
        &mut phdr_cursor,
        PT_LOAD,
        PF_R | PF_W,
        s1_fo,
        s1_lo,
        s1_lo,
        s1_fsz,
        s1_memsz,
        0x1000,
    );
    // PT_GNU_STACK
    write_phdr(
        out,
        &mut phdr_cursor,
        PT_GNU_STACK,
        PF_R | PF_W,
        0,
        0,
        0,
        0,
        0,
        0,
    );
    // PT_GNU_RELRO: 标识 RELRO 覆盖范围 (default; 由 link_opts 控制是否实际写入)
    if let Some((lo, hi)) = link_opts::compute_relro_range(layout) {
        // 文件偏移: 找出落在 (lo,hi) 范围内的最早一节 (用实际节偏移)
        let fo = layout
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.vaddr >= lo && s.vaddr < hi)
            .map(|(i, s)| section_offsets.get(i).copied().unwrap_or(s.file_off))
            .min()
            .unwrap_or(0);
        let fsz = (hi - lo).min(
            layout
                .sections
                .iter()
                .enumerate()
                .filter(|(_, s)| s.vaddr >= lo && s.vaddr < hi)
                .map(|(i, s)| {
                    let off = section_offsets.get(i).copied().unwrap_or(s.file_off);
                    off + s.size - fo
                })
                .sum::<u64>(),
        );
        write_phdr(
            out,
            &mut phdr_cursor,
            PT_GNU_RELRO,
            PF_R,
            fo,
            lo,
            lo,
            fsz,
            hi - lo,
            0x1000,
        );
    }
    if layout.has_dynamic {
        // 找到 .dynamic 节 (用实际节偏移)
        let dyn_idx = layout
            .sections
            .iter()
            .position(|s| s.kind == OutputSectionKind::Dynamic);
        let dyn_vaddr = dyn_idx.map(|i| layout.sections[i].vaddr).unwrap_or(0);
        let dyn_filesz = dyn_idx.map(|i| layout.sections[i].size).unwrap_or(0);
        let dyn_fileoff = dyn_idx
            .map(|i| section_offsets.get(i).copied().unwrap_or(0))
            .unwrap_or(0);
        write_phdr(
            out,
            &mut phdr_cursor,
            2, /*PT_DYNAMIC*/
            PF_R | PF_W,
            dyn_fileoff,
            dyn_vaddr,
            dyn_vaddr,
            dyn_filesz,
            dyn_filesz,
            8,
        );
    }
    // PT_INTERP: 动态链接器路径
    if let Some(ref interp) = layout.dynamic_linker {
        // interp 字符串已经在节数据中写入 (通过 .interp 节)
        // 此处只写 PHDR (用实际节偏移)
        let interp_idx = layout.sections.iter().position(|s| s.name == ".interp");
        let interp_vaddr = interp_idx.map(|i| layout.sections[i].vaddr).unwrap_or(0);
        let interp_filesz = (interp.len() + 1) as u64; // +1 for NUL
        let interp_fileoff = interp_idx
            .map(|i| section_offsets.get(i).copied().unwrap_or(0))
            .unwrap_or(0);
        write_phdr(
            out,
            &mut phdr_cursor,
            3, /*PT_INTERP*/
            PF_R,
            interp_fileoff,
            interp_vaddr,
            interp_vaddr,
            interp_filesz,
            interp_filesz,
            1,
        );
    }

    // 7. 节头表
    // 总数 = SHT_NULL + 用户节 + COMDAT 组 + .gnu.hash + .symtab + .strtab + .shstrtab
    let nshdr = 1 + layout.sections.len() + comdat_groups.len() + 5;
    // 预留所有节头空间
    out.resize(shdr_off as usize + nshdr * 64, 0);
    let mut shdr_cursor = shdr_off as usize;
    // SHT_NULL
    write_shdr(out, &mut shdr_cursor, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    // 各节
    for (i, sec) in layout.sections.iter().enumerate() {
        let sh_name = lookup_sh_name(shstrtab, &sec.name);
        let sh_link = if sec.kind == OutputSectionKind::Dynamic {
            // .dynamic 的 sh_link 指向 .dynstr
            let dynstr_idx = layout
                .sections
                .iter()
                .position(|s| s.kind == OutputSectionKind::DynStr)
                .unwrap_or(0) as u32
                + 1;
            dynstr_idx
        } else {
            0
        };
        write_shdr(
            out,
            &mut shdr_cursor,
            sh_name,
            sec.ty,
            sec.flags as u64,
            sec.vaddr,
            section_offsets[i],
            sec.size,
            sh_link,
            0,
            sec.align,
            sec.entsize,
        );
    }
    // COMDAT group section headers (SHT_GROUP)
    // sh_link must point to the .symtab section.
    // .symtab is at index: 1 (NULL) + N (user) + G (comdat) + 1 (.gnu.hash)
    let symtab_shdr_idx = (1 + layout.sections.len() + comdat_groups.len() + 1) as u32;
    for (i, (sh_name, _flags, member_indices)) in comdat_groups.iter().enumerate() {
        let data_size = (4 + member_indices.len() * 4) as u64;
        write_shdr(
            out,
            &mut shdr_cursor,
            *sh_name,
            SHT_GROUP,
            0,
            0,
            comdat_data_offsets[i],
            data_size,
            symtab_shdr_idx, // sh_link: index of .symtab
            0, // sh_info: signature symbol index (0 = unnamed group)
            4, // 4-byte alignment (flag word + section indices)
            4, // sh_entsize: size of each entry (section index)
        );
    }
    // .gnu.hash
    write_shdr(
        out,
        &mut shdr_cursor,
        0,
        0xa000_0000,
        0,
        0,
        gnu_hash_off,
        gnu_hash.len() as u64,
        0,
        0,
        8,
        0,
    );
    // .symtab
    let n_locals = layout.n_locals;
    // Section header indices: SHT_NULL(1) + user_sections(N) + comdat_groups(G)
    // then .gnu.hash, .symtab, .strtab, .shstrtab, .note.GNU-stack.
    // .strtab is at index (1 + N + G + 2).
    let strtab_shdr_idx = (1 + layout.sections.len() + comdat_groups.len() + 2) as u32;
    write_shdr(
        out,
        &mut shdr_cursor,
        0,
        SHT_SYMTAB,
        0,
        0,
        symtab_off,
        symtab.len() as u64,
        strtab_shdr_idx,
        n_locals,
        8,
        24,
    );
    // .strtab
    write_shdr(
        out,
        &mut shdr_cursor,
        0,
        SHT_STRTAB,
        0,
        0,
        strtab_off,
        strtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    // .shstrtab
    let shstrtab_shdr_idx = (1 + layout.sections.len() + comdat_groups.len() + 3) as u32;
    write_shdr(
        out,
        &mut shdr_cursor,
        0,
        SHT_STRTAB,
        0,
        0,
        shstrtab_off,
        shstrtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    // .note.GNU-stack
    write_shdr(
        out,
        &mut shdr_cursor,
        0,
        SHT_PROGBITS,
        0,
        0,
        shstrtab_off,
        0,
        0,
        0,
        1,
        0,
    );

    // 8. ELF 头
    write_elf_header(
        out,
        e_type,
        machine,
        layout.entry,
        phdr_off,
        shdr_off,
        nphdr,
        nshdr as u16,
        shstrtab_shdr_idx as u16,
    );
    let _ = shstrtab_shdr_idx;
}

fn lookup_sh_name(shstrtab: &[u8], name: &str) -> u32 {
    // 找 name 在 shstrtab 中的偏移
    if name.is_empty() {
        return 0;
    }
    let needle = name.as_bytes();
    let mut i = 0;
    while i < shstrtab.len() {
        let end = shstrtab[i..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(shstrtab.len() - i)
            + i;
        if end - i == needle.len() && &shstrtab[i..end] == needle {
            return i as u32;
        }
        i = end + 1;
    }
    0
}

fn write_phdr(
    out: &mut [u8],
    cur: &mut usize,
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
) {
    out[*cur..*cur + 4].copy_from_slice(&p_type.to_le_bytes());
    out[*cur + 4..*cur + 8].copy_from_slice(&p_flags.to_le_bytes());
    out[*cur + 8..*cur + 16].copy_from_slice(&p_offset.to_le_bytes());
    out[*cur + 16..*cur + 24].copy_from_slice(&p_vaddr.to_le_bytes());
    out[*cur + 24..*cur + 32].copy_from_slice(&p_paddr.to_le_bytes());
    out[*cur + 32..*cur + 40].copy_from_slice(&p_filesz.to_le_bytes());
    out[*cur + 40..*cur + 48].copy_from_slice(&p_memsz.to_le_bytes());
    out[*cur + 48..*cur + 56].copy_from_slice(&p_align.to_le_bytes());
    *cur += 56;
}

fn write_shdr(
    out: &mut [u8],
    cur: &mut usize,
    sh_name: u32,
    sh_type: u32,
    sh_flags: u64,
    sh_addr: u64,
    sh_offset: u64,
    sh_size: u64,
    sh_link: u32,
    sh_info: u32,
    sh_addralign: u64,
    sh_entsize: u64,
) {
    out[*cur..*cur + 4].copy_from_slice(&sh_name.to_le_bytes());
    out[*cur + 4..*cur + 8].copy_from_slice(&sh_type.to_le_bytes());
    out[*cur + 8..*cur + 16].copy_from_slice(&sh_flags.to_le_bytes());
    out[*cur + 16..*cur + 24].copy_from_slice(&sh_addr.to_le_bytes());
    out[*cur + 24..*cur + 32].copy_from_slice(&sh_offset.to_le_bytes());
    out[*cur + 32..*cur + 40].copy_from_slice(&sh_size.to_le_bytes());
    out[*cur + 40..*cur + 44].copy_from_slice(&sh_link.to_le_bytes());
    out[*cur + 44..*cur + 48].copy_from_slice(&sh_info.to_le_bytes());
    out[*cur + 48..*cur + 56].copy_from_slice(&sh_addralign.to_le_bytes());
    out[*cur + 56..*cur + 64].copy_from_slice(&sh_entsize.to_le_bytes());
    *cur += 64;
}

fn write_elf_header(
    out: &mut [u8],
    e_type: u16,
    machine: u16,
    entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_phnum: u16,
    e_shnum: u16,
    e_shstrndx: u16,
) {
    out[0..4].copy_from_slice(&ELFMAG);
    out[4] = ELFCLASS64;
    out[5] = ELFDATA2LSB;
    out[6] = EV_CURRENT;
    out[7] = 0;
    out[8..16].copy_from_slice(&[0u8; 8]);
    out[16..18].copy_from_slice(&e_type.to_le_bytes());
    out[18..20].copy_from_slice(&machine.to_le_bytes());
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[24..32].copy_from_slice(&entry.to_le_bytes());
    out[32..40].copy_from_slice(&e_phoff.to_le_bytes());
    out[40..48].copy_from_slice(&e_shoff.to_le_bytes());
    out[48..52].copy_from_slice(&0u32.to_le_bytes());
    out[52..54].copy_from_slice(&64u16.to_le_bytes());
    out[54..56].copy_from_slice(&56u16.to_le_bytes());
    out[56..58].copy_from_slice(&e_phnum.to_le_bytes());
    out[58..60].copy_from_slice(&64u16.to_le_bytes());
    out[60..62].copy_from_slice(&e_shnum.to_le_bytes());
    out[62..64].copy_from_slice(&e_shstrndx.to_le_bytes());
}

pub fn write_elf_exec_x86_64(layout: &Layout) -> Vec<u8> {
    write_elf_exec(layout, EM_X86_64)
}

pub fn write_elf_exec_aarch64(layout: &Layout) -> Vec<u8> {
    write_elf_exec(layout, crate::codegen::shared::elf::EM_AARCH64)
}

pub fn write_elf_exec_riscv64(layout: &Layout) -> Vec<u8> {
    write_elf_exec(layout, crate::codegen::shared::elf::EM_RISCV)
}

pub fn write_elf_exec_riscv32(layout: &Layout) -> Vec<u8> {
    // RISC-V 32 与 64 共享同一 e_machine (EM_RISCV).
    // TODO: 应使用 ELFCLASS32 而非 ELFCLASS64, 待完整 32-bit ELF 支持后修正.
    write_elf_exec(layout, crate::codegen::shared::elf::EM_RISCV)
}

pub fn write_elf_exec_mips64(layout: &Layout) -> Vec<u8> {
    write_elf_exec(layout, crate::codegen::shared::elf::EM_MIPS)
}

pub fn write_elf_exec_arm(layout: &Layout) -> Vec<u8> {
    write_elf_exec(layout, crate::codegen::shared::elf::EM_ARM)
}

/// 自研 -r 路径: 生成 ET_REL (可重定位) .o 文件
/// 与 host ld -r 不同, 此路径完全自研: 复制所有节、重写重定位、合并符号表
pub fn write_elf_relocatable_x86_64(layout: &Layout) -> Vec<u8> {
    use crate::codegen::shared::elf::ET_REL;
    // 简化: 复用 write_elf_exec, 改写 e_type
    // 完整 -r 路径需要: 不写 PHDR, 不解析符号, 直接复制节
    let mut out = write_elf_exec(layout, EM_X86_64);
    out[16..18].copy_from_slice(&ET_REL.to_le_bytes());
    out
}

pub fn write_elf_relocatable_aarch64(layout: &Layout) -> Vec<u8> {
    use crate::codegen::shared::elf::ET_REL;
    let mut out = write_elf_exec(layout, crate::codegen::shared::elf::EM_AARCH64);
    out[16..18].copy_from_slice(&ET_REL.to_le_bytes());
    out
}

pub fn write_elf_relocatable_riscv64(layout: &Layout) -> Vec<u8> {
    use crate::codegen::shared::elf::ET_REL;
    let mut out = write_elf_exec(layout, crate::codegen::shared::elf::EM_RISCV);
    out[16..18].copy_from_slice(&ET_REL.to_le_bytes());
    out
}

pub fn write_elf_relocatable_riscv32(layout: &Layout) -> Vec<u8> {
    use crate::codegen::shared::elf::ET_REL;
    let mut out = write_elf_exec(layout, crate::codegen::shared::elf::EM_RISCV);
    out[16..18].copy_from_slice(&ET_REL.to_le_bytes());
    out
}

pub fn write_elf_relocatable_mips64(layout: &Layout) -> Vec<u8> {
    use crate::codegen::shared::elf::ET_REL;
    let mut out = write_elf_exec(layout, crate::codegen::shared::elf::EM_MIPS);
    out[16..18].copy_from_slice(&ET_REL.to_le_bytes());
    out
}

pub fn write_elf_relocatable_arm(layout: &Layout) -> Vec<u8> {
    use crate::codegen::shared::elf::ET_REL;
    let mut out = write_elf_exec(layout, crate::codegen::shared::elf::EM_ARM);
    out[16..18].copy_from_slice(&ET_REL.to_le_bytes());
    out
}
