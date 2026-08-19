//! 模板嵌套深度限制。
//!
//! C++ 模板实例化 `T<U<...>>` 在 dcc 中由 `parse_mangle_type`
//! 递归解析。为与 GCC 的默认 `-ftemplate-depth=900` 保持兼容，
//! dcc 采用相同的默认上限；这也避免真实 GCC 源码在解析阶段因
//! 过低的实现限制而被拒绝。
//!
//! 解析器仍会在超过此上限时诊断，防止恶意输入导致递归栈耗尽。

/// GCC 默认的最大模板嵌套深度。
pub const MAX_TEMPLATE_DEPTH: u32 = 900;

/// 深度超限错误消息。
pub fn depth_error_msg(depth: u32) -> String {
    format!(
        "template nesting depth {} exceeds limit {}",
        depth, MAX_TEMPLATE_DEPTH
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_gcc_default_depth() {
        assert_eq!(MAX_TEMPLATE_DEPTH, 900);
    }

    #[test]
    fn error_message_mentions_depth() {
        let msg = depth_error_msg(MAX_TEMPLATE_DEPTH + 1);
        assert!(msg.contains("901"), "{}", msg);
        assert!(msg.contains("depth"), "{}", msg);
    }
}
