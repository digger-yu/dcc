// src/profile/perf_data.rs - Linux perf.data 二进制读取
//
// 第一性原理: Linux `perf record` 落盘的 perf.data 是 ELF-like 容器:
//   magic "PERFILE2" (8 bytes) + header size + event attrs (perf_event_attr)
//   + data section (mmap'd samples).
//
// 用途: `perf record -e cycles:pp -o perf.data ./prog` 抓 sample 后,
// 编译器读 perf.data 把样本按 (pid, ip) 折叠成函数 / 行级权重, 喂给
// AutoFDO 路径做函数布局 / inliner 提示.
//
// dcc 实装:
//   * 读 magic + header
//   * 读 perf_event_attr 数组, 提取 event id + 类型
//   * 解析 PERF_RECORD_SAMPLE 记录 (type=9)
//   * 事件关联: 将样本映射到对应 event attr
//
// perf_event_header: type(u32) misc(u16) size(u16)
// PERF_RECORD_SAMPLE: header + sample_id + ip(addr) + pid/tid + time

use std::collections::HashMap;
use std::fs;
use std::io;
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
    /// 原始数据 (用于样本解析)
    raw_data: Vec<u8>,
}

/// 解析出的样本记录
#[derive(Debug, Clone)]
pub struct PerfSample {
    /// 指令指针 (PC)
    pub ip: u64,
    /// 进程 ID
    pub pid: u32,
    /// 线程 ID
    pub tid: u32,
    /// 时间戳
    pub time: u64,
    /// 周期数
    pub period: u64,
    /// 关联的 event attr 索引
    pub attr_index: usize,
}

impl PerfDataFile {
    /// 读 perf.data 文件
    pub fn read(path: &Path) -> io::Result<Self> {
        let buf = fs::read(path)?;
        let file_size = buf.len() as u64;
        if buf.len() < 24 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "perf.data too short"));
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
            if off + step > buf.len() {
                break;
            }
            off += step;
        }
        Ok(Self {
            file_size,
            header_size,
            attrs_offset,
            attrs,
            data_offset,
            data_size,
            raw_data: buf,
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

    /// 解析 PERF_RECORD_SAMPLE 记录.
    /// 返回解析出的样本列表.
    pub fn parse_samples(&self) -> Vec<PerfSample> {
        let mut samples = Vec::new();
        let data_start = self.data_offset as usize;
        let data_end = (data_start + self.data_size as usize).min(self.raw_data.len());
        let mut off = data_start;

        while off + 8 <= data_end {
            // perf_event_header: type(u32) misc(u16) size(u16)
            if off + 8 > self.raw_data.len() {
                break;
            }
            let rec_type = u32::from_le_bytes(self.raw_data[off..off + 4].try_into().unwrap());
            let rec_size =
                u16::from_le_bytes(self.raw_data[off + 6..off + 8].try_into().unwrap()) as usize;
            if rec_size < 8 || off + rec_size > data_end {
                break;
            }
            // PERF_RECORD_SAMPLE = 9
            if rec_type == 9 && rec_size >= 40 {
                // Simplified sample record layout:
                // +0: header (8 bytes)
                // +8: ip (u64)
                // +16: pid (u32) + tid (u32)
                // +24: time (u64)
                let ip = u64::from_le_bytes(self.raw_data[off + 8..off + 16].try_into().unwrap());
                let pid = u32::from_le_bytes(self.raw_data[off + 16..off + 20].try_into().unwrap());
                let tid = u32::from_le_bytes(self.raw_data[off + 20..off + 24].try_into().unwrap());
                let time = if rec_size >= 32 {
                    u64::from_le_bytes(self.raw_data[off + 24..off + 32].try_into().unwrap())
                } else {
                    0
                };
                let period = if rec_size >= 40 {
                    u64::from_le_bytes(self.raw_data[off + 32..off + 40].try_into().unwrap())
                } else {
                    1
                };
                samples.push(PerfSample {
                    ip,
                    pid,
                    tid,
                    time,
                    period,
                    attr_index: 0, // simplified: all map to attr 0
                });
            }
            off += rec_size;
        }
        samples
    }

    /// 将样本关联到 event attrs, 返回 event_name -> total_count 映射.
    pub fn correlate_events(&self) -> HashMap<String, u64> {
        let samples = self.parse_samples();
        let names = self.event_names();
        let mut counts: HashMap<String, u64> = HashMap::new();
        for sample in &samples {
            let name = names
                .get(sample.attr_index)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            *counts.entry(name).or_insert(0) += sample.period;
        }
        counts
    }

    /// 按 IP 地址聚合样本, 返回 ip -> count 映射.
    pub fn aggregate_by_ip(&self) -> HashMap<u64, u64> {
        let samples = self.parse_samples();
        let mut counts: HashMap<u64, u64> = HashMap::new();
        for sample in &samples {
            *counts.entry(sample.ip).or_insert(0) += sample.period;
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

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
        f.write_all(&1u32.to_le_bytes())?; // type = SOFTWARE
        f.write_all(&120u32.to_le_bytes())?; // size
        f.write_all(&0x09u64.to_le_bytes())?; // config = PERF_COUNT_SW_CPU_CLOCK
        f.write_all(&[0u8; 104])?; // padding
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
        std::fs::write(&tmp, [0u8; 10]).unwrap();
        let err = PerfDataFile::read(&tmp).unwrap_err();
        assert!(err.to_string().contains("too short"));
        let _ = std::fs::remove_file(&tmp);
    }

    /// Build perf.data with PERF_RECORD_SAMPLE records.
    fn write_perfdata_with_samples(path: &Path) -> io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        // Header (56 bytes)
        f.write_all(b"PERFILE2")?;
        f.write_all(&56u64.to_le_bytes())?; // header_size
        f.write_all(&120u64.to_le_bytes())?; // attr_size
        f.write_all(&56u64.to_le_bytes())?; // attrs_offset
        f.write_all(&120u64.to_le_bytes())?; // attrs_size
        let data_off = 56u64 + 120u64;
        f.write_all(&data_off.to_le_bytes())?; // data_offset
                                               // data_size placeholder
        let data_size_pos = f.stream_position()?;
        f.write_all(&0u64.to_le_bytes())?; // data_size
                                           // 1 attr (120 bytes)
        f.write_all(&0u32.to_le_bytes())?; // type = HARDWARE
        f.write_all(&120u32.to_le_bytes())?; // size
        f.write_all(&0u64.to_le_bytes())?; // config = HW_CPU_CYCLES
        f.write_all(&[0u8; 104])?; // padding
                                   // Data section with 2 PERF_RECORD_SAMPLE records
        let data_start = f.stream_position()?;
        // Sample 1: ip=0x401000, pid=1234, tid=1234, time=100, period=10
        f.write_all(&9u32.to_le_bytes())?; // type = SAMPLE
        f.write_all(&0u16.to_le_bytes())?; // misc
        f.write_all(&40u16.to_le_bytes())?; // size = 40
        f.write_all(&0x401000u64.to_le_bytes())?; // ip
        f.write_all(&1234u32.to_le_bytes())?; // pid
        f.write_all(&1234u32.to_le_bytes())?; // tid
        f.write_all(&100u64.to_le_bytes())?; // time
        f.write_all(&10u64.to_le_bytes())?; // period
                                            // Sample 2: ip=0x402000, pid=1234, tid=1234, time=200, period=20
        f.write_all(&9u32.to_le_bytes())?; // type = SAMPLE
        f.write_all(&0u16.to_le_bytes())?; // misc
        f.write_all(&40u16.to_le_bytes())?; // size = 40
        f.write_all(&0x402000u64.to_le_bytes())?; // ip
        f.write_all(&1234u32.to_le_bytes())?; // pid
        f.write_all(&1234u32.to_le_bytes())?; // tid
        f.write_all(&200u64.to_le_bytes())?; // time
        f.write_all(&20u64.to_le_bytes())?; // period
        let data_end = f.stream_position()?;
        let data_size = data_end - data_start;
        // Patch data_size in header
        f.seek(std::io::SeekFrom::Start(data_size_pos))?;
        f.write_all(&data_size.to_le_bytes())?;
        Ok(())
    }

    #[test]
    fn parse_sample_records() {
        let tmp = std::env::temp_dir().join("dcc-perfdata-samples.bin");
        write_perfdata_with_samples(&tmp).unwrap();
        let f = PerfDataFile::read(&tmp).unwrap();
        let samples = f.parse_samples();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].ip, 0x401000);
        assert_eq!(samples[0].pid, 1234);
        assert_eq!(samples[0].period, 10);
        assert_eq!(samples[1].ip, 0x402000);
        assert_eq!(samples[1].period, 20);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn correlate_events_counts() {
        let tmp = std::env::temp_dir().join("dcc-perfdata-correlate.bin");
        write_perfdata_with_samples(&tmp).unwrap();
        let f = PerfDataFile::read(&tmp).unwrap();
        let counts = f.correlate_events();
        // All samples map to attr 0 = "event0"
        let total: u64 = counts.values().sum();
        assert_eq!(total, 30); // 10 + 20
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn aggregate_by_ip() {
        let tmp = std::env::temp_dir().join("dcc-perfdata-aggregate.bin");
        write_perfdata_with_samples(&tmp).unwrap();
        let f = PerfDataFile::read(&tmp).unwrap();
        let by_ip = f.aggregate_by_ip();
        assert_eq!(by_ip.get(&0x401000), Some(&10));
        assert_eq!(by_ip.get(&0x402000), Some(&20));
        let _ = std::fs::remove_file(&tmp);
    }
}
