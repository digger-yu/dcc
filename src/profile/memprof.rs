// src/profile/memprof.rs - MemProf / Page Weight profiler (骨架)
//
// MemProf 是 LLVM 18+ 的内存采样器, 与 PGO/AutoFDO 并列, 但关注
// 堆分配的生命周期与占用. 主要用途:
//   * `-fmemory-profile` : 在 malloc/free/operator new 路径上打点,
//     收集 (alloc_site, dealloc_site, lifetime, size) -> shadow
//   * `-fmemory-profile-use=file` : 读取 .memprofdata, 把
//     `cold_alloc` / `not_lifetime_extended` / `hinted` 等属性注入
//     到 IR (通常是 `__attribute__((malloc("cold")))`)
//
// 当前 dcc 仅定义: 配置 + 数据结构 + 简化读取器.
// 不在 libc malloc 上插桩 (那需要弱符号覆盖, 当前没有 runtime 库).

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct MemProfConfig {
    /// `-fmemory-profile` (instrument)
    pub instrument: bool,
    /// `-fmemory-profile-use=file`
    pub use_file: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MemProfData {
    /// callstack -> 分配次数
    pub alloc_counts: HashMap<Vec<String>, u64>,
    /// callstack -> 累计字节
    pub alloc_bytes: HashMap<Vec<String>, u64>,
    /// (alloc_stack, dealloc_stack) -> 累计字节
    pub lifetime: HashMap<(Vec<String>, Vec<String>), u64>,
}

impl MemProfData {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 解析 dcc 简化版 memprofdata 格式:
    ///   `alloc <frames...> = <count> <bytes>`
    ///   `lifetime <alloc_frames...> -> <dealloc_frames...> = <bytes>`
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut s = String::new();
        std::fs::File::open(path)?.read_to_string(&mut s)?;
        let mut d = Self::default();
        for line in s.lines() {
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
                    d.alloc_counts.insert(stack.clone(), count);
                    d.alloc_bytes.insert(stack, bytes);
                }
            } else if let Some(rest) = l.strip_prefix("lifetime ") {
                if let Some((pair, tail)) = rest.split_once(" = ") {
                    if let Some((af, df)) = pair.split_once(" -> ") {
                        let a: Vec<String> = af.split(" -> ").map(|s| s.to_string()).collect();
                        let de: Vec<String> = df.split(" -> ").map(|s| s.to_string()).collect();
                        let bytes: u64 = tail.trim().parse().unwrap_or(0);
                        d.lifetime.insert((a, de), bytes);
                    }
                }
            }
        }
        Ok(d)
    }

    /// 给定分配点 callstack, 返回 `is_cold` 启发 (alloc count < 阈值)
    pub fn is_cold(&self, stack: &[String], threshold: u64) -> bool {
        self.alloc_counts.get(stack).copied().unwrap_or(0) < threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_minimal_memprofdata() {
        let tmp = std::env::temp_dir().join("dcc-memprof-test.txt");
        std::fs::write(
            &tmp,
            b"alloc main -> foo -> malloc = 100 4096\nlifetime main -> foo -> malloc -> free = 4096\n",
        ).unwrap();
        let d = MemProfData::read(&tmp).unwrap();
        assert_eq!(
            d.alloc_counts
                .get(&vec!["main".into(), "foo".into(), "malloc".into()]),
            Some(&100)
        );
        // 100 < 200 -> is_cold == true (callstack 100, threshold 200)
        assert!(d.is_cold(&["main".into(), "foo".into(), "malloc".into()], 200));
        // 100 >= 50 -> is_cold == false (callstack 100, threshold 50)
        assert!(!d.is_cold(&["main".into(), "foo".into(), "malloc".into()], 50));
        // 未知 callstack 视为 0 < 任何 threshold -> is_cold == true
        assert!(d.is_cold(&["main".into()], 200));
        let _ = std::fs::remove_file(&tmp);
    }
}
