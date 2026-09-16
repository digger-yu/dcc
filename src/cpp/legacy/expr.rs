//! Legacy expr: C 表达式输出
//!
//! 从 lower.rs 中拆分出来，负责:
//! - C++ 表达式 → C 表达式字符串转换
//! - 成员访问降级
//! - 作用域解析降级
//! - 构造 / 析构调用降级
//!
//! Stage 1: 骨架 + 接口声明，具体实现逐步从 lower.rs 迁入。

/// 表达式 lowering 上下文
#[derive(Debug, Default, Clone)]
pub struct ExprLowerCtx {
    /// 当前所在类（用于 this 注入）
    pub current_class: Option<String>,
    /// 局部变量类型映射
    pub local_var_types: std::collections::HashMap<String, String>,
    /// 形参类型映射
    pub param_var_types: std::collections::HashMap<String, String>,
    /// 已知类方法
    pub class_methods: std::collections::HashMap<String, Vec<String>>,
    /// 是否在 const 方法中
    pub is_const_method: bool,
}

impl ExprLowerCtx {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_class(mut self, class: String) -> Self {
        self.current_class = Some(class);
        self
    }
}

/// Legacy 表达式 lowerer
#[derive(Debug, Default, Clone)]
pub struct LegacyExprLower {
    ctx: ExprLowerCtx,
}

impl LegacyExprLower {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ctx(ctx: ExprLowerCtx) -> Self {
        Self { ctx }
    }

    pub fn ctx(&self) -> &ExprLowerCtx {
        &self.ctx
    }

    pub fn ctx_mut(&mut self) -> &mut ExprLowerCtx {
        &mut self.ctx
    }

    /// 降低一个成员访问表达式
    /// obj.field -> 对应 C 表达式
    pub fn lower_member_access(&self, obj: &str, field: &str) -> String {
        // TODO(Stage 1): 从 lower.rs 的成员访问逻辑迁入
        // 简单版本: 直接返回 obj.field
        format!("{}.{}", obj, field)
    }

    /// 降低一个成员函数调用
    pub fn lower_member_call(&self, _obj: &str, method: &str, args: &[String]) -> String {
        // TODO(Stage 1): 从 lower.rs 迁入完整逻辑
        let args_str = args.join(", ");
        format!("{}_ZN{}{}E({})", self.class_prefix(method), method.len(), method, args_str)
    }

    fn class_prefix(&self, method: &str) -> String {
        if let Some(cls) = &self.ctx.current_class {
            format!("{}{}", cls.len(), cls)
        } else {
            format!("{}", method.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_access_simple() {
        let lower = LegacyExprLower::new();
        assert_eq!(lower.lower_member_access("obj", "x"), "obj.x");
    }

    #[test]
    fn ctx_default_is_empty() {
        let ctx = ExprLowerCtx::new();
        assert!(ctx.current_class.is_none());
        assert!(ctx.local_var_types.is_empty());
    }
}
