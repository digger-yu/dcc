// src/link/eh_frame.rs - .eh_frame_hdr 生成 (搜索表优化)
//
// .eh_frame 是 DWARF unwinding 信息，每条目 (FDE) 都需在异常抛出时被扫描。
// 默认实现是从头到尾线性扫描 .eh_frame 节，效率 O(n)。
//
// .eh_frame_hdr 是 .eh_frame 的搜索索引，使用 binary search：
//   header (8 bytes for 64-bit ELF):
//     u8  version
//     u8  eh_frame_ptr_enc    // DW_EH_PE encoding for .eh_frame base
//     u8  fde_count_enc
//     u8  table_enc
//     u32 eh_frame_ptr        // PC base, 取决于 eh_frame_ptr_enc
//   binary search table (binary search by FDE initial_location)
//     [fde_count]  { initial_location_enc, fde_pc_offset_enc }
//   augmentations (CIE augmentation data, optional)
//
// 我们只实现 binary search table 版本 (table_enc = DW_EH_PE_pcrel | DW_EH_PE_sdata4)。

#![allow(dead_code)]

/// DW_EH_PE_* 编码 (Definition in LSB 6.4.1)
#[allow(non_upper_case_globals)]
pub mod enc {
    pub const DW_EH_PE_absptr: u8 = 0x00;
    pub const DW_EH_PE_pcrel:  u8 = 0x10;
    pub const DW_EH_PE_sdata4: u8 = 0x0b;  // signed 4-byte
    pub const DW_EH_PE_udata4: u8 = 0x03;  // unsigned 4-byte
    pub const DW_EH_PE_sdata8: u8 = 0x0c;  // signed 8-byte
    pub const DW_EH_PE_udata8: u8 = 0x04;  // unsigned 8-byte
}

#[derive(Debug, Clone)]
pub struct FdeEntry {
    /// FDE 起始 PC (相对 .text 段的相对地址, 或 image base)
    pub initial_location: u64,
    /// FDE 字节偏移 (相对 .eh_frame 节起始)
    pub fde_offset: u64,
}

#[derive(Debug, Clone)]
pub struct EhFrameHdr {
    pub version: u8,
    pub eh_frame_ptr_enc: u8,
    pub fde_count_enc: u8,
    pub table_enc: u8,
    pub eh_frame_ptr: u64,
    pub binary_search_table: Vec<FdeEntry>,
}

impl Default for EhFrameHdr {
    fn default() -> Self {
        Self {
            version: 1,
            // 简化：均用 pcrel|sdata4
            eh_frame_ptr_enc: enc::DW_EH_PE_pcrel | enc::DW_EH_PE_sdata4,
            fde_count_enc: enc::DW_EH_PE_udata4,
            table_enc: enc::DW_EH_PE_pcrel | enc::DW_EH_PE_sdata4,
            eh_frame_ptr: 0,
            binary_search_table: vec![],
        }
    }
}

/// 生成 .eh_frame_hdr 字节
pub fn serialize(hdr: &EhFrameHdr) -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.push(hdr.version);
    out.push(hdr.eh_frame_ptr_enc);
    out.push(hdr.fde_count_enc);
    out.push(hdr.table_enc);
    // eh_frame_ptr: 按 enc 决定大小 (4 bytes for sdata4)
    out.extend_from_slice(&(hdr.eh_frame_ptr as i32).to_le_bytes());
    // fde_count: u32
    out.extend_from_slice(&(hdr.binary_search_table.len() as u32).to_le_bytes());
    // binary search table (sorted by initial_location)
    let mut sorted = hdr.binary_search_table.clone();
    sorted.sort_by_key(|f| f.initial_location);
    for f in &sorted {
        // initial_location as sdata4
        out.extend_from_slice(&(f.initial_location as i32).to_le_bytes());
        // fde_offset as sdata4
        out.extend_from_slice(&(f.fde_offset as i32).to_le_bytes());
    }
    out
}

/// 在运行时通过 binary search 查找给定 PC 对应的 FDE 偏移
/// （模拟运行时的查找过程, 主要用于验证表的正确性）
pub fn lookup(hdr: &EhFrameHdr, pc: u64) -> Option<u64> {
    let mut lo = 0usize;
    let mut hi = hdr.binary_search_table.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let e = &hdr.binary_search_table[mid];
        if pc < e.initial_location {
            hi = mid;
        } else if pc > e.initial_location {
            lo = mid + 1;
        } else {
            return Some(e.fde_offset);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_serialize() {
        let mut hdr = EhFrameHdr::default();
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x1000,
            fde_offset: 0x80,
        });
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x2000,
            fde_offset: 0xc0,
        });
        let bytes = serialize(&hdr);
        // header (4) + eh_frame_ptr (4) + fde_count (4) + table (16) = 28
        assert_eq!(bytes.len(), 4 + 4 + 4 + 2 * 8);
        assert_eq!(bytes[0], 1);  // version
    }

    #[test]
    fn test_binary_search() {
        let mut hdr = EhFrameHdr::default();
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x1000,
            fde_offset: 0x80,
        });
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x2000,
            fde_offset: 0xc0,
        });
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x3000,
            fde_offset: 0x100,
        });
        assert_eq!(lookup(&hdr, 0x2000), Some(0xc0));
        assert_eq!(lookup(&hdr, 0x1500), None);
        assert_eq!(lookup(&hdr, 0x3000), Some(0x100));
    }

    #[test]
    fn test_unsorted_input_is_sorted_in_serialize() {
        let mut hdr = EhFrameHdr::default();
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x3000,
            fde_offset: 0x100,
        });
        hdr.binary_search_table.push(FdeEntry {
            initial_location: 0x1000,
            fde_offset: 0x80,
        });
        let bytes = serialize(&hdr);
        // table[0] 应该按 initial_location 排序, 第一个 entry 的 pc 应为 0x1000
        let pc0 = i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        assert_eq!(pc0, 0x1000);
    }
}
