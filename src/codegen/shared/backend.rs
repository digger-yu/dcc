// src/codegen/backend.rs - 统一 Backend trait 契约
// First Principle: all architecture backends must implement the same interface

use crate::cli::{Arch, AsmFallback};
use crate::codegen::shared::asm_source::AsmPassthroughRecord;
use crate::codegen::shared::features::FeatureSet;
use crate::diagnostics::Diagnostic;
use crate::ir::Module;
use crate::sanitizer::SanitizerFlags;
use crate::target::Target;
use std::collections::BTreeMap;
/// Bundles all parameters that were previously scattered as individual arguments
/// This enables API unification across all target architectures
#[derive(Debug, Clone)]
pub struct EmitOptions<'a> {
    // Core settings
    pub relocatable: bool,
    pub emit_asm: bool,
    pub source_name: String,
    /// 内联汇编不可编码时的策略 (默认 Error; Gas = 整 TU 降级为 GAS 文本)
    pub asm_fallback: AsmFallback,

    // Debug and diagnostics
    pub debug_level: u8,
    /// `-O0..-O3` 的档位。发射侧需要它的理由不是「要不要优化」，而是几条
    /// **只在有优化时才成立**的形状护栏：区内家（S4）与 A2 标签汇合在 -O0 下
    /// 必须整体关——那一档连累活分析都不跑，「这个槽在本区内会不会再被写」根本没有
    /// 依据（方案 §3 S4 的硬互斥清单明写 `-O0`，并要求「`Codegen` 需补 `opt_level`」）。
    pub opt_level: u8,
    pub dwarf_version: u8,
    pub sanitizers: SanitizerFlags,

    // Optimization features
    pub lto_enabled: bool,
    pub function_sections: bool,
    pub parallel_codegen: bool,
    pub bounds_check: bool,
    pub trapv: bool,

    // Optional sections
    pub offload: Option<&'a crate::profile::offload::OffloadSection>,
    pub line_map: &'a [crate::preproc::LineSpan],
    pub verbose: bool,
}

/// Input data for backend compilation
///
/// `opts` 以引用形式持有：`emit()` 在调用后端前后都需要读取 EmitOptions
/// (例如 emit_asm / debug_level)，按值移动会让上层无法再访问。
pub struct CompileInput<'a> {
    pub module: &'a Module,
    pub target: &'a Target,
    pub opts: &'a EmitOptions<'a>,
}

/// Common image structure unified across all architectures
/// Each arch provides explicit field mapping, avoiding generic `...`
#[derive(Default)]
pub struct CommonImage {
    pub code: Vec<u8>,
    pub rodata: Vec<u8>,
    pub data: Vec<u8>,
    pub bss: u64,
    pub tls: TlsImage,          // Unified field, or enum for fidelity
    pub symbols: ObjectSymbols, // Adaptation period can use enum { Elf(..), ... }
    pub relocs: ObjectRelocs,
    pub extra_sections: Vec<ExtraSection>,
    pub diagnostics: Vec<Diagnostic>,
    pub func_sizes: BTreeMap<String, u64>,
    /// dcc 编不出来、必须透传给 GAS 的内联汇编行。这是 TU 级降级判据的
    /// **唯一**数据源 —— 不得再用扫 diagnostic 字符串的方式推断。
    pub asm_passthrough: Vec<AsmPassthroughRecord>,
    /// 透传语句在 .text 内的起点与其原始行: 降级成 GAS 文本时,
    /// 这些行按位置插入到解码出的指令流中 (保证文本与字节的位置对应)。
    pub asm_passthrough_regions: Vec<(u64, Vec<String>)>,
    // Additional fields as needed for debug/parallel results
}

/// TLS image structure
#[derive(Default)]
pub struct TlsImage {
    pub offset: u64,
    pub size: u64,
    pub decl: Option<String>,
}

/// Object symbols (can be adapted from arch-specific types)
#[derive(Default)]
pub struct ObjectSymbols {
    pub globals: Vec<String>,
    pub locals: Vec<String>,
    pub imports: Vec<String>,
}

/// Object relocs (can be adapted from arch-specific types)
#[derive(Default)]
pub struct ObjectRelocs {
    pub rellocs: Vec<String>,
}

/// Extra section for LTO and other special purposes
#[derive(Default)]
pub struct ExtraSection {
    pub name: String,
    pub content: Vec<u8>,
}

/// Unified Backend trait - all architectures must implement this
///
/// **设计原则:**
/// 1. 同 Tier 架构使用相同的 API
/// 2. 通过 `features()` 方法显式声明功能能力
/// 3. 能力缺失时显式拒绝 (不静默 no-op)
/// 4. 代码生成 (`compile`) 与打包 (`package`) 职责分离
pub trait Backend {
    /// Return the architecture this backend targets
    fn arch(&self) -> Arch;

    /// Return feature set indicating supported capabilities
    ///
    /// **Important:** This should match behavior probes from tests.
    /// Missing features should return explicit errors (E0xxx), not silent no-op.
    fn features(&self) -> FeatureSet;

    /// Perform the main code generation
    ///
    /// Returns a CommonImage with all generated sections populated.
    /// Architecture-specific lowering happens here.
    ///
    /// **Error handling:** Returns `Err(Vec<Diagnostic>)` if compilation fails
    /// according to the unified diagnostic policy (see `has_fatal_diagnostics`).
    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>>;

    /// Render assembly text (for `-S` flag)
    ///
    /// 返回汇编文本的 UTF-8 字节 (调用方直接 write_all)。
    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>>;

    /// 从**已经 `compile()` 完成的当前状态**渲染 GAS 文本, 不得重跑 codegen。
    ///
    /// 为什么需要这个方法: TU 级降级 (“这批字节里有编不出来的行, 整个 TU 改出
    /// 文本”) 发生在 `compile()` 之后。若回头再调 `render_asm(input)`, 就是在已填满
    /// 的缓冲上再跑一遍 codegen: 字节翻倍、符号重复、诊断累积, 且并行/串行策略
    // 两轮回不一样 —— 恰好制造了本方案要消灭的“同一源码两次结果不同”。
    ///
    /// 一次分配、两处复用: 本方法与 `build_object()` 共同消费同一次 `compile()`
    /// 的结果, 因此 `-c` 与降级 `-S` 的差异只可能出现在最终编码层, 不可能出现在
    /// 操作数分配层。实现时必须与 `render_asm()` 共用同一个文本渲染器。
    fn render_asm_from_state(&mut self, module: &Module) -> Result<Vec<u8>, Vec<Diagnostic>>;

    /// 将 `compile()` 生成的内部状态序列化为最终目标文件字节 (ELF .o / ET_EXEC)。
    ///
    /// 职责分离：`compile()` 只做 IR → 机器码 + 节数据，`build_object()` 只做
    /// 容器 (ELF/COFF) 打包。必须在 `compile()` 之后调用。
    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>>;
}
