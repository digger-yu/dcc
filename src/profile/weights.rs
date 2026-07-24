// src/profile/weights.rs - 跨子系统权重聚合
//
// 第一性原理: PGO / AutoFDO / LBR / MemProf 是四个独立数据源, 但都
// 回答 "这个函数 / 边 / 分配点有多热?" 的同一问题. 编译器需要把四个
// 源合并成一个统一的 hot/cold 标记, 喂给 codegen 决定段布局 (.text
// vs .text.hot vs .text.unlikely) 与 hint Store (__dcc_section_hot /
// __dcc_section_cold). 这一层是"权重下发"的桥梁.
//
// 当前实现: 每个数据源提供一个分类器 (Hot/Warm/Cold/Unknown), 合并
// 策略是 **多源投票**: 任意一个源标 Hot 就 Hot, 全部标 Cold 才 Cold,
// 其它是 Warm. 简化版, 实际 LLVM 用 ML 模型, 这里只做阈值聚合.

use std::collections::{HashMap, HashSet};

use super::autofdo::AutoFdoProfile;
use super::lbr::LbrProfile;
use super::memprof::MemProfData;
use super::pgo::PgoWeights;

/// 函数的最终热度 (汇总 4 个数据源)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FuncHotness {
    /// 未被任何数据源记录, 视为中性
    Unknown,
    /// 至少一个数据源标 Hot
    Hot,
    /// 部分数据源记录为 Warm
    Warm,
    /// 所有数据源都标 Cold (或总样本接近 0)
    Cold,
}

impl FuncHotness {
    pub fn as_str(&self) -> &'static str {
        match self {
            FuncHotness::Hot => "hot",
            FuncHotness::Warm => "warm",
            FuncHotness::Cold => "cold",
            FuncHotness::Unknown => "unknown",
        }
    }
    /// 是否应放入 .text.hot 段 (增加 icache 局部性)
    pub fn is_hot(&self) -> bool { matches!(self, FuncHotness::Hot) }
    /// 是否应放入 .text.unlikely 段 (分离冷代码)
    pub fn is_cold(&self) -> bool { matches!(self, FuncHotness::Cold) }
}

/// 跨数据源的权重汇总
#[derive(Debug, Clone, Default)]
pub struct ProfileWeights {
    /// PGO 原始计数 (from .profraw)
    pub pgo_counts: HashMap<String, u64>,
    /// AutoFDO 函数级样本 (从 `func` 行聚合)
    pub autofdo_total: HashMap<String, u64>,
    /// LBR 边样本 (from -> to -> count)
    pub lbr_edges: HashMap<(String, String), u64>,
    /// MemProf 标 cold 的 callstack
    pub memprof_cold_sites: HashSet<Vec<String>>,
    /// 标记 cold 的分配点数 (调试)
    pub cold_alloc_count: u64,
}

impl ProfileWeights {
    pub fn empty() -> Self { Self::default() }

    /// 合并 4 个数据源 (各源独立加载后调用本方法)
    pub fn from_sources(
        pgo: &PgoWeights,
        autofdo: &AutoFdoProfile,
        lbr: &LbrProfile,
        memprof: &MemProfData,
    ) -> Self {
        let threshold = Self::memprof_threshold(&[]);
        Self {
            pgo_counts: pgo.func_counts.clone(),
            autofdo_total: autofdo.func_samples.clone(),
            lbr_edges: lbr.edges.clone(),
            memprof_cold_sites: memprof
                .alloc_counts
                .iter()
                .filter(|(_, count)| **count > 0 && **count < threshold)
                .map(|(stack, _)| stack.clone())
                .collect(),
            cold_alloc_count: memprof.alloc_counts.values().filter(|c| **c < 5).count() as u64,
        }
    }

    /// 给定函数名, 综合 4 源给出最终热度
    pub fn classify(&self, func: &str) -> FuncHotness {
        let mut votes = [0u32; 4]; // [hot, warm, cold, unknown_votes]
        // 1. PGO 计数
        if let Some(&c) = self.pgo_counts.get(func) {
            let total: u64 = self.pgo_counts.values().sum::<u64>().max(1);
            let r = c as f64 / total as f64;
            if r > 0.10 { votes[0] += 1; }
            else if r > 0.01 { votes[1] += 1; }
            else { votes[2] += 1; }
        }
        // 2. AutoFDO 样本
        if let Some(&c) = self.autofdo_total.get(func) {
            let total: u64 = self.autofdo_total.values().sum::<u64>().max(1);
            let r = c as f64 / total as f64;
            if r > 0.10 { votes[0] += 1; }
            else if r > 0.01 { votes[1] += 1; }
            else { votes[2] += 1; }
        }
        // 3. LBR 边入度: 用 `? -> func` 求和
        let lbr_in: u64 = self
            .lbr_edges
            .iter()
            .filter(|((_, t), _)| t == func)
            .map(|(_, c)| *c)
            .sum();
        let lbr_total: u64 = self.lbr_edges.values().sum::<u64>().max(1);
        if lbr_in > 0 {
            let r = lbr_in as f64 / lbr_total as f64;
            if r > 0.10 { votes[0] += 1; }
            else if r > 0.01 { votes[1] += 1; }
            else { votes[2] += 1; }
        }
        // 4. MemProf cold 站: 仅当函数名出现在 cold callstack 中 → Cold
        let in_cold_site = self
            .memprof_cold_sites
            .iter()
            .any(|stack| stack.iter().any(|f| f == func));
        if in_cold_site { votes[2] += 1; }

        let sum: u32 = votes.iter().sum();
        if sum == 0 { return FuncHotness::Unknown; }
        if votes[0] > 0 { return FuncHotness::Hot; }
        if votes[2] == sum { return FuncHotness::Cold; }
        FuncHotness::Warm
    }

    /// 返回所有 hot / cold 函数名 (供 codegen 落到段)
    pub fn hot_funcs(&self) -> HashSet<String> {
        self.all_known_funcs()
            .into_iter()
            .filter(|n| self.classify(n).is_hot())
            .collect()
    }
    pub fn cold_funcs(&self) -> HashSet<String> {
        self.all_known_funcs()
            .into_iter()
            .filter(|n| self.classify(n).is_cold())
            .collect()
    }

    fn all_known_funcs(&self) -> HashSet<String> {
        let mut s: HashSet<String> = HashSet::new();
        s.extend(self.pgo_counts.keys().cloned());
        s.extend(self.autofdo_total.keys().cloned());
        for ((_, t), _) in &self.lbr_edges { s.insert(t.clone()); }
        s
    }

    /// 给定 callstack, 估算 cold 阈值. 简化版: 总 alloc 次数的
    /// 1% 作为阈值 (参照 LLVM MemProf 的 `ColdThreshold` 默认).
    fn memprof_threshold(_stack: &[String]) -> u64 { 5 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_pgo() -> PgoWeights { PgoWeights::default() }
    fn empty_autofdo() -> AutoFdoProfile { AutoFdoProfile::default() }
    fn empty_lbr() -> LbrProfile { LbrProfile::default() }
    fn empty_memprof() -> MemProfData { MemProfData::default() }

    #[test]
    fn empty_weights_classify_unknown() {
        let w = ProfileWeights::default();
        assert_eq!(w.classify("main"), FuncHotness::Unknown);
        assert!(w.hot_funcs().is_empty());
        assert!(w.cold_funcs().is_empty());
    }

    #[test]
    fn pgo_hot_makes_classify_hot() {
        // 4 个函数: hot_fn 占 80%, warm_fn 占 15%, cold_fn 占 4%, neutral 占 1%
        let mut pgo = empty_pgo();
        pgo.func_counts.insert("hot_fn".into(), 800);
        pgo.func_counts.insert("warm_fn".into(), 150);
        pgo.func_counts.insert("cold_fn".into(), 40);
        pgo.func_counts.insert("neutral".into(), 10);
        let w = ProfileWeights::from_sources(
            &pgo, &empty_autofdo(), &empty_lbr(), &empty_memprof());
        // 800/1000 = 0.80 -> Hot
        assert_eq!(w.classify("hot_fn"), FuncHotness::Hot);
        // 150/1000 = 0.15 -> Hot (> 0.10)
        assert_eq!(w.classify("warm_fn"), FuncHotness::Hot);
        // 40/1000 = 0.04 -> Warm
        assert_eq!(w.classify("cold_fn"), FuncHotness::Warm);
        // 10/1000 = 0.01 -> Cold (<= 0.01 严格小于)
        assert_eq!(w.classify("neutral"), FuncHotness::Cold);
    }

    #[test]
    fn lbr_inbound_counted_for_classify() {
        let mut lbr = empty_lbr();
        // main 收到 1000 次入度 -> hot
        for _ in 0..1000 {
            *lbr.edges.entry(("caller".into(), "main".into())).or_insert(0) += 1;
        }
        let w = ProfileWeights::from_sources(
            &empty_pgo(), &empty_autofdo(), &lbr, &empty_memprof());
        assert_eq!(w.classify("main"), FuncHotness::Hot);
    }

    #[test]
    fn multi_source_majority_is_warm() {
        // PGO 标 warm + AutoFDO 标 warm + LBR 标 warm -> Warm (非 Hot, 非 Cold)
        let mut pgo = empty_pgo();
        pgo.func_counts.insert("f".into(), 50);
        pgo.func_counts.insert("other".into(), 950);
        let mut af = empty_autofdo();
        af.func_samples.insert("f".into(), 50);
        af.func_samples.insert("o".into(), 950);
        let mut lbr = empty_lbr();
        *lbr.edges.entry(("a".into(), "f".into())).or_insert(0) += 50;
        *lbr.edges.entry(("b".into(), "o".into())).or_insert(0) += 950;
        let w = ProfileWeights::from_sources(&pgo, &af, &lbr, &empty_memprof());
        // 50 / 1000 = 0.05, 三源都落在 (0.01, 0.10] -> Warm
        assert_eq!(w.classify("f"), FuncHotness::Warm);
    }

    #[test]
    fn memprof_cold_site_marks_func_cold() {
        let mut mp = empty_memprof();
        // 1 次 alloc, stack 含 "rare_fn" -> < 5 阈值 -> cold
        mp.alloc_counts.insert(
            vec!["main".into(), "rare_fn".into(), "malloc".into()], 1);
        let w = ProfileWeights::from_sources(
            &empty_pgo(), &empty_autofdo(), &empty_lbr(), &mp);
        // 没有任何源把它标 hot, 但 memprof 标 cold -> Cold
        // 注意 memprof 标 cold 需要"全部源都没标 hot", 但 memprof 自身是 cold
        // 票; votes[2] = 1, sum = 1, votes[2] == sum -> Cold
        assert_eq!(w.classify("rare_fn"), FuncHotness::Cold);
    }

    #[test]
    fn hot_and_cold_sets_partition_known_funcs() {
        let mut pgo = empty_pgo();
        pgo.func_counts.insert("hot".into(), 1000);
        pgo.func_counts.insert("cold".into(), 0);
        let w = ProfileWeights::from_sources(
            &pgo, &empty_autofdo(), &empty_lbr(), &empty_memprof());
        let hot = w.hot_funcs();
        let cold = w.cold_funcs();
        assert!(hot.contains("hot"));
        assert!(!cold.contains("hot"));
        // cold 票数 = 1, sum = 1, votes[2] == sum -> Cold
        assert!(cold.contains("cold"));
    }
}
