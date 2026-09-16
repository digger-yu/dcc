//! Itanium C++ ABI: class layout, vtable, RTTI, thunks.
//!
//! 职责:
//! - class layout: field offset, non-virtual base, virtual base, vptr
//! - vtable generation: offset-to-top, typeinfo, virtual function slots, thunks
//! - RTTI: type_info, class type info, base class array
//! - this adjustment descriptor
//!
//! 设计原则:
//! - ABI layer 是 class layout 的**唯一来源**
//! - lower_ir 只能查询 ABI descriptor，不能自己重新计算偏移
//! - 所有 vtable/vbase/thunk/RTTI symbol 都由 ABI layer 分配
//!
//! 当前阶段: 类型定义骨架。实现随 Stage 4/5/6/7 推进逐步填充。

use crate::cpp::sema::ClassId;

// === Layout 描述符 ===

/// 字段布局
#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub align: u64,
}

/// 非虚基类布局
#[derive(Debug, Clone)]
pub struct BaseLayout {
    pub base: ClassId,
    pub offset: u64,
    pub is_primary: bool,
}

/// 虚基类布局
#[derive(Debug, Clone)]
pub struct VirtualBaseLayout {
    pub base: ClassId,
    /// vtable 中的 offset 位置（相对 address point）
    pub vtable_offset: i64,
    /// 虚基表索引
    pub vbase_index: usize,
}

/// vptr 布局
#[derive(Debug, Clone)]
pub struct VPtrLayout {
    pub offset: u64,
    /// 对应的 vtable
    pub vtable_index: usize,
}

/// 类 ABI 描述符
#[derive(Debug, Clone, Default)]
pub struct ClassAbi {
    pub class_id: ClassId,
    pub size: u64,
    pub align: u64,
    pub fields: Vec<FieldLayout>,
    pub bases: Vec<BaseLayout>,
    pub virtual_bases: Vec<VirtualBaseLayout>,
    pub primary_base: Option<ClassId>,
    pub vptrs: Vec<VPtrLayout>,
    pub vtables: Vec<VTableLayout>,
    pub rtti: RttiLayout,
}

// === VTable ===

/// vtable 布局
#[derive(Debug, Clone, Default)]
pub struct VTableLayout {
    /// 此 vtable 的全局符号名
    pub symbol: String,
    /// vtable 中 address point 的偏移（相对 vtable 起点）
    /// 也就是 vptr 指向的位置
    pub address_point: i64,
    /// vtable 组件（按偏移顺序）
    pub components: Vec<VTableComponent>,
}

/// vtable 条目
#[derive(Debug, Clone)]
pub enum VTableComponent {
    /// offset-to-top (i64)
    OffsetToTop(i64),
    /// typeinfo 指针
    TypeInfo(String),
    /// 虚函数指针
    Function {
        symbol: String,
        adjustment: ThisAdjust,
    },
    /// vcall offset
    VCallOffset(i64),
    /// vbase offset
    VBaseOffset { target: ClassId, offset: i64 },
    /// 空槽（unused）
    Unused,
}

// === this 调整 ===

/// this 指针调整描述符
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThisAdjust {
    /// 非虚偏移（字节）
    pub non_virtual: i64,
    /// 虚基偏移（虚继承时使用；None 表示无虚调整）
    pub vbase_index: Option<usize>,
}

impl ThisAdjust {
    pub fn is_zero(&self) -> bool {
        self.non_virtual == 0 && self.vbase_index.is_none()
    }

    pub fn offset(non_virtual: i64) -> Self {
        Self {
            non_virtual,
            vbase_index: None,
        }
    }
}

// === RTTI ===

/// RTTI 布局
#[derive(Debug, Clone, Default)]
pub struct RttiLayout {
    /// typeinfo 符号名 (e.g. _ZTI3Foo)
    pub typeinfo_symbol: String,
    /// 是否有虚基类（影响 RTTI 结构类型）
    pub has_virtual_bases: bool,
    /// 基类列表
    pub base_classes: Vec<RttiBaseClass>,
}

#[derive(Debug, Clone)]
pub struct RttiBaseClass {
    pub base: ClassId,
    pub offset: i64,
    pub is_virtual: bool,
    pub flags: u32,
}

// === Thunk ===

/// thunk 描述符
#[derive(Debug, Clone)]
pub struct ThunkDesc {
    pub symbol: String,
    pub target_symbol: String,
    pub this_adjust: ThisAdjust,
    /// 返回值调整（多用于协变返回类型）
    pub return_adjust: Option<ThisAdjust>,
}

// === ABI 计算器接口 ===

/// ABI 计算器 trait。
/// 输入类定义，输出 ClassAbi 描述符。
pub trait AbiCalculator {
    fn compute_class(&self, class: ClassId) -> ClassAbi;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_adjust_default_is_zero() {
        let a = ThisAdjust::default();
        assert!(a.is_zero());
        assert_eq!(a.non_virtual, 0);
        assert!(a.vbase_index.is_none());
    }

    #[test]
    fn this_adjust_offset() {
        let a = ThisAdjust::offset(16);
        assert_eq!(a.non_virtual, 16);
        assert!(a.vbase_index.is_none());
        assert!(!a.is_zero());
    }

    #[test]
    fn class_abi_default() {
        let abi = ClassAbi::default();
        assert_eq!(abi.size, 0);
        assert_eq!(abi.align, 0);
        assert!(abi.fields.is_empty());
        assert!(abi.vtables.is_empty());
    }
}
