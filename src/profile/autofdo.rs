// src/profile/autofdo.rs - AutoFDO 数据结构 (骨架)
//
// AutoFDO 用 perf sample (或 LLVM `xray`, 或 AutoFDO 工具) 收集 PC
// 样本, 把样本按 `(function, line, discriminator)` 折叠成 inline-stack
// 权重, 再喂给编译器做函数布局 / inliner / regalloc 提示.
//
// 文件格式 (`llvm-profdata` 输出的 "sample profile"):
//   * IndexedProfile / SampleProfile 协议缓冲
//   * 简化替代: `func_discriminator -> samples` 列表 (本模块采用)
//
// 当前 dcc 仅定义数据结构 + 极简 parser, 权重不下发到 codegen.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct AutoFdoProfile {
    /// function name -> total samples
    pub func_samples: HashMap<String, u64>,
    /// (func, line:col) -> samples
    pub line_samples: HashMap<(String, u32, u32), u64>,
}

impl AutoFdoProfile {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 解析简化格式:
    ///   `# header`
    ///   `func <name> <total>`
    ///   `line <name> <line> <col> <samples>`
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut s = String::new();
        std::fs::File::open(path)?.read_to_string(&mut s)?;
        let mut p = AutoFdoProfile::default();
        for line in s.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let mut it = l.split_whitespace();
            match it.next() {
                Some("func") => {
                    let name = it.next().unwrap_or("").to_string();
                    let total: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    p.func_samples.insert(name, total);
                }
                Some("line") => {
                    let name = it.next().unwrap_or("").to_string();
                    let line_n: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let col: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let total: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    p.line_samples.insert((name, line_n, col), total);
                }
                _ => {}
            }
        }
        Ok(p)
    }

    /// 把权重规整为 `func -> {hot, warm, cold}` 桶 (阈值见 LLVM AutoFDO).
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotness {
    Hot,
    Warm,
    Cold,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_profile() {
        let tmp = std::env::temp_dir().join("dcc-autofdo-test.txt");
        std::fs::write(
            &tmp,
            b"# dcc autofdo v0\nfunc main 100\nfunc foo 10\nline main 5 1 4\n",
        )
        .unwrap();
        let p = AutoFdoProfile::read(&tmp).unwrap();
        assert_eq!(p.func_samples.get("main"), Some(&100));
        assert_eq!(p.line_samples.get(&("main".into(), 5, 1)), Some(&4));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn bucketize_thresholds() {
        // total = 100 (hot) + 0 (warm) + 0 (cold) = 100
        // hot: 100/100 = 1.0  -> Hot (>0.10)
        // warm:   0/100 = 0.0  -> Cold (<=0.01)
        // cold:   0/100 = 0.0  -> Cold
        // 改用更分散的样本, 让每个桶都有非零计数:
        //   hot=80, warm=15, cold=5 => total=100
        //   hot ratio=0.80 -> Hot
        //   warm ratio=0.15 -> Warm
        //   cold ratio=0.05 -> Warm (>0.01)
        // 要 cold 必须 ratio <= 0.01, 改用 cold=1, total=200:
        //   cold ratio=0.005 -> Cold
        let mut p = AutoFdoProfile::default();
        p.func_samples.insert("hot".into(), 80);
        p.func_samples.insert("warm".into(), 15);
        p.func_samples.insert("cold".into(), 1);
        // 再补一个 neutral 使 cold 的 ratio 降到 0.005 (1/200)
        p.func_samples.insert("other".into(), 104);
        let b = p.bucketize();
        assert_eq!(b["hot"], Hotness::Hot);
        assert_eq!(b["warm"], Hotness::Warm);
        assert_eq!(b["cold"], Hotness::Cold);
    }
}
