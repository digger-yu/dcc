//! 模板嵌套深度限制 (M18.x)
//!
//! C++ 模板实例化 `T<U<...>>` 在 dcc 中由 parse_mangle_type 递归解析,
//! 嵌套深度不受控时存在两个隐患:
//!   1. `looks_like_template_args_after_lt` 每层递归都克隆 lexer 向前
//!      扫描, 深度 n 时总开销 O(n^2); 实测数万层嵌套会让编译挂起;
//!   2. 递归解析本身可能触发栈溢出.
//!
//! 主流 C++ 编译器均有默认深度限制: GCC -ftemplate-depth 默认 900,
//! clang 默认 1024. dcc 作为简化实现取 128: 真实代码嵌套模板极少
//! 超过此值 (STL 容器组合通常 < 8 层), 同时足以防御上述两类问题.

/// 最大模板嵌套深度 (解析层)
pub const MAX_TEMPLATE_DEPTH: u32 = 128;

/// 深度超限错误消息
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
    fn limit_constant_sane() {
        // 128 层足够覆盖 STL 常见组合, 且远低于栈溢出阈值 (编译期断言)
        const { assert!(MAX_TEMPLATE_DEPTH >= 64) };
        const { assert!(MAX_TEMPLATE_DEPTH <= 1024) };
    }

    #[test]
    fn error_message_mentions_depth() {
        let msg = depth_error_msg(129);
        assert!(msg.contains("129"), "{}", msg);
        assert!(msg.contains("depth"), "{}", msg);
    }
}
