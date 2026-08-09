// src/profile/perf_event.rs - Perf 事件 / TopdownL1 metrics
//
// TopdownL1 metrics: 把 CPU pipeline stalls 分解为四个一阶分类
//   Retiring          : 真正做有用工作的微操作
//   Bad Speculation   : 错误预测 / 取消的工作
//   Frontend Bound    : 前端取指/解码受限
//   Backend Bound     : 后端执行资源受限
//   (以及它们的二阶展开: Branch Mispredicts, Cache Misses, ...)
//
// 用途: 编译时输出 `__topdown_l1_*` 计数器 (在 PGO-instr 路径下),
// 或在运行时 `perf stat -M topdownl1` 输出, 给 dcc 或被编译程序做
// "为什么这么慢" 的诊断.
//
// dcc 简化文本格式 (perf stat 输出):
//   # dcc perf_event v1
//   event <name> <count> [<unit>]
//   topdown <slot> <count>
//   topdown_l2 <slot> <count>
//
// 已接入 CLI (-ftopdownmetrics) + driver.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

/// TopdownL1 四槽分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopdownL1 {
    Retiring,
    BadSpeculation,
    FrontendBound,
    BackendBound,
}

impl TopdownL1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            TopdownL1::Retiring => "retiring",
            TopdownL1::BadSpeculation => "bad_speculation",
            TopdownL1::FrontendBound => "frontend_bound",
            TopdownL1::BackendBound => "backend_bound",
        }
    }
    pub fn all() -> [TopdownL1; 4] {
        [
            Self::Retiring,
            Self::BadSpeculation,
            Self::FrontendBound,
            Self::BackendBound,
        ]
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "retiring" => Some(TopdownL1::Retiring),
            "bad_speculation" | "badspec" => Some(TopdownL1::BadSpeculation),
            "frontend_bound" | "frontend" => Some(TopdownL1::FrontendBound),
            "backend_bound" | "backend" => Some(TopdownL1::BackendBound),
            _ => None,
        }
    }
}

/// TopdownL2 二阶展开 (每个 L1 槽的子分类)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum TopdownL2 {
    // Retiring 子分类
    retiring_base,
    retiring_microcode,
    // Bad Speculation 子分类
    branch_mispredict,
    machine_clears,
    // Frontend Bound 子分类
    fetch_latency,
    fetch_bandwidth,
    // Backend Bound 子分类
    memory_bound,
    core_bound,
}

impl TopdownL2 {
    pub fn as_str(&self) -> &'static str {
        match self {
            TopdownL2::retiring_base => "retiring_base",
            TopdownL2::retiring_microcode => "retiring_microcode",
            TopdownL2::branch_mispredict => "branch_mispredict",
            TopdownL2::machine_clears => "machine_clears",
            TopdownL2::fetch_latency => "fetch_latency",
            TopdownL2::fetch_bandwidth => "fetch_bandwidth",
            TopdownL2::memory_bound => "memory_bound",
            TopdownL2::core_bound => "core_bound",
        }
    }
    pub fn parent(&self) -> TopdownL1 {
        match self {
            TopdownL2::retiring_base | TopdownL2::retiring_microcode => TopdownL1::Retiring,
            TopdownL2::branch_mispredict | TopdownL2::machine_clears => TopdownL1::BadSpeculation,
            TopdownL2::fetch_latency | TopdownL2::fetch_bandwidth => TopdownL1::FrontendBound,
            TopdownL2::memory_bound | TopdownL2::core_bound => TopdownL1::BackendBound,
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "retiring_base" => Some(TopdownL2::retiring_base),
            "retiring_microcode" => Some(TopdownL2::retiring_microcode),
            "branch_mispredict" => Some(TopdownL2::branch_mispredict),
            "machine_clears" => Some(TopdownL2::machine_clears),
            "fetch_latency" => Some(TopdownL2::fetch_latency),
            "fetch_bandwidth" => Some(TopdownL2::fetch_bandwidth),
            "memory_bound" => Some(TopdownL2::memory_bound),
            "core_bound" => Some(TopdownL2::core_bound),
            _ => None,
        }
    }
}

/// perf_event 记录的一个抽象 (仿 `perf record -e ...`)
#[derive(Debug, Clone)]
pub struct PerfEventSample {
    pub event: String,
    pub count: u64,
    pub unit: String,
}

/// perf 记录
#[derive(Debug, Clone, Default)]
pub struct PerfRecord {
    /// event -> count
    pub events: HashMap<String, u64>,
    /// 顶层 TopdownL1 metrics
    pub topdown_l1: HashMap<TopdownL1, u64>,
    /// 二阶 TopdownL2 metrics
    pub topdown_l2: HashMap<TopdownL2, u64>,
}

/// 概要统计
#[derive(Debug, Clone, Default)]
pub struct PerfRecordSummary {
    pub event_count: usize,
    pub total_events: u64,
    pub l1_slots_filled: usize,
    pub l2_slots_filled: usize,
}

impl PerfRecord {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从文件路径解析
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut s = String::new();
        std::fs::File::open(path)?.read_to_string(&mut s)?;
        Self::from_str(&s)
    }

    /// 从字符串解析 dcc 文本格式:
    ///   `event <name> <count> [<unit>]`
    ///   `topdown <slot> <count>`
    ///   `topdown_l2 <slot> <count>`
    pub fn from_str(input: &str) -> io::Result<Self> {
        let mut r = PerfRecord::default();
        for line in input.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let mut it = l.split_whitespace();
            match it.next() {
                Some("event") => {
                    let name = it.next().unwrap_or("").to_string();
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let _unit = it.next().unwrap_or("count").to_string();
                    if !name.is_empty() {
                        *r.events.entry(name).or_insert(0) += count;
                    }
                }
                Some("topdown") => {
                    let slot_str = it.next().unwrap_or("");
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if let Some(slot) = TopdownL1::from_str(slot_str) {
                        *r.topdown_l1.entry(slot).or_insert(0) += count;
                    }
                }
                Some("topdown_l2") => {
                    let slot_str = it.next().unwrap_or("");
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if let Some(slot) = TopdownL2::from_str(slot_str) {
                        *r.topdown_l2.entry(slot).or_insert(0) += count;
                    }
                }
                _ => {}
            }
        }
        Ok(r)
    }

    pub fn add(&mut self, ev: PerfEventSample) {
        *self.events.entry(ev.event.clone()).or_insert(0) += ev.count;
    }

    pub fn add_topdown(&mut self, slot: TopdownL1, count: u64) {
        *self.topdown_l1.entry(slot).or_insert(0) += count;
    }

    pub fn add_topdown_l2(&mut self, slot: TopdownL2, count: u64) {
        *self.topdown_l2.entry(slot).or_insert(0) += count;
    }

    /// 把 TopdownL1 四个 slot 规整为百分比 (假设四者之和为总 slots)
    pub fn topdown_percentages(&self) -> HashMap<TopdownL1, f64> {
        let total: u64 = TopdownL1::all()
            .iter()
            .map(|s| self.topdown_l1.get(s).copied().unwrap_or(0))
            .sum::<u64>()
            .max(1);
        let mut out = HashMap::new();
        for s in TopdownL1::all() {
            let c = self.topdown_l1.get(&s).copied().unwrap_or(0);
            out.insert(s, c as f64 / total as f64);
        }
        out
    }

    /// 合并另一个 PerfRecord 到自身
    pub fn merge(&mut self, other: &PerfRecord) {
        for (k, &v) in &other.events {
            *self.events.entry(k.clone()).or_insert(0) += v;
        }
        for (&k, &v) in &other.topdown_l1 {
            *self.topdown_l1.entry(k).or_insert(0) += v;
        }
        for (&k, &v) in &other.topdown_l2 {
            *self.topdown_l2.entry(k).or_insert(0) += v;
        }
    }

    /// 概要统计
    pub fn summary(&self) -> PerfRecordSummary {
        PerfRecordSummary {
            event_count: self.events.len(),
            total_events: self.events.values().copied().sum(),
            l1_slots_filled: self.topdown_l1.values().filter(|&&c| c > 0).count(),
            l2_slots_filled: self.topdown_l2.values().filter(|&&c| c > 0).count(),
        }
    }

    /// 获取主要瓶颈 (L1 中占比最大的槽, 排除 Retiring)
    pub fn primary_bottleneck(&self) -> Option<(TopdownL1, f64)> {
        let total: u64 = TopdownL1::all()
            .iter()
            .map(|s| self.topdown_l1.get(s).copied().unwrap_or(0))
            .sum();
        if total == 0 {
            return None;
        }
        let pcts = self.topdown_percentages();
        pcts.iter()
            .filter(|(slot, _)| **slot != TopdownL1::Retiring)
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(&slot, &pct)| (slot, pct))
    }

    /// 序列化为 dcc 文本格式
    pub fn serialize(&self) -> String {
        let mut out = String::from("# dcc perf_event v1\n");
        for (name, &count) in &self.events {
            out.push_str(&format!("event {} {}\n", name, count));
        }
        for slot in TopdownL1::all() {
            if let Some(&count) = self.topdown_l1.get(&slot) {
                if count > 0 {
                    out.push_str(&format!("topdown {} {}\n", slot.as_str(), count));
                }
            }
        }
        for (slot, &count) in &self.topdown_l2 {
            if count > 0 {
                out.push_str(&format!("topdown_l2 {} {}\n", slot.as_str(), count));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topdown_percent_sum_to_one() {
        let mut r = PerfRecord::default();
        r.add_topdown(TopdownL1::Retiring, 30);
        r.add_topdown(TopdownL1::BadSpeculation, 20);
        r.add_topdown(TopdownL1::FrontendBound, 30);
        r.add_topdown(TopdownL1::BackendBound, 20);
        let p = r.topdown_percentages();
        let sum: f64 = p.values().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sum = {sum}");
    }

    #[test]
    fn parse_events() {
        let r = PerfRecord::from_str(
            "# perf stat output\nevent cycles 1000000\nevent instructions 500000\n",
        )
        .unwrap();
        assert_eq!(r.events.get("cycles"), Some(&1000000));
        assert_eq!(r.events.get("instructions"), Some(&500000));
    }

    #[test]
    fn parse_topdown_l1() {
        let r = PerfRecord::from_str(
            "topdown retiring 30\ntopdown bad_speculation 20\ntopdown frontend_bound 30\ntopdown backend_bound 20\n",
        )
        .unwrap();
        assert_eq!(r.topdown_l1.get(&TopdownL1::Retiring), Some(&30));
        assert_eq!(r.topdown_l1.get(&TopdownL1::BadSpeculation), Some(&20));
    }

    #[test]
    fn parse_topdown_l2() {
        let r = PerfRecord::from_str(
            "topdown_l2 branch_mispredict 15\ntopdown_l2 memory_bound 10\n",
        )
        .unwrap();
        assert_eq!(r.topdown_l2.get(&TopdownL2::branch_mispredict), Some(&15));
        assert_eq!(r.topdown_l2.get(&TopdownL2::memory_bound), Some(&10));
    }

    #[test]
    fn l2_parent_mapping() {
        assert_eq!(TopdownL2::branch_mispredict.parent(), TopdownL1::BadSpeculation);
        assert_eq!(TopdownL2::memory_bound.parent(), TopdownL1::BackendBound);
        assert_eq!(TopdownL2::fetch_latency.parent(), TopdownL1::FrontendBound);
        assert_eq!(TopdownL2::retiring_base.parent(), TopdownL1::Retiring);
    }

    #[test]
    fn merge_records() {
        let mut a = PerfRecord::default();
        a.add_topdown(TopdownL1::Retiring, 30);
        a.events.insert("cycles".into(), 100);
        let mut b = PerfRecord::default();
        b.add_topdown(TopdownL1::Retiring, 20);
        b.add_topdown(TopdownL1::BackendBound, 50);
        b.events.insert("cycles".into(), 200);
        a.merge(&b);
        assert_eq!(a.topdown_l1.get(&TopdownL1::Retiring), Some(&50));
        assert_eq!(a.topdown_l1.get(&TopdownL1::BackendBound), Some(&50));
        assert_eq!(a.events.get("cycles"), Some(&300));
    }

    #[test]
    fn summary_stats() {
        let mut r = PerfRecord::default();
        r.events.insert("cycles".into(), 1000);
        r.events.insert("instructions".into(), 500);
        r.add_topdown(TopdownL1::Retiring, 30);
        r.add_topdown(TopdownL1::BackendBound, 20);
        r.add_topdown_l2(TopdownL2::memory_bound, 15);
        let s = r.summary();
        assert_eq!(s.event_count, 2);
        assert_eq!(s.total_events, 1500);
        assert_eq!(s.l1_slots_filled, 2);
        assert_eq!(s.l2_slots_filled, 1);
    }

    #[test]
    fn primary_bottleneck() {
        let mut r = PerfRecord::default();
        r.add_topdown(TopdownL1::Retiring, 20);
        r.add_topdown(TopdownL1::BadSpeculation, 10);
        r.add_topdown(TopdownL1::FrontendBound, 15);
        r.add_topdown(TopdownL1::BackendBound, 55);
        let (slot, pct) = r.primary_bottleneck().unwrap();
        assert_eq!(slot, TopdownL1::BackendBound);
        assert!((pct - 0.55).abs() < 0.01);
    }

    #[test]
    fn roundtrip_to_string() {
        let mut r = PerfRecord::default();
        r.events.insert("cycles".into(), 1000);
        r.add_topdown(TopdownL1::Retiring, 40);
        r.add_topdown(TopdownL1::BackendBound, 60);
        r.add_topdown_l2(TopdownL2::memory_bound, 40);
        let text = r.serialize();
        let r2 = PerfRecord::from_str(&text).unwrap();
        assert_eq!(r2.events, r.events);
        assert_eq!(r2.topdown_l1, r.topdown_l1);
        assert_eq!(r2.topdown_l2, r.topdown_l2);
    }

    #[test]
    fn read_from_file() {
        let tmp = std::env::temp_dir().join("dcc-perf-event-test-v1.txt");
        std::fs::write(
            &tmp,
            b"# perf stat\nevent cycles 5000\ntopdown retiring 50\ntopdown backend_bound 50\n",
        )
        .unwrap();
        let r = PerfRecord::read(&tmp).unwrap();
        assert_eq!(r.events.get("cycles"), Some(&5000));
        assert_eq!(r.topdown_l1.get(&TopdownL1::Retiring), Some(&50));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn empty_record() {
        let r = PerfRecord::empty();
        let s = r.summary();
        assert_eq!(s.event_count, 0);
        assert_eq!(s.total_events, 0);
        assert!(r.primary_bottleneck().is_none());
    }

    #[test]
    fn topdown_from_str_aliases() {
        assert_eq!(TopdownL1::from_str("badspec"), Some(TopdownL1::BadSpeculation));
        assert_eq!(TopdownL1::from_str("frontend"), Some(TopdownL1::FrontendBound));
        assert_eq!(TopdownL1::from_str("backend"), Some(TopdownL1::BackendBound));
        assert_eq!(TopdownL1::from_str("unknown"), None);
    }
}
