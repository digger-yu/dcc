//! Legacy runtime: typedef / runtime prologue / extern 声明
//!
//! 从 lower.rs 中拆分出来，负责:
//! - C++ runtime 前置声明 (new/delete/typeinfo 等)
//! - GCC 兼容运行时
//! - typedef / 类型别名前置输出
//!
//! Stage 1: 骨架 + 接口声明，具体实现逐步从 lower.rs 迁入。

/// Legacy runtime prologue 输出结果
#[derive(Debug, Default, Clone)]
pub struct LegacyRuntimePrologue {
    /// 输出的 C 源码（prologue 部分）
    pub code: String,
    /// 声明的 type
    pub declared_types: Vec<String>,
    /// 声明的函数
    pub declared_functions: Vec<String>,
}

impl LegacyRuntimePrologue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }
}

/// Legacy runtime 管理器
#[derive(Debug, Default, Clone)]
pub struct LegacyRuntime {
    /// 是否已输出过 prologue
    pub emitted: bool,
    /// 已声明的类型（避免重复）
    pub types: std::collections::HashSet<String>,
}

impl LegacyRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// 输出 runtime prologue（只输出一次）
    pub fn emit_prologue(&mut self) -> LegacyRuntimePrologue {
        if self.emitted {
            return LegacyRuntimePrologue::new();
        }
        self.emitted = true;
        // TODO(Stage 1): 从 lower.rs 的 runtime prologue 逻辑迁入
        // 当前为空，prologue 仍在 lower.rs 中生成
        LegacyRuntimePrologue::new()
    }

    /// 声明一个类型（返回 true 表示是新声明）
    pub fn declare_type(&mut self, name: &str) -> bool {
        self.types.insert(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prologue_emits_once() {
        let mut rt = LegacyRuntime::new();
        let p1 = rt.emit_prologue();
        let p2 = rt.emit_prologue();
        // 第二次应该是空的
        assert!(p2.is_empty());
        // 第一次可能非空（当前实现为空，但接口正确）
        let _ = p1;
    }

    #[test]
    fn declare_type_dedup() {
        let mut rt = LegacyRuntime::new();
        assert!(rt.declare_type("Foo"));
        assert!(!rt.declare_type("Foo"));
    }
}
