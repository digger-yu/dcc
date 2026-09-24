// src/profile/autofdo.rs - AutoFDO 采样分析
//
// AutoFDO 用 perf sample (或 LLVM `xray`, 或 AutoFDO 工具) 收集 PC
// 样本, 把样本按 `(function, line, discriminator)` 折叠成 inline-stack
// 权重, 再喂给编译器做函数布局 / inliner / regalloc 提示.
//
// dcc 文本格式:
//   # dcc autofdo v1
//   func <name> <total_samples>
//   line <name> <line> <col> <samples>
//   calls <caller> <callee> <count>
//   disc <name> <line> <discriminator> <samples>
//
// dcc 二进制格式 (extbinary 简化版):
//   magic    : "AFDO" 4 bytes
//   version  : u32 LE (2)
//   n_funcs  : u32 LE
//   func[]   : name_len(u32) name(u8*) samples(u64)
//   n_lines  : u32 LE
//   line[]   : name_len(u32) name(u8*) line(u32) col(u32) samples(u64)
//   n_calls  : u32 LE
//   calls[]  : caller_len(u32) caller(u8*) callee_len(u32) callee(u8*) count(u64)
//   n_discs  : u32 LE
//   disc[]   : name_len(u32) name(u8*) line(u32) disc(u32) samples(u64)
//
// 已接入 driver → ProfileWeights → codegen (.text.hot / .text.unlikely).

use std::collections::HashMap;
use std::fs;
use std::io;
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
    /// (func, line, discriminator) -> samples
    /// discriminator 区分同一行内不同基本块 (如分支目标)
    pub discriminated_samples: HashMap<(String, u32, u32), u64>,
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
    pub discriminated_count: usize,
    pub hot_funcs: usize,
    pub warm_funcs: usize,
    pub cold_funcs: usize,
}

const AFDO_BINARY_MAGIC: &[u8; 4] = b"AFDO";
const AFDO_BINARY_VERSION: u32 = 2;
const AFDO_TEXT_VERSION: u32 = 1;

impl AutoFdoProfile {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从文件路径解析 (自动检测文本/二进制格式)
    pub fn read(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        if data.len() >= 4 && &data[0..4] == AFDO_BINARY_MAGIC {
            return Self::read_binary(&data);
        }
        let s =
            String::from_utf8(data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Self::parse_text(&s)
    }

    /// 从字符串解析 dcc 简化文本格式:
    ///   `# header`
    ///   `func <name> <total>`
    ///   `line <name> <line> <col> <samples>`
    ///   `calls <caller> <callee> <count>`
    pub fn parse_text(input: &str) -> io::Result<Self> {
        let mut p = AutoFdoProfile::default();
        for line in input.lines() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            if let Some(rest) = l.strip_prefix("# dcc autofdo v") {
                let version = rest.trim().parse::<u32>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "bad autofdo text version")
                })?;
                if version != AFDO_TEXT_VERSION {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unsupported autofdo text version: {version}"),
                    ));
                }
                continue;
            }
            if l.starts_with('#') {
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
                },
                Some("line") => {
                    let name = it.next().unwrap_or("").to_string();
                    let line_n: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let col: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let total: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if !name.is_empty() {
                        *p.line_samples.entry((name, line_n, col)).or_insert(0) += total;
                    }
                },
                Some("calls") => {
                    let caller = it.next().unwrap_or("").to_string();
                    let callee = it.next().unwrap_or("").to_string();
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if !caller.is_empty() && !callee.is_empty() {
                        *p.calls.entry((caller, callee)).or_insert(0) += count;
                    }
                },
                Some("disc") => {
                    let name = it.next().unwrap_or("").to_string();
                    let line_n: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let disc: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let samples: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if !name.is_empty() {
                        *p.discriminated_samples
                            .entry((name, line_n, disc))
                            .or_insert(0) += samples;
                    }
                },
                _ => {},
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
        for (k, &v) in &other.discriminated_samples {
            *self.discriminated_samples.entry(k.clone()).or_insert(0) += v;
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
            discriminated_count: self.discriminated_samples.len(),
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
    pub fn serialize(&self) -> String {
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
        for ((name, line, disc), &count) in &self.discriminated_samples {
            out.push_str(&format!("disc {} {} {} {}\n", name, line, disc, count));
        }
        out
    }

    /// 序列化为 dcc 二进制格式
    pub fn write_binary(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(AFDO_BINARY_MAGIC);
        buf.extend_from_slice(&AFDO_BINARY_VERSION.to_le_bytes());
        // functions
        buf.extend_from_slice(&(self.func_samples.len() as u32).to_le_bytes());
        for (name, &samples) in &self.func_samples {
            write_binary_string(&mut buf, name);
            buf.extend_from_slice(&samples.to_le_bytes());
        }
        // line samples
        buf.extend_from_slice(&(self.line_samples.len() as u32).to_le_bytes());
        for ((name, line, col), &samples) in &self.line_samples {
            write_binary_string(&mut buf, name);
            buf.extend_from_slice(&line.to_le_bytes());
            buf.extend_from_slice(&col.to_le_bytes());
            buf.extend_from_slice(&samples.to_le_bytes());
        }
        // calls
        buf.extend_from_slice(&(self.calls.len() as u32).to_le_bytes());
        for ((caller, callee), &count) in &self.calls {
            write_binary_string(&mut buf, caller);
            write_binary_string(&mut buf, callee);
            buf.extend_from_slice(&count.to_le_bytes());
        }
        // discriminated samples
        buf.extend_from_slice(&(self.discriminated_samples.len() as u32).to_le_bytes());
        for ((name, line, disc), &samples) in &self.discriminated_samples {
            write_binary_string(&mut buf, name);
            buf.extend_from_slice(&line.to_le_bytes());
            buf.extend_from_slice(&disc.to_le_bytes());
            buf.extend_from_slice(&samples.to_le_bytes());
        }

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        fs::write(out, &buf)?;
        Ok(buf.len())
    }

    /// 从 dcc 二进制格式解析
    pub fn read_binary(data: &[u8]) -> io::Result<Self> {
        if data.len() < 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "autofdo binary too short"));
        }
        if &data[0..4] != AFDO_BINARY_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad autofdo magic"));
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if version != AFDO_BINARY_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported autofdo version: {version}"),
            ));
        }
        let mut off = 8;
        let mut p = AutoFdoProfile::default();
        // functions
        let n_funcs = read_u32(data, &mut off)? as usize;
        for _ in 0..n_funcs {
            let name = read_binary_string(data, &mut off)?;
            let samples = read_u64(data, &mut off)?;
            *p.func_samples.entry(name).or_insert(0) += samples;
        }
        // line samples
        let n_lines = read_u32(data, &mut off)? as usize;
        for _ in 0..n_lines {
            let name = read_binary_string(data, &mut off)?;
            let line = read_u32(data, &mut off)?;
            let col = read_u32(data, &mut off)?;
            let samples = read_u64(data, &mut off)?;
            *p.line_samples.entry((name, line, col)).or_insert(0) += samples;
        }
        // calls
        let n_calls = read_u32(data, &mut off)? as usize;
        for _ in 0..n_calls {
            let caller = read_binary_string(data, &mut off)?;
            let callee = read_binary_string(data, &mut off)?;
            let count = read_u64(data, &mut off)?;
            *p.calls.entry((caller, callee)).or_insert(0) += count;
        }
        // discriminated samples
        let n_discs = read_u32(data, &mut off)? as usize;
        for _ in 0..n_discs {
            let name = read_binary_string(data, &mut off)?;
            let line = read_u32(data, &mut off)?;
            let disc = read_u32(data, &mut off)?;
            let samples = read_u64(data, &mut off)?;
            *p.discriminated_samples
                .entry((name, line, disc))
                .or_insert(0) += samples;
        }
        Ok(p)
    }

    /// 获取指定 (func, line) 的所有 discriminator 样本
    pub fn discriminators_for(&self, func: &str, line: u32) -> Vec<(u32, u64)> {
        self.discriminated_samples
            .iter()
            .filter(|((f, l, _), _)| f == func && *l == line)
            .map(|((_, _, d), &s)| (*d, s))
            .collect()
    }
}

fn write_binary_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn read_binary_string(data: &[u8], off: &mut usize) -> io::Result<String> {
    let len = read_u32(data, off)? as usize;
    if *off + len > data.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated binary string"));
    }
    let s = std::str::from_utf8(&data[*off..*off + len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_string();
    *off += len;
    Ok(s)
}

fn read_u32(data: &[u8], off: &mut usize) -> io::Result<u32> {
    if *off + 4 > data.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated u32"));
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn read_u64(data: &[u8], off: &mut usize) -> io::Result<u64> {
    if *off + 8 > data.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated u64"));
    }
    let v = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_profile() {
        let p = AutoFdoProfile::parse_text(
            "# dcc autofdo v1\nfunc main 100\nfunc foo 10\nline main 5 1 4\n",
        )
        .unwrap();
        assert_eq!(p.func_samples.get("main"), Some(&100));
        assert_eq!(p.func_samples.get("foo"), Some(&10));
        assert_eq!(p.line_samples.get(&("main".into(), 5, 1)), Some(&4));
    }

    #[test]
    fn parse_rejects_unsupported_text_version() {
        let err = AutoFdoProfile::parse_text("# dcc autofdo v0\nfunc main 100\n").unwrap_err();
        assert!(err.to_string().contains("unsupported autofdo text version"), "{err}");
    }

    #[test]
    fn parse_calls() {
        let p = AutoFdoProfile::parse_text("func main 100\nfunc helper 50\ncalls main helper 30\n")
            .unwrap();
        assert_eq!(p.calls.get(&("main".into(), "helper".into())), Some(&30));
        let callers = p.callers_of("helper");
        assert_eq!(callers.len(), 1);
        assert_eq!(*callers[0].1, 30);
    }

    #[test]
    fn parse_accumulates_duplicates() {
        let p = AutoFdoProfile::parse_text("func main 50\nfunc main 50\n").unwrap();
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
        let text = p.serialize();
        let p2 = AutoFdoProfile::parse_text(&text).unwrap();
        assert_eq!(p2.func_samples, p.func_samples);
        assert_eq!(p2.line_samples, p.line_samples);
        assert_eq!(p2.calls, p.calls);
    }

    #[test]
    fn read_from_file() {
        let tmp = std::env::temp_dir().join("dcc-autofdo-test-v1.txt");
        std::fs::write(&tmp, b"# dcc autofdo v1\nfunc main 100\nfunc foo 10\ncalls main foo 5\n")
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

    /// Binary format roundtrip: write_binary then read_binary.
    #[test]
    fn autofdo_binary_roundtrip() {
        let mut p = AutoFdoProfile::default();
        p.func_samples.insert("main".into(), 100);
        p.func_samples.insert("foo".into(), 20);
        p.line_samples.insert(("main".into(), 5, 1), 40);
        p.calls.insert(("main".into(), "foo".into()), 15);
        p.discriminated_samples.insert(("main".into(), 10, 0), 30);
        p.discriminated_samples.insert(("main".into(), 10, 1), 20);

        let tmp = std::env::temp_dir().join("dcc-autofdo-bin.afdo");
        let _ = std::fs::remove_file(&tmp);
        let n = p.write_binary(&tmp).unwrap();
        assert!(n > 8);

        let p2 = AutoFdoProfile::read(&tmp).unwrap();
        assert_eq!(p2.func_samples, p.func_samples);
        assert_eq!(p2.line_samples, p.line_samples);
        assert_eq!(p2.calls, p.calls);
        assert_eq!(p2.discriminated_samples, p.discriminated_samples);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Text format discriminator parsing.
    #[test]
    fn autofdo_parse_discriminator_records() {
        let p = AutoFdoProfile::parse_text(
            "func main 100\n\
             disc main 10 0 30\n\
             disc main 10 1 20\n\
             disc main 15 0 5\n",
        )
        .unwrap();
        assert_eq!(p.discriminated_samples.len(), 3);
        assert_eq!(p.discriminated_samples.get(&("main".into(), 10, 0)), Some(&30));
        assert_eq!(p.discriminated_samples.get(&("main".into(), 10, 1)), Some(&20));
    }

    /// discriminators_for query returns all discriminators for a line.
    #[test]
    fn autofdo_discriminators_for_line() {
        let mut p = AutoFdoProfile::default();
        p.discriminated_samples.insert(("main".into(), 10, 0), 30);
        p.discriminated_samples.insert(("main".into(), 10, 1), 20);
        p.discriminated_samples.insert(("main".into(), 15, 0), 5);

        let discs = p.discriminators_for("main", 10);
        assert_eq!(discs.len(), 2);
        let total: u64 = discs.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 50);
    }

    /// Binary format rejects bad magic.
    #[test]
    fn autofdo_binary_rejects_bad_magic() {
        let result = AutoFdoProfile::read_binary(b"XXXX\x02\x00\x00\x00");
        assert!(result.is_err());
    }

    #[test]
    fn autofdo_binary_rejects_unsupported_version() {
        let mut data = Vec::new();
        data.extend_from_slice(AFDO_BINARY_MAGIC);
        data.extend_from_slice(&(AFDO_BINARY_VERSION - 1).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        let err = AutoFdoProfile::read_binary(&data).unwrap_err();
        assert!(err.to_string().contains("unsupported autofdo version"));
    }

    /// Merge includes discriminated samples.
    #[test]
    fn autofdo_merge_with_discriminators() {
        let mut a = AutoFdoProfile::default();
        a.discriminated_samples.insert(("f".into(), 5, 0), 10);
        let mut b = AutoFdoProfile::default();
        b.discriminated_samples.insert(("f".into(), 5, 0), 20);
        b.discriminated_samples.insert(("f".into(), 5, 1), 15);
        a.merge(&b);
        assert_eq!(a.discriminated_samples.get(&("f".into(), 5, 0)), Some(&30));
        assert_eq!(a.discriminated_samples.get(&("f".into(), 5, 1)), Some(&15));
    }

    /// Text roundtrip includes discriminated samples.
    #[test]
    fn autofdo_text_roundtrip_with_discriminators() {
        let mut p = AutoFdoProfile::default();
        p.func_samples.insert("main".into(), 100);
        p.discriminated_samples.insert(("main".into(), 10, 0), 30);
        p.discriminated_samples.insert(("main".into(), 10, 1), 20);
        let text = p.serialize();
        let p2 = AutoFdoProfile::parse_text(&text).unwrap();
        assert_eq!(p2.discriminated_samples, p.discriminated_samples);
    }
}
