//! Memory usage reporting for bare-metal embedded targets.
//!
//! This module provides utilities for analyzing and reporting
//! RAM and Flash usage of embedded firmware images, similar
//! to the `arm-none-eabi-size` tool.

use std::io::{self, Write};

/// Memory region types for embedded targets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegion {
    /// Flash/ROM for code and constants
    Flash,
    /// RAM for data and stack
    Ram,
}

/// Memory usage statistics
#[derive(Debug, Clone, Default)]
pub struct MemoryUsage {
    /// Code size (.text section) - Flash
    pub text: u64,
    /// Initialized data (.data section) - Flash + RAM
    pub data: u64,
    /// Uninitialized data (.bss section) - RAM
    pub bss: u64,
    /// Total Flash usage (text + data)
    pub flash_total: u64,
    /// Total RAM usage (data + bss)
    pub ram_total: u64,
}

impl MemoryUsage {
    /// Calculate totals from section sizes
    pub fn calculate_totals(&mut self) {
        self.flash_total = self.text + self.data;
        self.ram_total = self.data + self.bss;
    }
}

/// Memory region definition from linker script
#[derive(Debug, Clone)]
pub struct MemoryRegionDef {
    /// Region name (e.g., "FLASH", "RAM")
    pub name: String,
    /// Region type
    pub region_type: MemoryRegion,
    /// Start address
    pub origin: u64,
    /// Region length in bytes
    pub length: u64,
}

/// Memory map for embedded target
#[derive(Debug, Clone, Default)]
pub struct MemoryMap {
    /// Defined memory regions
    pub regions: Vec<MemoryRegionDef>,
}

impl MemoryMap {
    /// Create a new empty memory map
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    /// Add a memory region
    pub fn add_region(&mut self, name: &str, region_type: MemoryRegion, origin: u64, length: u64) {
        self.regions.push(MemoryRegionDef {
            name: name.to_string(),
            region_type,
            origin,
            length,
        });
    }

    /// Find a region by name
    pub fn find_region(&self, name: &str) -> Option<&MemoryRegionDef> {
        self.regions.iter().find(|r| r.name == name)
    }
}

/// Print memory usage report in `size` tool format.
///
/// Output format:
/// ```text
///    text    data     bss     dec     hex filename
///   12345     678     901   13924    3664 firmware.elf
/// ```
pub fn print_size_report<W: Write>(
    mut output: W,
    usage: &MemoryUsage,
    filename: &str,
) -> io::Result<()> {
    let total = usage.flash_total + usage.bss;

    writeln!(output, "   text    data     bss     dec     hex filename")?;
    writeln!(
        output,
        "{:>8}{:>8}{:>8}{:>8}{:>8} {}",
        usage.text,
        usage.data,
        usage.bss,
        total,
        format!("{:x}", total),
        filename
    )?;

    Ok(())
}

/// Print detailed memory usage report with region utilization.
///
/// Output format:
/// ```text
/// Memory region         Used Size  Available  % Used
/// -------------------   ---------  ---------  -----
/// FLASH (rx)               13023      65536   19.9%
/// RAM (rwx)                  901      32768    2.8%
/// ```
pub fn print_detailed_report<W: Write>(
    mut output: W,
    usage: &MemoryUsage,
    memory_map: &MemoryMap,
) -> io::Result<()> {
    writeln!(output, "Memory region         Used Size  Available  % Used")?;
    writeln!(output, "-------------------   ---------  ---------  -----")?;

    for region in &memory_map.regions {
        let (used, available) = match region.region_type {
            MemoryRegion::Flash => (usage.flash_total, region.length),
            MemoryRegion::Ram => (usage.ram_total, region.length),
        };

        let percent = if available > 0 {
            (used as f64 / available as f64) * 100.0
        } else {
            0.0
        };

        let type_str = match region.region_type {
            MemoryRegion::Flash => "(rx)",
            MemoryRegion::Ram => "(rwx)",
        };

        writeln!(
            output,
            "{:<16} {:4}{:>10}{:>11}{:>10.1}%",
            format!("{}{}", region.name, type_str),
            "",
            used,
            available,
            percent
        )?;
    }

    Ok(())
}

/// Parse section sizes from ELF file.
///
/// Returns MemoryUsage with text, data, and bss sizes.
pub fn parse_elf_section_sizes(elf_data: &[u8]) -> io::Result<MemoryUsage> {
    // Minimal ELF parsing to extract section sizes
    if elf_data.len() < 64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ELF file too small"));
    }

    // Check ELF magic
    if &elf_data[0..4] != b"\x7fELF" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not an ELF file"));
    }

    let is_64bit = elf_data[4] == 2;
    let is_little_endian = elf_data[5] == 1;

    if !is_little_endian {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "only little-endian ELF supported"));
    }

    let (shoff, shentsize, shnum) = if is_64bit {
        if elf_data.len() < 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ELF64 header too small"));
        }
        let shoff = u64::from_le_bytes(elf_data[40..48].try_into().unwrap());
        let shentsize = u16::from_le_bytes(elf_data[58..60].try_into().unwrap());
        let shnum = u16::from_le_bytes(elf_data[60..62].try_into().unwrap());
        (shoff, shentsize, shnum as u64)
    } else {
        if elf_data.len() < 52 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ELF32 header too small"));
        }
        let shoff = u32::from_le_bytes(elf_data[32..36].try_into().unwrap()) as u64;
        let shentsize = u16::from_le_bytes(elf_data[46..48].try_into().unwrap());
        let shnum = u16::from_le_bytes(elf_data[48..50].try_into().unwrap());
        (shoff, shentsize, shnum as u64)
    };

    // Get string table index
    let shstrndx = if is_64bit {
        u16::from_le_bytes(elf_data[62..64].try_into().unwrap()) as u64
    } else {
        u16::from_le_bytes(elf_data[50..52].try_into().unwrap()) as u64
    };

    // Parse section headers and accumulate sizes
    let mut usage = MemoryUsage::default();

    for i in 0..shnum {
        let offset = shoff + i * shentsize as u64;

        let (sh_name, sh_type, sh_size, sh_flags) = if is_64bit {
            if elf_data.len() < (offset + 64) as usize {
                continue;
            }
            let sh_name = u32::from_le_bytes(
                elf_data[offset as usize..(offset + 4) as usize]
                    .try_into()
                    .unwrap(),
            );
            let sh_type = u32::from_le_bytes(
                elf_data[(offset + 4) as usize..(offset + 8) as usize]
                    .try_into()
                    .unwrap(),
            );
            let sh_flags = u64::from_le_bytes(
                elf_data[(offset + 8) as usize..(offset + 16) as usize]
                    .try_into()
                    .unwrap(),
            );
            let sh_size = u64::from_le_bytes(
                elf_data[(offset + 32) as usize..(offset + 40) as usize]
                    .try_into()
                    .unwrap(),
            );
            (sh_name, sh_type, sh_size, sh_flags)
        } else {
            if elf_data.len() < (offset + 40) as usize {
                continue;
            }
            let sh_name = u32::from_le_bytes(
                elf_data[offset as usize..(offset + 4) as usize]
                    .try_into()
                    .unwrap(),
            );
            let sh_type = u32::from_le_bytes(
                elf_data[(offset + 4) as usize..(offset + 8) as usize]
                    .try_into()
                    .unwrap(),
            );
            let sh_flags = u32::from_le_bytes(
                elf_data[(offset + 8) as usize..(offset + 12) as usize]
                    .try_into()
                    .unwrap(),
            ) as u64;
            let sh_size = u32::from_le_bytes(
                elf_data[(offset + 20) as usize..(offset + 24) as usize]
                    .try_into()
                    .unwrap(),
            ) as u64;
            (sh_name, sh_type, sh_size, sh_flags)
        };

        // Get section name from string table
        if shstrndx > 0 && shstrndx < shnum {
            let strtab_offset = shoff + shstrndx * shentsize as u64;
            let strtab_sh_offset = if is_64bit {
                u64::from_le_bytes(
                    elf_data[(strtab_offset + 24) as usize..(strtab_offset + 32) as usize]
                        .try_into()
                        .unwrap(),
                )
            } else {
                u32::from_le_bytes(
                    elf_data[(strtab_offset + 16) as usize..(strtab_offset + 20) as usize]
                        .try_into()
                        .unwrap(),
                ) as u64
            };

            let name_start = (strtab_sh_offset + sh_name as u64) as usize;
            if name_start < elf_data.len() {
                let name_end = elf_data[name_start..]
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(0);
                let name =
                    std::str::from_utf8(&elf_data[name_start..name_start + name_end]).unwrap_or("");

                // Accumulate section sizes
                const SHT_PROGBITS: u32 = 1;
                const SHT_NOBITS: u32 = 8;
                const SHF_ALLOC: u64 = 0x2;
                const SHF_WRITE: u64 = 0x1;
                const SHF_EXECINSTR: u64 = 0x4;

                if sh_flags & SHF_ALLOC != 0 {
                    match name {
                        ".text" | ".rodata" | ".ARM.exidx" => {
                            usage.text += sh_size;
                        },
                        ".data" => {
                            usage.data += sh_size;
                        },
                        ".bss" => {
                            usage.bss += sh_size;
                        },
                        _ => {
                            // Other allocatable sections
                            if sh_type == SHT_PROGBITS {
                                if sh_flags & SHF_EXECINSTR != 0 {
                                    usage.text += sh_size;
                                } else if sh_flags & SHF_WRITE != 0 {
                                    usage.data += sh_size;
                                } else {
                                    usage.text += sh_size; // rodata-like
                                }
                            } else if sh_type == SHT_NOBITS {
                                usage.bss += sh_size;
                            }
                        },
                    }
                }
            }
        }
    }

    usage.calculate_totals();
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_usage_calculate_totals() {
        let mut usage = MemoryUsage {
            text: 1000,
            data: 100,
            bss: 50,
            ..Default::default()
        };
        usage.calculate_totals();
        assert_eq!(usage.flash_total, 1100);
        assert_eq!(usage.ram_total, 150);
    }

    #[test]
    fn memory_map_add_and_find_region() {
        let mut map = MemoryMap::new();
        map.add_region("FLASH", MemoryRegion::Flash, 0x08000000, 0x10000);
        map.add_region("RAM", MemoryRegion::Ram, 0x20000000, 0x8000);

        assert_eq!(map.regions.len(), 2);
        assert!(map.find_region("FLASH").is_some());
        assert!(map.find_region("RAM").is_some());
        assert!(map.find_region("UNKNOWN").is_none());
    }

    #[test]
    fn size_report_format() {
        let usage = MemoryUsage {
            text: 12345,
            data: 678,
            bss: 901,
            flash_total: 13023,
            ram_total: 1579,
        };

        let mut output = Vec::new();
        print_size_report(&mut output, &usage, "test.elf").unwrap();
        let report = String::from_utf8(output).unwrap();

        assert!(report.contains("text"));
        assert!(report.contains("data"));
        assert!(report.contains("bss"));
        assert!(report.contains("12345"));
        assert!(report.contains("test.elf"));
    }
}
