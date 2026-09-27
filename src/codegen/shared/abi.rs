// src/codegen/shared/abi.rs - ABI 调用约定框架
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：ABI 规范虽然因 ISA 而异，但参数传递、返回值、栈帧布局的逻辑模式相同
// 目标：提取通用 ABI 逻辑，减少每个后端的重复实现

use crate::ast::Type;
use std::collections::HashMap;

/// Calling convention types
#[derive(Debug, Clone)]
pub enum CallingConvention {
    /// Standard C calling convention
    CDecl,
    /// Fast call (optimized)
    FastCall,
    /// System V AMD64 ABI (x86_64 Linux/macOS)
    SystemVAMD64,
    /// Microsoft x64 (Windows)
    MicrosoftX64,
    /// System V ARM64 AArch64
    SystemVAarch64,
    /// RISC-V ABI
    RiscV,
    /// Custom/convention for special cases
    Custom(String),
}

/// Parameter passing information
#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub ty: Type,
    pub register: Option<String>,
    pub stack_offset: i32,
    pub is_variadic: bool,
}

impl Default for ParamInfo {
    fn default() -> Self {
        Self {
            ty: Type::Void,
            register: None,
            stack_offset: 0,
            is_variadic: false,
        }
    }
}

/// Return value location
#[derive(Debug, Clone)]
pub enum ReturnValueLocation {
    /// Single register
    Register(String),
    /// Multiple registers (for structs/unions)
    Registers(Vec<String>),
    /// Memory location (passed via pointer)
    Memory,
    /// Stack location
    Stack(i32),
}

/// Function signature information
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub return_ty: Type,
    pub is_variadic: bool,
    pub cconv: CallingConvention,
    pub needs_return_ptr: bool,
}

impl FunctionSignature {
    pub fn new(name: &str, return_ty: Type, cconv: CallingConvention) -> Self {
        Self {
            name: name.to_string(),
            params: Vec::new(),
            return_ty,
            is_variadic: false,
            cconv,
            needs_return_ptr: false,
        }
    }

    pub fn add_param(&mut self, ty: Type) {
        self.params.push(ParamInfo {
            ty,
            ..Default::default()
        });
    }
}

/// ABI lowering engine (ISA-independent logic)
pub struct ABILoweringEngine {
    cconv: CallingConvention,
    reg_classes: HashMap<TypeClass, Vec<String>>,
}

/// Type classification for ABI purposes
#[derive(Debug, Clone, Copy)]
pub enum TypeClass {
    Integer,
    Float,
    Vector,
    Aggregate,
}

impl ABILoweringEngine {
    pub fn new(cconv: CallingConvention) -> Self {
        Self {
            cconv,
            reg_classes: HashMap::new(),
        }
    }

    /// Determine parameter passing strategy
    pub fn determine_param_passing(&self, params: &[Type]) -> Vec<ParamInfo> {
        params
            .iter()
            .map(|ty| self.param_info_for_type(ty))
            .collect()
    }

    /// Build the default [`ParamInfo`] for a single parameter type.
    fn param_info_for_type(&self, ty: &Type) -> ParamInfo {
        ParamInfo {
            ty: ty.clone(),
            ..Default::default()
        }
    }

    /// Get register allocation for function parameters
    pub fn alloc_params_registers(&self, params: &[Type]) -> Vec<Option<String>> {
        let mut result: Vec<Option<String>> = Vec::new();

        match &self.cconv {
            CallingConvention::SystemVAMD64 => {
                // First 6 integer/pointer args in RDI, RSI, RDX, RCX, R8, R9
                let gp_regs = ["RDI", "RSI", "RDX", "RCX", "R8", "R9"];
                // First 8 floating-point args in XMM0-XMM7
                let fp_regs = [
                    "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
                ];

                // SysV: integer args are placed in GP slots by argument
                // position (so the 3rd argument lands in RDX even when an
                // FP arg intervenes), while XMM slots are numbered by the
                // count of floating arguments (the first FP arg is XMM0,
                // not XMM1).
                let mut fp_next = 0usize;
                for (idx, ty) in params.iter().enumerate() {
                    if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                        if fp_next < fp_regs.len() {
                            result.push(Some(fp_regs[fp_next].to_string()));
                            fp_next += 1;
                        } else {
                            result.push(None); // Stack
                        }
                    } else if idx < gp_regs.len() {
                        result.push(Some(gp_regs[idx].to_string()));
                    } else {
                        result.push(None); // Stack
                    }
                }
            },
            CallingConvention::SystemVAarch64 => {
                // First 8 arguments in X0-X7
                for idx in 0..params.len() {
                    if idx < 8 {
                        result.push(Some(format!("X{}", idx)));
                    } else {
                        result.push(None);
                    }
                }
            },
            CallingConvention::CDecl | CallingConvention::FastCall => {
                // Conservative approach: all on stack or first few in registers
                for _ty in params {
                    result.push(None); // Simplified
                }
            },
            _ => {
                // Default: conservative stack-based
                for _ty in params {
                    result.push(None);
                }
            },
        }

        result
    }

    /// Determine return value location
    pub fn alloc_return_location(&self, ty: &Type) -> ReturnValueLocation {
        if ty.size() <= 8 {
            ReturnValueLocation::Register("RAX".to_string())
        } else if ty.size() <= 16 {
            ReturnValueLocation::Registers(vec!["RAX".to_string(), "RDX".to_string()])
        } else {
            ReturnValueLocation::Memory
        }
    }

    /// Build callee-saved register set
    pub fn callee_saved_registers(&self) -> Vec<String> {
        match &self.cconv {
            CallingConvention::SystemVAMD64 => {
                vec![
                    "RBX".to_string(),
                    "RBP".to_string(),
                    "R12".to_string(),
                    "R13".to_string(),
                    "R14".to_string(),
                    "R15".to_string(),
                ]
            },
            CallingConvention::MicrosoftX64 => {
                vec![
                    "RBX".to_string(),
                    "RBP".to_string(),
                    "R12".to_string(),
                    "R13".to_string(),
                    "R14".to_string(),
                    "R15".to_string(),
                ]
            },
            CallingConvention::SystemVAarch64 => [
                "X19".to_string(),
                "X20".to_string(),
                "X21".to_string(),
                "X22".to_string(),
                "X23".to_string(),
                "X24".to_string(),
                "X25".to_string(),
                "X26".to_string(),
                "X27".to_string(),
                "X28".to_string(),
            ]
            .to_vec(),
            _ => Vec::new(),
        }
    }

    /// Calculate stack frame alignment requirement
    pub fn stack_alignment(&self) -> usize {
        match &self.cconv {
            CallingConvention::SystemVAMD64 => 16,
            CallingConvention::MicrosoftX64 => 16,
            CallingConvention::SystemVAarch64 => 16,
            _ => 8,
        }
    }

    /// Determine if red zone is available
    pub fn has_red_zone(&self) -> bool {
        matches!(&self.cconv, CallingConvention::SystemVAMD64 | CallingConvention::SystemVAarch64)
    }

    /// Calculate required stack space for local variables
    pub fn calc_stack_space(&self, locals: &[LocalVar], param_count: usize) -> usize {
        let align = self.stack_alignment();
        let mut total_size = 0;

        for local in locals {
            let aligned_size = local.size.div_ceil(align) * align;
            total_size += aligned_size.max(local.ty.size());
        }

        // Add space for parameters passed on stack
        let stack_param_bytes = std::cmp::max(param_count - 8, 0) * 8;
        total_size += stack_param_bytes;

        // Round to alignment boundary
        total_size.div_ceil(align) * align
    }
}

/// Local variable information
#[derive(Debug, Clone)]
pub struct LocalVar {
    pub name: String,
    pub ty: Type,
    pub size: usize,
    pub offset: i32,
}

/// ABI utilities
pub struct ABIUtils;

impl ABIUtils {
    /// Normalize type for ABI purposes (promote char/short to int)
    pub fn normalize_for_abi(ty: &Type) -> Type {
        match ty {
            Type::Char | Type::SChar | Type::UChar => Type::Int,
            Type::Short | Type::UShort => Type::Int,
            _ => ty.clone(),
        }
    }

    /// Check if type should be passed in register
    pub fn should_use_register(ty: &Type, reg_size: usize) -> bool {
        ty.size() <= reg_size
    }

    /// Calculate structure return optimization
    pub fn uses_sroa(ty: &Type) -> bool {
        // Small structs returned in registers
        ty.size() <= 16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_v_amd64_param_passing() {
        let engine = ABILoweringEngine::new(CallingConvention::SystemVAMD64);

        let params = vec![
            Type::Int,
            Type::Double,
            Type::Ptr(Box::new(crate::ast::QualType::new(Type::Int))),
        ];
        let regs = engine.alloc_params_registers(&params);

        assert_eq!(regs[0], Some("RDI".to_string()));
        assert_eq!(regs[1], Some("XMM0".to_string()));
        assert_eq!(regs[2], Some("RDX".to_string()));
    }

    #[test]
    fn test_aarch64_param_passing() {
        let engine = ABILoweringEngine::new(CallingConvention::SystemVAarch64);

        let params = vec![Type::Int; 10];
        let regs = engine.alloc_params_registers(&params);

        // First 8 in registers
        for (i, reg) in regs.iter().enumerate().take(8) {
            assert_eq!(*reg, Some(format!("X{}", i)));
        }

        // Rest on stack
        for reg in regs.iter().skip(8) {
            assert_eq!(*reg, None);
        }
    }

    #[test]
    fn test_callee_saved_registers() {
        let engine = ABILoweringEngine::new(CallingConvention::SystemVAMD64);
        let saved = engine.callee_saved_registers();

        assert!(saved.contains(&"RBX".to_string()));
        assert!(saved.contains(&"R12".to_string()));
        assert_eq!(saved.len(), 6);
    }
}
