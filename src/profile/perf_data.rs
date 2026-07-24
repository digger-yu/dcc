// src/profile/perf_data.rs - Linux perf.data 二进制读取 (简化版)
//
// 第一性原理: Linux `perf record` 落盘的 perf.data 是 ELF-like 容器:
//   magic "PERFILE2" (8 bytes) + header size + event attrs (perf_event_attr)
//   + data section (mmap'd samples).
//
// 用途: `perf record -e cycles:pp -o perf.data ./prog` 抓 sample 后,
// 编译器读 perf.data 把样本按 (pid, ip) 折叠成函数 / 行级权重, 喂给
// AutoFDO 路径做函数布局 / inliner 提示.
//
// 当前 dcc 仅做基础解析:
//   * 读 magic + header
//   * 读 perf_event_attr 数组, 提取 event id + 类型
//   * 跳过 data section (不解析具体 sample)
//
// 完整解析需要跟踪每个 event 的 type + sample_type + read_format, 然后
// 按 mmap page 翻 samples, 这是后续 M 的工作.

use std::io::{self, Read};
use std::path::Path;

pub const PERF_DATA_MAGIC: &[u8; 8] = b"PERFILE2";

/// 单个 perf_event_attr (Linux 5.x, 120 bytes)
///
/// 我们只读前 16 字节 (type + size + config), 后续字段 (sample_type 等)
/// 留作扩展, 当前不需要.
#[derive(Debug, Clone)]
pub struct PerfEventAttr {
    /// 事件类型: 0=hardware, 1=software, ...
    pub event_type: u32,
    /// 本 attr 结构大小
    pub attr_size: u32,
    /// event id (config 字段, 如 PERF_COUNT_HW_CPU_CYCLES = 0)
    pub config: u64,
    /// 事件名 (从 tracepoint / dynamic PMU 解析, 简化版固定为 None)
    pub name: Option<String>,
}

/// 解析后的 perf.data header
#[derive(Debug, Clone)]
pub struct PerfDataFile {
    /// 文件总大小
    pub file_size: u64,
    /// header 区大小
    pub header_size: u64,
    /// event attrs 起始偏移
    pub attrs_offset: u64,
    /// event attrs 总数
    pub attrs: Vec<PerfEventAttr>,
    /// data section 起始偏移
    pub data_offset: u64,
    /// data section 字节数
    pub data_size: u64,
}

impl PerfDataFile {
    /// 读 perf.data 文件
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let file_size = buf.len() as u64;
        if buf.len() < 24 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "perf.data too short",
            ));
        }
        if &buf[0..8] != PERF_DATA_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad perf.data magic (expected PERFILE2)",
            ));
        }
        let header_size = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        // buf[16..24] 通常是 attr_size 或 flags (perf 不同版本略有不同)
        // 我们不依赖这一段, 直接用 header_size 计算
        if header_size < 24 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("perf.data header too small: {header_size}"),
            ));
        }
        // 解析 header body
        // perf file header (v2):
        //   8  magic
        //   8  size
        //   8  attr_size (each perf_event_attr block size)
        //   8  attrs_offset
        //   8  attrs_size
        //   8  data_offset
        //   8  data_size
        //   ... event attrs ...
        //   ... feature sections (skip) ...
        if buf.len() < 56 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "perf.data header v2 fields incomplete",
            ));
        }
        let attr_size = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        let attrs_offset = u64::from_le_bytes(buf[24..32].try_into().unwrap());
        let _attrs_size = u64::from_le_bytes(buf[32..40].try_into().unwrap());
        let data_offset = u64::from_le_bytes(buf[40..48].try_into().unwrap());
        let data_size = u64::from_le_bytes(buf[48..56].try_into().unwrap());

        // 解析 perf_event_attr 数组
        let mut attrs: Vec<PerfEventAttr> = Vec::new();
        let mut off = attrs_offset as usize;
        let attr_size_us = if attr_size < 16 { 120 } else { attr_size as usize };
        // attrs 区在 header 之后, 跨越 [attrs_offset, attrs_offset + attrs_size),
        // 不应被 header_size 截断 (但不能超过 buf.len())
        let end = (attrs_offset + _attrs_size).min(buf.len() as u64) as usize;
        // 我们只读 16 字节 (type + size + config)
        while off + 16 <= end {
            let event_type = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            let sz = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
            let config = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap());
            attrs.push(PerfEventAttr {
                event_type,
                attr_size: sz,
                config,
                name: None,
            });
            let step = if attr_size_us < 16 { 120 } else { attr_size_us };
            if off + step > buf.len() { break; }
            off += step;
        }
        Ok(Self {
            file_size,
            header_size,
            attrs_offset,
            attrs,
            data_offset,
            data_size,
        })
    }

    /// 把所有 attr 的 config id 规整为字符串列表 (供 PerfRecord.add 使用)
    pub fn event_names(&self) -> Vec<String> {
        self.attrs
            .iter()
            .map(|a| {
                let mut n = format!("event{:x}", a.config);
                if let Some(name) = &a.name {
                    n = name.clone();
                }
                n
            })
            .collect()
    }

    /// 把 data section 字节数作为"采样原始字节计数" (粗略 proxy).
    /// 真正样本数 = data_size / 平均 sample 大小; 此处简化为计数
    /// 每个 attr 等分 data section 字节.
    pub fn synthetic_counts(&self) -> Vec<u64> {
        if self.attrs.is_empty() {
            return vec![];
        }
        let per = self.data_size / self.attrs.len() as u64;
        vec![per; self.attrs.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 构造一个最小 perf.data (v2 header + 1 attr + data section)
    fn write_minimal_perfdata(path: &Path) -> io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        // 8 magic
        f.write_all(b"PERFILE2")?;
        // 8 header_size
        f.write_all(&56u64.to_le_bytes())?;
        // 8 attr_size
        f.write_all(&120u64.to_le_bytes())?;
        // 8 attrs_offset
        f.write_all(&56u64.to_le_bytes())?;
        // 8 attrs_size
        f.write_all(&120u64.to_le_bytes())?;
        // 8 data_offset
        f.write_all(&(56u64 + 120u64).to_le_bytes())?;
        // 8 data_size
        f.write_all(&1024u64.to_le_bytes())?;
        // 1 perf_event_attr (120 bytes; 我们只填前 16)
        f.write_all(&1u32.to_le_bytes())?;     // type = SOFTWARE
        f.write_all(&120u32.to_le_bytes())?;   // size
        f.write_all(&0x09u64.to_le_bytes())?;   // config = PERF_COUNT_SW_CPU_CLOCK
        f.write_all(&[0u8; 104])?;             // padding
        // data section: 1024 字节
        f.write_all(&vec![0u8; 1024])?;
        Ok(())
    }

    #[test]
    fn parse_minimal_perf_data() {
        let tmp = std::env::temp_dir().join("dcc-perfdata-test.bin");
        write_minimal_perfdata(&tmp).unwrap();
        let f = PerfDataFile::read(&tmp).unwrap();
        assert_eq!(f.header_size, 56);
        assert_eq!(f.attrs.len(), 1);
        assert_eq!(f.attrs[0].event_type, 1); // SOFTWARE
        assert_eq!(f.attrs[0].config, 0x09);
        assert_eq!(f.data_size, 1024);
        let names = f.event_names();
        assert_eq!(names.len(), 1);
        let counts = f.synthetic_counts();
        assert_eq!(counts, vec![1024]);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn reject_bad_magic() {
        let tmp = std::env::temp_dir().join("dcc-perfdata-bad.bin");
        std::fs::write(&tmp, b"BADMAGIC\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").unwrap();
        let err = PerfDataFile::read(&tmp).unwrap_err();
        assert!(err.to_string().contains("magic"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn reject_too_short() {
        let tmp = std::env::temp_dir().join("dcc-perfdata-short.bin");
        std::fs::write(&tmp, &[0u8; 10]).unwrap();
        let err = PerfDataFile::read(&tmp).unwrap_err();
        assert!(err.to_string().contains("too short"));
        let _ = std::fs::remove_file(&tmp);
    }
}
