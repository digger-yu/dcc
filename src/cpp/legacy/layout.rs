//! Legacy layout: class layout / virtual base 模拟
//!
//! 从 lower.rs 中拆分出来，负责:
//! - 类大小、对齐计算
//! - 字段偏移计算
//! - 非虚基类 / 虚基类布局模拟
//! - vptr / vbptr 布局
//!
//! 这些是 legacy 文本降级路径的布局模拟。
//! native path 使用 abi_layout.rs（Itanium ABI 标准实现）。
//!
//! Stage 1: 骨架 + 接口声明，具体实现逐步从 lower.rs 迁入。

use std::collections::HashMap;

/// 字段布局信息
#[derive(Debug, Clone)]
pub struct FieldLayoutInfo {
    pub name: String,
    pub offset: i64,
    pub size: i64,
    pub align: i64,
}

/// 基类布局信息
#[derive(Debug, Clone)]
pub struct BaseLayoutInfo {
    pub name: String,
    pub offset: i64,
    pub is_virtual: bool,
}

/// Legacy 类布局计算器
///
/// 基于文本模式匹配和启发式规则模拟 C++ 类布局。
/// 与 abi_layout.rs 的区别:
/// - legacy 基于字符串匹配，不保证 ABI 兼容
/// - native abi_layout 严格遵循 Itanium ABI
#[derive(Debug, Default, Clone)]
pub struct LegacyLayout {
    /// 类名 -> 字段偏移表
    pub field_offsets: HashMap<String, HashMap<String, i64>>,
    /// 类名 -> 大小（字节）
    pub class_sizes: HashMap<String, i64>,
    /// 类名 -> 对齐（字节）
    pub class_aligns: HashMap<String, i64>,
    /// 类名 -> 虚基表
    pub vbase_tables: HashMap<String, Vec<i64>>,
    /// 类名 -> 传递虚基列表
    pub transitive_vbases: HashMap<String, Vec<String>>,
    /// 多态类集合（有虚函数）
    pub polymorphic: std::collections::HashSet<String>,
}

impl LegacyLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个类（空布局）
    pub fn register_class(&mut self, name: &str) {
        self.field_offsets.entry(name.to_string()).or_default();
        self.class_sizes.entry(name.to_string()).or_insert(0);
        self.class_aligns.entry(name.to_string()).or_insert(1);
    }

    /// 获取类大小
    pub fn class_size(&self, name: &str) -> Option<i64> {
        self.class_sizes.get(name).copied()
    }

    /// 获取字段偏移
    pub fn field_offset(&self, class_name: &str, field_name: &str) -> Option<i64> {
        self.field_offsets
            .get(class_name)
            .and_then(|m| m.get(field_name))
            .copied()
    }

    /// 设置字段偏移
    pub fn set_field_offset(&mut self, class_name: &str, field_name: &str, offset: i64) {
        self.field_offsets
            .entry(class_name.to_string())
            .or_default()
            .insert(field_name.to_string(), offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_class_creates_entries() {
        let mut layout = LegacyLayout::new();
        layout.register_class("Foo");
        assert_eq!(layout.class_size("Foo"), Some(0));
        assert!(layout.field_offset("Foo", "x").is_none());
    }

    #[test]
    fn field_offset_round_trip() {
        let mut layout = LegacyLayout::new();
        layout.register_class("Foo");
        layout.set_field_offset("Foo", "x", 0);
        layout.set_field_offset("Foo", "y", 4);
        assert_eq!(layout.field_offset("Foo", "x"), Some(0));
        assert_eq!(layout.field_offset("Foo", "y"), Some(4));
    }
}
