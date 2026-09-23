// src/codegen/shared/pipeline.rs - 完整编译流水线
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：编译阶段划分在所有 ISA 后端中是相同的
// 目标：提取通用编译器流水线逻辑

use crate::ir::Module;
use crate::codegen::EmitOptions;
use crate::diagnostics::Diagnostic;
use crate::ast;
use std::collections::HashMap;

/// Compilation phases (ISA-independent)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilePhase {
    /// Parse source → AST
    Parsing,
    /// Type check and semantic analysis
    SemanticAnalysis,
    /// Generate IR
    IRGeneration,
    /// Apply intermediate optimizations
    IRPasses,
    /// Lower IR to architecture-specific code
    CodeGeneration,
    /// Final optimization passes
    FinalOptimization,
    /// Code emission
    CodeEmission,
}

/// Compiler pipeline state
#[derive(Debug, Default)]
pub struct PipelineState {
    pub current_phase: Option<CompilePhase>,
    pub modules: HashMap<String, Module>,
    pub diagnostics: Vec<Diagnostic>,
    pub phase_stats: HashMap<CompilePhase, PhaseStats>,
}

impl PipelineState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_phase(&mut self, phase: CompilePhase) {
        self.current_phase = Some(phase);
        self.phase_stats
            .entry(phase)
            .or_insert_with(PhaseStats::new);
    }

    pub fn increment_stat(&mut self, stat: &str) {
        if let Some(stats) = self.phase_stats.get_mut(&self.current_phase.unwrap()) {
            stats.increment(stat);
        }
    }
}

/// Statistics for a compilation phase
#[derive(Debug, Default)]
pub struct PhaseStats {
    pub counters: HashMap<String, u64>,
    pub start_time: f64,
    pub end_time: f64,
}

impl PhaseStats {
    pub fn new() -> Self {
        Self {
            start_time: crate::timing::now(),
            ..Default::default()
        }
    }

    pub fn increment(&mut self, name: &str) {
        *self.counters.entry(name.to_string()).or_insert(0) += 1;
    }

    pub fn timing(&self) -> f64 {
        (self.end_time - self.start_time).max(0.0)
    }
}

/// Compilation results (ISA-independent)
#[derive(Debug)]
pub struct CompilationResult {
    pub success: bool,
    pub object_code: Vec<u8>,
    pub diagnostics: Vec<Diagnostic>,
    pub warnings_count: usize,
    pub errors_count: usize,
    pub stats: HashMap<CompilePhase, PhaseStats>,
}

impl CompilationResult {
    pub fn from_diagnostics(diagnostics: &[Diagnostic]) -> Self {
        let errors = diagnostics.iter().filter(|d| d.is_error()).count();
        let warnings = diagnostics.iter().filter(|d| d.is_warn()).count();

        Self {
            success: errors == 0,
            object_code: Vec::new(),
            diagnostics: diagnostics.to_vec(),
            warnings_count: warnings,
            errors_count: errors,
            stats: HashMap::new(),
        }
    }
}

/// Pipeline stage trait
pub trait PipelineStage {
    /// Name of the stage
    fn name(&self) -> &'static str;

    /// Execute this stage
    fn execute(
        &self,
        input: &PipelineInput,
    ) -> Result<PipelineOutput, Vec<Diagnostic>>;

    /// Check prerequisites
    fn prerequisites_met(&self, _input: &PipelineInput) -> bool {
        true
    }
}

/// Input to pipeline stages
pub struct PipelineInput<'a> {
    pub module: &'a Module,
    pub options: &'a EmitOptions<'a>,
    pub state: &'a mut PipelineState,
}

/// Output from pipeline stages
pub struct PipelineOutput {
    pub modified_module: Option<Module>,
    pub generated_code: Option<Vec<u8>>,
    pub diagnostics: Vec<Diagnostic>,
    pub extra_data: HashMap<String, Vec<u8>>,
}

impl PipelineOutput {
    pub fn new() -> Self {
        Self {
            modified_module: None,
            generated_code: None,
            diagnostics: Vec::new(),
            extra_data: HashMap::new(),
        }
    }

    pub fn with_module(module: Module) -> Self {
        Self {
            modified_module: Some(module),
            ..Default::default()
        }
    }

    pub fn with_code(code: Vec<u8>) -> Self {
        Self {
            generated_code: Some(code),
            ..Default::default()
        }
    }
}

/// Pre-defined pipeline stages
pub struct IRGenerationStage;

impl PipelineStage for IRGenerationStage {
    fn name(&self) -> &'static str {
        "IRGeneration"
    }

    fn execute(
        &self,
        input: &PipelineInput,
    ) -> Result<PipelineOutput, Vec<Diagnostic>> {
        // Convert AST → IR
        // This is where the actual IR generation happens
        let ir_module = Module::from_ast(input.module.source());
        
        Ok(PipelineOutput::with_module(ir_module))
    }

    fn prerequisites_met(&self, input: &PipelineInput) -> bool {
        // Check that parsing and semantic analysis are complete
        !input.module.source().decls.is_empty()
    }
}

pub struct OptimizationPassStage {
    pass_name: String,
    optimize_for_size: bool,
}

impl OptimizationPassStage {
    pub fn new(name: &str, optimize_for_size: bool) -> Self {
        Self {
            pass_name: name.to_string(),
            optimize_for_size,
        }
    }
}

impl PipelineStage for OptimizationPassStage {
    fn name(&self) -> &'static str {
        &self.pass_name
    }

    fn execute(
        &self,
        input: &PipelineInput,
    ) -> Result<PipelineOutput, Vec<Diagnostic>> {
        // TODO: Implement optimization passes
        // SROA, DSE, CSE, etc.
        
        let result = PipelineOutput::new();
        Ok(result)
    }
}

pub struct CodeGenerationStage<C: crate::codegen::shared::control_flow::ControlFlowBackend> {
    backend: &'static C,
}

impl<C: crate::codegen::shared::control_flow::ControlFlowBackend> CodeGenerationStage<C> {
    pub fn new(backend: &'static C) -> Self {
        Self { backend }
    }
}

impl<C: crate::codegen::shared::control_flow::ControlFlowBackend> PipelineStage
    for CodeGenerationStage<C>
{
    fn name(&self) -> &'static str {
        "CodeGeneration"
    }

    fn execute(
        &self,
        input: &PipelineInput,
    ) -> Result<PipelineOutput, Vec<Diagnostic>> {
        // Use shared control flow engine
        use crate::codegen::shared::control_flow::ControlFlowLoweringEngine;

        let ctx = crate::codegen::shared::expressions::EvalContext::new(input.module);
        let mut lowering_engine =
            ControlFlowLoweringEngine::new(self.backend, input.module);

        // Process functions
        for func in &input.module.functions {
            // TODO: generate code for each function
            eprintln!("Generating code for function: {}", func.name);
        }

        let result = PipelineOutput::new();
        Ok(result)
    }
}

/// Main compiler pipeline orchestrator
pub struct CompilerPipeline {
    stages: Vec<Box<dyn PipelineStage>>,
    state: PipelineState,
}

impl CompilerPipeline {
    pub fn new() -> Self {
        Self {
            stages: Vec::new(),
            state: PipelineState::new(),
        }
    }

    pub fn add_stage(&mut self, stage: Box<dyn PipelineStage>) {
        self.stages.push(stage);
    }

    pub fn execute(
        &mut self,
        module: &Module,
        options: &EmitOptions,
    ) -> Result<PipelineOutput, Vec<Diagnostic>> {
        let mut output = PipelineOutput::new();
        let mut current_input = PipelineInput {
            module,
            options,
            state: &mut self.state,
        };

        for stage in &self.stages {
            self.state.set_phase(match stage.name() {
                "IRGeneration" => CompilePhase::IRGeneration,
                "IRPasses" | "SROA" | "DSE" | "CSE" => CompilePhase::IRPasses,
                "CodeGeneration" => CompilePhase::CodeGeneration,
                _ => CompilePhase::CodeGeneration,
            });

            if !stage.prerequisites_met(&current_input) {
                return Err(vec![Diagnostic::err(
                    "E0500",
                    format!(
                        "Prerequisites not met for stage: {}",
                        stage.name()
                    ),
                    crate::diagnostics::Span::point("", 0, 0),
                )]);
            }

            match stage.execute(&current_input) {
                Ok(stage_output) => {
                    if let Some(mod_module) = stage_output.modified_module {
                        current_input.module = &mod_module;
                    }
                    if let Some(code) = stage_output.generated_code {
                        output.generated_code = Some(code);
                    }
                }
                Err(diags) => {
                    self.state.diagnostics.extend(diags);
                    return Err(self.state.diagnostics.clone());
                }
            }

            self.state.increment("instructions_processed");
        }

        Ok(output)
    }

    pub fn get_stats(&self) -> &HashMap<CompilePhase, PhaseStats> {
        &self.state.phase_stats
    }
}

impl Default for CompilerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_execution() {
        let mut pipeline = CompilerPipeline::new();
        pipeline.add_stage(Box::new(IRGenerationStage));

        // Create minimal mock module
        let module = Module::default();
        let options = EmitOptions {
            relocatable: false,
            emit_asm: false,
            source_name: "test.c".to_string(),
            debug_level: 0,
            dwarf_version: 4,
            sanitizers: Default::default(),
            lto_enabled: false,
            function_sections: false,
            parallel_codegen: false,
            bounds_check: false,
            trapv: false,
            offload: None,
            line_map: &[],
            verbose: false,
        };

        let result = pipeline.execute(&module, &options);
        assert!(result.is_ok());
    }

    #[test]
    fn test_phase_tracking() {
        let mut state = PipelineState::new();
        state.set_phase(CompilePhase::IRGeneration);
        assert_eq!(state.current_phase, Some(CompilePhase::IRGeneration));

        state.increment_stat("functions_generated");
        state.increment_stat("functions_generated");
        state.increment_stat("variables_declared");

        let stats = state.phase_stats.get(&CompilePhase::IRGeneration).unwrap();
        assert_eq!(*stats.counters.get("functions_generated").unwrap(), 2);
    }
}
