// src/profile/autofdo.rs - AutoFDO 采样分析
//
// AutoFDO 用 perf sample (或 LLVM `xray`, 或 AutoFDO 工具) 收集 PC
// 样本, 把样本按 `(function, line, discriminator)` 折叠成 inline-stack
// 权重, 再喂给编译器做函数布局 / inliner / regalloc 提示.
//
// dcc 简化文本格式:
//   # dcc autofdo v1
//   func <name> <total_samples>
//   line <name> <line> <col> <samples>
//   calls <caller> <callee> <count>
//
// 已接入 driver → ProfileWeights → codegen (.text.hot / .text.unlikely).

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

/// AutoFDO 采样概要
#[derive(Debug, Clone, Default)]
pub struct AutoFdoProfile {
    /// function name -> total samples
    pub func_samples: HashMap<String, u64>,
    /// (func, line, col) -> samples
    pub line_samples: HashMap<(String, u32, u32), u64>,
    /// (caller, callee) -> call count (inline 调用链)
    pub calls: HashMap<(String, String), u64>,
}

/// 函数热度分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotness {
    Hot,
    Warm,
    Cold,
}

impl Hotness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Hotness::Hot => "hot",
            Hotness::Warm => "warm",
            Hotness::Cold => "cold",
        }
    }
}

/// 概要统计
#[derive(Debug, Clone, Default)]
pub struct AutoFdoSummary {
    pub total_samples: u64,
    pub func_count: usize,
    pub line_count: usize,
    pub call_count: usize,
    pub hot_funcs: usize,
    pub warm_funcs: usize,
    pub cold_funcs: usize,
}

impl AutoFdoProfile {
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
    ///   `# header`
    ///   `func <name> <total>`
    ///   `line <name> <line> <col> <samples>`
    ///   `calls <caller> <callee> <count>`
    pub fn from_str(input: &str) -> io::Result<Self> {
        let mut p = AutoFdoProfile::default();
        for line in input.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let mut it = l.split_whitespace();
            match it.next() {
                Some("func") => {
                    let name = it.next().unwrap_or("").to_string();
                    let total: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if !name.is_empty() {
                        *p.func_samples.entry(name).or_insert(0) += total;
                    }
                }
                Some("line") => {
                    let name = it.next().unwrap_or("").to_string();
                    let line_n: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let col: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let total: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if !name.is_empty() {
                        *p.line_samples.entry((name, line_n, col)).or_insert(0) += total;
                    }
                }
                Some("calls") => {
                    let caller = it.next().unwrap_or("").to_string();
                    let callee = it.next().unwrap_or("").to_string();
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if !caller.is_empty() && !callee.is_empty() {
                        *p.calls.entry((caller, callee)).or_insert(0) += count;
                    }
                }
                _ => {}
            }
        }
        Ok(p)
    }

    /// 把权重规整为 `func -> {hot, warm, cold}` 桶.
    /// 阈值: >10% Hot, >1% Warm, <=1% Cold (参照 LLVM AutoFDO 默认).
    pub fn bucketize(&self) -> HashMap<String, Hotness> {
        let total: u64 = self.func_samples.values().copied().sum::<u64>().max(1);
        let mut out = HashMap::new();
        for (name, &c) in &self.func_samples {
            let ratio = c as f64 / total as f64;
            out.insert(
                name.clone(),
                if ratio > 0.10 {
                    Hotness::Hot
                } else if ratio > 0.01 {
                    Hotness::Warm
                } else {
                    Hotness::Cold
                },
            );
        }
        out
    }

    /// 合并另一个 profile 到自身 (样本累加)
    pub fn merge(&mut self, other: &AutoFdoProfile) {
        for (k, &v) in &other.func_samples {
            *self.func_samples.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.line_samples {
            *self.line_samples.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.calls {
            *self.calls.entry(k.clone()).or_insert(0) += v;
        }
    }

    /// 概要统计
    pub fn summary(&self) -> AutoFdoSummary {
        let buckets = self.bucketize();
        AutoFdoSummary {
            total_samples: self.func_samples.values().copied().sum(),
            func_count: self.func_samples.len(),
            line_count: self.line_samples.len(),
            call_count: self.calls.len(),
            hot_funcs: buckets.values().filter(|h| **h == Hotness::Hot).count(),
            warm_funcs: buckets.values().filter(|h| **h == Hotness::Warm).count(),
            cold_funcs: buckets.values().filter(|h| **h == Hotness::Cold).count(),
        }
    }

    /// 获取函数的 inline 调用者列表
    pub fn callers_of(&self, callee: &str) -> Vec<(&String, &u64)> {
        self.calls
            .iter()
            .filter(|((_, c), _)| c == callee)
            .map(|((caller, _), count)| (caller, count))
            .collect()
    }

    /// 序列化为 dcc 文本格式
    pub fn to_string(&self) -> String {
        let mut out = String::from("# dcc autofdo v1\n");
        for (name, &count) in &self.func_samples {
            out.push_str(&format!("func {} {}\n", name, count));
        }
        for ((name, line, col), &count) in &self.line_samples {
            out.push_str(&format!("line {} {} {} {}\n", name, line, col, count));
        }
        for ((caller, callee), &count) in &self.calls {
            out.push_str(&format!("calls {} {} {}\n", caller, callee, count));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_profile() {
        let p = AutoFdoProfile::from_str(
            "# dcc autofdo v0\nfunc main 100\nfunc foo 10\nline main 5 1 4\n",
        )
        .unwrap();
        assert_eq!(p.func_samples.get("main"), Some(&100));
        assert_eq!(p.func_samples.get("foo"), Some(&10));
        assert_eq!(p.line_samples.get(&("main".into(), 5, 1)), Some(&4));
    }

    #[test]
    fn parse_calls() {
        let p = AutoFdoProfile::from_str(
            "func main 100\nfunc helper 50\ncalls main helper 30\n",
        )
        .unwrap();
        assert_eq!(p.calls.get(&("main".into(), "helper".into())), Some(&30));
        let callers = p.callers_of("helper");
        assert_eq!(callers.len(), 1);
        assert_eq!(*callers[0].1, 30);
    }

    #[test]
    fn parse_accumulates_duplicates() {
        let p = AutoFdoProfile::from_str("func main 50\nfunc main 50\n").unwrap();
        assert_eq!(p.func_samples.get("main"), Some(&100));
    }

    #[test]
    fn bucketize_thresholds() {
        let mut p = AutoFdoProfile::default();
        p.func_samples.insert("hot".into(), 80);
        p.func_samples.insert("warm".into(), 15);
        p.func_samples.insert("cold".into(), 1);
        p.func_samples.insert("other".into(), 104);
        let b = p.bucketize();
        assert_eq!(b["hot"], Hotness::Hot);
        assert_eq!(b["warm"], Hotness::Warm);
        assert_eq!(b["cold"], Hotness::Cold);
    }

    #[test]
    fn merge_profiles() {
        let mut a = AutoFdoProfile::default();
        a.func_samples.insert("f".into(), 10);
        let mut b = AutoFdoProfile::default();
        b.func_samples.insert("f".into(), 20);
        b.func_samples.insert("g".into(), 5);
        a.merge(&b);
        assert_eq!(a.func_samples.get("f"), Some(&30));
        assert_eq!(a.func_samples.get("g"), Some(&5));
    }

    #[test]
    fn summary_stats() {
        let mut p = AutoFdoProfile::default();
        p.func_samples.insert("hot".into(), 80);
        p.func_samples.insert("warm".into(), 15);
        p.func_samples.insert("cold".into(), 1);
        p.func_samples.insert("other".into(), 104);
        p.line_samples.insert(("hot".into(), 1, 0), 40);
        p.calls.insert(("hot".into(), "warm".into()), 10);
        let s = p.summary();
        assert_eq!(s.total_samples, 200);
        assert_eq!(s.func_count, 4);
        assert_eq!(s.line_count, 1);
        assert_eq!(s.call_count, 1);
        assert_eq!(s.hot_funcs, 2); // hot(80) + other(104) both >10%
        assert!(s.warm_funcs > 0);
        assert!(s.cold_funcs > 0);
    }

    #[test]
    fn roundtrip_to_string() {
        let mut p = AutoFdoProfile::default();
        p.func_samples.insert("main".into(), 100);
        p.line_samples.insert(("main".into(), 5, 1), 4);
        p.calls.insert(("main".into(), "foo".into()), 10);
        let text = p.to_string();
        let p2 = AutoFdoProfile::from_str(&text).unwrap();
        assert_eq!(p2.func_samples, p.func_samples);
        assert_eq!(p2.line_samples, p.line_samples);
        assert_eq!(p2.calls, p.calls);
    }

    #[test]
    fn read_from_file() {
        let tmp = std::env::temp_dir().join("dcc-autofdo-test-v1.txt");
        std::fs::write(
            &tmp,
            b"# dcc autofdo v1\nfunc main 100\nfunc foo 10\ncalls main foo 5\n",
        )
        .unwrap();
        let p = AutoFdoProfile::read(&tmp).unwrap();
        assert_eq!(p.func_samples.get("main"), Some(&100));
        assert_eq!(p.calls.get(&("main".into(), "foo".into())), Some(&5));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn empty_profile_summary() {
        let p = AutoFdoProfile::empty();
        let s = p.summary();
        assert_eq!(s.total_samples, 0);
        assert_eq!(s.func_count, 0);
    }
}
