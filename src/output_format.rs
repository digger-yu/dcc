//! ELF to raw binary / Intel HEX conversion for bare-metal targets.
//!
//! This module provides output format conversion utilities for embedded
//! systems programming, where firmware images need to be in raw binary
//! or Intel HEX format for flashing to microcontrollers.

use std::io::{self, Write};

/// Output format for bare-metal targets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// ELF executable (default)
    #[default]
    Elf,
    /// Raw binary (flat memory image)
    Binary,
    /// Intel HEX format
    IntelHex,
}

/// Convert ELF to raw binary format.
///
/// Extracts loadable segments (PT_LOAD) from ELF and writes them
/// as a flat binary image. Gaps between segments are filled with
/// zeros (or can be skipped for separate output files).
///
/// # Arguments
/// * `elf_data` - ELF file contents
/// * `output` - Output writer for binary data
///
/// # Returns
/// Number of bytes written
pub fn elf_to_binary<W: Write>(elf_data: &[u8], mut output: W) -> io::Result<u64> {
    // Parse ELF header to find program headers
    let elf = parse_elf_minimal(elf_data)?;

    let mut total_written: u64 = 0;

    // Write each PT_LOAD segment
    for segment in &elf.segments {
        if segment.p_type != 1 {
            // PT_LOAD = 1
            continue;
        }

        // Seek to the correct offset (fill gaps with zeros)
        let current_pos = total_written;
        if segment.p_paddr < current_pos {
            // Overlapping segments - skip
            continue;
        }

        // Write padding for gap
        let gap = segment.p_paddr - current_pos;
        if gap > 0 {
            let zeros = vec![0u8; gap as usize];
            output.write_all(&zeros)?;
            total_written += gap;
        }

        // Write segment data
        let start = segment.p_offset as usize;
        let end = start + segment.p_filesz as usize;
        if end > elf_data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "segment extends beyond ELF file",
            ));
        }
        output.write_all(&elf_data[start..end])?;
        total_written += segment.p_filesz;
    }

    Ok(total_written)
}

/// Convert ELF to Intel HEX format.
///
/// Generates Intel HEX records from ELF loadable segments.
/// Supports 32-bit linear address format (:02 for address records).
///
/// # Arguments
/// * `elf_data` - ELF file contents
/// * `output` - Output writer for HEX data (ASCII text)
///
/// # Returns
/// Number of records written
pub fn elf_to_intel_hex<W: Write>(elf_data: &[u8], mut output: W) -> io::Result<u64> {
    let elf = parse_elf_minimal(elf_data)?;
    let mut record_count: u64 = 0;

    // Extended Linear Address record for 32-bit addresses
    let mut last_upper_addr: u32 = 0xFFFFFFFF;

    for segment in &elf.segments {
        if segment.p_type != 1 {
            continue;
        }

        let mut offset = 0u64;
        while offset < segment.p_filesz {
            let addr = segment.p_paddr + offset;
            let upper_addr = (addr >> 16) as u32;

            // Write Extended Linear Address record if needed
            if upper_addr != last_upper_addr {
                let record = format_extended_linear_address(upper_addr as u16);
                output.write_all(record.as_bytes())?;
                last_upper_addr = upper_addr;
                record_count += 1;
            }

            // Write Data record (max 16 bytes per record)
            let chunk_size = std::cmp::min(16, (segment.p_filesz - offset) as usize);
            let start = segment.p_offset as usize + offset as usize;
            let end = start + chunk_size;
            let data = &elf_data[start..end];

            let record = format_data_record(addr as u16, data);
            output.write_all(record.as_bytes())?;
            record_count += 1;

            offset += chunk_size as u64;
        }
    }

    // End of File record
    output.write_all(b":00000001FF\n")?;
    record_count += 1;

    Ok(record_count)
}

// === Internal ELF parsing (minimal) ===

#[derive(Debug)]
struct ElfSegment {
    p_type: u32,
    p_offset: u64,
    p_paddr: u64,
    p_filesz: u64,
}

#[derive(Debug)]
struct MinimalElf {
    segments: Vec<ElfSegment>,
}

fn parse_elf_minimal(data: &[u8]) -> io::Result<MinimalElf> {
    if data.len() < 64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ELF file too small"));
    }

    // Check ELF magic
    if &data[0..4] != b"\x7fELF" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not an ELF file"));
    }

    let is_64bit = data[4] == 2;
    let is_little_endian = data[5] == 1;

    if !is_little_endian {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "only little-endian ELF supported"));
    }

    let (phoff, phentsize, phnum) = if is_64bit {
        if data.len() < 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ELF64 header too small"));
        }
        let phoff = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let phentsize = u16::from_le_bytes(data[54..56].try_into().unwrap());
        let phnum = u16::from_le_bytes(data[56..58].try_into().unwrap());
        (phoff, phentsize, phnum as u64)
    } else {
        if data.len() < 52 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ELF32 header too small"));
        }
        let phoff = u32::from_le_bytes(data[28..32].try_into().unwrap()) as u64;
        let phentsize = u16::from_le_bytes(data[42..44].try_into().unwrap());
        let phnum = u16::from_le_bytes(data[44..46].try_into().unwrap());
        (phoff, phentsize, phnum as u64)
    };

    let mut segments = Vec::new();
    for i in 0..phnum {
        let offset = phoff + i * phentsize as u64;
        let seg = if is_64bit {
            if data.len() < (offset + 56) as usize {
                continue;
            }
            ElfSegment {
                p_type: u32::from_le_bytes(
                    data[offset as usize..(offset + 4) as usize]
                        .try_into()
                        .unwrap(),
                ),
                p_offset: u64::from_le_bytes(
                    data[(offset + 8) as usize..(offset + 16) as usize]
                        .try_into()
                        .unwrap(),
                ),
                p_paddr: u64::from_le_bytes(
                    data[(offset + 24) as usize..(offset + 32) as usize]
                        .try_into()
                        .unwrap(),
                ),
                p_filesz: u64::from_le_bytes(
                    data[(offset + 32) as usize..(offset + 40) as usize]
                        .try_into()
                        .unwrap(),
                ),
            }
        } else {
            if data.len() < (offset + 32) as usize {
                continue;
            }
            ElfSegment {
                p_type: u32::from_le_bytes(
                    data[offset as usize..(offset + 4) as usize]
                        .try_into()
                        .unwrap(),
                ),
                p_offset: u32::from_le_bytes(
                    data[(offset + 4) as usize..(offset + 8) as usize]
                        .try_into()
                        .unwrap(),
                ) as u64,
                p_paddr: u32::from_le_bytes(
                    data[(offset + 12) as usize..(offset + 16) as usize]
                        .try_into()
                        .unwrap(),
                ) as u64,
                p_filesz: u32::from_le_bytes(
                    data[(offset + 16) as usize..(offset + 20) as usize]
                        .try_into()
                        .unwrap(),
                ) as u64,
            }
        };
        segments.push(seg);
    }

    Ok(MinimalElf { segments })
}

// === Intel HEX formatting ===

fn format_data_record(addr: u16, data: &[u8]) -> String {
    let len = data.len() as u8;
    let mut record = String::with_capacity(11 + data.len() * 2);
    record.push(':');
    record.push_str(&format!("{:02X}", len));
    record.push_str(&format!("{:04X}", addr));
    record.push_str("00"); // Data record type

    let mut checksum = len.wrapping_add((addr >> 8) as u8).wrapping_add(addr as u8);
    for &byte in data {
        record.push_str(&format!("{:02X}", byte));
        checksum = checksum.wrapping_add(byte);
    }
    checksum = checksum.wrapping_add(0x00); // record type
    checksum = (!checksum).wrapping_add(1); // two's complement
    record.push_str(&format!("{:02X}", checksum));
    record.push('\n');
    record
}

fn format_extended_linear_address(upper_addr: u16) -> String {
    let mut record = String::from(":02000004");
    record.push_str(&format!("{:04X}", upper_addr));
    let mut checksum = 0u8
        .wrapping_add(0x02) // length
        .wrapping_add(0x00) // address high
        .wrapping_add(0x00) // address low
        .wrapping_add(0x04) // record type
        .wrapping_add((upper_addr >> 8) as u8)
        .wrapping_add(upper_addr as u8);
    checksum = (!checksum).wrapping_add(1);
    record.push_str(&format!("{:02X}", checksum));
    record.push('\n');
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_default_is_elf() {
        assert_eq!(OutputFormat::default(), OutputFormat::Elf);
    }

    #[test]
    fn format_data_record_correct() {
        let record = format_data_record(0x0000, &[0x01, 0x02, 0x03]);
        assert!(record.starts_with(":03000000"));
        assert!(record.ends_with("\n"));
    }

    #[test]
    fn format_extended_linear_address_correct() {
        let record = format_extended_linear_address(0x0800);
        assert!(record.starts_with(":02000004"));
        assert!(record.contains("0800"));
    }
}
