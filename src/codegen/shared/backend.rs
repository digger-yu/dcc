// src/codegen/backend.rs - 统一 Backend trait 契约
// First Principle: all architecture backends must implement the same interface

use crate::cli::Arch;
use crate::diagnostics::Diagnostic;
use crate::ir::Module;
use crate::sanitizer::SanitizerFlags;
use crate::target::Target;
use crate::codegen::shared::features::FeatureSet;
use std::collections::BTreeMap;
/// Bundles all parameters that were previously scattered as individual arguments
/// This enables API unification across all target architectures
#[derive(Debug, Clone)]
pub struct EmitOptions<'a> {
    // Core settings
    pub relocatable: bool,
    pub emit_asm: bool,
    pub source_name: String,
    
    // Debug and diagnostics
    pub debug_level: u8,
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
    pub tls: TlsImage,              // Unified field, or enum for fidelity
    pub symbols: ObjectSymbols,     // Adaptation period can use enum { Elf(..), ... }
    pub relocs: ObjectRelocs,
    pub extra_sections: Vec<ExtraSection>,
    pub diagnostics: Vec<Diagnostic>,
    pub func_sizes: BTreeMap<String, u64>,
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
    
    /// 将 `compile()` 生成的内部状态序列化为最终目标文件字节 (ELF .o / ET_EXEC)。
    ///
    /// 职责分离：`compile()` 只做 IR → 机器码 + 节数据，`build_object()` 只做
    /// 容器 (ELF/COFF) 打包。必须在 `compile()` 之后调用。
    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>>;
}
