// src/profile/mod.rs - 性能分析与插桩子系统
//
// 第一性原理: 编译器在 release/profile 路径上需要回答三个问题
//   1. **哪里跑得慢?**         -> GCOV/PGO 计数器 (基本块/边/调用计数)
//   2. **为什么是热点?**       -> AutoFDO/LBR 样本 (精确指令级权重)
//   3. **内存分配合理吗?**     -> MemProf 堆采样 / Page Weight
// 再加:
//   4. **代码真的占满流水线?** -> perf 事件 (TopdownL1, Frontend/Backend/BadSpeculation/Retiring)
//   5. **offload 目标**        -> OpenMP target / CUDA / HIP 入口路由
//
// 当前实现分层:
//   * 端到端: gcov / pgo / autofdo / lbr / memprof / perf_event
//     (CLI 接受、模块结构、数据结构、解析器、权重下发 codegen)
//   * 骨架:   offload (CLI 接受 + 元数据 + device codegen stub)
//
// 设计: 与 `src/sanitizer.rs` 类似, AST 改写 + codegen 钩子。
// GCOV/PGO 在函数 prologue 注入 `inc <counter>` (默认 edge-counter 形式),
//   notes 文件 `.gcno` 由 `gcov::emit_notes` 写盘,
//   数据文件 `.gcda` 由 `gcov_runtime` 在程序退出时落盘。
// AutoFDO/LBR/MemProf/PerfEvent 已接入 driver → ProfileWeights → codegen,
//   支持多源权重聚合 + hot/cold 函数段布局 (.text.hot / .text.unlikely).

pub mod autofdo;
pub mod device_codegen;
pub mod fuzzer_protocol;
pub mod gcov;
pub mod kcov_merge;
pub mod lbr;
pub mod memprof;
pub mod offload;
pub mod offload_driver_link;
pub mod perf_data;
pub mod perf_event;
pub mod pgo;
pub mod pgo_integration;
pub mod weights;

use std::path::{Path, PathBuf};

/// 性能分析配置。集中开关 + 子系统参数; 序列化到 codegen。
#[derive(Debug, Clone, Default)]
pub struct ProfileConfig {
    /// `-fprofile-generate[=dir]` / `-fprofile-instr-generate[=dir]`
    pub pgo_generate: Option<PathBuf>,
    /// `-fprofile-use=file` / `-fprofile-instr-use=file`
    pub pgo_use: Option<PathBuf>,
    /// `-fprofile-sample-use=file` (AutoFDO)
    pub sample_use: Option<PathBuf>,
    /// `-ftest-coverage` (GCOV notes)
    pub gcov: bool,
    /// `-fprofile-arcs` (GCOV data)
    pub profile_arcs: bool,
    /// `--fmemory-profile` / `-fmemprof`
    pub memprof: bool,
    /// `--fmemory-profile-use=file`
    pub memprof_use: Option<PathBuf>,
    /// `--fprofile-list[=file]` 输出函数级别 perf 概要
    pub profile_list: Option<PathBuf>,
    /// `-ftopdownmetrics`  (TopdownL1 metrics 输出)
    pub topdown_metrics: bool,
    /// `-fopenmp` / `-fopenmp=simd` / `-foffload-...`
    pub openmp: bool,
    pub openmp_target: Option<String>,
    /// `-fcuda` / `-fno-cuda`
    pub cuda: bool,
    pub hip: bool,
    /// Offloading 完整配置 (openmp/cuda/hip 标志 + targets + archs + 校验)
    pub offload: crate::profile::offload::OffloadConfig,
    /// AutoFDO 反馈路径 (`-fauto-profile=file`)
    pub auto_profile: Option<PathBuf>,
    /// LBR 反馈路径 (`-flbr-profile=file`)
    pub lbr_profile: Option<PathBuf>,
    /// perf-event 输出路径 (`-fperf-event-record=path` / `-frecord-profile`)
    pub perf_record: Option<PathBuf>,
    /// PGO 集成: profile-guided 内联提示
    pub pgo_inline: bool,
    /// PGO 集成: profile-guided IPA-SRA 决策
    pub pgo_ipa_sra: bool,
    /// PGO 集成: hot 函数阈值 (占总执行次数比例)
    pub pgo_hot_threshold: f64,
    /// PGO 集成: cold 函数阈值
    pub pgo_cold_threshold: f64,
}

impl ProfileConfig {
    /// 是否启用任何分析相关插桩
    pub fn any_instrumentation(&self) -> bool {
        self.pgo_generate.is_some() || self.gcov || self.profile_arcs || self.memprof
    }
    /// 是否启用任何使用概要的优化
    pub fn any_use(&self) -> bool {
        self.pgo_use.is_some()
            || self.auto_profile.is_some()
            || self.sample_use.is_some()
            || self.memprof_use.is_some()
            || self.lbr_profile.is_some()
    }
    /// 把概要使用路径规整为 `dir/<name>.gcda` 形式 (PGO default)
    pub fn gcda_path(&self, source: &Path) -> Option<PathBuf> {
        let dir = self.pgo_generate.as_deref()?;
        let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("a");
        Some(dir.join(format!("{stem}.gcda")))
    }
}
