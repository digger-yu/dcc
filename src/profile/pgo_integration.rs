// src/profile/pgo_integration.rs - PGO 集成: 概要数据与优化决策的桥梁
//
// 第一性原理: PGO (Profile-Guided Optimization) 的核心价值在于把
// 运行时采集的 profile 计数值转化为编译器的优化决策。本模块是
// 现有 profile 读/写基础设施 (pgo.rs / weights.rs) 与优化 pass
// 之间的桥梁。
//
// 职责:
//   1. PGO 概要数据加载与缓存 (从 profraw 或 .dcc.profdata)
//   2. 函数级 hot/cold 分类 (复用 weights.rs 的聚合逻辑)
//   3. 为 IPA-SRA 提供 profile 感知的拆分决策
//   4. Profile-guided 内联提示 (hot 函数优先内联)
//   5. 基本块频率元数据传播 (供 loop unroll / 指令调度)
//
// 设计: 与现有 profile::pgo / profile::weights 互补, 不重复
// 已有的 profraw 读写和权重聚合逻辑。

use std::collections::HashMap;
use std::path::Path;

use super::pgo::{PgoWeights, Profraw};
use super::weights::ProfileWeights;
use crate::ast::{Decl, Program};

/// PGO 集成配置
#[derive(Debug, Clone)]
pub struct PgoIntegrationConfig {
    /// 是否启用 PGO 集成优化
    pub enabled: bool,
    /// -O 级别
    pub opt_level: u8,
    /// profile 数据文件路径 (profraw 格式)
    pub profile_path: Option<String>,
    /// 是否启用 profile-guided 内联
    pub inline_hints: bool,
    /// 是否启用 profile-guided IPA-SRA
    pub ipa_sra_hints: bool,
    /// hot 函数阈值 (占总执行次数的比例)
    pub hot_threshold: f64,
    /// cold 函数阈值
    pub cold_threshold: f64,
}

impl Default for PgoIntegrationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            opt_level: 0,
            profile_path: None,
            inline_hints: false,
            ipa_sra_hints: false,
            hot_threshold: 0.1,
            cold_threshold: 0.01,
        }
    }
}

impl PgoIntegrationConfig {
    /// 是否应该运行 PGO 集成
    pub fn should_run(&self) -> bool {
        self.enabled && self.opt_level >= 2
    }
}

/// PGO 集成上下文: 缓存 profile 数据和分析结果
#[derive(Debug, Clone)]
pub struct PgoIntegrationContext {
    /// 函数级入口计数
    pub func_counts: HashMap<String, u64>,
    /// 函数热度分类
    pub func_hotness: HashMap<String, FuncHotness>,
    /// 总执行次数
    pub total_count: u64,
}

/// 函数热度分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FuncHotness {
    Hot,
    Warm,
    Cold,
    Unknown,
}

impl FuncHotness {
    pub fn is_hot(&self) -> bool {
        matches!(self, FuncHotness::Hot)
    }
    pub fn is_cold(&self) -> bool {
        matches!(self, FuncHotness::Cold)
    }
}

impl PgoIntegrationContext {
    /// 从多个数据源构建 PGO 集成上下文
    pub fn from_sources(
        pgo: &PgoWeights,
        profile: &ProfileWeights,
        config: &PgoIntegrationConfig,
    ) -> Self {
        let mut func_counts: HashMap<String, u64> = HashMap::new();
        let mut func_hotness: HashMap<String, FuncHotness> = HashMap::new();

        // 合并所有已知函数名
        let mut all_funcs: Vec<String> = Vec::new();
        for name in pgo.func_counts.keys() {
            if !all_funcs.contains(name) {
                all_funcs.push(name.clone());
            }
        }
        for name in profile.pgo_counts.keys() {
            if !all_funcs.contains(name) {
                all_funcs.push(name.clone());
            }
        }

        // 统计总计数
        let mut total: u64 = 0;
        for name in &all_funcs {
            let count = pgo
                .count_of(name)
                .max(*profile.pgo_counts.get(name).unwrap_or(&0));
            func_counts.insert(name.clone(), count);
            total += count;
        }

        // 分类 (safe_total 防除零, 但 total_count 保持原始值)
        let safe_total = total.max(1);
        for name in &all_funcs {
            let count = func_counts.get(name).copied().unwrap_or(0);
            let ratio = count as f64 / safe_total as f64;
            let hotness = if ratio > config.hot_threshold {
                FuncHotness::Hot
            } else if ratio < config.cold_threshold && count > 0 {
                FuncHotness::Cold
            } else if count > 0 {
                FuncHotness::Warm
            } else {
                // 使用 weights 的分类
                let w = profile.classify(name);
                if w.is_hot() {
                    FuncHotness::Hot
                } else if w.is_cold() {
                    FuncHotness::Cold
                } else {
                    FuncHotness::Unknown
                }
            };
            func_hotness.insert(name.clone(), hotness);
        }

        PgoIntegrationContext {
            func_counts,
            func_hotness,
            total_count: total,
        }
    }

    /// 从 profraw 文件直接加载
    pub fn from_profraw(path: &Path, config: &PgoIntegrationConfig) -> Option<Self> {
        let profraw = Profraw::read(path).ok()?;
        let pgo_weights = PgoWeights::from_profraw(&profraw);
        let empty_profile = ProfileWeights::default();
        Some(Self::from_sources(&pgo_weights, &empty_profile, config))
    }

    /// 获取函数热度
    pub fn hotness(&self, func_name: &str) -> FuncHotness {
        self.func_hotness
            .get(func_name)
            .copied()
            .unwrap_or(FuncHotness::Unknown)
    }

    /// 获取函数计数
    pub fn count(&self, func_name: &str) -> u64 {
        self.func_counts.get(func_name).copied().unwrap_or(0)
    }

    /// 判断函数是否为 hot
    pub fn is_hot(&self, func_name: &str) -> bool {
        self.hotness(func_name).is_hot()
    }

    /// 判断函数是否为 cold
    pub fn is_cold(&self, func_name: &str) -> bool {
        self.hotness(func_name).is_cold()
    }

    /// 返回所有 hot 函数名
    pub fn hot_funcs(&self) -> Vec<String> {
        self.func_hotness
            .iter()
            .filter(|(_, h)| h.is_hot())
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// 返回所有 cold 函数名
    pub fn cold_funcs(&self) -> Vec<String> {
        self.func_hotness
            .iter()
            .filter(|(_, h)| h.is_cold())
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// 为 IPA-SRA 提供决策提示: 如果调用者是 hot 而 callee 是 cold,
    /// 建议不拆分 (避免 hot 路径上的额外参数传递开销)
    pub fn should_split_for_ipa_sra(&self, callee_name: &str, caller_name: Option<&str>) -> bool {
        let callee_hot = self.is_hot(callee_name);
        let caller_hot = caller_name.map(|n| self.is_hot(n)).unwrap_or(false);

        // hot 函数调用 cold 函数: 不拆分 (hot 路径不关心 cold 的优化)
        if caller_hot && !callee_hot {
            return false;
        }
        // cold 函数调用 hot 函数: 拆分 (cold 路径减少不必要的 struct 拷贝)
        if !caller_hot && callee_hot {
            return true;
        }
        // 默认: 拆分
        true
    }

    /// 为内联提供决策提示: hot 函数调用的函数优先内联
    pub fn should_inline(&self, callee_name: &str, caller_name: Option<&str>) -> bool {
        if self.is_hot(callee_name) {
            return true;
        }
        // hot 调用者调用的 small 函数也值得内联
        if let Some(caller) = caller_name {
            if self.is_hot(caller) && !self.is_cold(callee_name) {
                return true;
            }
        }
        false
    }
}

/// 把 PGO 热度标记应用到 Program 的 FuncDecl 上
pub fn apply_pgo_hotness(prog: &mut Program, ctx: &PgoIntegrationContext) {
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let hotness = ctx.hotness(&f.name.name);
            f.is_hot = hotness.is_hot();
            f.is_cold = hotness.is_cold();
        }
    }
}

/// 收集 Program 中所有函数名列表
pub fn collect_func_names(prog: &Program) -> Vec<String> {
    prog.decls
        .iter()
        .filter_map(|d| match d {
            Decl::Func(f) => Some(f.name.name.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::pgo::PgoWeights;
    use crate::profile::weights::ProfileWeights;

    #[test]
    fn test_empty_context() {
        let pgo = PgoWeights::default();
        let profile = ProfileWeights::default();
        let config = PgoIntegrationConfig {
            enabled: true,
            opt_level: 2,
            profile_path: None,
            inline_hints: true,
            ipa_sra_hints: true,
            hot_threshold: 0.1,
            cold_threshold: 0.01,
        };
        let ctx = PgoIntegrationContext::from_sources(&pgo, &profile, &config);
        assert_eq!(ctx.hotness("main"), FuncHotness::Unknown);
        assert_eq!(ctx.total_count, 0);
    }

    #[test]
    fn test_hot_func_classification() {
        let mut pgo = PgoWeights::default();
        pgo.func_counts.insert("hot_fn".into(), 800);
        pgo.func_counts.insert("warm_fn".into(), 50);
        pgo.func_counts.insert("cold_fn".into(), 1);
        pgo.func_counts.insert("other".into(), 80);

        let profile = ProfileWeights::default();
        let config = PgoIntegrationConfig {
            enabled: true,
            opt_level: 2,
            profile_path: None,
            inline_hints: true,
            ipa_sra_hints: true,
            hot_threshold: 0.1,
            cold_threshold: 0.01,
        };
        let ctx = PgoIntegrationContext::from_sources(&pgo, &profile, &config);
        assert_eq!(ctx.hotness("hot_fn"), FuncHotness::Hot);
        assert_eq!(ctx.hotness("warm_fn"), FuncHotness::Warm);
        assert_eq!(ctx.hotness("cold_fn"), FuncHotness::Cold);
        assert_eq!(ctx.hotness("other"), FuncHotness::Warm);
    }

    #[test]
    fn test_inline_hint_hot_callee() {
        let mut pgo = PgoWeights::default();
        pgo.func_counts.insert("hot_fn".into(), 800);
        pgo.func_counts.insert("caller".into(), 100);
        let profile = ProfileWeights::default();
        let config = PgoIntegrationConfig {
            enabled: true,
            opt_level: 2,
            ..Default::default()
        };
        let ctx = PgoIntegrationContext::from_sources(&pgo, &profile, &config);
        assert!(ctx.should_inline("hot_fn", Some("caller")));
        // caller 是 hot (100/900=0.111 > 0.1), unknown 非 cold,
        // hot 调用者调用的非 cold 函数值得内联
        assert!(ctx.should_inline("unknown", Some("caller")));
    }

    #[test]
    fn test_ipa_sra_hint_hot_caller_cold_callee() {
        let mut pgo = PgoWeights::default();
        pgo.func_counts.insert("hot_caller".into(), 800);
        pgo.func_counts.insert("cold_callee".into(), 1);
        let profile = ProfileWeights::default();
        let config = PgoIntegrationConfig {
            enabled: true,
            opt_level: 2,
            ..Default::default()
        };
        let ctx = PgoIntegrationContext::from_sources(&pgo, &profile, &config);
        assert!(
            !ctx.should_split_for_ipa_sra("cold_callee", Some("hot_caller")),
            "hot caller -> cold callee: 不拆分"
        );
    }

    #[test]
    fn test_ipa_sra_hint_cold_caller_hot_callee() {
        let mut pgo = PgoWeights::default();
        pgo.func_counts.insert("cold_caller".into(), 1);
        pgo.func_counts.insert("hot_callee".into(), 800);
        let profile = ProfileWeights::default();
        let config = PgoIntegrationConfig {
            enabled: true,
            opt_level: 2,
            ..Default::default()
        };
        let ctx = PgoIntegrationContext::from_sources(&pgo, &profile, &config);
        assert!(
            ctx.should_split_for_ipa_sra("hot_callee", Some("cold_caller")),
            "cold caller -> hot callee: 拆分"
        );
    }

    #[test]
    fn test_collect_func_names() {
        use crate::ast::{Block, FuncDecl, Ident, Program, Type};
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut prog = Program {
            decls: vec![],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        prog.decls.push(Decl::Func(FuncDecl {
            name: Ident {
                name: "foo".into(),
                span: span.clone(),
            },
            ret_ty: Type::Int,
            params: vec![],
            body: Some(Block {
                items: vec![],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
        }));
        let names = collect_func_names(&prog);
        assert_eq!(names, vec!["foo"]);
    }
}
