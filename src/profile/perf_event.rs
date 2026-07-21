// src/profile/perf_event.rs - Perf 事件 / TopdownL1 metrics (骨架)
//
// TopdownL1 metrics: 把 CPU pipeline stalls 分解为四个一阶分类
//   Retiring          : 真正做有用工作的微操作
//   Bad Speculation   : 错误预测 / 取消的工作
//   Frontend Bound    : 前端取指/解码受限
//   Backend Bound     : 后端执行资源受限
//   (以及它们的二阶展开: Branch Mispredicts, Cache Misses, ...)
//
// 用途: 编译时输出 `__topdown_l1_*` 计数器 (在 PGO-instrument 路径
// 下), 或在运行时 `perf stat -M topdownl1` 输出, 给 dcc 自身或被
// 编译程序做"为什么这么慢"的诊断.
//
// 当前 dcc 仅定义数据结构 + TopdownL1 枚举, 不读 perf.data 二进制.

use std::collections::HashMap;

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
        [Self::Retiring, Self::BadSpeculation, Self::FrontendBound, Self::BackendBound]
    }
}

/// perf_event 记录的一个抽象 (仿 `perf record -e ...`).
#[derive(Debug, Clone)]
pub struct PerfEventSample {
    pub event: String,
    pub count: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Default)]
pub struct PerfRecord {
    /// event -> count
    pub events: HashMap<String, u64>,
    /// 顶层 TopdownL1 metrics
    pub topdown_l1: HashMap<TopdownL1, u64>,
}

impl PerfRecord {
    pub fn empty() -> Self { Self::default() }

    pub fn add(&mut self, ev: PerfEventSample) {
        *self.events.entry(ev.event.clone()).or_insert(0) += ev.count;
    }

    pub fn add_topdown(&mut self, slot: TopdownL1, count: u64) {
        *self.topdown_l1.entry(slot).or_insert(0) += count;
    }

    /// 把 TopdownL1 四个 slot 规整为百分比 (假设四者之和为总 slots).
    pub fn topdown_percentages(&self) -> HashMap<TopdownL1, f64> {
        let total: u64 = TopdownL1::all().iter()
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
}
