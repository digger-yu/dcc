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
// 数据格式 (perf script -F brstack):
//   <from_func> <from_addr> <to_func> <to_addr> [<mispred>|<pred>] [<cycles>]
//   简化: `func1 0xADDR1 func2 0xADDR2 [cycles]`
//
// 已接入 driver → ProfileWeights → codegen (.text.hot / .text.unlikely).

use std::collections::HashMap;
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
}
