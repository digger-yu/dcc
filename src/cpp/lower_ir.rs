//! C++ HIR → dcc typed IR lowering (native path)
//!
//! 职责:
//! - 把 HIR 表达式/语句降到 dcc typed IR
//! - 使用 abi_layout 提供的布局描述符
//! - 不生成临时 C 文件，不走 C parser
//!
//! 当前阶段: 骨架 + 接口定义。
//! 随 Stage 3 开始逐步实现。

use crate::cpp::abi_layout::ClassAbi;
use crate::cpp::hir::HirTranslationUnit;

/// Lowering 结果
#[derive(Debug, Default)]
pub struct LowerIrOutput {
    /// 生成的函数（IR 函数名列表）
    pub functions: Vec<String>,
    /// 生成的全局变量
    pub globals: Vec<String>,
    /// 生成的 vtable / RTTI 等只读数据
    pub rodata: Vec<String>,
}

/// IR Lower
pub struct IrLowerer<'a> {
    /// 输入 HIR
    pub hir: &'a HirTranslationUnit,
    /// ABI 描述符表（class_id → ClassAbi）
    pub abis: std::collections::HashMap<usize, ClassAbi>,
    /// 输出
    pub output: LowerIrOutput,
}

impl<'a> IrLowerer<'a> {
    pub fn new(hir: &'a HirTranslationUnit) -> Self {
        Self {
            hir,
            abis: std::collections::HashMap::new(),
            output: LowerIrOutput::default(),
        }
    }

    /// 注册一个类的 ABI 描述符
    pub fn register_abi(&mut self, class_id: usize, abi: ClassAbi) {
        self.abis.insert(class_id, abi);
    }

    /// 执行 lowering（当前为骨架，返回空结果）
    pub fn lower(&mut self) -> Result<(), LowerIrError> {
        // TODO(Stage 3): 实现完整 lowering
        Ok(())
    }

    /// 消费 lowerer，返回输出
    pub fn finish(self) -> LowerIrOutput {
        self.output
    }
}

/// Lowering 错误
#[derive(Debug, Clone)]
pub enum LowerIrError {
    /// 不支持的 HIR 节点（auto 模式下可 fallback 到 legacy）
    Unsupported(String),
    /// 内部错误
    Internal(String),
}

impl std::fmt::Display for LowerIrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerIrError::Unsupported(s) => write!(f, "unsupported HIR feature: {}", s),
            LowerIrError::Internal(s) => write!(f, "internal lowering error: {}", s),
        }
    }
}

impl std::error::Error for LowerIrError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_empty_hir() {
        let hir = HirTranslationUnit::new();
        let mut lowerer = IrLowerer::new(&hir);
        assert!(lowerer.lower().is_ok());
        let out = lowerer.finish();
        assert!(out.functions.is_empty());
    }
}
