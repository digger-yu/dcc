// src/profile/lbr.rs - LBR (Last Branch Record) profiling
//
// LBR: CPU 在每条分支指令后落 (from_ip, to_ip, cycle) 三元组到 MSR,
// perf record `-b` 抓出来后按源/目标落入函数 + 边 + 调用链.
//
// LBR 相对 AutoFDO 的优势:
//   * 完全硬件, 不需要插桩, 0 开销
//   * 精确到分支目标, 不需要偏移补偿
// 局限:
//   * LBR stack 深度有限 (典型 16/32 条)
//   * 跨函数调用 / 中断会污染样本
//
// 数据格式:
// 1. perf script -F brstack 文本:
//    <from_func> <from_addr> <to_func> <to_addr> [<mispred>|<pred>] [<cycles>]
// 2. perf.data 二进制 (PERF_RECORD_SAMPLE with branch_stack):
//    每条 LBR entry: from(u64) to(u64) flags(u64) cycles(u16)
//
// 已接入 driver → ProfileWeights → codegen (.text.hot / .text.unlikely).

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

/// 单条 LBR 边的详细信息
#[derive(Debug, Clone, Default)]
pub struct LbrEdge {
    pub from_func: String,
    pub from_addr: u64,
    pub to_func: String,
    pub to_addr: u64,
    /// 分支预测错误标记
    pub mispredicted: bool,
    /// 周期数 (若可用)
    pub cycles: Option<u64>,
}

/// LBR 采样概要
#[derive(Debug, Clone, Default)]
pub struct LbrProfile {
    /// (from_func, to_func) -> count
    pub edges: HashMap<(String, String), u64>,
    /// 入口候选热点 (按 from→to 入度)
    pub hot_entries: Vec<(String, u64)>,
    /// 详细边信息 (保留最近 N 条)
    pub detailed_edges: Vec<LbrEdge>,
    /// 错误预测边统计 (from_func, to_func) -> mispredict_count
    pub mispred_edges: HashMap<(String, String), u64>,
    /// 累计周期数 (from_func, to_func) -> total_cycles
    pub edge_cycles: HashMap<(String, String), u64>,
}

/// LBR 概要统计
#[derive(Debug, Clone, Default)]
pub struct LbrSummary {
    pub total_edges: u64,
    pub unique_edges: usize,
    pub hot_entry_count: usize,
    pub mispred_count: u64,
    pub total_cycles: u64,
}

impl LbrProfile {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从文件路径解析 (自动检测文本 brstack 或 perf.data 二进制)
    pub fn read(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        // Check for perf.data magic "PERFILE2"
        if data.len() >= 8 && &data[0..8] == b"PERFILE2" {
            return Self::read_perf_data_binary(&data);
        }
        // Otherwise treat as text brstack
        let s = String::from_utf8(data).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e)
        })?;
        Self::from_str(&s)
    }

    /// 从文件路径解析 `perf script -F brstack` 输出
    pub fn read_perf_brstack(path: &Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        let mut profile = LbrProfile::default();
        for line in std::io::BufReader::new(f).lines() {
            let l = line?;
            profile.parse_brstack_line(&l);
        }
        profile.finalize();
        Ok(profile)
    }

    /// 从字符串解析 brstack 格式
    pub fn from_str(input: &str) -> io::Result<Self> {
        let mut profile = LbrProfile::default();
        for line in input.lines() {
            profile.parse_brstack_line(line);
        }
        profile.finalize();
        Ok(profile)
    }

    /// 解析单行 brstack 输入:
    /// `from_func 0xADDR to_func 0xADDR [M|P] [cycles]`
    fn parse_brstack_line(&mut self, line: &str) {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            return;
        }
        let mut it = l.split_whitespace();
        let from_func = it.next().unwrap_or("").to_string();
        let from_addr = it
            .next()
            .and_then(|s| s.strip_prefix("0x").or(Some(s)))
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        let to_func = it.next().unwrap_or("").to_string();
        let to_addr = it
            .next()
            .and_then(|s| s.strip_prefix("0x").or(Some(s)))
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0);

        if from_func.is_empty() || to_func.is_empty() {
            return;
        }

        // 解析可选的 mispredict 标记和 cycles
        let mut mispredicted = false;
        let mut cycles: Option<u64> = None;
        for token in it {
            match token {
                "M" => mispredicted = true,
                "P" => mispredicted = false,
                _ => {
                    if let Ok(c) = token.parse::<u64>() {
                        cycles = Some(c);
                    }
                }
            }
        }

        // 更新边计数
        *self.edges.entry((from_func.clone(), to_func.clone())).or_insert(0) += 1;

        // 更新 mispredict 统计
        if mispredicted {
            *self
                .mispred_edges
                .entry((from_func.clone(), to_func.clone()))
                .or_insert(0) += 1;
        }

        // 更新 cycles 统计
        if let Some(c) = cycles {
            *self
                .edge_cycles
                .entry((from_func.clone(), to_func.clone()))
                .or_insert(0) += c;
        }

        // 保留详细边信息 (限制数量避免内存爆炸)
        if self.detailed_edges.len() < 10000 {
            self.detailed_edges.push(LbrEdge {
                from_func,
                from_addr,
                to_func,
                to_addr,
                mispredicted,
                cycles,
            });
        }
    }

    /// 计算 hot_entries (入口热点)
    fn finalize(&mut self) {
        let mut entries: HashMap<String, u64> = HashMap::new();
        for ((f, t), c) in &self.edges {
            if f == "unknown" || f.is_empty() {
                *entries.entry(t.clone()).or_insert(0) += *c;
            }
        }
        let mut entries_v: Vec<_> = entries.into_iter().collect();
        entries_v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        self.hot_entries = entries_v;
    }

    pub fn hot_edge_count(&self, from: &str, to: &str) -> u64 {
        self.edges
            .get(&(from.into(), to.into()))
            .copied()
            .unwrap_or(0)
    }

    /// 合并另一个 LBR profile 到自身
    pub fn merge(&mut self, other: &LbrProfile) {
        for (k, &v) in &other.edges {
            *self.edges.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.mispred_edges {
            *self.mispred_edges.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.edge_cycles {
            *self.edge_cycles.entry(k.clone()).or_insert(0) += v;
        }
        self.finalize();
    }

    /// 概要统计
    pub fn summary(&self) -> LbrSummary {
        LbrSummary {
            total_edges: self.edges.values().copied().sum(),
            unique_edges: self.edges.len(),
            hot_entry_count: self.hot_entries.len(),
            mispred_count: self.mispred_edges.values().copied().sum(),
            total_cycles: self.edge_cycles.values().copied().sum(),
        }
    }

    /// 获取函数的入度边 (所有 caller -> func)
    pub fn inbound_edges(&self, func: &str) -> Vec<(&String, u64)> {
        self.edges
            .iter()
            .filter(|((_, t), _)| t == func)
            .map(|((from, _), count)| (from, *count))
            .collect()
    }

    /// 获取函数的出度边 (func -> 所有 callee)
    pub fn outbound_edges(&self, func: &str) -> Vec<(&String, u64)> {
        self.edges
            .iter()
            .filter(|((f, _), _)| f == func)
            .map(|((_, to), count)| (to, *count))
            .collect()
    }

    /// 错误预测率 (mispred / total) for a specific edge
    pub fn mispred_rate(&self, from: &str, to: &str) -> f64 {
        let total = self.hot_edge_count(from, to);
        if total == 0 {
            return 0.0;
        }
        let mispred = self
            .mispred_edges
            .get(&(from.into(), to.into()))
            .copied()
            .unwrap_or(0);
        mispred as f64 / total as f64
    }

    /// 从 perf.data 二进制解析 LBR 记录.
    /// perf.data 格式: magic("PERFILE2") + header + attrs + records
    /// PERF_RECORD_SAMPLE (type=9) 含 branch_stack:
    ///   nr(u64) + entries[]: from(u64) to(u64) flags(u64) cycles(u16)
    pub fn read_perf_data_binary(data: &[u8]) -> io::Result<Self> {
        if data.len() < 8 || &data[0..8] != b"PERFILE2" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a perf.data file",
            ));
        }
        let mut profile = LbrProfile::default();
        // Parse header to find record offset
        if data.len() < 32 {
            return Ok(profile);
        }
        let header_size = u64::from_le_bytes(data[8..16].try_into().unwrap()) as usize;
        let attr_size = u64::from_le_bytes(data[16..24].try_into().unwrap()) as usize;
        let n_attrs = u64::from_le_bytes(data[24..32].try_into().unwrap()) as usize;

        // Skip to data section (simplified: assume fixed offset)
        // Real perf.data has complex header with feature sections
        let mut off = header_size.max(32);
        // Skip attr table
        off += n_attrs * attr_size.max(64);
        // Align to 8 bytes
        off = (off + 7) & !7;

        // Parse records
        while off + 8 <= data.len() {
            // perf_event_header: type(u32) misc(u16) size(u16)
            let rec_type = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            let rec_size = u16::from_le_bytes(data[off + 6..off + 8].try_into().unwrap()) as usize;
            if rec_size < 8 || off + rec_size > data.len() {
                break;
            }
            // PERF_RECORD_SAMPLE = 9
            if rec_type == 9 {
                // Sample record: skip sample_id, look for branch_stack
                // Simplified: scan for branch_stack pattern
                let mut pos = off + 8; // skip header
                // Skip sample_id (pid, tid, time, etc.) - variable size
                // In practice, branch_stack comes after fixed fields
                // For simplicity, look for nr field followed by valid entries
                while pos + 24 <= off + rec_size {
                    let nr = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                    if nr > 0 && nr < 100 && pos + 8 + nr as usize * 26 <= off + rec_size {
                        // Likely branch_stack
                        let mut entry_off = pos + 8;
                        for _ in 0..nr {
                            let from = u64::from_le_bytes(
                                data[entry_off..entry_off + 8].try_into().unwrap(),
                            );
                            let to = u64::from_le_bytes(
                                data[entry_off + 8..entry_off + 16].try_into().unwrap(),
                            );
                            let flags = u64::from_le_bytes(
                                data[entry_off + 16..entry_off + 24].try_into().unwrap(),
                            );
                            let cycles = if entry_off + 26 <= off + rec_size {
                                Some(u16::from_le_bytes(
                                    data[entry_off + 24..entry_off + 26].try_into().unwrap(),
                                ) as u64)
                            } else {
                                None
                            };
                            let mispredicted = (flags & 1) != 0;
                            // Use addresses as function names (hex)
                            let from_func = format!("0x{:x}", from);
                            let to_func = format!("0x{:x}", to);
                            *profile
                                .edges
                                .entry((from_func.clone(), to_func.clone()))
                                .or_insert(0) += 1;
                            if mispredicted {
                                *profile
                                    .mispred_edges
                                    .entry((from_func.clone(), to_func.clone()))
                                    .or_insert(0) += 1;
                            }
                            if let Some(c) = cycles {
                                *profile
                                    .edge_cycles
                                    .entry((from_func.clone(), to_func.clone()))
                                    .or_insert(0) += c;
                            }
                            if profile.detailed_edges.len() < 10000 {
                                profile.detailed_edges.push(LbrEdge {
                                    from_func,
                                    from_addr: from,
                                    to_func,
                                    to_addr: to,
                                    mispredicted,
                                    cycles,
                                });
                            }
                            entry_off += 26;
                        }
                        break;
                    }
                    pos += 8;
                }
            }
            off += rec_size;
        }
        profile.finalize();
        Ok(profile)
    }

    /// 序列化为文本格式 (与 brstack 兼容)
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for edge in &self.detailed_edges {
            out.push_str(&format!(
                "{} 0x{:x} {} 0x{:x}",
                edge.from_func, edge.from_addr, edge.to_func, edge.to_addr
            ));
            if edge.mispredicted {
                out.push_str(" M");
            } else {
                out.push_str(" P");
            }
            if let Some(c) = edge.cycles {
                out.push_str(&format!(" {}", c));
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_brstack() {
        let p = LbrProfile::from_str(
            "main 0x1000 foo 0x2000 5\nfoo 0x2010 main 0x1020 7\nunknown 0x0 main 0x1000 3\n",
        )
        .unwrap();
        assert_eq!(p.hot_edge_count("main", "foo"), 1);
        assert_eq!(p.hot_edge_count("foo", "main"), 1);
        assert!(p.hot_entries.iter().any(|(f, _)| f == "main"));
    }

    #[test]
    fn parse_with_mispredict() {
        let p = LbrProfile::from_str(
            "main 0x1000 foo 0x2000 M 10\nmain 0x1000 foo 0x2000 P 8\n",
        )
        .unwrap();
        assert_eq!(p.hot_edge_count("main", "foo"), 2);
        assert_eq!(
            p.mispred_edges.get(&("main".into(), "foo".into())),
            Some(&1)
        );
        let rate = p.mispred_rate("main", "foo");
        assert!((rate - 0.5).abs() < 0.01);
    }

    #[test]
    fn parse_with_cycles() {
        let p = LbrProfile::from_str("main 0x1000 foo 0x2000 100\nmain 0x1000 foo 0x2000 200\n").unwrap();
        assert_eq!(
            p.edge_cycles.get(&("main".into(), "foo".into())),
            Some(&300)
        );
    }

    #[test]
    fn inbound_outbound_edges() {
        let p = LbrProfile::from_str(
            "a 0x0 c 0x0\nb 0x0 c 0x0\nb 0x0 c 0x0\nc 0x0 d 0x0\n",
        )
        .unwrap();
        let inbound = p.inbound_edges("c");
        assert_eq!(inbound.len(), 2);
        let outbound = p.outbound_edges("c");
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].1, 1);
    }

    #[test]
    fn merge_profiles() {
        let mut a = LbrProfile::from_str("main 0x0 foo 0x0 5\n").unwrap();
        let b = LbrProfile::from_str("main 0x0 foo 0x0 3\nbar 0x0 baz 0x0 1\n").unwrap();
        a.merge(&b);
        assert_eq!(a.hot_edge_count("main", "foo"), 2);
        assert_eq!(a.hot_edge_count("bar", "baz"), 1);
    }

    #[test]
    fn summary_stats() {
        let p = LbrProfile::from_str(
            "main 0x0 foo 0x0 M 10\nfoo 0x0 main 0x0 P 20\nunknown 0x0 main 0x0 5\n",
        )
        .unwrap();
        let s = p.summary();
        assert_eq!(s.total_edges, 3);
        assert_eq!(s.unique_edges, 3);
        assert_eq!(s.mispred_count, 1);
        assert_eq!(s.total_cycles, 35);
        assert!(s.hot_entry_count > 0);
    }

    #[test]
    fn hot_entries_sorted_by_count() {
        let p = LbrProfile::from_str(
            "unknown 0x0 a 0x0\nunknown 0x0 b 0x0\nunknown 0x0 b 0x0\nunknown 0x0 b 0x0\nunknown 0x0 b 0x0\nunknown 0x0 b 0x0\nunknown 0x0 c 0x0\nunknown 0x0 c 0x0\nunknown 0x0 c 0x0\n",
        )
        .unwrap();
        // b: 5, c: 3, a: 1
        assert_eq!(p.hot_entries[0].0, "b");
        assert_eq!(p.hot_entries[0].1, 5);
    }

    #[test]
    fn read_from_file() {
        let tmp = std::env::temp_dir().join("dcc-lbr-test-v1.txt");
        std::fs::write(
            &tmp,
            b"main 0x1000 foo 0x2000 5\nfoo 0x2010 main 0x1020 7\n",
        )
        .unwrap();
        let p = LbrProfile::read_perf_brstack(&tmp).unwrap();
        assert_eq!(p.hot_edge_count("main", "foo"), 1);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn empty_profile() {
        let p = LbrProfile::empty();
        let s = p.summary();
        assert_eq!(s.total_edges, 0);
        assert_eq!(s.unique_edges, 0);
    }

    /// perf.data binary LBR parsing with synthetic data.
    #[test]
    fn lbr_perf_data_binary_parsing() {
        // Build a minimal perf.data with one PERF_RECORD_SAMPLE
        let mut data: Vec<u8> = Vec::new();
        // Header: magic + header_size + attr_size + n_attrs
        data.extend_from_slice(b"PERFILE2");
        data.extend_from_slice(&104u64.to_le_bytes()); // header_size
        data.extend_from_slice(&64u64.to_le_bytes()); // attr_size
        data.extend_from_slice(&0u64.to_le_bytes()); // n_attrs
        // Pad header to 104 bytes
        data.resize(104, 0);

        // PERF_RECORD_SAMPLE record
        let rec_start = data.len();
        data.extend_from_slice(&9u32.to_le_bytes()); // type = PERF_RECORD_SAMPLE
        data.extend_from_slice(&0u16.to_le_bytes()); // misc
        // size placeholder (will fill later)
        let size_pos = data.len();
        data.extend_from_slice(&0u16.to_le_bytes()); // size at offset 6
        // branch_stack: nr=2, then 2 entries
        data.extend_from_slice(&2u64.to_le_bytes()); // nr = 2
        // Entry 1: from=0x1000, to=0x2000, flags=0, cycles=100
        data.extend_from_slice(&0x1000u64.to_le_bytes());
        data.extend_from_slice(&0x2000u64.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // flags
        data.extend_from_slice(&100u16.to_le_bytes()); // cycles
        // Entry 2: from=0x2000, to=0x3000, flags=1 (mispred), cycles=200
        data.extend_from_slice(&0x2000u64.to_le_bytes());
        data.extend_from_slice(&0x3000u64.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes()); // flags (mispred)
        data.extend_from_slice(&200u16.to_le_bytes()); // cycles
        // Fill in record size
        let rec_size = (data.len() - rec_start) as u16;
        data[size_pos..size_pos + 2].copy_from_slice(&rec_size.to_le_bytes());

        let p = LbrProfile::read_perf_data_binary(&data).unwrap();
        assert!(p.detailed_edges.len() >= 2);
        assert_eq!(p.detailed_edges[0].from_addr, 0x1000);
        assert_eq!(p.detailed_edges[0].to_addr, 0x2000);
        assert!(!p.detailed_edges[0].mispredicted);
        assert_eq!(p.detailed_edges[0].cycles, Some(100));
        assert!(p.detailed_edges[1].mispredicted);
    }

    /// read() auto-detects text vs binary format.
    #[test]
    fn lbr_read_auto_detects_format() {
        // Text file
        let tmp_txt = std::env::temp_dir().join("dcc-lbr-auto.txt");
        std::fs::write(&tmp_txt, b"main 0x1000 foo 0x2000 5\n").unwrap();
        let p1 = LbrProfile::read(&tmp_txt).unwrap();
        assert_eq!(p1.hot_edge_count("main", "foo"), 1);
        let _ = std::fs::remove_file(&tmp_txt);

        // Binary file (perf.data magic)
        let tmp_bin = std::env::temp_dir().join("dcc-lbr-auto.bin");
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"PERFILE2");
        data.resize(200, 0);
        std::fs::write(&tmp_bin, &data).unwrap();
        let p2 = LbrProfile::read(&tmp_bin).unwrap();
        // Empty but valid
        assert_eq!(p2.summary().total_edges, 0);
        let _ = std::fs::remove_file(&tmp_bin);
    }

    /// serialize() produces text output compatible with brstack.
    #[test]
    fn lbr_serialize_roundtrip() {
        let p = LbrProfile::from_str(
            "main 0x1000 foo 0x2000 M 100\nfoo 0x2000 bar 0x3000 P 200\n",
        )
        .unwrap();
        let text = p.serialize();
        assert!(text.contains("main 0x1000 foo 0x2000 M 100"));
        assert!(text.contains("foo 0x2000 bar 0x3000 P 200"));
        // Parse back
        let p2 = LbrProfile::from_str(&text).unwrap();
        assert_eq!(p2.hot_edge_count("main", "foo"), 1);
        assert_eq!(p2.hot_edge_count("foo", "bar"), 1);
    }

    /// perf.data rejects non-perf files.
    #[test]
    fn lbr_perf_data_rejects_bad_magic() {
        let result = LbrProfile::read_perf_data_binary(b"NOTPERF!");
        assert!(result.is_err());
    }
}
