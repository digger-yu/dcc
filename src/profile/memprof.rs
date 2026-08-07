// src/profile/memprof.rs - MemProf / Page Weight profiler
//
// MemProf 是 LLVM 18+ 的内存采样器, 与 PGO/AutoFDO 并列, 但关注
// 堆分配的生命周期与占用. 主要用途:
//   * `-fmemory-profile` : 在 malloc/free/operator new 路径上打点,
//     收集 (alloc_site, dealloc_site, lifetime, size) -> shadow
//   * `-fmemory-profile-use=file` : 读取 .memprofdata, 把
//     `cold_alloc` / `not_lifetime_extended` / `hinted` 等属性注入
//     到 IR (通常是 `__attribute__((malloc("cold")))`)
//
// dcc 简化文本格式:
//   # dcc memprof v1
//   alloc <frames...> = <count> <bytes>
//   lifetime <alloc_frames> -> <dealloc_frames> = <bytes>
//   dealloc <frames...> = <count>
//
// 已接入 driver → ProfileWeights → codegen (cold 函数下沉 .text.unlikely).

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

/// MemProf 配置
#[derive(Debug, Clone, Default)]
pub struct MemProfConfig {
    /// `-fmemory-profile` (instrument)
    pub instrument: bool,
    /// `-fmemory-profile-use=file`
    pub use_file: Option<String>,
    /// cold 分配阈值 (alloc count < 此值视为 cold)
    pub cold_threshold: u64,
}

impl MemProfConfig {
    pub fn new() -> Self {
        Self {
            cold_threshold: 5,
            ..Default::default()
        }
    }
}

/// 分配分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocClass {
    /// 高频分配 (count >= hot_threshold)
    Hot,
    /// 正常分配
    Normal,
    /// 低频分配 (count < cold_threshold)
    Cold,
    /// 短命分配 (alloc 到 dealloc 极快)
    Ephemeral,
}

impl AllocClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            AllocClass::Hot => "hot",
            AllocClass::Normal => "normal",
            AllocClass::Cold => "cold",
            AllocClass::Ephemeral => "ephemeral",
        }
    }
}

/// MemProf 数据
#[derive(Debug, Clone, Default)]
pub struct MemProfData {
    /// callstack -> 分配次数
    pub alloc_counts: HashMap<Vec<String>, u64>,
    /// callstack -> 累计字节
    pub alloc_bytes: HashMap<Vec<String>, u64>,
    /// (alloc_stack, dealloc_stack) -> 累计字节
    pub lifetime: HashMap<(Vec<String>, Vec<String>), u64>,
    /// dealloc callstack -> 释放次数
    pub dealloc_counts: HashMap<Vec<String>, u64>,
}

/// MemProf 概要统计
#[derive(Debug, Clone, Default)]
pub struct MemProfSummary {
    pub total_allocs: u64,
    pub total_bytes: u64,
    pub unique_alloc_sites: usize,
    pub unique_dealloc_sites: usize,
    pub lifetime_entries: usize,
    pub hot_sites: usize,
    pub cold_sites: usize,
    pub ephemeral_sites: usize,
}

impl MemProfData {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从文件路径解析
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut s = String::new();
        std::fs::File::open(path)?.read_to_string(&mut s)?;
        Self::from_str(&s)
    }

    /// 从字符串解析 dcc 简化文本格式:
    ///   `alloc <frames...> = <count> <bytes>`
    ///   `lifetime <alloc_frames...> -> <dealloc_frames...> = <bytes>`
    ///   `dealloc <frames...> = <count>`
    pub fn from_str(input: &str) -> io::Result<Self> {
        let mut d = Self::default();
        for line in input.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some(rest) = l.strip_prefix("alloc ") {
                if let Some((frames, tail)) = rest.split_once(" = ") {
                    let stack: Vec<String> = frames.split(" -> ").map(|s| s.to_string()).collect();
                    let mut it = tail.split_whitespace();
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let bytes: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    *d.alloc_counts.entry(stack.clone()).or_insert(0) += count;
                    *d.alloc_bytes.entry(stack).or_insert(0) += bytes;
                }
            } else if let Some(rest) = l.strip_prefix("lifetime ") {
                if let Some((pair, tail)) = rest.split_once(" = ") {
                    // 用 " => " 分隔 alloc_stack 和 dealloc_stack
                    // (内部帧用 " -> " 分隔)
                    if let Some((af, df)) = pair.split_once(" => ") {
                        let a: Vec<String> = af.split(" -> ").map(|s| s.to_string()).collect();
                        let de: Vec<String> = df.split(" -> ").map(|s| s.to_string()).collect();
                        let bytes: u64 = tail.trim().parse().unwrap_or(0);
                        *d.lifetime.entry((a, de)).or_insert(0) += bytes;
                    }
                }
            } else if let Some(rest) = l.strip_prefix("dealloc ") {
                if let Some((frames, tail)) = rest.split_once(" = ") {
                    let stack: Vec<String> = frames.split(" -> ").map(|s| s.to_string()).collect();
                    let count: u64 = tail.trim().parse().unwrap_or(0);
                    *d.dealloc_counts.entry(stack).or_insert(0) += count;
                }
            }
        }
        Ok(d)
    }

    /// 给定分配点 callstack, 返回 `is_cold` 启发 (alloc count < 阈值)
    pub fn is_cold(&self, stack: &[String], threshold: u64) -> bool {
        self.alloc_counts.get(stack).copied().unwrap_or(0) < threshold
    }

    /// 分类分配点
    pub fn classify(&self, stack: &[String], cold_threshold: u64, hot_threshold: u64) -> AllocClass {
        let count = self.alloc_counts.get(stack).copied().unwrap_or(0);
        // 检查是否 ephemeral (有极短 lifetime)
        let is_ephemeral = self.lifetime.iter().any(|((a, _), bytes)| {
            a == stack && *bytes < 64 // 小于 64 字节视为短命
        });
        if is_ephemeral && count < cold_threshold * 2 {
            return AllocClass::Ephemeral;
        }
        if count >= hot_threshold {
            AllocClass::Hot
        } else if count < cold_threshold {
            AllocClass::Cold
        } else {
            AllocClass::Normal
        }
    }

    /// 合并另一个 profile 到自身
    pub fn merge(&mut self, other: &MemProfData) {
        for (k, &v) in &other.alloc_counts {
            *self.alloc_counts.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.alloc_bytes {
            *self.alloc_bytes.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.lifetime {
            *self.lifetime.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.dealloc_counts {
            *self.dealloc_counts.entry(k.clone()).or_insert(0) += v;
        }
    }

    /// 概要统计
    pub fn summary(&self) -> MemProfSummary {
        let cold_threshold = 5;
        let hot_threshold = 100;
        let mut hot_sites = 0;
        let mut cold_sites = 0;
        let mut ephemeral_sites = 0;
        for stack in self.alloc_counts.keys() {
            match self.classify(stack, cold_threshold, hot_threshold) {
                AllocClass::Hot => hot_sites += 1,
                AllocClass::Cold => cold_sites += 1,
                AllocClass::Ephemeral => ephemeral_sites += 1,
                AllocClass::Normal => {}
            }
        }
        MemProfSummary {
            total_allocs: self.alloc_counts.values().copied().sum(),
            total_bytes: self.alloc_bytes.values().copied().sum(),
            unique_alloc_sites: self.alloc_counts.len(),
            unique_dealloc_sites: self.dealloc_counts.len(),
            lifetime_entries: self.lifetime.len(),
            hot_sites,
            cold_sites,
            ephemeral_sites,
        }
    }

    /// 获取所有分配点的 callstack 列表
    pub fn alloc_sites(&self) -> Vec<&Vec<String>> {
        self.alloc_counts.keys().collect()
    }

    /// 获取某分配点的字节数
    pub fn bytes_for_site(&self, stack: &[String]) -> u64 {
        self.alloc_bytes.get(stack).copied().unwrap_or(0)
    }

    /// 序列化为 dcc 文本格式
    pub fn to_string(&self) -> String {
        let mut out = String::from("# dcc memprof v1\n");
        for (stack, &count) in &self.alloc_counts {
            let frames = stack.join(" -> ");
            let bytes = self.alloc_bytes.get(stack).copied().unwrap_or(0);
            out.push_str(&format!("alloc {} = {} {}\n", frames, count, bytes));
        }
        for ((a, d), &bytes) in &self.lifetime {
            let af = a.join(" -> ");
            let df = d.join(" -> ");
            out.push_str(&format!("lifetime {} => {} = {}\n", af, df, bytes));
        }
        for (stack, &count) in &self.dealloc_counts {
            let frames = stack.join(" -> ");
            out.push_str(&format!("dealloc {} = {}\n", frames, count));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_memprofdata() {
        let d = MemProfData::from_str(
            "alloc main -> foo -> malloc = 100 4096\nlifetime main -> foo -> malloc => free = 4096\n",
        )
        .unwrap();
        assert_eq!(
            d.alloc_counts
                .get(&vec!["main".into(), "foo".into(), "malloc".into()]),
            Some(&100)
        );
        // 100 < 200 -> is_cold == true
        assert!(d.is_cold(&["main".into(), "foo".into(), "malloc".into()], 200));
        // 100 >= 50 -> is_cold == false
        assert!(!d.is_cold(&["main".into(), "foo".into(), "malloc".into()], 50));
        // 未知 callstack 视为 0 < 任何 threshold -> is_cold == true
        assert!(d.is_cold(&["main".into()], 200));
    }

    #[test]
    fn parse_dealloc() {
        let d = MemProfData::from_str("dealloc free = 50\ndealloc my_free = 30\n").unwrap();
        assert_eq!(d.dealloc_counts.get(&vec!["free".into()]), Some(&50));
        assert_eq!(d.dealloc_counts.get(&vec!["my_free".into()]), Some(&30));
    }

    #[test]
    fn parse_accumulates_duplicates() {
        let d = MemProfData::from_str(
            "alloc main = 10 100\nalloc main = 20 200\n",
        )
        .unwrap();
        assert_eq!(d.alloc_counts.get(&vec!["main".into()]), Some(&30));
        assert_eq!(d.alloc_bytes.get(&vec!["main".into()]), Some(&300));
    }

    #[test]
    fn classify_alloc_sites() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["hot_alloc".into()], 200);
        d.alloc_counts.insert(vec!["normal_alloc".into()], 50);
        d.alloc_counts.insert(vec!["cold_alloc".into()], 2);
        assert_eq!(d.classify(&["hot_alloc".into()], 5, 100), AllocClass::Hot);
        assert_eq!(d.classify(&["normal_alloc".into()], 5, 100), AllocClass::Normal);
        assert_eq!(d.classify(&["cold_alloc".into()], 5, 100), AllocClass::Cold);
    }

    #[test]
    fn classify_ephemeral() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["fast".into()], 8);
        d.lifetime
            .insert((vec!["fast".into()], vec!["free".into()]), 16);
        assert_eq!(d.classify(&["fast".into()], 5, 100), AllocClass::Ephemeral);
    }

    #[test]
    fn merge_profiles() {
        let mut a = MemProfData::default();
        a.alloc_counts.insert(vec!["f".into()], 10);
        let mut b = MemProfData::default();
        b.alloc_counts.insert(vec!["f".into()], 20);
        b.alloc_counts.insert(vec!["g".into()], 5);
        a.merge(&b);
        assert_eq!(a.alloc_counts.get(&vec!["f".into()]), Some(&30));
        assert_eq!(a.alloc_counts.get(&vec!["g".into()]), Some(&5));
    }

    #[test]
    fn summary_stats() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["hot".into()], 200);
        d.alloc_counts.insert(vec!["cold".into()], 1);
        d.alloc_bytes.insert(vec!["hot".into()], 8000);
        d.alloc_bytes.insert(vec!["cold".into()], 64);
        d.lifetime
            .insert((vec!["hot".into()], vec!["free".into()]), 4000);
        d.dealloc_counts.insert(vec!["free".into()], 100);
        let s = d.summary();
        assert_eq!(s.total_allocs, 201);
        assert_eq!(s.total_bytes, 8064);
        assert_eq!(s.unique_alloc_sites, 2);
        assert_eq!(s.unique_dealloc_sites, 1);
        assert_eq!(s.lifetime_entries, 1);
        assert_eq!(s.hot_sites, 1);
        assert_eq!(s.cold_sites, 1);
    }

    #[test]
    fn roundtrip_to_string() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["main".into(), "malloc".into()], 100);
        d.alloc_bytes.insert(vec!["main".into(), "malloc".into()], 4096);
        d.lifetime.insert(
            (vec!["main".into(), "malloc".into()], vec!["free".into()]),
            4096,
        );
        d.dealloc_counts.insert(vec!["free".into()], 50);
        let text = d.to_string();
        let d2 = MemProfData::from_str(&text).unwrap();
        assert_eq!(d2.alloc_counts, d.alloc_counts);
        assert_eq!(d2.alloc_bytes, d.alloc_bytes);
        assert_eq!(d2.lifetime, d.lifetime);
        assert_eq!(d2.dealloc_counts, d.dealloc_counts);
    }

    #[test]
    fn read_from_file() {
        let tmp = std::env::temp_dir().join("dcc-memprof-test-v1.txt");
        std::fs::write(
            &tmp,
            b"alloc main -> foo -> malloc = 100 4096\ndealloc free = 50\n",
        )
        .unwrap();
        let d = MemProfData::read(&tmp).unwrap();
        assert_eq!(
            d.alloc_counts
                .get(&vec!["main".into(), "foo".into(), "malloc".into()]),
            Some(&100)
        );
        assert_eq!(d.dealloc_counts.get(&vec!["free".into()]), Some(&50));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn alloc_sites_and_bytes() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["a".into()], 10);
        d.alloc_counts.insert(vec!["b".into()], 20);
        d.alloc_bytes.insert(vec!["a".into()], 100);
        assert_eq!(d.alloc_sites().len(), 2);
        assert_eq!(d.bytes_for_site(&["a".into()]), 100);
        assert_eq!(d.bytes_for_site(&["unknown".into()]), 0);
    }

    #[test]
    fn config_defaults() {
        let c = MemProfConfig::new();
        assert!(!c.instrument);
        assert!(c.use_file.is_none());
        assert_eq!(c.cold_threshold, 5);
    }
}
