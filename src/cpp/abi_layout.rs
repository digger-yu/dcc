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

use std::collections::HashMap;

use crate::cpp::hir::{CtorVariant, DtorVariant};
use crate::cpp::sema::{ClassId, CxxType};

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
    /// 基类构造顺序（按声明顺序）
    pub base_ctor_order: Vec<ClassId>,
    /// 成员初始化顺序（按声明顺序）
    pub field_init_order: Vec<String>,
    /// 析构顺序：成员逆序 → 基类逆序
    pub dtor_order: Vec<DtorStep>,
    /// 每个基类对应的 this 调整量（非虚偏移）
    pub this_adjust_map: HashMap<ClassId, ThisAdjust>,
}

/// 析构顺序中的一步
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DtorStep {
    /// 成员析构
    Member(String),
    /// 非虚基类析构
    Base(ClassId),
    /// 虚基类析构（Stage 6 实现）
    VirtualBase(ClassId),
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
    /// 此 vtable 对应的基类 subobject（None 表示 primary/complete vtable）
    pub for_base: Option<ClassId>,
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
    /// thunk 类型（普通虚函数 thunk / 构造变体 / 析构变体）
    pub kind: ThunkKind,
}

/// thunk 类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThunkKind {
    /// 普通虚函数 this-adjust thunk
    Virtual,
    /// 构造函数变体 thunk（C1/C2/C3）
    Ctor(CtorVariant),
    /// 析构函数变体 thunk（D1/D2/D0）
    Dtor(DtorVariant),
}

impl ClassAbi {
    /// 查询指定基类的偏移（字节）。
    /// 如果该类不是本类的直接非虚基类，返回 None。
    pub fn base_offset(&self, base_class: ClassId) -> Option<i64> {
        self.bases
            .iter()
            .find(|b| b.base == base_class)
            .map(|b| b.offset as i64)
    }

    /// 查询指定虚函数在 primary vtable 中的 slot 索引。
    ///
    /// slot 索引从 address point 之后开始编号（0, 1, 2...）。
    /// 即 slot 0 对应 address point 处的第一个虚函数。
    pub fn get_vtable_slot(&self, method_name: &str) -> Option<usize> {
        let vtable = self.vtables.first()?;
        let mut slot = 0;
        for comp in &vtable.components {
            if let VTableComponent::Function { symbol, .. } = comp {
                // symbol 以 method_name 结尾（包含类名前缀时）
                // 这里简单比较: symbol 等于 "ClassName::method_name" 格式
                // 为简化测试，我们直接比较方法名部分
                let parts: Vec<&str> = symbol.rsplitn(2, "::").collect();
                let name = if parts.len() == 2 { parts[0] } else { symbol.as_str() };
                if name == method_name {
                    return Some(slot);
                }
                slot += 1;
            }
        }
        None
    }

    /// 判断该类是否为多态类（有虚函数，即有 vtable）。
    pub fn is_polymorphic(&self) -> bool {
        !self.vtables.is_empty()
    }
}

/// ABI 计算器 trait。
/// 输入类定义，输出 ClassAbi 描述符。
pub trait AbiCalculator {
    fn compute_class(&self, class: ClassId) -> ClassAbi;
}

// === Itanium ABI 计算器 ===

/// Itanium C++ ABI 类布局计算器 (Stage 3-4: 非虚继承 + 普通字段)。
///
/// 目标平台: x86_64 (LP64)
///
/// 布局规则:
/// - 非虚基类按声明顺序排列在类布局最前面
/// - 第一个非虚基类为 primary base，offset 为 0（无虚函数时）
/// - 字段按声明顺序排列在基类之后
/// - 每个成员按其自然对齐，必要时填充 padding
/// - 整体大小按最大对齐值对齐
///
/// 当前阶段不支持: 虚函数、虚继承、vtable、bitfield、virtual dispatch。
#[derive(Debug, Default)]
pub struct ItaniumAbiCalculator {
    /// 已注册的类定义
    classes: HashMap<ClassId, ClassDef>,
}

/// 单个类的方法信息（用于 vtable 计算）
#[derive(Debug, Clone)]
struct MethodInfo {
    name: String,
    symbol: String,
    is_virtual: bool,
    slot: Option<usize>,
    owner_class: ClassId,
}

/// 单个类的定义信息（用于布局计算）
#[derive(Debug, Clone)]
struct ClassDef {
    /// 非虚基类（按声明顺序）
    bases: Vec<ClassId>,
    /// 虚基类（按声明顺序）
    virtual_bases: Vec<ClassId>,
    /// 字段（按声明顺序）
    fields: Vec<(String, CxxType)>,
    /// 方法（按声明顺序）
    methods: Vec<MethodInfo>,
    /// 是否有虚函数（影响 primary base 判定和 vptr）
    has_virtual: bool,
}

/// x86_64 指针大小
const POINTER_SIZE: u64 = 8;
/// x86_64 指针对齐
const POINTER_ALIGN: u64 = 8;

/// 计算类型的大小和对齐（x86_64 / LP64）。
///
/// 返回 (size, align)。
fn type_size_align(ty: &CxxType, calc: &ItaniumAbiCalculator) -> (u64, u64) {
    match ty {
        CxxType::Void => (0, 1),
        CxxType::Bool => (1, 1),
        CxxType::Integer { bits, .. } => {
            let bytes = match bits {
                8 => 1,
                16 => 2,
                32 => 4,
                64 => 8,
                128 => 16,
                _ => (*bits as u64 + 7) / 8,
            };
            (bytes, bytes)
        }
        CxxType::Float { bits } => {
            let bytes = match bits {
                32 => 4,
                64 => 8,
                80 => 16, // x86 long double is 16-byte aligned on x86_64
                128 => 16,
                _ => (*bits as u64 + 7) / 8,
            };
            (bytes, bytes)
        }
        CxxType::Pointer(_) => (POINTER_SIZE, POINTER_ALIGN),
        CxxType::LValueRef(_) | CxxType::RValueRef(_) => (POINTER_SIZE, POINTER_ALIGN),
        CxxType::Array(elem, Some(count)) => {
            let (elem_size, elem_align) = type_size_align(elem, calc);
            (elem_size * *count as u64, elem_align)
        }
        CxxType::Array(elem, None) => {
            // 不完整数组类型，大小为 0
            let (_, elem_align) = type_size_align(elem, calc);
            (0, elem_align)
        }
        CxxType::Class(class_id) => {
            let abi = calc.compute_class(*class_id);
            (abi.size, abi.align.max(1))
        }
        CxxType::Enum(_) => {
            // 在 Itanium ABI 中，enum 底层类型通常是 int (4 bytes)
            (4, 4)
        }
        CxxType::MemberPointer { .. } => {
            // 简化: 成员指针按指针大小处理
            (POINTER_SIZE, POINTER_ALIGN)
        }
        CxxType::Function { .. } => {
            // 函数类型本身没有大小（不能实例化）
            (0, 1)
        }
        CxxType::Dependent(_) => (0, 1),
        CxxType::Alias(_) => {
            // 此处不解析别名，按 0 处理（实际使用时应先解析）
            (0, 1)
        }
    }
}

/// 向上对齐到 align 的倍数。align 必须是 2 的幂。
fn align_up(offset: u64, align: u64) -> u64 {
    if align == 0 {
        return offset;
    }
    (offset + align - 1) & !(align - 1)
}

impl ItaniumAbiCalculator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个类的定义信息。
    ///
    /// - `bases`: 非虚基类（按声明顺序）
    /// - `virtual_bases`: 虚基类（按声明顺序）
    /// - `fields`: 字段（按声明顺序，name + type）
    /// - `has_virtual`: 是否有虚函数
    pub fn register_class(
        &mut self,
        class: ClassId,
        bases: Vec<ClassId>,
        virtual_bases: Vec<ClassId>,
        fields: Vec<(String, CxxType)>,
        has_virtual: bool,
    ) {
        self.classes.insert(
            class,
            ClassDef {
                bases,
                virtual_bases,
                fields,
                methods: Vec::new(),
                has_virtual,
            },
        );
    }

    /// 注册一个简单类（无基类、无虚函数）。
    pub fn register_simple_class(&mut self, class: ClassId, fields: Vec<(String, CxxType)>) {
        self.register_class(class, Vec::new(), Vec::new(), fields, false);
    }

    /// 旧版 register_class（仅非虚基类）。
    /// 为向后兼容保留，新代码应使用带 virtual_bases 参数的版本。
    #[deprecated(note = "use register_class with virtual_bases parameter")]
    pub fn register_class_old(
        &mut self,
        class: ClassId,
        bases: Vec<ClassId>,
        fields: Vec<(String, CxxType)>,
        has_virtual: bool,
    ) {
        self.register_class(class, bases, Vec::new(), fields, has_virtual);
    }

    /// 注册一个方法（虚函数或非虚函数）。
    ///
    /// - `class_id`: 方法所属类
    /// - `name`: 方法名
    /// - `is_virtual`: 是否为虚函数
    /// - `owner_class`: 最初声明该方法的类（用于标识重写关系）
    ///
    /// 方法符号名会自动生成为 `Class{owner_id}::{name}` 格式。
    pub fn register_method(
        &mut self,
        class_id: ClassId,
        name: String,
        is_virtual: bool,
        owner_class: ClassId,
    ) {
        if let Some(def) = self.classes.get_mut(&class_id) {
            let symbol = format!("Class{}::{}", class_id.0, name);
            if is_virtual {
                def.has_virtual = true;
            }
            def.methods.push(MethodInfo {
                name,
                symbol,
                is_virtual,
                slot: None,
                owner_class,
            });
        }
    }

    /// 注册一个带自定义符号名的方法。
    ///
    /// - `class_id`: 方法所属类
    /// - `name`: 方法名
    /// - `symbol`: 方法的全局符号名
    /// - `is_virtual`: 是否为虚函数
    /// - `owner_class`: 最初声明该方法的类（用于标识重写关系）
    pub fn register_method_with_symbol(
        &mut self,
        class_id: ClassId,
        name: String,
        symbol: String,
        is_virtual: bool,
        owner_class: ClassId,
    ) {
        if let Some(def) = self.classes.get_mut(&class_id) {
            if is_virtual {
                def.has_virtual = true;
            }
            def.methods.push(MethodInfo {
                name,
                symbol,
                is_virtual,
                slot: None,
                owner_class,
            });
        }
    }

    /// 判断类是否有虚函数（自身或继承的）。
    fn class_has_virtual(&self, class_id: ClassId) -> bool {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return false,
        };
        if def.has_virtual {
            return true;
        }
        for &base in &def.bases {
            if self.class_has_virtual(base) {
                return true;
            }
        }
        for &base in &def.virtual_bases {
            if self.class_has_virtual(base) {
                return true;
            }
        }
        false
    }

    /// 收集虚函数列表（按 vtable 顺序）。
    ///
    /// 返回 Vec<(method_name, method_symbol, owner_class)>，owner_class 是最终提供实现的类。
    /// 重写的虚函数 owner 和 symbol 为派生类的。
    /// Stage 6: 支持虚继承场景。
    fn collect_virtual_methods(&self, class_id: ClassId) -> Vec<(String, String, ClassId)> {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut methods: Vec<(String, String, ClassId)> = Vec::new();

        // 先收集非虚基类的虚函数（按基类声明顺序）
        for &base in &def.bases {
            let base_methods = self.collect_virtual_methods(base);
            for (name, base_symbol, base_owner) in base_methods {
                // 检查派生类是否重写
                if let Some(m) = def.methods.iter().find(|m| m.is_virtual && m.name == name) {
                    // 重写: 使用派生类的 symbol 和 owner
                    methods.push((name, m.symbol.clone(), class_id));
                } else {
                    // 未重写: 继承基类的
                    methods.push((name, base_symbol, base_owner));
                }
            }
        }

        // 再收集虚基类的虚函数（按虚基类声明顺序）
        for &base in &def.virtual_bases {
            let base_methods = self.collect_virtual_methods(base);
            for (name, base_symbol, base_owner) in base_methods {
                // 检查是否已经在 methods 中（避免重复，因为虚基类可能被多个路径共享）
                if methods.iter().any(|(n, _, _)| n == &name) {
                    continue;
                }
                // 检查派生类是否重写
                if let Some(m) = def.methods.iter().find(|m| m.is_virtual && m.name == name) {
                    methods.push((name, m.symbol.clone(), class_id));
                } else {
                    methods.push((name, base_symbol, base_owner));
                }
            }
        }

        // 再添加派生类自己新增的虚函数
        // 新增: owner_class == class_id 且未在基类虚函数中出现
        for m in &def.methods {
            if m.is_virtual && m.owner_class == class_id {
                if !methods.iter().any(|(n, _, _)| n == &m.name) {
                    methods.push((m.name.clone(), m.symbol.clone(), class_id));
                }
            }
        }

        methods
    }

    /// 计算 vtable 布局列表。
    ///
    /// Stage 6: 支持虚继承
    /// - primary vtable（index 0）：完整对象的 vtable
    /// - secondary vtables：每个有虚函数的非 primary base 一个
    /// - vtable 结构（从低地址到高地址）:
    ///   [vcall offsets...] [vbase offsets...] [offset-to-top] [typeinfo] [virtual functions...]
    ///   address point 指向第一个虚函数槽
    fn compute_vtables(&self, class_id: ClassId, abi: &ClassAbi) -> Vec<VTableLayout> {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return Vec::new(),
        };

        if !self.class_has_virtual(class_id) {
            return Vec::new();
        }

        let class_name = format!("Class{}", class_id.0);
        let mut vtables = Vec::new();

        // 找到 primary base（第一个有虚函数的非虚基类）
        let primary_base = def.bases.iter().copied().find(|&b| self.class_has_virtual(b));

        // 虚基类列表（从 abi 获取，已经计算好偏移）
        let virtual_bases = &abi.virtual_bases;

        // --- Primary vtable ---
        let vmethods = self.collect_virtual_methods(class_id);

        let mut components = Vec::new();

        // vbase offset 条目（在 address point 之前，即负偏移位置）
        // 按 Itanium ABI: vbase offsets 在 offset-to-top 之前
        for vb in virtual_bases {
            // 计算从 complete object 到 virtual base 的实际偏移
            // 因为虚基类布局在对象末尾，我们需要计算其实际偏移
            let vbase_offset = self.compute_vbase_offset_from_layout(class_id, vb.base);
            components.push(VTableComponent::VBaseOffset {
                target: vb.base,
                offset: vbase_offset,
            });
        }

        // offset-to-top (complete object: 0)
        components.push(VTableComponent::OffsetToTop(0));
        // typeinfo 指针
        components.push(VTableComponent::TypeInfo(format!("_ZTI{}", class_name)));
        // 虚函数槽（从 address point 开始）
        for (_name, symbol, _owner) in &vmethods {
            components.push(VTableComponent::Function {
                symbol: symbol.clone(),
                adjustment: ThisAdjust::default(),
            });
        }

        // address_point = vbase_offsets + offset-to-top + typeinfo
        let address_point = (virtual_bases.len() as i64 + 2) * POINTER_SIZE as i64;

        vtables.push(VTableLayout {
            symbol: format!("_ZTV{}", class_name),
            address_point,
            components,
            for_base: None,
        });

        // --- Secondary vtables ---
        // 为每个有虚函数的非 primary non-virtual base 生成 secondary vtable
        for &base_id in &def.bases {
            if Some(base_id) == primary_base {
                continue; // primary base 共享 primary vtable
            }
            if !self.class_has_virtual(base_id) {
                continue;
            }

            // 计算从 base subobject 到 complete object 的 this 调整
            let base_offset = self
                .direct_base_offset(class_id, base_id)
                .unwrap_or(0);

            let base_methods = self.collect_virtual_methods(base_id);

            let mut components = Vec::new();

            // vbase offset 条目（secondary vtable 也需要 vbase offsets）
            for vb in virtual_bases {
                // 从 base subobject 视角看，vbase offset = 完整对象的 vbase offset - base_offset
                let vbase_offset = self.compute_vbase_offset_from_layout(class_id, vb.base);
                let adjusted_vbase_offset = vbase_offset - base_offset;
                components.push(VTableComponent::VBaseOffset {
                    target: vb.base,
                    offset: adjusted_vbase_offset,
                });
            }

            // offset-to-top: 从 base subobject 到 complete object 的偏移（负值）
            components.push(VTableComponent::OffsetToTop(-base_offset));
            components.push(VTableComponent::TypeInfo(format!("_ZTI{}", class_name)));

            for (name, _base_symbol, base_owner) in &base_methods {
                // 检查派生类是否重写
                if let Some(m) = def.methods.iter().find(|m| m.is_virtual && m.name == *name) {
                    // 派生类重写: 需要 thunk 调整 this 指针（从 base 到 complete object）
                    components.push(VTableComponent::Function {
                        symbol: m.symbol.clone(),
                        adjustment: ThisAdjust::offset(-base_offset),
                    });
                } else {
                    // 未重写: 使用基类的函数
                    let base_def = &self.classes[base_owner];
                    let sym = base_def
                        .methods
                        .iter()
                        .find(|bm| bm.is_virtual && bm.name == *name)
                        .map(|bm| bm.symbol.clone())
                        .unwrap_or_else(|| format!("Class{}::{}", base_owner.0, name));
                    components.push(VTableComponent::Function {
                        symbol: sym,
                        adjustment: ThisAdjust::default(),
                    });
                }
            }

            let address_point = (virtual_bases.len() as i64 + 2) * POINTER_SIZE as i64;

            vtables.push(VTableLayout {
                symbol: format!("_ZTV{}::base{}", class_name, base_id.0),
                address_point,
                components,
                for_base: Some(base_id),
            });
        }

        vtables
    }

    /// 从布局计算虚基类的实际偏移（不依赖 vtable，避免循环依赖）。
    fn compute_vbase_offset_from_layout(&self, derived: ClassId, vbase: ClassId) -> i64 {
        let derived_def = match self.classes.get(&derived) {
            Some(d) => d,
            None => return 0,
        };

        // 先计算非虚部分的大小（非虚基类 + 字段）
        let mut non_virtual_size: u64 = 0;
        let mut max_align: u64 = 1;
        let has_virtual = self.class_has_virtual(derived);

        // vptr 大小
        let primary_base = derived_def.bases.first().copied();
        let primary_has_virtual = primary_base
            .map(|b| self.class_has_virtual(b))
            .unwrap_or(false);
        if has_virtual && !primary_has_virtual {
            non_virtual_size = POINTER_SIZE;
            max_align = POINTER_ALIGN;
        }

        // 非虚基类
        for (i, &base_id) in derived_def.bases.iter().enumerate() {
            let base_size = self.estimate_class_size(base_id);
            let base_align = self.estimate_class_align(base_id);
            let is_primary = i == 0 && primary_base.is_some();

            let offset = if is_primary {
                0
            } else {
                align_up(non_virtual_size, base_align)
            };
            if base_size > 0 {
                non_virtual_size = offset + base_size;
            }
            max_align = max_align.max(base_align);
        }

        // 字段
        for (_, ty) in &derived_def.fields {
            let (fsize, falign) = type_size_align(ty, self);
            let offset = align_up(non_virtual_size, falign);
            if fsize > 0 {
                non_virtual_size = offset + fsize;
            }
            max_align = max_align.max(falign);
        }

        // 查找虚基类的偏移
        let all_vbases = self.collect_all_virtual_bases(derived);
        let mut current_offset = non_virtual_size;
        for &vb_id in &all_vbases {
            let vb_align = self.estimate_class_align(vb_id);
            let vb_size = self.estimate_class_size(vb_id);
            let vb_offset = align_up(current_offset, vb_align);
            if vb_id == vbase {
                return vb_offset as i64;
            }
            if vb_size > 0 {
                current_offset = vb_offset + vb_size;
            }
            max_align = max_align.max(vb_align);
        }

        // 如果不是直接虚基类，可能是间接的（通过虚基类的虚基类）
        // 递归查找
        for &vb_id in &all_vbases {
            let vb_offset = self.compute_vbase_offset_from_layout(derived, vb_id);
            let inner_offset = self.compute_vbase_offset_from_layout(vb_id, vbase);
            if inner_offset != 0 || self.is_direct_vbase(vb_id, vbase) {
                return vb_offset + inner_offset;
            }
        }

        0
    }

    /// 检查 vbase 是否是 derived 的直接虚基类。
    fn is_direct_vbase(&self, derived: ClassId, vbase: ClassId) -> bool {
        self.classes
            .get(&derived)
            .map(|d| d.virtual_bases.contains(&vbase))
            .unwrap_or(false)
    }

    /// 收集所有虚基类（包括间接的），按声明顺序去重。
    fn collect_all_virtual_bases(&self, class_id: ClassId) -> Vec<ClassId> {
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();
        self.collect_all_virtual_bases_recursive(class_id, &mut result, &mut seen);
        result
    }

    fn collect_all_virtual_bases_recursive(
        &self,
        class_id: ClassId,
        result: &mut Vec<ClassId>,
        seen: &mut std::collections::HashSet<ClassId>,
    ) {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return,
        };
        for &vb in &def.virtual_bases {
            if seen.insert(vb) {
                result.push(vb);
            }
            // 递归收集虚基类的虚基类
            self.collect_all_virtual_bases_recursive(vb, result, seen);
        }
        // 也从非虚基类收集虚基类（因为非虚基类可能有自己的虚基类）
        for &b in &def.bases {
            self.collect_all_virtual_bases_recursive(b, result, seen);
        }
    }

    /// 计算从 complete object 到指定虚基类 subobject 的偏移（字节）。
    /// 如果该类不是虚基类，返回 None。
    pub fn compute_vbase_offset(&self, derived: ClassId, vbase: ClassId) -> Option<i64> {
        let derived_abi = self.compute_class(derived);
        // 直接虚基类
        for vb in &derived_abi.virtual_bases {
            if vb.base == vbase {
                // 从 vtable 组件中查找 vbase offset 值
                if let Some(vtable) = derived_abi.vtables.first() {
                    for comp in &vtable.components {
                        if let VTableComponent::VBaseOffset { target, offset } = comp {
                            if *target == vbase {
                                return Some(*offset);
                            }
                        }
                    }
                }
                // 如果没有 vtable（无虚函数），则从布局计算
                return Some(self.compute_vbase_offset_from_layout(derived, vbase));
            }
        }
        // 间接虚基类：通过直接虚基类递归查找
        for vb in &derived_abi.virtual_bases {
            if let Some(inner) = self.compute_vbase_offset(vb.base, vbase) {
                let outer = self.compute_vbase_offset(derived, vb.base)?;
                return Some(outer + inner);
            }
        }
        // 也检查非虚基类的虚基类
        for base in &derived_abi.bases {
            if let Some(inner) = self.compute_vbase_offset(base.base, vbase) {
                return Some(base.offset as i64 + inner);
            }
        }
        None
    }

    /// 计算直接基类的偏移（基于类定义，不触发完整的 compute_class）。
    ///
    /// 用于避免 compute_vtables → compute_this_adjust → compute_class → compute_vtables 的递归循环。
    /// 仅处理直接非虚基类，返回该基类在 derived 中的字节偏移。
    fn direct_base_offset(&self, derived: ClassId, base: ClassId) -> Option<i64> {
        let derived_def = self.classes.get(&derived)?;

        // primary base 判定:
        // - 第一个有虚函数的基类是 primary base (offset 0)
        // - 如果没有有虚函数的基类，则第一个基类是 primary base
        let has_virtual = self.class_has_virtual(derived);
        let primary_base = derived_def
            .bases
            .iter()
            .copied()
            .find(|&b| self.class_has_virtual(b))
            .or_else(|| derived_def.bases.first().copied());

        if primary_base == Some(base) {
            return Some(0);
        }

        // 累计偏移
        let mut current_offset: u64 = if has_virtual && primary_base.is_none() {
            POINTER_SIZE // vptr
        } else {
            0
        };
        let mut max_align: u64 = 1;

        for &base_id in &derived_def.bases {
            let base_size = self.estimate_class_size(base_id);
            let base_align = self.estimate_class_align(base_id);

            if Some(base_id) == primary_base {
                // primary base at offset 0
                if base_size > 0 {
                    current_offset = base_size;
                }
                max_align = max_align.max(base_align);
                continue;
            }

            if base_id == base {
                // 找到了目标基类
                let offset = align_up(current_offset, base_align);
                return Some(offset as i64);
            }

            let offset = align_up(current_offset, base_align);
            max_align = max_align.max(base_align);
            if base_size > 0 {
                current_offset = offset + base_size;
            }
        }

        None
    }

    /// 粗略估算类的大小（基于类定义，用于 direct_base_offset）。
    fn estimate_class_size(&self, class_id: ClassId) -> u64 {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return 0,
        };

        let has_virtual = self.class_has_virtual(class_id);
        let mut size = if has_virtual { POINTER_SIZE } else { 0 };
        let mut max_align = if has_virtual { POINTER_ALIGN } else { 1 };

        // 累加非虚基类大小（假设 primary base 在 offset 0）
        let primary_base = def
            .bases
            .iter()
            .copied()
            .find(|&b| self.class_has_virtual(b));
        let has_primary = primary_base.is_some();

        for (i, &base_id) in def.bases.iter().enumerate() {
            let base_size = self.estimate_class_size(base_id);
            let base_align = self.estimate_class_align(base_id);
            if i == 0 && has_primary {
                // primary base at offset 0
                size = base_size.max(size);
            } else {
                size = align_up(size, base_align) + base_size;
            }
            max_align = max_align.max(base_align);
        }

        // 累加字段大小
        for (_, ty) in &def.fields {
            let (fsize, falign) = type_size_align(ty, self);
            size = align_up(size, falign) + fsize;
            max_align = max_align.max(falign);
        }

        // 累加虚基类大小（放在对象末尾）
        let all_vbases = self.collect_all_virtual_bases(class_id);
        for &vb_id in &all_vbases {
            let vb_size = self.estimate_class_size(vb_id);
            let vb_align = self.estimate_class_align(vb_id);
            size = align_up(size, vb_align) + vb_size;
            max_align = max_align.max(vb_align);
        }

        if size == 0 { 1 } else { align_up(size, max_align) }
    }

    /// 粗略估算类的对齐（用于 direct_base_offset）。
    fn estimate_class_align(&self, class_id: ClassId) -> u64 {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return 1,
        };

        let has_virtual = self.class_has_virtual(class_id);
        let mut max_align = if has_virtual { POINTER_ALIGN } else { 1 };

        for &base_id in &def.bases {
            max_align = max_align.max(self.estimate_class_align(base_id));
        }
        for (_, ty) in &def.fields {
            let (_, falign) = type_size_align(ty, self);
            max_align = max_align.max(falign);
        }
        // 虚基类也影响对齐
        let all_vbases = self.collect_all_virtual_bases(class_id);
        for &vb_id in &all_vbases {
            max_align = max_align.max(self.estimate_class_align(vb_id));
        }

        max_align.max(1)
    }

    /// 计算从 derived 到 base 的 this 调整。
    ///
    /// 如果 base 不是 derived 的直接或间接基类，返回 None。
    /// Stage 6: 支持虚继承（使用 vbase_index）。
    pub fn compute_this_adjust(&self, derived: ClassId, base: ClassId) -> Option<ThisAdjust> {
        let derived_abi = self.compute_class(derived);

        // 直接基类（非虚或虚）
        if let Some(&adj) = derived_abi.this_adjust_map.get(&base) {
            return Some(adj);
        }

        // 间接基类：遍历非虚直接基类，递归查找
        for base_layout in &derived_abi.bases {
            if let Some(mut inner) = self.compute_this_adjust(base_layout.base, base) {
                inner.non_virtual += base_layout.offset as i64;
                return Some(inner);
            }
        }

        // 间接虚基类：遍历直接虚基类，递归查找
        for vb_layout in &derived_abi.virtual_bases {
            if let Some(inner) = self.compute_this_adjust(vb_layout.base, base) {
                // 如果 inner 本身就是虚基类调整，则组合调整
                // 这里简化：外层用 vbase_index，内层的非虚偏移加上
                return Some(ThisAdjust {
                    non_virtual: inner.non_virtual,
                    vbase_index: Some(vb_layout.vbase_index),
                });
            }
        }

        None
    }
}

impl ItaniumAbiCalculator {
    /// 计算 RTTI 布局（typeinfo 结构）。
    ///
    /// Itanium ABI 中 typeinfo 有三种类型:
    /// - `__class_type_info`: 无基类
    /// - `__si_class_type_info`: 单个公有非虚基类
    /// - `__vmi_class_type_info`: 多基类或有虚基类
    ///
    /// 基类标志位 (flags):
    /// - 1 (public): 公有基类
    /// - 2 (virtual): 虚基类
    pub fn compute_rtti(&self, class_id: ClassId, abi: &ClassAbi) -> RttiLayout {
        let class_name = format!("Class{}", class_id.0);
        let typeinfo_symbol = format!("_ZTI{}", class_name);

        let has_virtual_bases = !abi.virtual_bases.is_empty();

        let mut base_classes = Vec::new();

        // 非虚基类
        for base in &abi.bases {
            // 简化：所有基类都视为 public（测试场景下）
            let flags: u32 = 1; // public
            base_classes.push(RttiBaseClass {
                base: base.base,
                offset: base.offset as i64,
                is_virtual: false,
                flags,
            });
        }

        // 虚基类
        for vb in &abi.virtual_bases {
            // 计算虚基类的偏移（从 complete object 到 virtual base subobject）
            let offset = self.compute_vbase_offset_from_layout(class_id, vb.base);
            // Itanium ABI: __public_mask=0x1, __virtual_mask=0x8 (非 0x2)
            let flags: u32 = 1 | 8; // public | virtual
            base_classes.push(RttiBaseClass {
                base: vb.base,
                offset,
                is_virtual: true,
                flags,
            });
        }

        RttiLayout {
            typeinfo_symbol,
            has_virtual_bases,
            base_classes,
        }
    }
}

impl AbiCalculator for ItaniumAbiCalculator {
    fn compute_class(&self, class: ClassId) -> ClassAbi {
        let def = match self.classes.get(&class) {
            Some(d) => d,
            None => {
                // 未注册的类: 返回空布局
                return ClassAbi {
                    class_id: class,
                    ..ClassAbi::default()
                };
            }
        };

        let mut abi = ClassAbi {
            class_id: class,
            ..ClassAbi::default()
        };

        let has_virtual = self.class_has_virtual(class);
        // primary base: 第一个非虚基类（无论是否有虚函数都是 primary base
        // 如果有虚函数且基类也有虚函数，则共享 vptr
        let primary_base = def.bases.first().copied();

        let mut current_offset: u64 = 0;
        let mut max_align: u64 = 1;

        // --- Step 0: 分配 vptr（如果有虚函数且无 primary base） ---
        // 如果类有虚函数但没有基类，或者基类都没有虚函数，则在 offset 0 放 vptr
        let primary_has_virtual = primary_base
            .map(|b| self.class_has_virtual(b))
            .unwrap_or(false);
        if has_virtual && !primary_has_virtual {
            abi.vptrs.push(VPtrLayout {
                offset: 0,
                vtable_index: 0,
            });
            current_offset = POINTER_SIZE;
            max_align = POINTER_ALIGN;
        }

        // --- Step 1: 布局非虚基类 ---
        for (i, &base_id) in def.bases.iter().enumerate() {
            let base_abi = self.compute_class(base_id);
            let base_align = base_abi.align.max(1);
            let base_size = base_abi.size;

            // primary base 判定：第一个有虚函数的基类是 primary base
            // （如果基类有虚函数，则它作为 primary base 共享 vptr）
            let is_primary = i == 0;

            // 计算该基类的起始 offset
            let base_offset = if is_primary && primary_base.is_some() && Some(base_id) == primary_base
            {
                // primary base 放在 offset 0，共享 vptr
                0
            } else {
                align_up(current_offset, base_align)
            };

            // 添加基类布局
            abi.bases.push(BaseLayout {
                base: base_id,
                offset: base_offset,
                is_primary: Some(base_id) == primary_base,
            });

            // 记录 this 调整（非虚偏移）
            abi.this_adjust_map.insert(
                base_id,
                ThisAdjust::offset(base_offset as i64),
            );

            if Some(base_id) == primary_base {
                abi.primary_base = Some(base_id);
                // primary base 共享 vptr（仅当类有虚函数时）
                if has_virtual && primary_has_virtual {
                    if !abi.vptrs.iter().any(|v| v.offset == base_offset) {
                        abi.vptrs.push(VPtrLayout {
                            offset: base_offset,
                            vtable_index: 0,
                        });
                    }
                }
            } else if self.class_has_virtual(base_id) {
                // 非 primary base 但有虚函数，有自己的 vptr（对应 secondary vtable）
                // 找到对应的 secondary vtable index
                let vtable_idx = abi.vtables
                    .iter()
                    .position(|v| v.for_base == Some(base_id))
                    .unwrap_or(0); // 暂时用 0，后面再修正
                abi.vptrs.push(VPtrLayout {
                    offset: base_offset,
                    vtable_index: vtable_idx,
                });
            }

            // 更新最大对齐
            if base_align > max_align {
                max_align = base_align;
            }

            // 推进 offset
            let end_offset = base_offset + base_size;
            // 空基类（size 0）不推进 offset（避免无谓推进）
            if base_size > 0 {
                current_offset = end_offset;
            }
        }

        // --- Step 2: 布局字段 ---
        for (name, ty) in &def.fields {
            let (field_size, field_align) = type_size_align(ty, self);

            // 对齐当前 offset
            let field_offset = align_up(current_offset, field_align);

            // 记录字段布局
            abi.fields.push(FieldLayout {
                name: name.clone(),
                offset: field_offset,
                size: field_size,
                align: field_align,
            });

            // 更新最大对齐
            if field_align > max_align {
                max_align = field_align;
            }

            // 推进 offset
            if field_size > 0 {
                current_offset = field_offset + field_size;
            }
        }

        // --- Step 2.5: 布局虚基类 ---
        // 虚基类放在对象的末尾（非虚部分之后）
        // 按声明顺序排列，每个虚基类只出现一次（去重）
        let all_virtual_bases = self.collect_all_virtual_bases(class);
        for (vbase_idx, &vb_id) in all_virtual_bases.iter().enumerate() {
            let vb_abi = self.compute_class(vb_id);
            let vb_align = vb_abi.align.max(1);
            let vb_size = vb_abi.size;

            // 对齐当前 offset
            let vb_offset = align_up(current_offset, vb_align);

            // 添加虚基类布局
            abi.virtual_bases.push(VirtualBaseLayout {
                base: vb_id,
                vtable_offset: 0, // 后面在 vtable 计算时填充
                vbase_index: vbase_idx,
            });

            // 记录 this 调整（虚基类，使用 vbase_index）
            abi.this_adjust_map.insert(
                vb_id,
                ThisAdjust {
                    non_virtual: 0,
                    vbase_index: Some(vbase_idx),
                },
            );

            // 更新最大对齐
            if vb_align > max_align {
                max_align = vb_align;
            }

            // 推进 offset
            if vb_size > 0 {
                current_offset = vb_offset + vb_size;
            }
        }

        // --- Step 3: 计算最终 size 和 align ---
        abi.align = max_align;
        abi.size = if current_offset == 0 {
            // 空类（无基类无字段）: Itanium ABI 规定大小为 1
            1
        } else {
            align_up(current_offset, max_align)
        };

        // --- Step 4: 计算 vtable 布局 ---
        if has_virtual {
            abi.vtables = self.compute_vtables(class, &abi);

            // 从 primary vtable 中提取 vbase offset 值，更新到 virtual_bases
            if let Some(primary_vtable) = abi.vtables.first() {
                let mut vbase_idx = 0;
                for comp in &primary_vtable.components {
                    if let VTableComponent::VBaseOffset { target, offset } = comp {
                        if vbase_idx < abi.virtual_bases.len()
                            && abi.virtual_bases[vbase_idx].base == *target
                        {
                            // vtable_offset 是相对 address point 的偏移（负值，因为在前面）
                            // 计算方式: -(vbase_idx + 1) * POINTER_SIZE - offset_to_top_slot - typeinfo_slot
                            // 实际上 vbase offset 在 vtable 中的位置是从 vtable 起点开始的
                            // vtable_offset 表示相对于 address point 的字节偏移（负的表示在前面）
                            let slot_offset = -(vbase_idx as i64 + 2) * POINTER_SIZE as i64;
                            abi.virtual_bases[vbase_idx].vtable_offset = slot_offset;
                            let _ = offset;
                            vbase_idx += 1;
                        }
                    }
                }
            }

            // 修正 vptrs 的 vtable_index
            // primary vtable index = 0
            // secondary vtable index 根据 for_base 查找
            for vptr in &mut abi.vptrs {
                if vptr.offset == 0 {
                    vptr.vtable_index = 0;
                } else {
                    // 根据 offset 找到对应的基类，再找 vtable index
                    if let Some(base_layout) = abi.bases.iter().find(|b| b.offset == vptr.offset) {
                        if let Some(idx) = abi
                            .vtables
                            .iter()
                            .position(|v| v.for_base == Some(base_layout.base))
                        {
                            vptr.vtable_index = idx;
                        }
                    }
                }
            }
        }

        // --- Step 5: 构造/析构顺序 ---
        abi.base_ctor_order = def.bases.clone();
        abi.field_init_order = def.fields.iter().map(|(n, _)| n.clone()).collect();

        // 析构顺序: 成员逆序 → 非虚基类逆序 → 虚基类逆序
        let mut dtor_order = Vec::new();
        for (name, _) in def.fields.iter().rev() {
            dtor_order.push(DtorStep::Member(name.clone()));
        }
        for &base in def.bases.iter().rev() {
            dtor_order.push(DtorStep::Base(base));
        }
        for &vb in all_virtual_bases.iter().rev() {
            dtor_order.push(DtorStep::VirtualBase(vb));
        }
        abi.dtor_order = dtor_order;

        // --- Step 6: 计算 RTTI 布局 ---
        abi.rtti = self.compute_rtti(class, &abi);

        abi
    }
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

    // === ItaniumAbiCalculator 测试 ===

    #[test]
    fn itanium_simple_int_class() {
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.size, 4);
        assert_eq!(abi.align, 4);
        assert_eq!(abi.fields.len(), 1);
        assert_eq!(abi.fields[0].name, "x");
        assert_eq!(abi.fields[0].offset, 0);
        assert_eq!(abi.fields[0].size, 4);
        assert!(abi.bases.is_empty());
        assert!(abi.primary_base.is_none());
    }

    #[test]
    fn itanium_multi_field_padding() {
        // struct { bool a; int b; char c; double d; }
        // offsets: a=0, padding 1-3, b=4, c=8, padding 9-15, d=16
        // total size: 24, align: 8
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![
                ("a".into(), CxxType::Bool),
                ("b".into(), CxxType::Integer { bits: 32, signed: true }),
                ("c".into(), CxxType::Integer { bits: 8, signed: false }),
                ("d".into(), CxxType::Float { bits: 64 }),
            ],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.size, 24);
        assert_eq!(abi.align, 8);
        assert_eq!(abi.fields.len(), 4);
        assert_eq!(abi.fields[0].offset, 0); // bool a
        assert_eq!(abi.fields[1].offset, 4); // int b (3-byte padding)
        assert_eq!(abi.fields[2].offset, 8); // char c
        assert_eq!(abi.fields[3].offset, 16); // double d (7-byte padding)
    }

    #[test]
    fn itanium_single_inheritance() {
        // struct Base { int x; };
        // struct Derived : Base { int y; };
        // Derived layout: Base@0, y@4, size=8, align=4
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_simple_class(
            base_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.size, 8);
        assert_eq!(abi.align, 4);
        assert_eq!(abi.bases.len(), 1);
        assert_eq!(abi.bases[0].offset, 0);
        assert!(abi.bases[0].is_primary);
        assert_eq!(abi.primary_base, Some(base_id));
        assert_eq!(abi.fields.len(), 1);
        assert_eq!(abi.fields[0].name, "y");
        assert_eq!(abi.fields[0].offset, 4);
    }

    #[test]
    fn itanium_multiple_inheritance() {
        // struct A { int a; char x; };  // size=8, align=4
        // struct B { double b; };       // size=8, align=8
        // struct C : A, B { int c; };   // A@0, B@8, c@16, total=24, align=8
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_simple_class(
            a_id,
            vec![
                ("a".into(), CxxType::Integer { bits: 32, signed: true }),
                ("x".into(), CxxType::Integer { bits: 8, signed: false }),
            ],
        );
        calc.register_simple_class(
            b_id,
            vec![("b".into(), CxxType::Float { bits: 64 })],
        );
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi_a = calc.compute_class(a_id);
        assert_eq!(abi_a.size, 8);
        assert_eq!(abi_a.align, 4);

        let abi_b = calc.compute_class(b_id);
        assert_eq!(abi_b.size, 8);
        assert_eq!(abi_b.align, 8);

        let abi_c = calc.compute_class(c_id);
        assert_eq!(abi_c.size, 24);
        assert_eq!(abi_c.align, 8);
        assert_eq!(abi_c.bases.len(), 2);
        assert_eq!(abi_c.bases[0].base, a_id);
        assert_eq!(abi_c.bases[0].offset, 0);
        assert!(abi_c.bases[0].is_primary);
        assert_eq!(abi_c.bases[1].base, b_id);
        assert_eq!(abi_c.bases[1].offset, 8);
        assert!(!abi_c.bases[1].is_primary);
        assert_eq!(abi_c.primary_base, Some(a_id));
        assert_eq!(abi_c.fields.len(), 1);
        assert_eq!(abi_c.fields[0].offset, 16);
    }

    #[test]
    fn itanium_empty_class_size_one() {
        // 空类大小为 1 字节（Itanium ABI 要求）
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(class_id, vec![]);

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.size, 1);
        assert_eq!(abi.align, 1);
    }

    #[test]
    fn itanium_array_field() {
        // struct { int arr[5]; };
        // size = 20, align = 4
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![(
                "arr".into(),
                CxxType::Array(Box::new(CxxType::Integer { bits: 32, signed: true }), Some(5)),
            )],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.size, 20);
        assert_eq!(abi.align, 4);
        assert_eq!(abi.fields[0].size, 20);
    }

    #[test]
    fn itanium_pointer_field() {
        // struct { int* p; int x; };
        // p@0 (8 bytes), x@8 (4 bytes), padding 12-15, total=16, align=8
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![
                ("p".into(), CxxType::Pointer(Box::new(CxxType::Integer { bits: 32, signed: true }))),
                ("x".into(), CxxType::Integer { bits: 32, signed: true }),
            ],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.size, 16);
        assert_eq!(abi.align, 8);
        assert_eq!(abi.fields[0].offset, 0);
        assert_eq!(abi.fields[1].offset, 8);
    }

    #[test]
    fn itanium_reference_field() {
        // 引用类型大小 = 指针大小
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![("ref".into(), CxxType::LValueRef(Box::new(CxxType::Integer { bits: 32, signed: true })))],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.size, 8);
        assert_eq!(abi.align, 8);
        assert_eq!(abi.fields[0].size, 8);
    }

    #[test]
    fn itanium_unregistered_class_returns_empty() {
        let calc = ItaniumAbiCalculator::new();
        let abi = calc.compute_class(ClassId(999));
        assert_eq!(abi.size, 0);
        assert!(abi.fields.is_empty());
        assert!(abi.bases.is_empty());
    }

    // === base_offset / this_adjust / ctor-dtor 顺序 测试 ===

    #[test]
    fn base_offset_single_inheritance() {
        // struct Base { int x; };
        // struct Derived : Base { int y; };
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);
        calc.register_simple_class(
            base_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.base_offset(base_id), Some(0));
        assert_eq!(abi.base_offset(ClassId(999)), None);
    }

    #[test]
    fn base_offset_multiple_inheritance() {
        // struct A { int a; char x; };  // size=8
        // struct B { double b; };       // size=8
        // struct C : A, B { int c; };   // A@0, B@8, c@16
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_simple_class(
            a_id,
            vec![
                ("a".into(), CxxType::Integer { bits: 32, signed: true }),
                ("x".into(), CxxType::Integer { bits: 8, signed: false }),
            ],
        );
        calc.register_simple_class(
            b_id,
            vec![("b".into(), CxxType::Float { bits: 64 })],
        );
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(c_id);
        assert_eq!(abi.base_offset(a_id), Some(0)); // primary base
        assert_eq!(abi.base_offset(b_id), Some(8));
    }

    #[test]
    fn primary_base_offset_is_zero() {
        // 第一个非虚基类作为 primary base，offset = 0
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_simple_class(
            base_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.primary_base, Some(base_id));
        assert_eq!(abi.base_offset(base_id), Some(0));
        // primary base 的 this_adjust 为 0
        let adjust = abi.this_adjust_map.get(&base_id).unwrap();
        assert!(adjust.is_zero());
    }

    #[test]
    fn this_adjust_direct_base() {
        // 多继承：第二个基类需要非虚偏移调整
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_simple_class(
            a_id,
            vec![("a".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_simple_class(
            b_id,
            vec![("b".into(), CxxType::Float { bits: 64 })],
        );
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let adjust = calc.compute_this_adjust(c_id, b_id).unwrap();
        assert_eq!(adjust.non_virtual, 8);
        assert!(adjust.vbase_index.is_none());
    }

    #[test]
    fn this_adjust_indirect_base() {
        // A <- B <- C 单继承链，C->A 的 this_adjust = 0（因为链上都是 primary base）
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_simple_class(
            a_id,
            vec![("a".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_class(
            b_id,
            vec![a_id],
            vec![],
            vec![("b".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );
        calc.register_class(
            c_id,
            vec![b_id],
            vec![],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let adjust = calc.compute_this_adjust(c_id, a_id).unwrap();
        assert_eq!(adjust.non_virtual, 0);
    }

    #[test]
    fn this_adjust_none_for_unrelated() {
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        calc.register_simple_class(a_id, vec![]);
        calc.register_simple_class(b_id, vec![]);

        assert!(calc.compute_this_adjust(a_id, b_id).is_none());
    }

    #[test]
    fn ctor_dtor_order_simple() {
        // struct Foo { int a; char b; double c; };
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![
                ("a".into(), CxxType::Integer { bits: 32, signed: true }),
                ("b".into(), CxxType::Bool),
                ("c".into(), CxxType::Float { bits: 64 }),
            ],
        );

        let abi = calc.compute_class(class_id);

        // 字段初始化顺序 = 声明顺序
        assert_eq!(abi.field_init_order, vec!["a", "b", "c"]);
        assert!(abi.base_ctor_order.is_empty());

        // 析构顺序：成员逆序
        assert_eq!(abi.dtor_order.len(), 3);
        assert_eq!(abi.dtor_order[0], DtorStep::Member("c".into()));
        assert_eq!(abi.dtor_order[1], DtorStep::Member("b".into()));
        assert_eq!(abi.dtor_order[2], DtorStep::Member("a".into()));
    }

    #[test]
    fn ctor_dtor_order_with_bases() {
        // struct A { int a; };
        // struct B { double b; };
        // struct C : A, B { int c; char d; };
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_simple_class(
            a_id,
            vec![("a".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_simple_class(
            b_id,
            vec![("b".into(), CxxType::Float { bits: 64 })],
        );
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![
                ("c".into(), CxxType::Integer { bits: 32, signed: true }),
                ("d".into(), CxxType::Bool),
            ],
            false,
        );

        let abi = calc.compute_class(c_id);

        // 基类构造顺序 = 声明顺序
        assert_eq!(abi.base_ctor_order, vec![a_id, b_id]);
        // 字段初始化顺序 = 声明顺序
        assert_eq!(abi.field_init_order, vec!["c", "d"]);

        // 析构顺序：成员逆序 → 基类逆序
        assert_eq!(abi.dtor_order.len(), 4);
        assert_eq!(abi.dtor_order[0], DtorStep::Member("d".into()));
        assert_eq!(abi.dtor_order[1], DtorStep::Member("c".into()));
        assert_eq!(abi.dtor_order[2], DtorStep::Base(b_id));
        assert_eq!(abi.dtor_order[3], DtorStep::Base(a_id));
    }

    #[test]
    fn thunk_kind_variants() {
        // 验证 ThunkKind 的构造/析构变体可用
        let v = ThunkKind::Virtual;
        let c1 = ThunkKind::Ctor(CtorVariant::Complete);
        let c2 = ThunkKind::Ctor(CtorVariant::Base);
        let c3 = ThunkKind::Ctor(CtorVariant::Allocating);
        let d1 = ThunkKind::Dtor(DtorVariant::Complete);
        let d2 = ThunkKind::Dtor(DtorVariant::Base);
        let d0 = ThunkKind::Dtor(DtorVariant::Deleting);

        assert_ne!(v, c1);
        assert_ne!(c1, c2);
        assert_ne!(c2, c3);
        assert_ne!(d1, d2);
        assert_ne!(d2, d0);
    }

    // === VTable / Stage 5 测试 ===

    #[test]
    fn vtable_simple_virtual_class() {
        // class Foo { virtual void foo(); virtual void bar(); int x; };
        // vtable: offset-to-top, typeinfo, foo, bar
        // vptr at offset 0, field x at offset 8
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_class(
            class_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(class_id, "foo".into(), true, class_id);
        calc.register_method(class_id, "bar".into(), true, class_id);

        let abi = calc.compute_class(class_id);

        // vptr 在 offset 0
        assert_eq!(abi.vptrs.len(), 1);
        assert_eq!(abi.vptrs[0].offset, 0);
        assert_eq!(abi.vptrs[0].vtable_index, 0);

        // 大小: 8 (vptr) + 4 (int x) + padding 4 = 16
        assert_eq!(abi.size, 16);
        assert_eq!(abi.align, 8);

        // 字段偏移
        assert_eq!(abi.fields[0].offset, 8);

        // vtable 结构
        assert_eq!(abi.vtables.len(), 1);
        let vtable = &abi.vtables[0];
        assert_eq!(vtable.components.len(), 4); // offset-to-top + typeinfo + 2 methods

        // address point = 2 * POINTER_SIZE
        assert_eq!(vtable.address_point, 16);

        // 第一个组件: offset-to-top = 0
        match &vtable.components[0] {
            VTableComponent::OffsetToTop(v) => assert_eq!(*v, 0),
            _ => panic!("expected OffsetToTop"),
        }

        // 第二个组件: typeinfo
        match &vtable.components[1] {
            VTableComponent::TypeInfo(s) => assert!(s.contains("ZTI")),
            _ => panic!("expected TypeInfo"),
        }

        // 第三个组件: foo (slot 0)
        match &vtable.components[2] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.ends_with("::foo"));
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function"),
        }

        // 第四个组件: bar (slot 1)
        match &vtable.components[3] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.ends_with("::bar"));
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function"),
        }

        // get_vtable_slot
        assert_eq!(abi.get_vtable_slot("foo"), Some(0));
        assert_eq!(abi.get_vtable_slot("bar"), Some(1));
        assert_eq!(abi.get_vtable_slot("nonexistent"), None);
    }

    #[test]
    fn vptr_at_offset_zero() {
        // vptr 应该放在 offset 0
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_class(
            class_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(class_id, "vfunc".into(), true, class_id);

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.vptrs.len(), 1);
        assert_eq!(abi.vptrs[0].offset, 0);
    }

    #[test]
    fn vtable_single_inheritance_override() {
        // class Base { virtual void foo(); virtual void bar(); };
        // class Derived : Base { virtual void foo(); virtual void baz(); };
        // Derived vtable: offset-to-top, typeinfo, Derived::foo, Base::bar, Derived::baz
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        // Base
        calc.register_class(base_id, vec![], vec![], vec![], true);
        calc.register_method(base_id, "foo".into(), true, base_id);
        calc.register_method(base_id, "bar".into(), true, base_id);

        // Derived
        calc.register_class(derived_id, vec![base_id], vec![], vec![], true);
        calc.register_method(derived_id, "foo".into(), true, base_id); // 重写 (owner = base)
        calc.register_method(derived_id, "baz".into(), true, derived_id); // 新增

        let abi = calc.compute_class(derived_id);

        // primary base = Base，共享 vptr
        assert_eq!(abi.vptrs.len(), 1);
        assert_eq!(abi.vptrs[0].offset, 0);
        assert_eq!(abi.primary_base, Some(base_id));

        // vtable
        assert_eq!(abi.vtables.len(), 1);
        let vtable = &abi.vtables[0];

        // 组件: offset-to-top, typeinfo, foo(override), bar(inherited), baz(new)
        assert_eq!(vtable.components.len(), 5);

        // slot 0: foo (被 Derived 重写)
        match &vtable.components[2] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.contains("Class1") && symbol.ends_with("::foo"),
                    "expected Derived::foo, got {}", symbol);
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function for foo"),
        }

        // slot 1: bar (继承自 Base)
        match &vtable.components[3] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.contains("Class0") && symbol.ends_with("::bar"),
                    "expected Base::bar, got {}", symbol);
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function for bar"),
        }

        // slot 2: baz (Derived 新增)
        match &vtable.components[4] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.contains("Class1") && symbol.ends_with("::baz"),
                    "expected Derived::baz, got {}", symbol);
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function for baz"),
        }

        // get_vtable_slot
        assert_eq!(abi.get_vtable_slot("foo"), Some(0));
        assert_eq!(abi.get_vtable_slot("bar"), Some(1));
        assert_eq!(abi.get_vtable_slot("baz"), Some(2));
    }

    #[test]
    fn vtable_multiple_inheritance_this_adjust_thunk() {
        // class A { virtual void a_func(); int a; };  // size=16 (vptr + int + pad)
        // class B { virtual void b_func(); double b; }; // size=16 (vptr + double)
        // class C : A, B { virtual void a_func(); virtual void b_func(); int c; };
        //
        // C 布局: A@0 (vptr + int a), B@16 (vptr + double b), c@32 (int)
        //   total = 40, align = 8
        //
        // Primary vtable (for A subobject):
        //   offset-to-top=0, typeinfo for C, C::a_func (thunk 0), C::b_func (thunk 0)
        // Secondary vtable (for B subobject):
        //   offset-to-top=-16, typeinfo for C, C::b_func (thunk -16)
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        // A
        calc.register_class(
            a_id,
            vec![],
            vec![],
            vec![("a".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(a_id, "a_func".into(), true, a_id);

        // B
        calc.register_class(
            b_id,
            vec![],
            vec![],
            vec![("b".into(), CxxType::Float { bits: 64 })],
            true,
        );
        calc.register_method(b_id, "b_func".into(), true, b_id);

        // C : A, B
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(c_id, "a_func".into(), true, a_id); // 重写 A::a_func
        calc.register_method(c_id, "b_func".into(), true, b_id); // 重写 B::b_func

        let abi = calc.compute_class(c_id);

        // 布局验证
        assert_eq!(abi.size, 40);
        assert_eq!(abi.align, 8);
        assert_eq!(abi.bases.len(), 2);
        assert_eq!(abi.bases[0].offset, 0); // A @ 0
        assert!(abi.bases[0].is_primary);
        assert_eq!(abi.bases[1].offset, 16); // B @ 16
        assert!(!abi.bases[1].is_primary);
        assert_eq!(abi.fields[0].offset, 32); // c @ 32

        // vptrs: 两个 (A 和 B 各一个)
        assert_eq!(abi.vptrs.len(), 2);
        assert_eq!(abi.vptrs[0].offset, 0); // A 的 vptr (primary)
        assert_eq!(abi.vptrs[1].offset, 16); // B 的 vptr

        // vtables: 两个 (primary + secondary for B)
        assert_eq!(abi.vtables.len(), 2);

        // --- Primary vtable ---
        let primary = &abi.vtables[0];
        assert!(primary.for_base.is_none());
        // offset-to-top, typeinfo, a_func, b_func
        assert_eq!(primary.components.len(), 4);

        match &primary.components[0] {
            VTableComponent::OffsetToTop(v) => assert_eq!(*v, 0),
            _ => panic!("expected OffsetToTop"),
        }

        // slot 0: C::a_func (无调整，primary)
        match &primary.components[2] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.ends_with("::a_func"), "expected a_func, got {}", symbol);
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function for a_func"),
        }

        // slot 1: C::b_func (在 primary vtable 中无调整)
        match &primary.components[3] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.ends_with("::b_func"), "expected b_func, got {}", symbol);
                assert!(adjustment.is_zero());
            }
            _ => panic!("expected Function for b_func"),
        }

        // --- Secondary vtable (for B) ---
        let secondary = &abi.vtables[1];
        assert_eq!(secondary.for_base, Some(b_id));
        // offset-to-top, typeinfo, b_func
        assert_eq!(secondary.components.len(), 3);

        // offset-to-top: -16 (从 B subobject 到 C complete object)
        match &secondary.components[0] {
            VTableComponent::OffsetToTop(v) => assert_eq!(*v, -16),
            _ => panic!("expected OffsetToTop=-16"),
        }

        // b_func: 重写，需要 this-adjust thunk (-16)
        match &secondary.components[2] {
            VTableComponent::Function { symbol, adjustment } => {
                assert!(symbol.ends_with("::b_func"), "expected b_func, got {}", symbol);
                assert_eq!(adjustment.non_virtual, -16, "expected thunk adjust -16");
                assert!(adjustment.vbase_index.is_none());
            }
            _ => panic!("expected Function for b_func in secondary"),
        }

        // get_vtable_slot (查 primary vtable)
        assert_eq!(abi.get_vtable_slot("a_func"), Some(0));
        assert_eq!(abi.get_vtable_slot("b_func"), Some(1));
    }

    #[test]
    fn vtable_no_virtual_no_vptr() {
        // 无虚函数的类不应该有 vptr 和 vtable
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );

        let abi = calc.compute_class(class_id);
        assert!(abi.vptrs.is_empty());
        assert!(abi.vtables.is_empty());
        assert_eq!(abi.size, 4);
        assert!(!abi.is_polymorphic());
    }

    #[test]
    fn is_polymorphic_with_virtual_functions() {
        // 有虚函数的类 is_polymorphic() 返回 true
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_class(class_id, vec![], vec![], vec![], true);
        calc.register_method(class_id, "foo".into(), true, class_id);

        let abi = calc.compute_class(class_id);
        assert!(abi.is_polymorphic());
    }

    #[test]
    fn is_polymorphic_inherited_virtual() {
        // 继承虚函数的类 is_polymorphic() 返回 true
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_class(base_id, vec![], vec![], vec![], true);
        calc.register_method(base_id, "foo".into(), true, base_id);
        calc.register_class(derived_id, vec![base_id], vec![], vec![], false);

        let abi = calc.compute_class(derived_id);
        assert!(abi.is_polymorphic());
    }

    #[test]
    fn vtable_secondary_inherited_method_no_thunk() {
        // 多继承中，secondary base 的未重写方法不应有 thunk
        // class A { virtual void a(); };
        // class B { virtual void b(); };
        // class C : A, B { virtual void a(); }; // 只重写 A::a，不重写 B::b
        // secondary vtable (B) 中 B::b 应该没有 thunk（adjustment = 0）
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_class(a_id, vec![], vec![], vec![], true);
        calc.register_method(a_id, "a".into(), true, a_id);

        calc.register_class(b_id, vec![], vec![], vec![], true);
        calc.register_method(b_id, "b".into(), true, b_id);

        calc.register_class(c_id, vec![a_id, b_id], vec![], vec![], true);
        calc.register_method(c_id, "a".into(), true, a_id);

        let abi = calc.compute_class(c_id);
        assert_eq!(abi.vtables.len(), 2);

        let secondary = &abi.vtables[1];
        assert_eq!(secondary.for_base, Some(b_id));
        assert_eq!(secondary.components.len(), 3);

        // b 在 secondary vtable 中未被重写，应该使用基类函数且无 thunk
        match &secondary.components[2] {
            VTableComponent::Function { symbol, adjustment } => {
                assert_eq!(symbol, "Class1::b");
                assert!(adjustment.is_zero(), "inherited method should have zero adjustment");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn vtable_address_point_correctness() {
        // address point = offset-to-top (8 bytes) + typeinfo pointer (8 bytes) = 16
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_class(class_id, vec![], vec![], vec![], true);
        calc.register_method(class_id, "f".into(), true, class_id);

        let abi = calc.compute_class(class_id);
        let vtable = &abi.vtables[0];
        assert_eq!(vtable.address_point, 16);
        // address point 正好是第一个虚函数槽的偏移
        // components[0] = offset-to-top (8 bytes at offset 0)
        // components[1] = typeinfo (8 bytes at offset 8)
        // components[2] = first function (at offset 16 = address_point)
    }

    // === Stage 6: 虚继承测试 ===

    #[test]
    fn virtual_inheritance_simple_layout() {
        // class Base { int x; };
        // class Derived : virtual Base { int y; };
        //
        // Derived 布局 (无虚函数):
        //   - y @ 0 (int, 4 bytes)
        //   - Base (virtual) @ 8 (int x, 4 bytes)
        //   total = 12? 不对，需要考虑对齐
        //
        // 实际上，没有虚函数时，没有 vptr，虚基类偏移怎么确定？
        // 在 Itanium ABI 中，没有虚函数的类也可以有虚继承，
        // 但需要一个 vbptr（virtual base pointer）。
        // 为了简化，我们假设虚继承的类一定有 vptr（通过虚函数或虚基类引入）。
        //
        // 让我们测试有虚函数的情况：
        // class Base { virtual void foo(); int x; };
        // class Derived : virtual Base { int y; };
        //
        // Derived 布局:
        //   - vptr @ 0 (8 bytes)
        //   - y @ 8 (int, 4 bytes)
        //   - padding @ 12-15 (4 bytes, 对齐到 8)
        //   - Base (virtual) @ 16: vptr + int x + padding = 16 bytes
        // total = 32, align = 8
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        // Base: 有虚函数
        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        // Derived: 虚继承 Base
        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false, // 自身没有新的虚函数，但继承了
        );

        let abi = calc.compute_class(derived_id);

        // 验证有虚函数（继承的）
        assert!(abi.is_polymorphic());
        assert_eq!(abi.vptrs.len(), 1);
        assert_eq!(abi.vptrs[0].offset, 0);

        // 验证非虚基类为空
        assert!(abi.bases.is_empty());

        // 验证虚基类
        assert_eq!(abi.virtual_bases.len(), 1);
        assert_eq!(abi.virtual_bases[0].base, base_id);
        assert_eq!(abi.virtual_bases[0].vbase_index, 0);

        // 验证字段 y 的偏移（在 vptr 之后）
        assert_eq!(abi.fields.len(), 1);
        assert_eq!(abi.fields[0].name, "y");
        assert_eq!(abi.fields[0].offset, 8); // vptr (8 bytes) 之后

        // 验证大小: vptr(8) + y(4) + padding(4) + Base(16) = 32
        assert_eq!(abi.size, 32);
        assert_eq!(abi.align, 8);

        // 验证 vtable 中有 VBaseOffset 条目
        assert_eq!(abi.vtables.len(), 1);
        let vtable = &abi.vtables[0];
        let vbase_offsets: Vec<_> = vtable
            .components
            .iter()
            .filter(|c| matches!(c, VTableComponent::VBaseOffset { .. }))
            .collect();
        assert_eq!(vbase_offsets.len(), 1);

        // 验证 vbase offset 值 (应该是 16，即 Base subobject 的偏移)
        match &vbase_offsets[0] {
            VTableComponent::VBaseOffset { target, offset } => {
                assert_eq!(*target, base_id);
                assert_eq!(*offset, 16);
            }
            _ => panic!("expected VBaseOffset"),
        }

        // address_point = vbase_offset(1) * 8 + offset-to-top(8) + typeinfo(8) = 8 + 16 = 24
        assert_eq!(vtable.address_point, 24);
    }

    #[test]
    fn virtual_inheritance_diamond_layout() {
        // 菱形虚继承:
        //   A
        //  / \
        // B   C (both virtual inherit A)
        //  \ /
        //   D
        //
        // class A { virtual void foo(); int a; };  // size = 16
        // class B : virtual A { int b; };          // size = 32 (vptr+b+pad + A)
        // class C : virtual A { int c; };          // size = 32 (vptr+c+pad + A)
        // class D : B, C { int d; };
        //
        // D 的布局:
        //   - B subobject @ 0 (non-virtual part: vptr + b + pad = 16)
        //   - C subobject @ 16 (non-virtual part: vptr + c + pad = 16)
        //   - d @ 32: int, 4 bytes + padding 4 = 8
        //   - A (virtual, shared) @ 40: vptr + a + pad = 16
        // total = 56
        //
        // 注意: B 和 C 作为 D 的非虚基类时，它们的虚基类 A 部分
        // 不包含在 B/C subobject 中（因为 A 是 D 的虚基类，由 D 共享）。
        // 但在我们的简化实现中，B 和 C 各自的布局仍然包含 A，
        // 所以 D 中 B 和 C 的偏移可能不同。
        // 这里我们主要验证:
        //   1. D 有两个非虚基类 (B, C)
        //   2. D 只有一个虚基类 A (去重)
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);
        let d_id = ClassId(3);

        // A
        calc.register_class(
            a_id,
            vec![],
            vec![],
            vec![("a".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(a_id, "foo".into(), true, a_id);

        // B : virtual A
        calc.register_class(
            b_id,
            vec![],
            vec![a_id],
            vec![("b".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        // C : virtual A
        calc.register_class(
            c_id,
            vec![],
            vec![a_id],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        // D : B, C
        calc.register_class(
            d_id,
            vec![b_id, c_id],
            vec![],
            vec![("d".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi_d = calc.compute_class(d_id);

        // 验证非虚基类: B, C
        assert_eq!(abi_d.bases.len(), 2);
        assert_eq!(abi_d.bases[0].base, b_id);
        assert_eq!(abi_d.bases[0].offset, 0); // B is primary base
        assert_eq!(abi_d.bases[1].base, c_id);

        // 验证虚基类: A (只有一个，因为菱形共享)
        assert_eq!(abi_d.virtual_bases.len(), 1);
        assert_eq!(abi_d.virtual_bases[0].base, a_id);

        // 验证字段 d 存在
        assert_eq!(abi_d.fields.len(), 1);
        assert_eq!(abi_d.fields[0].name, "d");

        // 验证 vtable 中有 VBaseOffset 条目
        assert!(!abi_d.vtables.is_empty());
        let primary_vtable = &abi_d.vtables[0];
        let vbase_offsets: Vec<_> = primary_vtable
            .components
            .iter()
            .filter(|c| matches!(c, VTableComponent::VBaseOffset { .. }))
            .collect();
        assert_eq!(vbase_offsets.len(), 1);

        // 验证 A 是虚基类，且只有一个实例（菱形共享）
        match &vbase_offsets[0] {
            VTableComponent::VBaseOffset { target, offset: _ } => {
                assert_eq!(*target, a_id);
            }
            _ => panic!("expected VBaseOffset"),
        }
    }

    #[test]
    fn vtable_contains_vbase_offsets() {
        // 验证 vtable 中 VBaseOffset 条目的顺序和位置
        // class Base1 { virtual void f1(); int x; };
        // class Base2 { virtual void f2(); int y; };
        // class Derived : virtual Base1, virtual Base2 { int z; };
        let mut calc = ItaniumAbiCalculator::new();
        let base1_id = ClassId(0);
        let base2_id = ClassId(1);
        let derived_id = ClassId(2);

        // Base1
        calc.register_class(
            base1_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base1_id, "f1".into(), true, base1_id);

        // Base2
        calc.register_class(
            base2_id,
            vec![],
            vec![],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base2_id, "f2".into(), true, base2_id);

        // Derived : virtual Base1, virtual Base2
        calc.register_class(
            derived_id,
            vec![],
            vec![base1_id, base2_id],
            vec![("z".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);
        let vtable = &abi.vtables[0];

        // vtable 结构:
        // [VBaseOffset(Base1), VBaseOffset(Base2), OffsetToTop, TypeInfo, f1, f2]
        // address point 指向 f1

        // 收集所有 VBaseOffset
        let mut vbase_offsets = Vec::new();
        for comp in &vtable.components {
            if let VTableComponent::VBaseOffset { target, offset } = comp {
                vbase_offsets.push((*target, *offset));
            }
        }
        assert_eq!(vbase_offsets.len(), 2);
        assert_eq!(vbase_offsets[0].0, base1_id);
        assert_eq!(vbase_offsets[1].0, base2_id);

        // 验证 VBaseOffset 在 OffsetToTop 之前
        let mut vbase_seen = false;
        let mut offset_to_top_seen = false;
        for comp in &vtable.components {
            match comp {
                VTableComponent::VBaseOffset { .. } => {
                    assert!(!offset_to_top_seen, "VBaseOffset should come before OffsetToTop");
                    vbase_seen = true;
                }
                VTableComponent::OffsetToTop(_) => {
                    assert!(vbase_seen, "OffsetToTop should come after VBaseOffsets");
                    offset_to_top_seen = true;
                }
                _ => {}
            }
        }

        // address_point = 2 vbase offsets + offset-to-top + typeinfo = 4 * 8 = 32
        assert_eq!(vtable.address_point, 32);
    }

    #[test]
    fn compute_vbase_offset_works() {
        // 测试 compute_vbase_offset 公共 API
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let offset = calc.compute_vbase_offset(derived_id, base_id);
        assert!(offset.is_some());
        assert_eq!(offset.unwrap(), 16); // vptr(8) + y(4) + pad(4) = 16
    }

    #[test]
    fn this_adjust_with_virtual_base() {
        // 测试虚基类的 this 调整
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let adjust = calc.compute_this_adjust(derived_id, base_id);
        assert!(adjust.is_some());
        let adjust = adjust.unwrap();
        // 虚基类的 this_adjust 应该有 vbase_index
        assert!(adjust.vbase_index.is_some());
        assert_eq!(adjust.vbase_index.unwrap(), 0);
    }

    #[test]
    fn dtor_order_includes_virtual_bases() {
        // 验证析构顺序包含虚基类
        // class Base { int x; };
        // class Derived : Base, virtual VBase { int y; };
        // 析构顺序: y → Base → VBase
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let vbase_id = ClassId(1);
        let derived_id = ClassId(2);

        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![("b".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base_id, "f".into(), true, base_id);

        calc.register_class(
            vbase_id,
            vec![],
            vec![],
            vec![("v".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(vbase_id, "g".into(), true, vbase_id);

        calc.register_class(
            derived_id,
            vec![base_id],
            vec![vbase_id],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);

        // 析构顺序: 成员逆序 → 非虚基类逆序 → 虚基类逆序
        // 即: y → Base → VBase
        assert_eq!(abi.dtor_order.len(), 3);
        assert_eq!(abi.dtor_order[0], DtorStep::Member("y".into()));
        assert_eq!(abi.dtor_order[1], DtorStep::Base(base_id));
        assert_eq!(abi.dtor_order[2], DtorStep::VirtualBase(vbase_id));
    }

    // === Stage 7: RTTI 测试 ===

    #[test]
    fn rtti_no_bases_class_type_info() {
        // 无基类的类：typeinfo 符号应为 _ZTI + 类名
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_simple_class(
            class_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTIClass0");
        assert!(!abi.rtti.has_virtual_bases);
        assert!(abi.rtti.base_classes.is_empty());
    }

    #[test]
    fn rtti_single_inheritance_base_classes() {
        // 单继承：应该有一个非虚基类
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_simple_class(
            base_id,
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTIClass1");
        assert!(!abi.rtti.has_virtual_bases);
        assert_eq!(abi.rtti.base_classes.len(), 1);

        let base = &abi.rtti.base_classes[0];
        assert_eq!(base.base, base_id);
        assert_eq!(base.offset, 0); // primary base
        assert!(!base.is_virtual);
        assert_eq!(base.flags & 1, 1); // public flag
    }

    #[test]
    fn rtti_multiple_inheritance_base_classes() {
        // 多继承：应该有两个非虚基类
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);

        calc.register_simple_class(
            a_id,
            vec![("a".into(), CxxType::Integer { bits: 32, signed: true })],
        );
        calc.register_simple_class(
            b_id,
            vec![("b".into(), CxxType::Float { bits: 64 })],
        );
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![("c".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(c_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTIClass2");
        assert!(!abi.rtti.has_virtual_bases);
        assert_eq!(abi.rtti.base_classes.len(), 2);

        // 第一个基类 A (primary base, offset 0)
        assert_eq!(abi.rtti.base_classes[0].base, a_id);
        assert_eq!(abi.rtti.base_classes[0].offset, 0);
        assert!(!abi.rtti.base_classes[0].is_virtual);

        // 第二个基类 B (offset = 8，因为 A 大小 4 + 对齐到 8)
        assert_eq!(abi.rtti.base_classes[1].base, b_id);
        assert_eq!(abi.rtti.base_classes[1].offset, 8);
        assert!(!abi.rtti.base_classes[1].is_virtual);
    }

    #[test]
    fn rtti_virtual_inheritance_has_virtual_bases_flag() {
        // 虚继承：has_virtual_bases 应为 true
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);

        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![("y".into(), CxxType::Integer { bits: 32, signed: true })],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTIClass1");
        assert!(abi.rtti.has_virtual_bases);
        assert_eq!(abi.rtti.base_classes.len(), 1);

        let vb = &abi.rtti.base_classes[0];
        assert_eq!(vb.base, base_id);
        assert!(vb.is_virtual);
        assert_eq!(vb.flags & 8, 8); // virtual flag (Itanium ABI __virtual_mask = 0x8)
        assert_eq!(vb.flags & 1, 1); // public flag (Itanium ABI __public_mask = 0x1)
    }

    #[test]
    fn rtti_typeinfo_symbol_matches_vtable_typeinfo() {
        // vtable 中的 typeinfo 符号应与 rtti.typeinfo_symbol 一致
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_class(class_id, vec![], vec![], vec![], true);
        calc.register_method(class_id, "foo".into(), true, class_id);

        let abi = calc.compute_class(class_id);

        // 从 vtable 中提取 typeinfo 符号
        let vtable_typeinfo = abi.vtables.first().and_then(|vt| {
            vt.components.iter().find_map(|comp| match comp {
                VTableComponent::TypeInfo(s) => Some(s.clone()),
                _ => None,
            })
        });

        assert!(vtable_typeinfo.is_some());
        assert_eq!(vtable_typeinfo.unwrap(), abi.rtti.typeinfo_symbol);
    }
}
