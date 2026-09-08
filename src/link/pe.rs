// src/link/pe.rs - PE (Portable Executable) output generator
//
// Generates PE32+ (64-bit) executable files for x86_64-pc-windows-msvc
// targets from the unified Layout produced by the linker pipeline.
//
// Reference: Microsoft PE/COFF specification
//   https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
//
// Layout:
//   [DOS Header + Stub]
//   [PE Signature "PE\0\0"]
//   [COFF File Header (20 bytes)]
//   [Optional Header PE32+ (240 bytes)]
//   [Section Table (40 bytes × N)]
//   [Section Data: .text, .rdata, .data, .bss]

#![allow(dead_code)]

use super::layout::Layout;
use crate::codegen::elf::{SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE, SHT_NOBITS};

// ===== PE constants =====

/// DOS header magic
const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D; // "MZ"
/// PE signature
const PE_SIGNATURE: u32 = 0x0000_4550; // "PE\0\0"

/// PE32+ optional header magic
const IMAGE_NT_OPTIONAL_HDR64_MAGIC: u16 = 0x020B;

/// Machine types
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;

/// File characteristics
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const IMAGE_FILE_LARGE_ADDRESS_AWARE: u16 = 0x0020;

/// Optional header subsystem
const IMAGE_SUBSYSTEM_CONSOLE: u16 = 3;

/// Section characteristics
const IMAGE_SCN_CNT_CODE: u32 = 0x0000_0020;
const IMAGE_SCN_CNT_INITIALIZED_DATA: u32 = 0x0000_0040;
const IMAGE_SCN_CNT_UNINITIALIZED_DATA: u32 = 0x0000_0080;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;
const IMAGE_SCN_ALIGN_16BYTES: u32 = 0x0050_0000;
const IMAGE_SCN_ALIGN_4BYTES: u32 = 0x0030_0000;

/// Data directory indices
const IMAGE_DIRECTORY_ENTRY_IMPORT: usize = 1;

/// Default PE image parameters for x86_64
const PE_IMAGE_BASE: u64 = 0x0001_4000_0000;
const PE_SECTION_ALIGNMENT: u32 = 0x1000; // 4 KB
const PE_FILE_ALIGNMENT: u32 = 0x200; // 512 bytes

/// DOS header size (64 bytes)
const DOS_HEADER_SIZE: usize = 64;
/// Minimal DOS stub ("This program cannot be run in DOS mode\r\n$")
const DOS_STUB: &[u8] = b"This program cannot be run in DOS mode.\r\n$";
/// PE signature + COFF header + optional header sizes
const PE_SIGNATURE_SIZE: usize = 4;
const COFF_HEADER_SIZE: usize = 20;
const OPTIONAL_HEADER_PE32_PLUS_SIZE: usize = 112; // standard fields
const NUM_DATA_DIRECTORIES: usize = 16;
const DATA_DIRECTORY_ENTRY_SIZE: usize = 8;

/// Build a PE32+ executable from a linker Layout.
///
/// The Layout is produced by the same pipeline used for ELF output;
/// this function serializes it into PE format instead.
pub fn write_pe_exec_x86_64(layout: &Layout) -> Vec<u8> {
    let image_base = if layout.base_addr != 0 && layout.base_addr != 0x400000 {
        layout.base_addr
    } else {
        PE_IMAGE_BASE
    };

    // Collect output sections that have ALLOC flag (loadable sections)
    let loadable: Vec<&super::layout::OutputSection> = layout
        .sections
        .iter()
        .filter(|s| s.flags & SHF_ALLOC != 0 && s.size > 0)
        .collect();

    // Map layout sections to PE sections
    let pe_sections = build_pe_sections(&loadable, image_base, layout.base_addr);

    // Calculate sizes
    let dos_stub_size = align_up(DOS_STUB.len(), 8);
    let pe_header_offset = align_up(DOS_HEADER_SIZE + dos_stub_size, 8);
    let optional_header_size =
        OPTIONAL_HEADER_PE32_PLUS_SIZE + NUM_DATA_DIRECTORIES * DATA_DIRECTORY_ENTRY_SIZE;
    let section_table_offset =
        pe_header_offset + PE_SIGNATURE_SIZE + COFF_HEADER_SIZE + optional_header_size;
    let num_sections = pe_sections.len();
    let section_table_size = num_sections * 40;

    // Calculate section data offsets (file-aligned)
    let mut data_start = align_up(section_table_offset + section_table_size, PE_FILE_ALIGNMENT as usize);

    let mut pe_secs_with_offsets: Vec<PeSection> = Vec::new();
    for ps in &pe_sections {
        let file_offset = if ps.raw_size > 0 {
            data_start
        } else {
            0
        };
        pe_secs_with_offsets.push(PeSection {
            name: ps.name.clone(),
            virtual_size: ps.virtual_size,
            virtual_address: ps.virtual_address,
            raw_size: ps.raw_size,
            raw_offset: file_offset,
            characteristics: ps.characteristics,
            data: ps.data.clone(),
        });
        if ps.raw_size > 0 {
            data_start = align_up(file_offset + ps.raw_size, PE_FILE_ALIGNMENT as usize);
        }
    }

    // Find entry point RVA
    // The layout.entry is an ELF-style virtual address. For PE, we need to
    // calculate the RVA relative to the first loadable section.
    // Since we're building a simple executable, the entry is typically
    // at the start of .text section.
    let entry_rva: u64 = if layout.entry != 0 {
        // Find the first code section's RVA
        pe_secs_with_offsets
            .iter()
            .find(|s| s.characteristics & IMAGE_SCN_CNT_CODE != 0)
            .map(|s| s.virtual_address as u64)
            .unwrap_or(0)
    } else {
        0
    };

    // Calculate image size (aligned to section alignment)
    let mut image_size = 0u32;
    for ps in &pe_secs_with_offsets {
        let end = ps.virtual_address + align_up_u32(ps.virtual_size, PE_SECTION_ALIGNMENT);
        if end > image_size {
            image_size = end;
        }
    }
    // Ensure at least header + section table fits
    let headers_size = align_up(
        section_table_offset + section_table_size,
        PE_SECTION_ALIGNMENT as usize,
    );
    if image_size == 0 {
        image_size = headers_size as u32;
    }

    // Calculate sizes of code/data
    let mut size_of_code = 0u32;
    let mut size_of_initialized_data = 0u32;
    let mut size_of_uninitialized_data = 0u32;
    for ps in &pe_secs_with_offsets {
        if ps.characteristics & IMAGE_SCN_CNT_CODE != 0 {
            size_of_code += ps.raw_size as u32;
        } else if ps.characteristics & IMAGE_SCN_CNT_UNINITIALIZED_DATA != 0 {
            size_of_uninitialized_data += ps.virtual_size;
        } else {
            size_of_initialized_data += ps.raw_size as u32;
        }
    }

    // Find base of code and data
    let base_of_code = pe_secs_with_offsets
        .iter()
        .find(|s| s.characteristics & IMAGE_SCN_CNT_CODE != 0)
        .map(|s| s.virtual_address)
        .unwrap_or(0);
    let base_of_data = pe_secs_with_offsets
        .iter()
        .find(|s| {
            s.characteristics & IMAGE_SCN_CNT_INITIALIZED_DATA != 0
                && s.characteristics & IMAGE_SCN_CNT_CODE == 0
        })
        .map(|s| s.virtual_address)
        .unwrap_or(0);

    // ===== Emit =====
    let total_size = data_start
        + pe_secs_with_offsets
            .iter()
            .filter(|s| s.raw_size > 0)
            .map(|s| s.raw_size)
            .sum::<usize>();
    let mut out = Vec::with_capacity(total_size.max(data_start));

    // 1. DOS Header (64 bytes)
    write_dos_header(&mut out, pe_header_offset as u32);

    // 2. DOS Stub
    out.extend_from_slice(DOS_STUB);
    // Pad to alignment
    while out.len() < pe_header_offset {
        out.push(0);
    }

    // 3. PE Signature
    out.extend_from_slice(&PE_SIGNATURE.to_le_bytes());

    // 4. COFF File Header (20 bytes)
    out.extend_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes()); // Machine
    out.extend_from_slice(&(num_sections as u16).to_le_bytes()); // NumberOfSections
    out.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToSymbolTable
    out.extend_from_slice(&0u32.to_le_bytes()); // NumberOfSymbols
    out.extend_from_slice(&(optional_header_size as u16).to_le_bytes()); // SizeOfOptionalHeader
    let characteristics = IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_LARGE_ADDRESS_AWARE;
    out.extend_from_slice(&characteristics.to_le_bytes()); // Characteristics

    // 5. Optional Header PE32+ (112 bytes standard + data directories)
    write_optional_header(
        &mut out,
        entry_rva,
        image_base,
        PE_SECTION_ALIGNMENT as u64,
        PE_FILE_ALIGNMENT as u64,
        image_size,
        headers_size as u32,
        size_of_code,
        size_of_initialized_data,
        size_of_uninitialized_data,
        base_of_code,
        base_of_data,
    );

    // 6. Section Table
    for ps in &pe_secs_with_offsets {
        write_section_header(&mut out, ps);
    }

    // Pad to first section data
    while out.len() < data_start {
        out.push(0);
    }

    // 7. Section Data
    for ps in &pe_secs_with_offsets {
        if ps.raw_size > 0 && !ps.data.is_empty() {
            // Pad to section offset
            while out.len() < ps.raw_offset {
                out.push(0);
            }
            out.extend_from_slice(&ps.data);
            // Pad to file alignment
            let end = ps.raw_offset + ps.raw_size;
            let aligned_end = align_up(end, PE_FILE_ALIGNMENT as usize);
            while out.len() < aligned_end {
                out.push(0);
            }
        }
    }

    out
}

struct PeSectionTemplate {
    name: String,
    virtual_size: u32,
    virtual_address: u32,
    raw_size: usize,
    characteristics: u32,
    data: Vec<u8>,
}

struct PeSection {
    name: String,
    virtual_size: u32,
    virtual_address: u32,
    raw_size: usize,
    raw_offset: usize,
    characteristics: u32,
    data: Vec<u8>,
}

/// Map Layout output sections to PE sections.
///
/// We group sections by their ELF-like flags into PE sections:
/// - .text: code (EXECINSTR)
/// - .rdata: read-only data (ALLOC, no WRITE, no EXECINSTR)
/// - .data: initialized data (ALLOC, WRITE)
/// - .bss: uninitialized data (NOBITS)
fn build_pe_sections(
    loadable: &[&super::layout::OutputSection],
    _image_base: u64,
    _elf_base_addr: u64,
) -> Vec<PeSectionTemplate> {
    // Group sections by PE category
    let mut text_data = Vec::new();
    let mut rdata_data = Vec::new();
    let mut data_data = Vec::new();
    let mut bss_size: u64 = 0;

    for sec in loadable {
        if sec.ty == SHT_NOBITS {
            bss_size += sec.size;
            continue;
        }
        if sec.flags & SHF_EXECINSTR != 0 {
            text_data.extend_from_slice(&sec.data);
        } else if sec.flags & SHF_WRITE != 0 {
            data_data.extend_from_slice(&sec.data);
        } else {
            rdata_data.extend_from_slice(&sec.data);
        }
    }

    let mut result = Vec::new();
    let mut rva = PE_SECTION_ALIGNMENT; // first section at 1 page

    // .text section
    if !text_data.is_empty() {
        let text_len = text_data.len();
        result.push(PeSectionTemplate {
            name: ".text".into(),
            virtual_size: text_len as u32,
            virtual_address: rva,
            raw_size: align_up(text_len, PE_FILE_ALIGNMENT as usize),
            characteristics: IMAGE_SCN_CNT_CODE
                | IMAGE_SCN_MEM_EXECUTE
                | IMAGE_SCN_MEM_READ
                | IMAGE_SCN_ALIGN_16BYTES,
            data: text_data,
        });
        rva += align_up_u32(text_len as u32, PE_SECTION_ALIGNMENT);
    }

    // .rdata section
    if !rdata_data.is_empty() {
        let rdata_len = rdata_data.len();
        result.push(PeSectionTemplate {
            name: ".rdata".into(),
            virtual_size: rdata_len as u32,
            virtual_address: rva,
            raw_size: align_up(rdata_len, PE_FILE_ALIGNMENT as usize),
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA
                | IMAGE_SCN_MEM_READ
                | IMAGE_SCN_ALIGN_16BYTES,
            data: rdata_data,
        });
        rva += align_up_u32(rdata_len as u32, PE_SECTION_ALIGNMENT);
    }

    // .data section
    if !data_data.is_empty() {
        let data_len = data_data.len();
        result.push(PeSectionTemplate {
            name: ".data".into(),
            virtual_size: data_len as u32,
            virtual_address: rva,
            raw_size: align_up(data_len, PE_FILE_ALIGNMENT as usize),
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA
                | IMAGE_SCN_MEM_WRITE
                | IMAGE_SCN_MEM_READ
                | IMAGE_SCN_ALIGN_16BYTES,
            data: data_data,
        });
        rva += align_up_u32(data_len as u32, PE_SECTION_ALIGNMENT);
    }

    // .bss section
    if bss_size > 0 {
        result.push(PeSectionTemplate {
            name: ".bss".into(),
            virtual_size: bss_size as u32,
            virtual_address: rva,
            raw_size: 0, // BSS has no raw data
            characteristics: IMAGE_SCN_CNT_UNINITIALIZED_DATA
                | IMAGE_SCN_MEM_WRITE
                | IMAGE_SCN_MEM_READ
                | IMAGE_SCN_ALIGN_16BYTES,
            data: Vec::new(),
        });
    }

    result
}

/// Write the DOS header (64 bytes).
fn write_dos_header(out: &mut Vec<u8>, pe_offset: u32) {
    out.resize(DOS_HEADER_SIZE, 0);
    // e_magic = "MZ"
    out[0] = (IMAGE_DOS_SIGNATURE & 0xFF) as u8;
    out[1] = (IMAGE_DOS_SIGNATURE >> 8) as u8;
    // e_cblp = 0x90
    out[2] = 0x90;
    // e_cp = 3
    out[4] = 0x03;
    // e_cparhdr = 4
    out[8] = 0x04;
    // e_minalloc = 0
    // e_maxalloc = 0xFFFF
    out[12] = 0xFF;
    out[13] = 0xFF;
    // e_ss = 0xB8
    out[16] = 0xB8;
    // e_sp = 0
    // e_csum = 0
    // e_ip = 0
    // e_cs = 0
    // e_lfarlc = 0x40
    out[24] = 0x40;
    // e_lfanew = PE header offset
    out[60] = (pe_offset & 0xFF) as u8;
    out[61] = ((pe_offset >> 8) & 0xFF) as u8;
    out[62] = ((pe_offset >> 16) & 0xFF) as u8;
    out[63] = ((pe_offset >> 24) & 0xFF) as u8;
}

/// Write PE32+ optional header (112 bytes standard + data directories).
#[allow(clippy::too_many_arguments)]
fn write_optional_header(
    out: &mut Vec<u8>,
    entry_rva: u64,
    image_base: u64,
    section_alignment: u64,
    file_alignment: u64,
    size_of_image: u32,
    size_of_headers: u32,
    size_of_code: u32,
    size_of_initialized_data: u32,
    size_of_uninitialized_data: u32,
    base_of_code: u32,
    _base_of_data: u32,
) {
    let start = out.len();

    // Standard fields (PE32+) - 24 bytes
    out.extend_from_slice(&IMAGE_NT_OPTIONAL_HDR64_MAGIC.to_le_bytes()); // Magic (2)
    out.push(14); // MajorLinkerVersion (1)
    out.push(0); // MinorLinkerVersion (1)
    out.extend_from_slice(&size_of_code.to_le_bytes()); // SizeOfCode (4)
    out.extend_from_slice(&size_of_initialized_data.to_le_bytes()); // SizeOfInitializedData (4)
    out.extend_from_slice(&size_of_uninitialized_data.to_le_bytes()); // SizeOfUninitializedData (4)
    out.extend_from_slice(&(entry_rva as u32).to_le_bytes()); // AddressOfEntryPoint (4)
    out.extend_from_slice(&base_of_code.to_le_bytes()); // BaseOfCode (4)

    // Windows-specific fields (PE32+) - 88 bytes
    out.extend_from_slice(&image_base.to_le_bytes()); // ImageBase (8)
    out.extend_from_slice(&(section_alignment as u32).to_le_bytes()); // SectionAlignment (4)
    out.extend_from_slice(&(file_alignment as u32).to_le_bytes()); // FileAlignment (4)
    out.extend_from_slice(&6u16.to_le_bytes()); // MajorOperatingSystemVersion (2)
    out.extend_from_slice(&0u16.to_le_bytes()); // MinorOperatingSystemVersion (2)
    out.extend_from_slice(&0u16.to_le_bytes()); // MajorImageVersion (2)
    out.extend_from_slice(&0u16.to_le_bytes()); // MinorImageVersion (2)
    out.extend_from_slice(&6u16.to_le_bytes()); // MajorSubsystemVersion (2)
    out.extend_from_slice(&0u16.to_le_bytes()); // MinorSubsystemVersion (2)
    out.extend_from_slice(&0u32.to_le_bytes()); // Win32VersionValue (4)
    out.extend_from_slice(&size_of_image.to_le_bytes()); // SizeOfImage (4)
    out.extend_from_slice(&size_of_headers.to_le_bytes()); // SizeOfHeaders (4)
    out.extend_from_slice(&0u32.to_le_bytes()); // CheckSum (4)
    out.extend_from_slice(&IMAGE_SUBSYSTEM_CONSOLE.to_le_bytes()); // Subsystem (2)
    out.extend_from_slice(&0u16.to_le_bytes()); // DllCharacteristics (2)
    out.extend_from_slice(&0x100000u64.to_le_bytes()); // SizeOfStackReserve (8)
    out.extend_from_slice(&0x1000u64.to_le_bytes()); // SizeOfStackCommit (8)
    out.extend_from_slice(&0x100000u64.to_le_bytes()); // SizeOfHeapReserve (8)
    out.extend_from_slice(&0x1000u64.to_le_bytes()); // SizeOfHeapCommit (8)
    out.extend_from_slice(&0u32.to_le_bytes()); // LoaderFlags (4)
    out.extend_from_slice(&(NUM_DATA_DIRECTORIES as u32).to_le_bytes()); // NumberOfRvaAndSizes (4)

    // Data directories (16 entries × 8 bytes = 128 bytes)
    for _ in 0..NUM_DATA_DIRECTORIES {
        out.extend_from_slice(&0u32.to_le_bytes()); // VirtualAddress (4)
        out.extend_from_slice(&0u32.to_le_bytes()); // Size (4)
    }

    // Verify we wrote the correct size
    let written = out.len() - start;
    let expected = OPTIONAL_HEADER_PE32_PLUS_SIZE + NUM_DATA_DIRECTORIES * DATA_DIRECTORY_ENTRY_SIZE;
    debug_assert_eq!(
        written, expected,
        "optional header size mismatch: wrote {} expected {}",
        written, expected
    );
}

/// Write a PE section header (40 bytes).
fn write_section_header(out: &mut Vec<u8>, sec: &PeSection) {
    // Name (8 bytes)
    let name_bytes = sec.name.as_bytes();
    let mut name_field = [0u8; 8];
    let copy_len = name_bytes.len().min(8);
    name_field[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
    out.extend_from_slice(&name_field);

    out.extend_from_slice(&sec.virtual_size.to_le_bytes()); // VirtualSize
    out.extend_from_slice(&sec.virtual_address.to_le_bytes()); // VirtualAddress
    out.extend_from_slice(&(sec.raw_size as u32).to_le_bytes()); // SizeOfRawData
    out.extend_from_slice(&(sec.raw_offset as u32).to_le_bytes()); // PointerToRawData
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToRelocations
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToLinenumbers
    out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfRelocations
    out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfLinenumbers
    out.extend_from_slice(&sec.characteristics.to_le_bytes()); // Characteristics
}

fn align_up(val: usize, align: usize) -> usize {
    if align <= 1 {
        return val;
    }
    (val + align - 1) & !(align - 1)
}

fn align_up_u32(val: u32, align: u32) -> u32 {
    if align <= 1 {
        return val;
    }
    (val + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::layout::*;
    use crate::debug::ArchKind;

    fn minimal_layout() -> Layout {
        Layout {
            is_pie: false,
            is_shared: false,
            arch: ArchKind::X86_64,
            base_addr: 0,
            file_base: 0,
            sections: vec![
                OutputSection {
                    kind: OutputSectionKind::Text,
                    name: ".text".into(),
                    sh_name_off: 0,
                    ty: crate::codegen::elf::SHT_PROGBITS,
                    flags: SHF_ALLOC | SHF_EXECINSTR,
                    align: 16,
                    data: vec![0xC3], // ret
                    size: 1,
                    entsize: 0,
                    output_index: 0,
                    load_segment: 0,
                    vaddr: 0x401000,
                    file_off: 0x1000,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
                OutputSection {
                    kind: OutputSectionKind::Rodata,
                    name: ".rodata".into(),
                    sh_name_off: 0,
                    ty: crate::codegen::elf::SHT_PROGBITS,
                    flags: SHF_ALLOC,
                    align: 4,
                    data: b"hello\0".to_vec(),
                    size: 6,
                    entsize: 0,
                    output_index: 1,
                    load_segment: 0,
                    vaddr: 0x402000,
                    file_off: 0x2000,
                    input_offsets: vec![],
                    input_sections: vec![],
                },
            ],
            segments: vec![],
            symbols: vec![],
            entry: 0x401000,
            has_symtab: false,
            has_dynamic: false,
            n_locals: 0,
            output_relas: vec![],
            needed: vec![],
            dynamic_linker: None,
            comdat_groups: vec![],
        }
    }

    #[test]
    fn pe_has_mz_signature() {
        let layout = minimal_layout();
        let pe = write_pe_exec_x86_64(&layout);
        assert!(pe.len() >= 2);
        assert_eq!(pe[0], b'M');
        assert_eq!(pe[1], b'Z');
    }

    #[test]
    fn pe_has_pe_signature() {
        let layout = minimal_layout();
        let pe = write_pe_exec_x86_64(&layout);
        let e_lfanew = u32::from_le_bytes(pe[60..64].try_into().unwrap()) as usize;
        assert!(pe.len() > e_lfanew + 4);
        assert_eq!(&pe[e_lfanew..e_lfanew + 4], b"PE\0\0");
    }

    #[test]
    fn pe_machine_is_amd64() {
        let layout = minimal_layout();
        let pe = write_pe_exec_x86_64(&layout);
        let e_lfanew = u32::from_le_bytes(pe[60..64].try_into().unwrap()) as usize;
        let machine_off = e_lfanew + 4; // after PE signature
        let machine = u16::from_le_bytes(pe[machine_off..machine_off + 2].try_into().unwrap());
        assert_eq!(machine, IMAGE_FILE_MACHINE_AMD64);
    }

    #[test]
    fn pe_optional_header_magic_is_pe32_plus() {
        let layout = minimal_layout();
        let pe = write_pe_exec_x86_64(&layout);
        let e_lfanew = u32::from_le_bytes(pe[60..64].try_into().unwrap()) as usize;
        let opt_off = e_lfanew + 4 + COFF_HEADER_SIZE;
        let magic = u16::from_le_bytes(pe[opt_off..opt_off + 2].try_into().unwrap());
        assert_eq!(magic, IMAGE_NT_OPTIONAL_HDR64_MAGIC);
    }

    #[test]
    fn pe_has_text_section() {
        let layout = minimal_layout();
        let pe = write_pe_exec_x86_64(&layout);
        let e_lfanew = u32::from_le_bytes(pe[60..64].try_into().unwrap()) as usize;
        let num_secs_off = e_lfanew + 4 + 2; // Machine(2) then NumberOfSections
        let num_secs =
            u16::from_le_bytes(pe[num_secs_off..num_secs_off + 2].try_into().unwrap());
        assert!(num_secs >= 1, "should have at least .text section");
        // Check first section name is .text
        let sec_table_off = e_lfanew + 4 + COFF_HEADER_SIZE
            + OPTIONAL_HEADER_PE32_PLUS_SIZE
            + NUM_DATA_DIRECTORIES * DATA_DIRECTORY_ENTRY_SIZE;
        let name = &pe[sec_table_off..sec_table_off + 5];
        assert_eq!(name, b".text");
    }
}
