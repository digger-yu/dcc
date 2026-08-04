// src/profile/lbr.rs - LBR (Last Branch Record) profiling (骨架)
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
// 数据结构 (简化, 与 Linux `perf script -F brstack` 对齐):
//   * 边样本: (from_func, from_pc, to_func, to_pc, cycles) -> count

use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct LbrProfile {
    /// (from_func, to_func) -> count
    pub edges: HashMap<(String, String), u64>,
    /// 入口候选热点 (按 from→to 入度)
    pub hot_entries: Vec<(String, u64)>,
}

impl LbrProfile {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 解析 `perf script -F brstack` 输出 (简化, 一次读一行).
    /// 输入格式: `func1 0xADDR1 func2 0xADDR2 cycles\n`
    pub fn read_perf_brstack(path: &Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        let mut p = LbrProfile::default();
        for line in std::io::BufReader::new(f).lines() {
            let l = line?;
            let mut it = l.split_whitespace();
            let from_func = it.next().unwrap_or("").to_string();
            let _from_addr = it.next();
            let to_func = it.next().unwrap_or("").to_string();
            let _to_addr = it.next();
            let _cycles = it.next();
            if from_func.is_empty() || to_func.is_empty() {
                continue;
            }
            *p.edges.entry((from_func, to_func)).or_insert(0) += 1;
        }
        // 入口热点: `unknown -> func` 计数
        let mut entries: HashMap<String, u64> = HashMap::new();
        for ((f, t), c) in &p.edges {
            if f == "unknown" || f.is_empty() {
                *entries.entry(t.clone()).or_insert(0) += *c;
            }
        }
        let mut entries_v: Vec<_> = entries.into_iter().collect();
        entries_v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        p.hot_entries = entries_v;
        Ok(p)
    }

    pub fn hot_edge_count(&self, from: &str, to: &str) -> u64 {
        self.edges
            .get(&(from.into(), to.into()))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_brstack() {
        let tmp = std::env::temp_dir().join("dcc-lbr-test.txt");
        std::fs::write(
            &tmp,
            b"main 0x1000 foo 0x2000 5\nfoo 0x2010 main 0x1020 7\nunknown 0x0 main 0x1000 3\n",
        )
        .unwrap();
        let p = LbrProfile::read_perf_brstack(&tmp).unwrap();
        assert_eq!(p.hot_edge_count("main", "foo"), 1);
        assert!(p.hot_entries.iter().any(|(f, _)| f == "main"));
        let _ = std::fs::remove_file(&tmp);
    }
}
