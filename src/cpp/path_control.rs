//! C++ 编译路径控制 (Stage 0).
//!
//! 该模块提供三件事:
//!
//! 1. **`PathDecision`**：根据 `--cxx-native=off|on|auto` 与 feature 检测结果,
//!    决定一个 TU 该走 native 还是 legacy 路径, 并产出结构化的 `FallbackReason`
//!    (列明不支持的特性) 供 verbose 诊断输出。
//!
//! 2. **`PathStats`**：进程级统计, 记录 native / legacy 各走了多少 TU,
//!    以及每个 fallback 原因触发的次数, 便于 `--verbose` summary 与测试断言。
//!
//! 3. **`legacy_lower_to_c`**：把 C++ 声明列表降到完整 C 源码字符串,
//!    作为 Off / fallback 路径的真实入口. 该函数是对 `cpp::lower::Lower`
//!    `lower_decls` + `finish` 的封装, 保留 lower_decls 的公共 API.
//!
//! 本模块严格遵守方案中"Off 模式真实走 legacy 路径"与"路径统计可审计"的
//! 要求: 任何走 legacy 的 TU 都会被 `PathStats::record_legacy` 计入,
//! fallback 原因会被 `PathStats::record_fallback` 计入并出现在 verbose 输出中.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cpp::feature_matrix::{CppFeature, CppSupportLevel, CxxNativeMode};

/// 单条 fallback 原因：触发该原因的特性名 + 触发次数
#[derive(Debug, Clone)]
pub struct FallbackReason {
    pub feature: CppFeature,
    pub count: u64,
}

impl FallbackReason {
    pub fn new(feature: CppFeature) -> Self {
        Self { feature, count: 0 }
    }

    pub fn bump(&mut self) {
        self.count += 1;
    }

    pub fn label(&self) -> String {
        format!("{} ({}x)", self.feature.name(), self.count)
    }
}

/// 路径决策结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDecision {
    /// 强制走 native 路径
    Native,
    /// 强制走 legacy 路径（Off 模式）
    Legacy { reason: &'static str },
    /// Auto 模式：根据 feature 决定
    Auto {
        /// 是否所有特性都受支持
        all_native: bool,
        /// 不支持的特性名列表
        unsupported: Vec<String>,
    },
}

impl PathDecision {
    pub fn path_name(&self) -> &'static str {
        match self {
            PathDecision::Native => "native",
            PathDecision::Legacy { .. } => "legacy",
            PathDecision::Auto {
                all_native: true, ..
            } => "native",
            PathDecision::Auto {
                all_native: false, ..
            } => "legacy",
        }
    }

    pub fn fallback_reason(&self) -> Option<String> {
        match self {
            PathDecision::Legacy { reason } => Some((*reason).to_string()),
            PathDecision::Auto {
                all_native: false,
                unsupported,
            } => Some(unsupported.join(", ")),
            _ => None,
        }
    }
}

/// 给定 `--cxx-native` 模式与 feature 集合, 计算路径决策
///
/// 决策依据 (Stage 0 多档支持矩阵):
///
/// - `Off` → 强制 legacy, 给出明确 reason
/// - `On`  → 强制 native; 由 driver 在调用方把低于 `EndToEnd` 的特性升级为
///   诊断错误 (避免静默 fallback)
/// - `Auto`→ 低于 `EndToEnd` 的特性触发 fallback, 列出每条特性当前档位
///   便于 verbose 诊断
pub fn decide(mode: CxxNativeMode, features: &[CppFeature]) -> PathDecision {
    match mode {
        CxxNativeMode::Off => PathDecision::Legacy {
            reason: "--cxx-native=off forced legacy backend",
        },
        CxxNativeMode::On => PathDecision::Native,
        CxxNativeMode::Auto => {
            let mut unsupported: Vec<(String, CppSupportLevel)> = features
                .iter()
                .filter(|f| f.level() < CppSupportLevel::EndToEnd)
                .map(|f| (f.name().to_string(), f.level()))
                .collect();
            // 让输出稳定 (按特性名升序)
            unsupported.sort_by(|a, b| a.0.cmp(&b.0));
            PathDecision::Auto {
                all_native: unsupported.is_empty(),
                unsupported: unsupported
                    .into_iter()
                    .map(|(n, l)| format!("{n}@{}", l.name()))
                    .collect(),
            }
        },
    }
}

/// 进程级路径统计
///
/// 使用 `AtomicU64` 计数器, 允许跨 TU 并行编译时安全累加.
#[derive(Debug, Default)]
pub struct PathStats {
    native_tus: AtomicU64,
    legacy_tus: AtomicU64,
    fallback_reasons: std::sync::Mutex<HashMap<String, u64>>,
}

impl PathStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一个 TU 走 native 路径
    pub fn record_native(&self) {
        self.native_tus.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一个 TU 走 legacy 路径
    pub fn record_legacy(&self) {
        self.legacy_tus.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次 fallback 原因
    pub fn record_fallback(&self, reason: &str) {
        if let Ok(mut map) = self.fallback_reasons.lock() {
            *map.entry(reason.to_string()).or_insert(0) += 1;
        }
    }

    /// Stage 10.3: 记录一次不支持的特性 (按 `feature@level` 聚合, 形如
    /// `FunctionTemplate@EndToEnd` / `Concept@Parsed`)。与 [`Self::record_fallback`]
    /// 共用底层 map, 调用方按"是不是 feature"二选一。
    ///
    /// `level` 通常取自 [`crate::cpp::feature_matrix::CppSupportLevel::name`],
    /// 让 verbose 一眼看出"该特性在矩阵中处于哪一档"。
    pub fn record_unsupported_feature(
        &self,
        feature: crate::cpp::feature_matrix::CppFeature,
        level: crate::cpp::feature_matrix::CppSupportLevel,
    ) {
        let key = format!("{}@{}", feature.variant_name(), level.name());
        if let Ok(mut map) = self.fallback_reasons.lock() {
            *map.entry(key).or_insert(0) += 1;
        }
    }

    pub fn native_count(&self) -> u64 {
        self.native_tus.load(Ordering::Relaxed)
    }

    pub fn legacy_count(&self) -> u64 {
        self.legacy_tus.load(Ordering::Relaxed)
    }

    pub fn fallback_counts(&self) -> Vec<(String, u64)> {
        self.fallback_reasons
            .lock()
            .map(|m| {
                let mut v: Vec<(String, u64)> = m.iter().map(|(k, v)| (k.clone(), *v)).collect();
                v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                v
            })
            .unwrap_or_default()
    }

    /// 进程级 summary, 供 verbose / summary 输出使用
    pub fn summary(&self) -> String {
        let n = self.native_count();
        let l = self.legacy_count();
        let mut s = format!("native={} legacy={}", n, l);
        let fb = self.fallback_counts();
        if !fb.is_empty() {
            s.push_str(" fallback[");
            let parts: Vec<String> = fb
                .into_iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect();
            s.push_str(&parts.join(", "));
            s.push(']');
        }
        s
    }
}

/// 进程级全局 `PathStats`, 通过 `track()` / `summary()` API 访问.
///
/// dcc 通常以单进程串行方式编译, 这里使用全局静态即可; 跨 TU 并行
/// (multi-file `-j`) 时, 累加是线程安全的 (`AtomicU64` + `Mutex`).
pub fn global_stats() -> &'static PathStats {
    use std::sync::OnceLock;
    static STATS: OnceLock<PathStats> = OnceLock::new();
    STATS.get_or_init(PathStats::new)
}

/// 实际执行 legacy lowering: 完整 C 源码字符串, 可直接写到临时 `.c`.
///
/// 这是 Off / fallback 路径的真实入口:
///
/// - 接受完整 C++ AST (`Vec<CppDecl>`), 转发给 `cpp::legacy::lower_to_c`
/// - Stage 0 要求 legacy 边界只通过 `cpp::legacy` 入口, 本函数是薄转发
/// - 错误情况返回 `Err(String)` 而非 panic, 让调用方可以走 fallback 兜底
pub fn legacy_lower_to_c(
    decls: &[crate::cpp::parser::CppDecl],
    constexpr_fns: std::collections::HashMap<String, crate::cpp::cpp_const_eval::ConstexprFunction>,
) -> Result<String, String> {
    crate::cpp::legacy::lower_to_c(decls, constexpr_fns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::feature_matrix::{CppFeature, CxxNativeMode};

    #[test]
    fn decide_off_forces_legacy() {
        let d = decide(CxxNativeMode::Off, &[]);
        assert_eq!(d.path_name(), "legacy");
        assert!(d.fallback_reason().unwrap().contains("--cxx-native=off"));
    }

    #[test]
    fn decide_on_forces_native() {
        let d = decide(CxxNativeMode::On, &[]);
        assert_eq!(d.path_name(), "native");
        assert!(d.fallback_reason().is_none());
    }

    #[test]
    fn decide_auto_all_supported_native() {
        let d = decide(CxxNativeMode::Auto, &[CppFeature::FreeFunction]);
        assert_eq!(d.path_name(), "native");
        assert!(d.fallback_reason().is_none());
    }

    #[test]
    fn decide_auto_unsupported_falls_back() {
        // Auto 的 fallback 门槛完全由能力矩阵驱动: 不得在测试里硬编码
        // “某个特性不支持”, 否则矩阵升级后测试会人为失败.
        // 这里从全量矩阵中取一个真实低于 EndToEnd 的特性来验证机制;
        // 若矩阵已把所有特性升到 EndToEnd, 则验证这一事实本身.
        let unsupported: Vec<CppFeature> = CppFeature::ALL_FEATURES
            .iter()
            .copied()
            .filter(|f| !f.end_to_end_supported())
            .collect();
        if unsupported.is_empty() {
            let d = decide(CxxNativeMode::Auto, CppFeature::ALL_FEATURES);
            assert_eq!(d.path_name(), "native");
            assert!(d.fallback_reason().is_none());
            return;
        }
        let probe = unsupported[0];
        let d = decide(CxxNativeMode::Auto, &[CppFeature::FreeFunction, probe]);
        assert_eq!(d.path_name(), "legacy");
        let r = d.fallback_reason().unwrap();
        assert!(r.contains(probe.name()), "got: {}", r);
        // Stage 0 多档: 原因里要带当前档位 (parsed/sema/lowered/...) 以便审计.
        assert!(r.contains("@"), "got: {}", r);
    }

    #[test]
    fn decide_auto_lists_unsupported_reasons_sorted() {
        // verbose 诊断依赖“列出全部原因且按特性名升序”: 反序传入全量矩阵,
        // 输出的 unsupported 列表必须是有序的。
        let mut matrix: Vec<CppFeature> = CppFeature::ALL_FEATURES.to_vec();
        assert!(matrix.len() >= 2);
        matrix.reverse();
        let d = decide(CxxNativeMode::Auto, &matrix);
        match d {
            PathDecision::Auto { unsupported, .. } => {
                let mut sorted = unsupported.clone();
                sorted.sort();
                assert_eq!(unsupported, sorted, "unsupported list not sorted");
            },
            other => panic!("expected Auto decision, got {other:?}"),
        }
    }

    #[test]
    fn decide_auto_end_to_end_features_stay_native() {
        let d = decide(
            CxxNativeMode::Auto,
            &[
                CppFeature::FreeFunction,
                CppFeature::PlainClass,
                CppFeature::MemberFunction,
                CppFeature::CtorDtor,
                CppFeature::ReferenceType,
                CppFeature::Exception,
            ],
        );
        assert_eq!(d.path_name(), "native");
        assert!(d.fallback_reason().is_none());
    }

    #[test]
    fn path_stats_tracks_native_and_legacy() {
        let s = PathStats::new();
        s.record_native();
        s.record_native();
        s.record_legacy();
        s.record_fallback("foo");
        s.record_fallback("foo");
        s.record_fallback("bar");
        assert_eq!(s.native_count(), 2);
        assert_eq!(s.legacy_count(), 1);
        let counts = s.fallback_counts();
        assert_eq!(counts[0], ("foo".to_string(), 2));
        assert_eq!(counts[1], ("bar".to_string(), 1));
        let sum = s.summary();
        assert!(sum.contains("native=2"));
        assert!(sum.contains("legacy=1"));
        assert!(sum.contains("foo: 2"));
    }

    #[test]
    fn record_unsupported_feature_aggregates_by_feature_name_and_level() {
        // Stage 10.3: 必须按 `feature@level` 聚合 (Concept@Parsed 等),
        // 与 `record_fallback` 同入口, 但 key 形式不同. summary 文本要把
        // 两个特性独立成两条 (而不是合并到一个计数)。
        use crate::cpp::feature_matrix::CppSupportLevel;
        let s = PathStats::new();
        s.record_native();
        s.record_native();
        s.record_legacy();
        s.record_unsupported_feature(CppFeature::Concept, CppSupportLevel::Parsed);
        s.record_unsupported_feature(CppFeature::Concept, CppSupportLevel::Parsed);
        s.record_unsupported_feature(
            CppFeature::PartialSpecialization,
            CppSupportLevel::Parsed,
        );
        s.record_unsupported_feature(
            CppFeature::PartialSpecialization,
            CppSupportLevel::Parsed,
        );
        s.record_unsupported_feature(
            CppFeature::PartialSpecialization,
            CppSupportLevel::Parsed,
        );
        let sum = s.summary();
        // 排序按计数降序: PartialSpecialization (3) 先, Concept (2) 后
        assert!(
            sum.contains("PartialSpecialization@Parsed: 3"),
            "summary 缺 PartialSpecialization 行: {}",
            sum
        );
        assert!(
            sum.contains("Concept@Parsed: 2"),
            "summary 缺 Concept 行: {}",
            sum
        );
        // 前缀 native/legacy 不变
        assert!(sum.contains("native=2 legacy=1"));
    }

    #[test]
    fn record_unsupported_feature_key_format() {
        // Stage 10.3 验收: key 形式 `feature_name@level_name`, 不能漏 `@`
        use crate::cpp::feature_matrix::CppSupportLevel;
        let s = PathStats::new();
        s.record_unsupported_feature(CppFeature::FunctionTemplate, CppSupportLevel::EndToEnd);
        let counts = s.fallback_counts();
        assert_eq!(
            counts,
            vec![("FunctionTemplate@EndToEnd".to_string(), 1)],
            "key 必须含 `@` 与 level name, 不能合并到 fallback_counts 的字符串维度"
        );
    }

    #[test]
    fn legacy_lower_to_c_produces_c_for_plain_class() {
        let src = b"class Counter { public: Counter() { } };";
        let mut p = crate::cpp::parser::CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let constexpr_fns = p.into_constexpr_fns();
        let c = legacy_lower_to_c(&decls, constexpr_fns).unwrap();
        assert!(c.contains("struct Counter"));
        assert!(c.contains("_ZN7CounterC1Ev"));
    }

    #[test]
    fn global_stats_returns_same_instance() {
        let a = global_stats();
        let b = global_stats();
        assert!(std::ptr::eq(a, b));
    }
}
