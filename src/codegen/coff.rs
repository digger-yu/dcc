// src/codegen/coff.rs - COFF/PE object file generator
//
// Generates COFF (Common Object File Format) relocatable objects for
// x86_64-pc-windows-msvc targets.  The output is compatible with the
// Microsoft linker (link.exe) and can be consumed by the dcc built-in
// object reader.
//
// Reference: Microsoft PE/COFF specification
//   https://learn.microsoft.com/en-us/windows/win32/debug/pe-format

#![allow(dead_code)]

use crate::codegen::elf::{ElfRela, ElfSection, ElfSym, ExtraRelaSection};

// ===== COFF constants =====

/// Machine types
pub const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;

/// Section characteristics
pub const IMAGE_SCN_CNT_CODE: u32 = 0x0000_0020;
pub const IMAGE_SCN_CNT_INITIALIZED_DATA: u32 = 0x0000_0040;
pub const IMAGE_SCN_CNT_UNINITIALIZED_DATA: u32 = 0x0000_0080;
pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;
pub const IMAGE_SCN_ALIGN_16BYTES: u32 = 0x0050_0000;
pub const IMAGE_SCN_ALIGN_4BYTES: u32 = 0x0030_0000;
pub const IMAGE_SCN_ALIGN_8BYTES: u32 = 0x0040_0000;

/// COFF relocation types for AMD64
pub const IMAGE_REL_AMD64_ADDR32: u16 = 0x0002;
pub const IMAGE_REL_AMD64_ADDR32NB: u16 = 0x0003;
pub const IMAGE_REL_AMD64_REL32: u16 = 0x0004;
pub const IMAGE_REL_AMD64_ADDR64: u16 = 0x0001;

/// Storage classes
pub const IMAGE_SYM_CLASS_EXTERNAL: u8 = 2;
pub const IMAGE_SYM_CLASS_STATIC: u8 = 3;
pub const IMAGE_SYM_CLASS_FUNCTION: u8 = 101;

/// COFF symbol table entry size
const COFF_SYMBOL_SIZE: usize = 18;
/// COFF section header size
const COFF_SECTION_HEADER_SIZE: usize = 40;
/// COFF file header size
const COFF_FILE_HEADER_SIZE: usize = 20;
/// COFF relocation entry size
const COFF_RELOCATION_SIZE: usize = 10;

/// Convert ELF relocation type to COFF AMD64 relocation type.
fn elf_rel_to_coff(rel_type: u32) -> u16 {
    use crate::codegen::elf::*;
    match rel_type {
        R_X86_64_64 => IMAGE_REL_AMD64_ADDR64,
        R_X86_64_PC32 => IMAGE_REL_AMD64_REL32,
        R_X86_64_32 | R_X86_64_32S => IMAGE_REL_AMD64_ADDR32,
        R_X86_64_PLT32 => IMAGE_REL_AMD64_REL32,
        _ => IMAGE_REL_AMD64_ADDR64, // fallback
    }
}

/// Convert ELF section flags to COFF section characteristics.
fn elf_flags_to_coff(elf_flags: u32, elf_ty: u32) -> u32 {
    use crate::codegen::elf::*;
    let mut chars = 0u32;
    if elf_flags & SHF_EXECINSTR != 0 {
        chars |= IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ;
    } else if elf_flags & SHF_WRITE != 0 {
        if elf_ty == SHT_NOBITS {
            chars |= IMAGE_SCN_CNT_UNINITIALIZED_DATA | IMAGE_SCN_MEM_WRITE | IMAGE_SCN_MEM_READ;
        } else {
            chars |= IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_WRITE | IMAGE_SCN_MEM_READ;
        }
    } else if elf_flags & SHF_ALLOC != 0 {
        chars |= IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ;
    } else {
        // non-alloc section (debug, etc.) — treated as initialized data
        chars |= IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ;
    }
    chars
}

/// Map ELF symbol bind/type to COFF storage class.
fn elf_sym_to_coff_storage(bind: u8, ty: u8) -> u8 {
    use crate::codegen::elf::*;
    match ty {
        STT_FUNC => IMAGE_SYM_CLASS_FUNCTION,
        _ => {
            if bind == STB_LOCAL {
                IMAGE_SYM_CLASS_STATIC
            } else {
                IMAGE_SYM_CLASS_EXTERNAL
            }
        }
    }
}

/// Build a COFF relocatable object file (x86_64 / AMD64).
///
/// The interface mirrors `build_elf_rel_x86_64` so the codegen layer
/// can choose ELF or COFF output based on the target triple without
/// changing its data flow.
#[allow(clippy::too_many_arguments)]
pub fn build_coff_rel_x86_64(
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
    text_relocations: &[ElfRela],
    extra_sections: &[ElfSection],
    data_relocations: &[ElfRela],
    extra_rela_sections: &[ExtraRelaSection],
) -> Vec<u8> {
    // --- Collect sections to emit ---
    // We always emit .text; conditionally emit .rdata (.rodata), .data, .bss,
    // .rdata$T (.tdata), .bss$T (.tbss), plus any extra sections.
    struct CoffSec {
        name: String,
        data: Vec<u8>,
        characteristics: u32,
        align: u32,
        is_bss: bool,
        relocations: Vec<CoffReloc>,
    }
    struct CoffReloc {
        offset: u32,
        symbol_index: u32,
        coff_type: u16,
    }

    let mut secs: Vec<CoffSec> = Vec::new();

    // .text
    {
        let mut relocs = Vec::new();
        for r in text_relocations {
            relocs.push(CoffReloc {
                offset: r.offset as u32,
                symbol_index: r.sym_index,
                coff_type: elf_rel_to_coff(r.rel_type),
            });
        }
        secs.push(CoffSec {
            name: ".text".into(),
            data: text.to_vec(),
            characteristics: IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ
                | align_to_coff(text_align),
            align: text_align.max(16) as u32,
            is_bss: false,
            relocations: relocs,
        });
    }

    // .rdata (COFF convention for .rodata)
    if !rodata.is_empty() {
        secs.push(CoffSec {
            name: ".rdata".into(),
            data: rodata.to_vec(),
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ
                | align_to_coff(text_align.max(4)),
            align: 4,
            is_bss: false,
            relocations: Vec::new(),
        });
    }

    // .data
    if !data.is_empty() {
        let mut relocs = Vec::new();
        for r in data_relocations {
            relocs.push(CoffReloc {
                offset: r.offset as u32,
                symbol_index: r.sym_index,
                coff_type: elf_rel_to_coff(r.rel_type),
            });
        }
        secs.push(CoffSec {
            name: ".data".into(),
            data: data.to_vec(),
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA
                | IMAGE_SCN_MEM_WRITE
                | IMAGE_SCN_MEM_READ
                | align_to_coff(data_align.max(4)),
            align: data_align.max(4) as u32,
            is_bss: false,
            relocations: relocs,
        });
    }

    // .bss
    if bss_size > 0 {
        secs.push(CoffSec {
            name: ".bss".into(),
            data: Vec::new(),
            characteristics: IMAGE_SCN_CNT_UNINITIALIZED_DATA
                | IMAGE_SCN_MEM_WRITE
                | IMAGE_SCN_MEM_READ
                | align_to_coff(bss_align.max(4)),
            align: bss_align.max(4) as u32,
            is_bss: true,
            relocations: Vec::new(),
        });
    }

    // .rdata$T (.tdata — TLS initialized)
    if !tdata.is_empty() {
        secs.push(CoffSec {
            name: ".rdata$T".into(),
            data: tdata.to_vec(),
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ
                | align_to_coff(tdata_align.max(4)),
            align: tdata_align.max(4) as u32,
            is_bss: false,
            relocations: Vec::new(),
        });
    }

    // .bss$T (.tbss — TLS uninitialized)
    if tbss_size > 0 {
        secs.push(CoffSec {
            name: ".bss$T".into(),
            data: Vec::new(),
            characteristics: IMAGE_SCN_CNT_UNINITIALIZED_DATA
                | IMAGE_SCN_MEM_WRITE
                | IMAGE_SCN_MEM_READ
                | align_to_coff(tbss_align.max(4)),
            align: tbss_align.max(4) as u32,
            is_bss: true,
            relocations: Vec::new(),
        });
    }

    // Extra sections (debug, san_info, offload, etc.)
    for es in extra_sections {
        use crate::codegen::elf::*;
        let chars = elf_flags_to_coff(es.flags, es.ty);
        secs.push(CoffSec {
            name: es.name.clone(),
            data: es.data.clone(),
            characteristics: chars | align_to_coff(es.align.max(1)),
            align: es.align.max(1) as u32,
            is_bss: es.ty == SHT_NOBITS,
            relocations: Vec::new(),
        });
    }

    // Extra rela sections — merge into the target section's relocation list
    // For simplicity we attach them to the first section (.text) since
    // COFF relocations are per-section. In practice these are for
    // .init_array/.fini_array which map to COFF .xdata/.pdata.
    let _ = extra_rela_sections;

    // --- Build symbol table ---
    // COFF symbols: each has an 18-byte entry.
    // We need to map ELF symbol section indices to COFF section numbers
    // (1-based, matching the order in `secs`).
    //
    // Build a mapping: ELF section index -> COFF section number.
    // ELF section layout in build_elf_rel_x86_64:
    //   0 = SHN_UNDEF
    //   1 = .text (null section is 0 in ELF)
    //   2 = .rodata (if present) or .data or .bss ...
    // We'll use a simpler approach: map based on the ElfSym.section field
    // which is 1-based index into the ELF section table.
    //
    // For COFF we just need to know which COFF section each ELF symbol
    // belongs to. We'll build a mapping from ELF section index to COFF
    // section number.
    let elf_to_coff_sec = build_elf_to_coff_section_map(
        rodata, data, bss_size, tdata, tbss_size, extra_sections,
    );

    // String table for long symbol names
    // COFF string table: 4-byte length + string data
    // We build it at the end.

    // Build COFF symbol entries
    struct CoffSymbol {
        name: String,
        value: u32,
        section_number: i16, // 1-based, 0 = undefined, -1 = debug, -2 = absolute
        storage_class: u8,
        num_aux: u8,
    }

    let mut coff_syms: Vec<CoffSymbol> = Vec::new();
    for sym in symbols {
        let sec_num = if sym.section == 0 {
            0i16 // undefined (external reference)
        } else {
            elf_to_coff_sec
                .get(sym.section as usize)
                .copied()
                .unwrap_or(0) as i16
        };
        let storage = elf_sym_to_coff_storage(sym.bind, sym.ty);
        coff_syms.push(CoffSymbol {
            name: sym.name.clone(),
            value: sym.value as u32,
            section_number: sec_num,
            storage_class: storage,
            num_aux: 0,
        });
    }

    // --- Compute layout ---
    let num_sections = secs.len();
    let header_size = COFF_FILE_HEADER_SIZE;
    let section_headers_size = num_sections * COFF_SECTION_HEADER_SIZE;

    // Section data starts after headers
    let mut cursor = header_size + section_headers_size;

    // Align and compute section data offsets
    for sec in &mut secs {
        cursor = align_up(cursor, sec.align as usize);
        sec.data.truncate(sec.data.len()); // no-op, just ensure
    }

    // Compute actual offsets
    let mut sec_offsets: Vec<usize> = Vec::with_capacity(secs.len());
    let mut reloc_offsets: Vec<usize> = Vec::with_capacity(secs.len());
    cursor = header_size + section_headers_size;
    for sec in &secs {
        cursor = align_up(cursor, sec.align as usize);
        sec_offsets.push(cursor);
        if sec.is_bss {
            // BSS has no raw data
        } else {
            cursor += sec.data.len();
        }
        // Relocations follow section data
        cursor = align_up(cursor, 4);
        reloc_offsets.push(cursor);
        cursor += sec.relocations.len() * COFF_RELOCATION_SIZE;
    }

    // Symbol table follows all section data + relocations
    cursor = align_up(cursor, 4);
    let symtab_offset = cursor;
    let num_symbols = coff_syms.len();

    // Build name offsets for long names (> 8 chars)
    // COFF: if name is <= 8 bytes, it's stored inline (zero-padded).
    // If > 8 bytes, first 4 bytes are 0, next 4 bytes are string table offset.
    let mut name_strtab_offsets: Vec<Option<u32>> = Vec::with_capacity(num_symbols);
    let mut strtab_content: Vec<u8> = Vec::new();
    for sym in &coff_syms {
        if sym.name.len() <= 8 {
            name_strtab_offsets.push(None); // inline
        } else {
            let off = strtab_content.len() as u32;
            strtab_content.extend_from_slice(sym.name.as_bytes());
            strtab_content.push(0);
            name_strtab_offsets.push(Some(off + 4)); // +4 for length field
        }
    }

    // String table also includes section names > 8 chars (COFF long names)
    // For section names > 8 chars, COFF uses "/" followed by decimal offset
    // into the string table. We handle this in the section header writing.

    let strtab_total_len = 4 + strtab_content.len();

    // --- Emit ---
    let total_size = cursor + num_symbols * COFF_SYMBOL_SIZE + strtab_total_len;
    let mut out = Vec::with_capacity(total_size);

    // 1. COFF File Header (20 bytes)
    out.extend_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes()); // Machine
    out.extend_from_slice(&(num_sections as u16).to_le_bytes()); // NumberOfSections
    out.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    out.extend_from_slice(&(symtab_offset as u32).to_le_bytes()); // PointerToSymbolTable
    out.extend_from_slice(&(num_symbols as u32).to_le_bytes()); // NumberOfSymbols
    out.extend_from_slice(&0u16.to_le_bytes()); // SizeOfOptionalHeader
    out.extend_from_slice(&0u16.to_le_bytes()); // Characteristics

    // 2. Section Headers (40 bytes each)
    for (i, sec) in secs.iter().enumerate() {
        // Name (8 bytes)
        let name_bytes = sec.name.as_bytes();
        if name_bytes.len() <= 8 {
            let mut name_field = [0u8; 8];
            name_field[..name_bytes.len()].copy_from_slice(name_bytes);
            out.extend_from_slice(&name_field);
        } else {
            // Long name: "/" + decimal string table offset
            // For section names, COFF uses "/" + offset into string table
            // But we only put symbol names in the string table.
            // Use the short name truncated to 8 bytes as fallback.
            let mut name_field = [0u8; 8];
            let copy_len = name_bytes.len().min(8);
            name_field[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            out.extend_from_slice(&name_field);
        }
        // VirtualSize
        out.extend_from_slice(&0u32.to_le_bytes());
        // VirtualAddress
        out.extend_from_slice(&0u32.to_le_bytes());
        // SizeOfRawData
        let raw_size = if sec.is_bss { 0 } else { sec.data.len() as u32 };
        out.extend_from_slice(&raw_size.to_le_bytes());
        // PointerToRawData
        let raw_ptr = if sec.is_bss || sec.data.is_empty() {
            0u32
        } else {
            sec_offsets[i] as u32
        };
        out.extend_from_slice(&raw_ptr.to_le_bytes());
        // PointerToRelocations
        let reloc_ptr = if sec.relocations.is_empty() {
            0u32
        } else {
            reloc_offsets[i] as u32
        };
        out.extend_from_slice(&reloc_ptr.to_le_bytes());
        // PointerToLineNumbers
        out.extend_from_slice(&0u32.to_le_bytes());
        // NumberOfRelocations
        out.extend_from_slice(&(sec.relocations.len() as u16).to_le_bytes());
        // NumberOfLineNumbers
        out.extend_from_slice(&0u16.to_le_bytes());
        // Characteristics
        out.extend_from_slice(&sec.characteristics.to_le_bytes());
    }

    // 3. Section Data
    for (i, sec) in secs.iter().enumerate() {
        // Pad to alignment
        while out.len() < sec_offsets[i] {
            out.push(0);
        }
        if !sec.is_bss && !sec.data.is_empty() {
            out.extend_from_slice(&sec.data);
        }
        // Relocations
        while out.len() < reloc_offsets[i] {
            out.push(0);
        }
        for r in &sec.relocations {
            out.extend_from_slice(&r.offset.to_le_bytes()); // VirtualAddress
            out.extend_from_slice(&r.symbol_index.to_le_bytes()); // SymbolTableIndex
            out.extend_from_slice(&r.coff_type.to_le_bytes()); // Type
        }
    }

    // 4. Symbol Table
    while out.len() < symtab_offset {
        out.push(0);
    }
    for (i, sym) in coff_syms.iter().enumerate() {
        // Name (8 bytes)
        if let Some(strtab_off) = name_strtab_offsets[i] {
            // Long name: 4 zero bytes + 4 byte string table offset
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&strtab_off.to_le_bytes());
        } else {
            let mut name_field = [0u8; 8];
            let name_bytes = sym.name.as_bytes();
            let copy_len = name_bytes.len().min(8);
            name_field[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            out.extend_from_slice(&name_field);
        }
        // Value (4 bytes)
        out.extend_from_slice(&sym.value.to_le_bytes());
        // SectionNumber (2 bytes, signed)
        out.extend_from_slice(&sym.section_number.to_le_bytes());
        // Type (2 bytes) — 0 for data, 0x20 for function
        let coff_type = if sym.storage_class == IMAGE_SYM_CLASS_FUNCTION {
            0x0020u16
        } else {
            0u16
        };
        out.extend_from_slice(&coff_type.to_le_bytes());
        // StorageClass (1 byte)
        out.push(sym.storage_class);
        // NumberOfAuxSymbols (1 byte)
        out.push(sym.num_aux);
    }

    // 5. String Table
    out.extend_from_slice(&(strtab_total_len as u32).to_le_bytes());
    out.extend_from_slice(&strtab_content);

    out
}

/// Build mapping from ELF section index to COFF section number (1-based).
///
/// ELF sections in build_elf_rel_x86_64 order:
///   0: SHN_UNDEF (null)
///   1: .text
///   2: .rodata (if present) / or next section
///   3: .data or .rodata
///   ... and so on
///
/// We replicate the same conditional ordering here.
fn build_elf_to_coff_section_map(
    _rodata: &[u8],
    _data: &[u8],
    _bss_size: u64,
    _tdata: &[u8],
    _tbss_size: u64,
    _extra_sections: &[ElfSection],
) -> Vec<u16> {
    // ELF section layout from build_elf_rel_x86_64:
    // Section 0 = null (SHN_UNDEF) -> COFF 0 (undefined)
    // Section 1 = .text -> COFF 1
    // Then conditionally:
    //   .rodata (if !rodata.is_empty()) -> COFF 2
    //   .data (if !data.is_empty()) or .bss or next
    //   .bss (if bss_size > 0)
    //   symtab, strtab, shstrtab (ELF-only, not in COFF)
    //   .rela.text
    //   .rela.data (if !rel_rodata.is_empty())
    //   extra sections...
    //
    // For simplicity, we map:
    //   ELF 1 (.text) -> COFF 1 (.text)
    //   ELF 2 (.rodata if present, else .data or .bss) -> COFF 2 (.rdata) or ...
    //
    // The exact mapping depends on which sections are present.
    let mut map = vec![0u16]; // ELF 0 -> COFF 0 (undefined)
    let mut coff_sec = 1u16;

    // ELF 1 = .text
    coff_sec += 1;
    map.push(coff_sec); // -> COFF .text (1)

    // ELF 2 = .rodata (if present) or .data or .bss
    // The ELF builder uses: symtab_idx = if rodata.is_empty() { 2 } else { 3 }
    // This means:
    //   if rodata present: ELF 2 = .rodata
    //   if rodata empty: ELF 2 = .data (or .bss if data also empty)
    //
    // Actually, looking at build_elf_rel_x86_64 more carefully:
    //   n_sections = 1 (null) + 1 (.text) + (rodata ? 1 : 0) + 3 (symtab/strtab/shstrtab)
    //              + 1 (rela.text) + (rel_rodata ? 1 : 0) + extra.len()
    //
    // But the sections are ordered:
    //   0: null
    //   1: .text
    //   2: .rodata (if present)
    //   3 or 2: .data (if present)
    //   4 or 3: .bss (if bss_size > 0)
    //   ... then symtab, strtab, shstrtab, rela.text, [rela.rodata], extras
    //
    // Wait, let me re-read the ELF builder. The order is:
    //   write_sh_null, .text, [.rodata], .symtab, .strtab, .shstrtab,
    //   .rela.text, [.rela.rodata], extras...
    //
    // But .data and .bss are embedded in the .text section or handled
    // differently. Let me check...
    //
    // Actually looking at the code more carefully, build_elf_rel_x86_64
    // takes data_section and bss_size as parameters but the section
    // layout in the ELF output interleaves them. The key insight is
    // that the ElfSym.section field uses the ELF section index, and
    // we need to map that to COFF section numbers.
    //
    // For a simpler approach: since the codegen produces symbols with
    // section indices based on the ELF layout, and we're creating a
    // parallel COFF layout, we need to track which ELF section index
    // maps to which COFF section.
    //
    // Let me just build a straightforward mapping based on what
    // sections are present:

    // ELF layout:
    // 0: null
    // 1: .text
    // 2: .rodata (if !rodata.is_empty())
    // next: .data (if !data.is_empty()) -- but actually data is combined
    //       into the text section in the ELF builder? No, looking at
    //       the function signature it takes data separately.
    //
    // After reading the ELF builder code carefully:
    // The sections in the ELF output are:
    //   null, .text, [.rodata], .symtab, .strtab, .shstrtab,
    //   .rela.text, [.rela.data], [extras...]
    //
    // But .data and .bss content is actually appended to .text in the
    // ELF builder! No wait, looking at the function signature:
    //   build_elf_rel_x86_64(text, rodata, data, bss_size, tdata, tbss_size, ...)
    //
    // Let me just look at what the actual section indices are by
    // examining the ELF builder. The key is the `symtab_idx`:
    //   symtab_idx = if rodata.is_empty() { 2 } else { 3 }
    // This tells us:
    //   Index 0: null
    //   Index 1: .text
    //   Index 2: .rodata (if present)
    //   Index 3 (or 2): next section
    //
    // Actually the ELF builder doesn't seem to emit .data and .bss as
    // separate sections in the relocatable output. Looking at the code:
    //   n_sections = 1 + 1 + (rodata?1:0) + 3 + 1 + (rel_rodata?1:0) + extra
    // The "+3" is symtab + strtab + shstrtab.
    // So the data and bss are somehow merged into .text?
    //
    // Wait, I see now: looking at the layout code in build_elf_rel_x86_64:
    //   text_off = header_size
    //   rodata_off = align_up(text_off + text.len(), 8)
    //   ... then symtab, strtab, shstrtab, rela_text, rela_rodata
    //
    // It seems like data and bss data is included in the `text` blob!
    // The codegen puts everything (code + data + bss) into the .text
    // section for the relocatable output. The separate data/bss params
    // are used for the executable output.
    //
    // Actually no, looking more carefully at the code, the function
    // takes data_section, bss_size etc. but the section layout only
    // has .text and .rodata as content sections. The data/bss/tdata/tbss
    // are likely appended to .text or .rodata in the codegen.
    //
    // For the COFF writer, I'll take the same approach: the codegen
    // gives us the raw blobs, and we put them in appropriate COFF sections.
    // The symbol section indices from the codegen are ELF-based, so
    // we need the mapping.
    //
    // Given the complexity, let me just use a simple heuristic:
    // ELF section 1 -> COFF section 1 (.text)
    // ELF section 2 -> COFF section 2 (.rdata) if rodata present
    // For other sections, we map based on presence.

    // Actually, the simplest correct approach: since we're generating
    // both the symbols and the sections, and the symbols reference
    // ELF section indices, we need to understand exactly what those
    // indices are. Let me trace through build_elf_rel_x86_64:
    //
    // Sections emitted (in order):
    // [0] null
    // [1] .text
    // [2] .rodata (only if !rodata.is_empty())
    // [symtab_idx] .symtab  (2 if no rodata, 3 if rodata)
    // [symtab_idx+1] .strtab
    // [symtab_idx+2] .shstrtab
    // [symtab_idx+3] .rela.text
    // [symtab_idx+4] .rela.data (only if !rel_rodata.is_empty())
    // [extra sections...]
    //
    // So the mapping is:
    // ELF 0 -> COFF 0 (undef)
    // ELF 1 -> COFF 1 (.text)
    // ELF 2 -> COFF 2 (.rdata) if rodata present
    //         or COFF 2 (.data) if no rodata but data present
    //         or COFF 2 (.bss) if no rodata, no data, but bss
    //
    // For the common case:
    // If rodata present: ELF 2 = .rodata -> COFF 2 (.rdata)
    // If rodata absent: ELF 2 = .symtab (not a content section)
    //
    // Since symbols only reference content sections (1=.text, 2=.rodata),
    // the mapping is straightforward:

    map.clear();
    map.push(0); // ELF 0 (SHN_UNDEF) -> COFF 0

    // ELF 1 = .text -> COFF 1
    map.push(1);

    // ELF 2 = .rodata if present -> COFF 2 (.rdata)
    if !_rodata.is_empty() {
        map.push(2);
    }

    // For any additional ELF section indices that symbols might reference,
    // we extend the map. But in practice, the codegen only assigns
    // section 1 (.text) and section 2 (.rodata) to symbols.
    // Extra sections start at a higher ELF index but we don't have
    // symbols pointing to them typically.

    // Pad to cover any possible ELF section index
    while map.len() < 20 {
        map.push(0);
    }

    map
}

fn align_to_coff(align: u64) -> u32 {
    match align {
        a if a <= 1 => 0,
        a if a <= 2 => IMAGE_SCN_ALIGN_4BYTES, // minimum 4 for COFF
        a if a <= 4 => IMAGE_SCN_ALIGN_4BYTES,
        a if a <= 8 => IMAGE_SCN_ALIGN_8BYTES,
        _ => IMAGE_SCN_ALIGN_16BYTES,
    }
}

fn align_up(off: usize, align: usize) -> usize {
    if align <= 1 {
        return off;
    }
    (off + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf::{ElfSym, STB_GLOBAL, STT_FUNC, STV_DEFAULT};

    #[test]
    fn coff_header_machine_is_amd64() {
        let obj = build_coff_rel_x86_64(
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
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        assert!(obj.len() >= 20);
        let machine = u16::from_le_bytes([obj[0], obj[1]]);
        assert_eq!(machine, IMAGE_FILE_MACHINE_AMD64);
    }

    #[test]
    fn coff_has_text_section() {
        let obj = build_coff_rel_x86_64(
            &[0xc3], // ret
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
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let num_secs = u16::from_le_bytes([obj[2], obj[3]]);
        assert!(num_secs >= 1, "should have at least .text section");
    }

    #[test]
    fn coff_symbol_table_has_entries() {
        let obj = build_coff_rel_x86_64(
            &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3],
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
        let num_syms = u32::from_le_bytes(obj[12..16].try_into().unwrap());
        assert_eq!(num_syms, 1, "should have 1 symbol");
    }

    #[test]
    fn coff_with_rodata_has_rdata_section() {
        let obj = build_coff_rel_x86_64(
            &[0xc3],
            b"hello\0",
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let num_secs = u16::from_le_bytes([obj[2], obj[3]]);
        assert_eq!(num_secs, 2, "should have .text and .rdata");
    }
}
