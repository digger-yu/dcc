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
    /// 虚基子对象在 complete object 中的**静态**偏移（Step 2.5 布局结果）。
    ///
    /// vbtable 里存的是运行时值（对最派生对象而言等于该静态偏移），
    /// 但 abi_layout 作为布局唯一真源必须自己知道它: 字段展平、
    /// 二级 vbtable 的 offset 计算、RTTI 基类偏移都直接读它，
    /// 不再由 lower_ir 二次推导。
    pub static_offset: u64,
    /// 虚基子对象在对象里的具名字段 (`__vb_<Name>`)，供 `&obj.__vb_Base` 这类
    /// 显式子对象访问与字段展平使用。
    pub subobject_field: String,
}

/// 虚基子对象具名字段前缀。
///
/// 语义需求: 菱形虚继承下「共享子对象的地址」必须在 IR 里可表达
/// (`(char*)pb != (char*)&fo.__vb_Base`), 因此布局层为每个虚基子对象暴露
/// 一个具名句柄字段。sema 侧用同名 helper 注册对应 Field 符号。
pub const VBASE_SUBOBJECT_PREFIX: &str = "__vb_";

/// 由虚基类名生成其子对象具名字段名 (`Base` -> `__vb_Base`)。
pub fn vbase_subobject_field_name(class_name: &str) -> String {
    format!("{}{}", VBASE_SUBOBJECT_PREFIX, class_name)
}

/// 判断一个字段名是否是布局层合成的虚基子对象句柄字段。
pub fn is_vbase_subobject_field(name: &str) -> bool {
    name.starts_with(VBASE_SUBOBJECT_PREFIX)
}

/// vptr 布局
#[derive(Debug, Clone)]
pub struct VPtrLayout {
    pub offset: u64,
    /// 对应的 vtable
    pub vtable_index: usize,
    /// 该 vptr 只在**构造完整对象** (最派生类 ctor 的最后一步) 时写入。
    ///
    /// 虚基子对象的 vbptr 属于此类: 它指向的是「本类作为最派生类时为该
    /// 子对象准备的 secondary vbtable」, 基类构造变体 (C2) 不得写它。
    pub complete_only: bool,
    /// vptr 应写入的具体字节地址: `vtable.symbol + address_point_override`。
    ///
    /// `None` = 用该 vtable 自己的 `address_point` (主 vftable 的情形)。
    /// 一个 vtable symbol 可以同时承载多张表 (主 vftable + 各 chain 的
    /// secondary vftable + vbtable), 每个被写入的指针指向不同的 address
    /// point, 所以下标 `vtable_index` 不够用, 必须显式给出。
    pub address_point_override: Option<i64>,
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
    /// 本类的 vtable 需要发射的 this-adjust thunk 列表。
    ///
    /// 多继承下非 primary 基子对象的 vtable 槽若指向本类的覆写实现,
    /// 必须经 thunk 把 `this` 从基子对象调回完整对象, 否则实现会读到
    /// 错位的成员 (实测 `C : A, B` 里 `B* p; p->fb()` 读到 `A::a`)。
    pub thunks: Vec<ThunkDesc>,
    /// 对象里是否需要 vptr。它**不等于**「有虚函数」: 虚继承即使没有虚函数
    /// 也需要 vbptr 来承载 vbtable (Itanium 把 vbtable 放在 vtable 的负偏移区)。
    pub needs_vptr: bool,
    /// 本类是否有**直接**虚基类 (决定 standalone 布局偏移 0 处是否放 vbptr)。
    pub has_direct_virtual_bases: bool,
    /// 是否是多态类 (有虚函数)。`typeid(e)` 的动态路径只对该类成立。
    pub has_virtual_functions: bool,
    /// **非虚部分**大小: 不含本类自己的虚基子对象。
    ///
    /// 语义需求: 虚基子对象是共享的, 它只存在于最派生对象的尾部。当该类作为
    /// 另一个对象的 (虚/非虚) 基类子对象嵌入时, 只占据前 `non_virtual_size`
    /// 字节; 若误用 `size` (该类作为独立完整对象时的大小, 含其虚基尾部),
    /// 会把后续子对象错位/重叠。`0` 表示尚未计算。
    pub non_virtual_size: u64,
    /// 多态类作为**虚基子对象**嵌入时的首 vptr 槽大小 (0 或 8)。
    ///
    /// GCC 实测 (`M : virtual V{virtual fv}`):
    ///   M 布局 = [M.vptr][m][pad] | V 子对象 = [V 在 M 内的 vptr][v]
    ///   M ctor 写 `object+16 = &_ZTV1M[+56]` —— 虚基子对象**自己有一个**
    ///   vptr 槽, 由最派生类指向「本类为该链准备的 secondary vftable」。
    /// 该槽不属于 standalone 布局 (standalone V 的 vptr 在偏移 0), 所以单独
    /// 记账, 只用于虚基嵌入尺寸计算 (槽位本身由 `ClassAbi::vptrs` 逐条登记)。
    pub vbase_embedded_vptr: u64,
    /// 每个虚基子对象**自己的** vptr 槽位置: `(虚基类, 完整对象内偏移)`。
    ///
    /// 语义需求: 没有这个槽, `V* p; p->fv()` 只能读到 V 自己构造函数写的
    /// `_ZTV1V`, 最终覆写者被静默丢失 (实测 gcc 107 / dcc 7)。偏移已考虑
    /// 子对象内的 vbptr (vbptr 在 0, vptr 紧随其后)。
    pub vbase_own_vptr_slots: Vec<(ClassId, u64)>,
}

/// 展平后的字段描述（偏移相对对象首地址）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatField {
    /// 字段名（可能是布局层合成的 `__vb_<Name>` 子对象句柄）
    pub name: String,
    /// 相对对象首地址的偏移
    pub offset: u64,
    pub size: u64,
    pub align: u64,
    /// 该字段**声明所在**的类（用于回查字段类型）
    pub owner: ClassId,
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
    /// 每个虚槽对应的 **chain** (即该槽属于哪条基类子对象链)。
    ///
    /// `chains[slot] = Some(base)` 表示第 `slot` 个虚函数槽属于 `base`
    /// 的 secondary chain; `None` = primary chain。
    ///
    /// 语义需求 (Itanium + GCC 实测): `V* p; p->fv()` 在 GCC 里不是从
    /// `&d` 的主 vftable 取槽, 而是从 V 子对象自己的 vptr (= 本 symbol 内
    /// V chain 的 address point) 取槽, 那里放的是带 virtual thunk 的入口。
    /// 发射端必须按「this 静态类型所属 chain」选槽, 因此 chain 归属要落在
    /// 描述符里, 不能靠名字猜。
    pub chains: Vec<Option<ClassId>>,
}

/// this 调整量 (`ThisAdjust`) 到 thunk 入口符号的映射。
///
/// Itanium ABI 规定: 当一个虚槽的实现需要先调整 `this` 才能进入最终
/// 覆写函数时, vtable 里存的不是实现本身而是它的 **thunk 入口**。
/// - 只含固定非虚偏移 → non-virtual thunk
/// - `this` 位于虚基子对象内 → virtual thunk, 入口还要从 vbtable 读
///   vcall/vbase offset (偏移存在 vbtable 里, 对每个最派生类都不同,
///   无法在链接期折叠成常量)
///
/// 命名: 采用 `dcc_thunk_<p|m><|offset>_<target>` 而不是 Itanium 的
/// `_ZThn8_...`, 因为 thunk 在本后端是**真实的函数定义** (走
/// `ast::FuncDecl` → C 前端 → codegen), 符号必须是合法的 C 标识符;
/// Itanium 形式在负偏移时会出现 `-`, 无法过 lexer。入口地址与
/// GCC 逐字节一致 (同一 vtable 槽指向同一调整后入口), 只是名字不同 ——
/// thunk 名字不会跳 TU 引用, 不影响二进制布局兼容。
pub fn non_virtual_thunk_symbol(final_symbol: &str, non_virtual: i64) -> Option<String> {
    if non_virtual == 0 {
        return None;
    }
    let (sign, abs) = if non_virtual < 0 {
        ('m', -non_virtual)
    } else {
        ('p', non_virtual)
    };
    Some(format!("dcc_thunk_{}{}_{}", sign, abs, final_symbol))
}

/// 生成 thunk 入口符号。`vbase_index` 为 `Some` 时产出 virtual thunk 名。
pub fn thunk_symbol_for(
    final_symbol: &str,
    non_virtual: i64,
    vbase_index: Option<usize>,
) -> Option<String> {
    match vbase_index {
        // virtual thunk: 偏移部分是 vcall offset 在 vtable 里的槽序 (见
        // `VTableComponent::VCallOffset`), 与静态字节偏移不同轴, 单独编码。
        Some(idx) => Some(format!("dcc_vthunk_{}_{}_{}", idx, non_virtual, final_symbol)),
        None => non_virtual_thunk_symbol(final_symbol, non_virtual),
    }
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
        /// 虚方法名 (与 `collect_virtual_methods` 同源): 查虚调用槽号的键。
        /// 继承来的槽保留基类声明处的名字 (析构函数为 `~Base`), 重写不换名。
        name: String,
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

impl VTableLayout {
    /// 按虚方法名查槽号 (相对 address point, 即发射用的 `vptr + 8*slot`)。
    ///
    /// 只数 `Function` 分量: offset-to-top / typeinfo / vcall / vbase offset 都在
    /// address point 之前或不占虚槽序。名也以实现符号 (`M_id`) 作键, 便于调用方
    /// 用任一种标识查询。
    ///
    /// 析构函数: Itanium 里基类与派生类的析构共用同一槽 (槽里存的实现名带各自
    /// 类名 `~Base`/`~Derived`), 故两边都以 `~` 开头即视为同一槽。
    pub fn slot_of(&self, method_name: &str) -> Option<usize> {
        let want_dtor = method_name.starts_with('~');
        let mut slot = 0usize;
        for comp in &self.components {
            if let VTableComponent::Function { name, symbol, .. } = comp {
                if name == method_name
                    || symbol == method_name
                    || (want_dtor && name.starts_with('~'))
                {
                    return Some(slot);
                }
                slot += 1;
            }
        }
        None
    }

    /// address point 之后的虚函数槽序列 (方法名, 实现符号) —— 与布局层
    /// `collect_virtual_methods` 的展平结果一一对应, 供发射与调试对照。
    pub fn vfunc_slots(&self) -> Vec<(&str, &str)> {
        self.components
            .iter()
            .filter_map(|comp| match comp {
                VTableComponent::Function { name, symbol, .. } => {
                    Some((name.as_str(), symbol.as_str()))
                },
                _ => None,
            })
            .collect()
    }

    /// 某个虚槽在 vtable 里**实际存放的地址**对应的符号。
    ///
    /// 需要 `this` 调整时返回 thunk 入口, 否则返回最终覆写的实现符号。
    /// 发射 (`lower_ir::emit_vtables`) 与 thunk 定义侧都走这里, 保证
    /// 「vtable 里填的入口」与「发射出来的函数」同源。
    pub fn entry_symbol_at(&self, comp_index: usize) -> Option<String> {
        let comp = self.components.get(comp_index)?;
        match comp {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => Some(self.entry_symbol_for(symbol, adjustment)),
            _ => None,
        }
    }

    /// 给定虚槽的实现符号, 返回它在本 vtable 里应写入的入口符号
    /// (需要 `this` 调整时是 thunk 入口)。与 [`Self::components`]
    /// 的登记顺序无关, 供发射与 thunk 定义两侧共用。
    pub fn entry_symbol_for(&self, symbol: &str, adjustment: &ThisAdjust) -> String {
        thunk_symbol_for(symbol, adjustment.non_virtual, adjustment.vbase_index)
            .unwrap_or_else(|| symbol.to_string())
    }
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
    /// typeinfo 的 mangled 名字符串符号 (e.g. _ZTS3Foo)
    pub typeinfo_name_symbol: String,
    /// 是否有虚基类（影响 RTTI 结构类型）
    pub has_virtual_bases: bool,
    /// 基类列表
    pub base_classes: Vec<RttiBaseClass>,
    /// typeinfo 对象的具体类形态 (决定发射哪些槽)
    pub kind: TypeInfoKind,
}

/// `std::type_info` 在 Itanium ABI 下的三种具体类形态。
///
/// 三者共享同一段头部 (`[0]=vptr`, `[8]=const char* __name`), 之后按
/// kind 追加基类信息; `__do_dyncast`/`__do_catch` 等虚槽实现正是按这个
/// 约定解读对象, 所以 kind 判定与字段顺序必须与 libstdc++ 逐字节一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeInfoKind {
    /// `__class_type_info`: 无基类, 共 2 槽 (vptr, name)
    #[default]
    Class,
    /// `__si_class_type_info`: 恰好一个**非虚**基类, 3 槽 (vptr, name, base)
    SingleInheritance,
    /// `__vmi_class_type_info`: 多基类或含虚基类,
    /// 槽序 (vptr, name, flags|base_count, [base_ti, offset_flags]...)
    Vmi,
}

/// Itanium `__base_info::__offset_flags` 低字节标志位。
///
/// 真值来源: 本机 libstdc++ 实测 (`objdump -s .data.rel.ro._ZTI5Multi`:
/// 两个公有非虚基 -> `0x2` / `0x802`, 即 `offset<<8 | __public_mask`;
/// `_ZTI7Derived` 单个公有虚基 -> `0xffffffffffe00003`, 即 `-32<<8 | 0x3`)。
pub const RTTI_BASE_VIRTUAL_MASK: u32 = 0x1;
/// `__offset_flags` 的 public 标志位 (非旧文档里的 0x1)。
pub const RTTI_BASE_PUBLIC_MASK: u32 = 0x2;
/// `__offset_flags` 里偏移量的左移位数。
pub const RTTI_BASE_OFFSET_SHIFT: u32 = 8;

/// typeinfo 对象发射用的一个 8 字节槽 —— 布局层唯一真源, native 后端只负责
/// 把它翻译成 `.data.rel.ro` 里的一个字 (符号地址或立即数)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeInfoWord {
    /// 该 kind 的 `type_info` 虚表 address point 符号。
    VTable(&'static str),
    /// 指向 `_ZTS...` mangled 名字符串的指针 (`__name` 字段)。
    Name(String),
    /// 指向另一个类的 `_ZTI...` (基类 typeinfo / `__base_info::__type`)。
    BaseTypeInfo(String),
    /// 8 字节立即数: `__flags|__base_count` 打包字与 `__offset_flags`。
    Const(i64),
}

#[derive(Debug, Clone)]
pub struct RttiBaseClass {
    pub base: ClassId,
    pub offset: i64,
    pub is_virtual: bool,
    pub flags: u32,
}

impl RttiBaseClass {
    /// `__base_info::__offset_flags` = `(offset << 8) | flags` (Itanium ABI)。
    pub fn offset_flags(&self) -> i64 {
        (self.offset << RTTI_BASE_OFFSET_SHIFT) | (self.flags as i64 & 0xff)
    }
}

impl TypeInfoKind {
    /// 该 kind 在 libstdc++ 里的**标准**虚表符号名。
    ///
    /// 仅作交叉验证/文档用途: dcc **不**以该名字发射定义。实测
    /// (`nm -D libstdc++.so.6` 显示其为 `V` weak 导出, 且把同名 weak 定义放进
    /// 可执行文件会 interpose 覆盖 libstdc++ 的表 —— 用垃圾表链入 g++ 产物即可
    /// 观察到 `typeid(Derived)` 的 vptr 指向了可执行文件里的定义), 一旦 dcc 自带
    /// 一份不完全一致的表, 整个进程 (含 std::exception 的 `__do_catch`) 都会被带
    /// 偏。故 dcc 发射自己的私有命名虚表, 槽值逐个引用 libstdc++ 导出的真实实现。
    pub fn standard_vtable_symbol(self) -> &'static str {
        match self {
            Self::Class => "_ZTVN10__cxxabiv117__class_type_infoE",
            Self::SingleInheritance => "_ZTVN10__cxxabiv120__si_class_type_infoE",
            Self::Vmi => "_ZTVN10__cxxabiv121__vmi_class_type_infoE",
        }
    }

    /// dcc 为该 kind 发射的虚表符号 (地址点 = 符号起始处)。
    ///
    /// 与标准名的唯一差别是「符号地址即 address point」: dcc 的静态初始化只
    /// 能发无加数的符号重定位 (`&sym`, 不支持 `&sym + 16`), 而 vptr 的语义本来
    /// 就是「指向 slot 0」, 消费方 (libstdc++ 虚调用 / dcc 自身) 只从 vptr 正向
    /// 取槽, 不会去读 `vptr[-1]/[-2]`, 所以少发两个头部字不影响任何调用。
    pub fn vtable_symbol(self) -> &'static str {
        match self {
            Self::Class => "dcc_ti_class_vtable",
            Self::SingleInheritance => "dcc_ti_si_vtable",
            Self::Vmi => "dcc_ti_vmi_vtable",
        }
    }

    /// 9 个虚槽的实现符号 (libstdc++ 导出, 按 `nm -D` + 运行期 `dladdr` 实测)。
    ///
    /// 槽序: `[0]=D1 ~type_info` `[1]=D0 deleting` `[2]=__is_pointer_p`
    /// `[3]=__is_function_p` `[4]=__do_catch` `[5]=__do_upcast(2参)`
    /// `[6]=__do_upcast(3参)` `[7]=__do_dyncast` `[8]=__do_find_public_src`。
    /// 其中 `[4]/[5]` 三种 kind 都继承自 `__class_type_info` (libstdc++ 的表里
    /// 就是同两个地址)。
    pub fn slot_symbols(self) -> [&'static str; 9] {
        const IS_PTR: &str = "_ZNKSt9type_info14__is_pointer_pEv";
        const IS_FN: &str = "_ZNKSt9type_info15__is_function_pEv";
        const DO_CATCH: &str = "_ZNK10__cxxabiv117__class_type_info10__do_catchEPKSt9type_infoPPvj";
        const DO_UPCAST2: &str = "_ZNK10__cxxabiv117__class_type_info11__do_upcastEPKS0_PPv";
        match self {
            Self::Class => [
                "_ZN10__cxxabiv117__class_type_infoD1Ev",
                "_ZN10__cxxabiv117__class_type_infoD0Ev",
                IS_PTR,
                IS_FN,
                DO_CATCH,
                DO_UPCAST2,
                "_ZNK10__cxxabiv117__class_type_info11__do_upcastEPKS0_PKvRNS0_15__upcast_resultE",
                "_ZNK10__cxxabiv117__class_type_info12__do_dyncastElNS0_10__sub_kindEPKS0_PKvS3_S5_RNS0_16__dyncast_resultE",
                "_ZNK10__cxxabiv117__class_type_info20__do_find_public_srcElPKvPKS0_S2_",
            ],
            Self::SingleInheritance => [
                "_ZN10__cxxabiv120__si_class_type_infoD1Ev",
                "_ZN10__cxxabiv120__si_class_type_infoD0Ev",
                IS_PTR,
                IS_FN,
                DO_CATCH,
                DO_UPCAST2,
                "_ZNK10__cxxabiv120__si_class_type_info11__do_upcastEPKNS_17__class_type_infoEPKvRNS1_15__upcast_resultE",
                "_ZNK10__cxxabiv120__si_class_type_info12__do_dyncastElNS_17__class_type_info10__sub_kindEPKS1_PKvS4_S6_RNS1_16__dyncast_resultE",
                "_ZNK10__cxxabiv120__si_class_type_info20__do_find_public_srcElPKvPKNS_17__class_type_infoES2_",
            ],
            Self::Vmi => [
                "_ZN10__cxxabiv121__vmi_class_type_infoD1Ev",
                "_ZN10__cxxabiv121__vmi_class_type_infoD0Ev",
                IS_PTR,
                IS_FN,
                DO_CATCH,
                DO_UPCAST2,
                "_ZNK10__cxxabiv121__vmi_class_type_info11__do_upcastEPKNS_17__class_type_infoEPKvRNS1_15__upcast_resultE",
                "_ZNK10__cxxabiv121__vmi_class_type_info12__do_dyncastElNS_17__class_type_info10__sub_kindEPKS1_PKvS4_S6_RNS1_16__dyncast_resultE",
                "_ZNK10__cxxabiv121__vmi_class_type_info20__do_find_public_srcElPKvPKNS_17__class_type_infoES2_",
            ],
        }
    }
}

impl RttiLayout {
    /// 按 Itanium ABI 展开 typeinfo 对象的全部 8 字节槽。
    ///
    /// `__vmi_class_type_info` 的 `__flags`(u32)/`__base_count`(u32) 打包进同一个
    /// 8 字节字 (小端: 低 4 字节 flags, 高 4 字节 base_count), 之后每个基类占
    /// 两个字: `__type` 指针 + `__offset_flags`。`__flags` 的 `__flag_marked`
    /// 只在 libstdc++ 自身的构造期优化里用到, 实测 g++ 产物恒为 0。
    pub fn typeinfo_words(&self, name_symbol: impl Fn(ClassId) -> String) -> Vec<TypeInfoWord> {
        let mut words = vec![
            TypeInfoWord::VTable(self.kind.vtable_symbol()),
            TypeInfoWord::Name(self.typeinfo_name_symbol.clone()),
        ];
        match self.kind {
            TypeInfoKind::Class => {},
            TypeInfoKind::SingleInheritance => {
                if let Some(b) = self.base_classes.first() {
                    words.push(TypeInfoWord::BaseTypeInfo(name_symbol(b.base)));
                }
            },
            TypeInfoKind::Vmi => {
                let flags: i64 = 0;
                let count: i64 = self.base_classes.len() as i64;
                words.push(TypeInfoWord::Const(flags | (count << 32)));
                for b in &self.base_classes {
                    words.push(TypeInfoWord::BaseTypeInfo(name_symbol(b.base)));
                    words.push(TypeInfoWord::Const(b.offset_flags()));
                }
            },
        }
        words
    }
}

// === Thunk ===

/// thunk 描述符
///
/// 布局层是 thunk 的唯一分配者: `compute_vtables` 在发现某个虚槽的实现需要
/// `this` 调整时登记一条 `ThunkDesc`, 并把 vtable 槽的入口切成
/// [`non_virtual_thunk_symbol`] 生成的 thunk 符号。发射端 (`lower_ir`) 只按
/// 本表逐条生成真实函数符号, 不再自行推导偏移。
#[derive(Debug, Clone)]
pub struct ThunkDesc {
    /// thunk 入口符号 (写进 vtable 的那个地址)
    pub symbol: String,
    /// 最终覆写实现的符号 (thunk 尾调用的目标)
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

    /// 查询指定虚基类子对象在本对象中的静态偏移。
    pub fn vbase_static_offset(&self, base_class: ClassId) -> Option<i64> {
        self.virtual_bases
            .iter()
            .find(|vb| vb.base == base_class)
            .map(|vb| vb.static_offset as i64)
    }

    /// 展平本类所有可寻址字段（含基类子对象字段、虚基共享子对象字段），
    /// 偏移相对对象首地址。
    ///
    /// 这是布局层唯一真源: lower_ir / native 发射 StructDef 时只查询,
    /// 不再自己从 vtable 条目里重算虚基偏移。
    ///
    /// 规则:
    /// - 非虚基类子对象在前（按布局顺序），然后是本类字段，最后是虚基
    ///   共享子对象的字段（偏移取 `virtual_bases[i].static_offset`）；
    /// - 子对象（`embedded == true`）内部的 `__vb_*` 句柄字段与其虚基类字段
    ///   **不**参入展平——那些虚基类是共享的，已出现在最外层对象尾部；
    /// - 同名字段只保留首个（共享子对象只有一份实例）。
    pub fn flatten_fields(&self, abi_map: &HashMap<ClassId, ClassAbi>) -> Vec<FlatField> {
        let mut out = Vec::new();
        self.flatten_fields_into(abi_map, 0, false, &mut out);
        let mut seen: Vec<String> = Vec::new();
        out.retain(|f| {
            if seen.contains(&f.name) {
                false
            } else {
                seen.push(f.name.clone());
                true
            }
        });
        out
    }

    fn flatten_fields_into(
        &self,
        abi_map: &HashMap<ClassId, ClassAbi>,
        base_offset: u64,
        embedded: bool,
        out: &mut Vec<FlatField>,
    ) {
        self.flatten_fields_into_shifted(abi_map, base_offset, 0, embedded, out)
    }

    /// 字段展平主体。
    ///
    /// `field_shift`: 本类的字段相对 `base_offset` 还要往后推的字节数。
    /// 它只在一个场景非零 —— 多态类作为**虚基子对象**嵌入时, 子对象头部
    /// 被最派生类用来放它自己的 vptr (GCC 实测 `V` 嵌入形态 = [vptr][v]),
    /// 因此字段不能按 standalone 的偏移 0 排放, 否则成员访问会读到 vptr 槽。
    fn flatten_fields_into_shifted(
        &self,
        abi_map: &HashMap<ClassId, ClassAbi>,
        base_offset: u64,
        field_shift: u64,
        embedded: bool,
        out: &mut Vec<FlatField>,
    ) {
        for base in &self.bases {
            if let Some(babi) = abi_map.get(&base.base) {
                babi.flatten_fields_into_shifted(abi_map, base_offset + base.offset, 0, true, out);
            }
        }
        for f in &self.fields {
            if embedded && is_vbase_subobject_field(&f.name) {
                continue;
            }
            out.push(FlatField {
                name: f.name.clone(),
                offset: base_offset + field_shift + f.offset,
                size: f.size,
                align: f.align,
                owner: self.class_id,
            });
        }
        if embedded {
            return;
        }
        for vb in &self.virtual_bases {
            let Some(vbabi) = abi_map.get(&vb.base) else {
                continue;
            };
            // 虚基子对象的字段要跳过嵌入时**额外**预留的头部槽; 复用
            // standalone 布局时不偏移 (否则会把字段往后多推 8 字节)。
            let shift = vb_embedded_extra_head(vbabi);
            vbabi.flatten_fields_into_shifted(
                abi_map,
                base_offset + vb.static_offset,
                shift,
                true,
                out,
            );
        }
    }

    /// 查询指定虚函数在 primary vtable 中的 slot 索引。
    ///
    /// slot 索引从 address point 之后开始编号（0, 1, 2...）。
    /// 即 slot 0 对应 address point 处的第一个虚函数。
    pub fn get_vtable_slot(&self, method_name: &str) -> Option<usize> {
        let vtable = self.vtables.first()?;
        vtable.slot_of(method_name)
    }

    /// 虚调用槽号的**单一真源**查询 (供 lower_ir 发射 `call *[vptr + 8*slot]`)。
    ///
    /// 为何以 this 指针的**静态类型**类的主 vtable 顺序为准:
    /// - Itanium 下派生类主 vtable 以 primary base 链的槽序为**前缀**,
    ///   重写不换槽 —— 基类的槽号在最派生类里不变;
    /// - 非 primary 基子对象的 vptr 指向该基类自己的 secondary vtable,
    ///   其函数顺序与该基类自身的 primary vtable 一致。
    ///
    /// sema 解析期给的槽号只是占位 (登记顺序决定, 多继承/虚析构下与真实
    /// 布局不同序), 发射前必须用本查询校正。
    pub fn vcall_slot(&self, method_name: &str) -> Option<usize> {
        self.vtables.first().and_then(|vt| vt.slot_of(method_name))
    }

    /// 按方法名查 vtable 槽号（含非 primary 基子对象的 secondary vtable）。
    pub fn vcall_slot_for_base(&self, base: ClassId, method_name: &str) -> Option<usize> {
        self.vtables
            .iter()
            .find(|vt| vt.for_base == Some(base))
            .and_then(|vt| vt.slot_of(method_name))
    }

    /// 判断该类是否为多态类（有虚函数，即有 vtable）。
    ///
    /// 注意: 只携带 vbtable 的虚继承类**不是**多态类 ([basic.def.odr]/typeid
    /// 的 polymorphic 定义 = 声明了虚函数)，因此这里看虚函数而不是 vptr。
    pub fn is_polymorphic(&self) -> bool {
        self.has_virtual_functions
    }
}

/// ABI 计算器 trait。
/// 输入类定义，输出 ClassAbi 描述符。
pub trait AbiCalculator {
    fn compute_class(&self, class: ClassId) -> ClassAbi;
}

/// 一个虚函数槽的展开结果 (由 `collect_virtual_methods` 按 vtable 顺序产出)。
///
/// - `symbol` / `owner`: **最终覆写者**的实现符号与它所属的类 (查 this 调整量用)
/// - `declaring`: **声明这条虚槽**的类 —— 槽的不变身份。多个继承路径共享
///   同一个虚基 (菱形) 时, 只有按 `declaring` 去重才能得到与 GCC 一致的槽序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VSlot {
    /// 虚函数名 (析构函数为 `~Class`)
    pub name: String,
    /// 最终实现的符号名
    pub symbol: String,
    /// 提供实现的类
    pub owner: ClassId,
    /// 声明这条虚槽的类
    pub declaring: ClassId,
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
    /// ClassId -> 源语言类名 (用于生成真实 `_ZTV` / `_ZTI` / `_ZTS` 符号)。
    ///
    /// 之前 vtable/typeinfo 符号名用 `Class{id}` 占位, 而 id 是符号表下标:
    /// 同一份 class 定义在不同 TU 会得到不同 id, 多 TU 链接时既不能合并
    /// (名字不同) 也不符合 ABI。现在按真实类名 mangle。
    class_names: HashMap<ClassId, String>,
    /// ClassId -> **限定**源语言名 (`ns::Outer::Inner`), 仅用于 `_ZTV` / `_ZTI`
    /// / `_ZTS` 符号 mangle。
    ///
    /// 与 `class_names` 分开的理由: 布局层还会用**简单类名**合成与 sema 协商
    /// 好的内部标识 (虚基子对象句柄字段 `__vb_Base`), 那些不能变成限定名。
    /// 未注册时回退到 `class_names`, 保证旧调用路径行为不变。
    class_mangle_names: HashMap<ClassId, String>,
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
    /// 实现符号 -> **声明该类**的映射。
    ///
    /// `MethodInfo::owner_class` 写的是「被覆写的基类方法」(供同槽识别),
    /// 不能拿来回答「函数体按哪个类的子对象布局访问 this」。后者需要
    /// 声明类: 它就是发射这个符号的那个类。虚基链的 vftable 要用它算
    /// this 调整 (否则给未覆写的基实现插上一个莫须有的 -vbase_off thunk,
    /// vtable 里出现未定义符号, 运行期跳空指针)。
    symbol_decl: HashMap<String, ClassId>,
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
                _ => (*bits as u64).div_ceil(8),
            };
            (bytes, bytes)
        },
        CxxType::Float { bits } => {
            let bytes = match bits {
                32 => 4,
                64 => 8,
                80 => 16, // x86 long double is 16-byte aligned on x86_64
                128 => 16,
                _ => (*bits as u64).div_ceil(8),
            };
            (bytes, bytes)
        },
        CxxType::Pointer(_) => (POINTER_SIZE, POINTER_ALIGN),
        CxxType::LValueRef(_) | CxxType::RValueRef(_) => (POINTER_SIZE, POINTER_ALIGN),
        CxxType::Array(elem, Some(count)) => {
            let (elem_size, elem_align) = type_size_align(elem, calc);
            (elem_size * *count as u64, elem_align)
        },
        CxxType::Array(elem, None) => {
            // 不完整数组类型，大小为 0
            let (_, elem_align) = type_size_align(elem, calc);
            (0, elem_align)
        },
        CxxType::Class(class_id) => {
            let abi = calc.compute_class(*class_id);
            (abi.size, abi.align.max(1))
        },
        CxxType::Enum(_) => {
            // 在 Itanium ABI 中，enum 底层类型通常是 int (4 bytes)
            (4, 4)
        },
        CxxType::MemberPointer { .. } => {
            // 简化: 成员指针按指针大小处理
            (POINTER_SIZE, POINTER_ALIGN)
        },
        CxxType::Function { .. } => {
            // 函数类型本身没有大小（不能实例化）
            (0, 1)
        },
        CxxType::Dependent(_) => (0, 1),
        CxxType::Alias(_) => {
            // 此处不解析别名，按 0 处理（实际使用时应先解析）
            (0, 1)
        },
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

    /// 注册类的源语言名字，用于生成真实 mangled 的 vtable / typeinfo 符号。
    pub fn register_class_name(&mut self, class: ClassId, name: impl Into<String>) {
        self.class_names.insert(class, name.into());
    }

    /// 注册用于 ABI 符号 mangle 的**限定**类名 (`ns::Widget`)。
    pub fn register_class_mangle_name(&mut self, class: ClassId, name: impl Into<String>) {
        let n = name.into();
        if !n.is_empty() {
            self.class_mangle_names.insert(class, n);
        }
    }

    /// ABI 符号 (`_ZTV` / `_ZTI` / `_ZTS`) 用的类名: 优先限定名, 未注册时退回
    /// 简单名 (再由 `class_name` 退回 `Class{id}` 占位)。
    pub fn class_mangle_name(&self, class: ClassId) -> String {
        self.class_mangle_names
            .get(&class)
            .cloned()
            .unwrap_or_else(|| self.class_name(class))
    }

    /// 查询类名；未注册时回退到 `Class{id}` 占位（仅用于不完整注册的测试场景）。
    pub fn class_name(&self, class: ClassId) -> String {
        self.class_names
            .get(&class)
            .cloned()
            .unwrap_or_else(|| format!("Class{}", class.0))
    }

    /// 本 TU 里该类的 vtable 符号名 (`_ZTV<encoding>`)。
    pub fn vtable_symbol(&self, class: ClassId) -> String {
        crate::cpp::mangle::mangle_vtable(&self.class_mangle_name(class))
    }

    /// 本 TU 里该类的 typeinfo 符号名 (`_ZTI<encoding>`)。
    pub fn typeinfo_symbol(&self, class: ClassId) -> String {
        crate::cpp::mangle::mangle_typeinfo(&self.class_mangle_name(class))
    }

    /// 本 TU 里该类的 typeinfo 名字符串符号 (`_ZTS<encoding>`)。
    pub fn typeinfo_name_symbol(&self, class: ClassId) -> String {
        format!("_ZTS{}", crate::cpp::mangle::encode_type_name(&self.class_mangle_name(class)))
    }

    /// 虚基子对象 `base` 在 `class` 里的 secondary vbtable 符号名。
    ///
    /// 该符号不遵守 Itanium `_ZTV` 规范 (Itanium 用局部标签), 但必须
    /// 对同一对 (class, virtual base) 在所有 TU 里稳定, 否则多 TU weak
    /// 链接无法合并, 所以用真实类名而非 ClassId 下标拼接。
    fn sub_vbtable_symbol(&self, class: ClassId, base: ClassId) -> String {
        format!("_ZTV{}::vb{}", self.class_name(class), self.class_name(base))
    }

    /// 注册一个类的定义信息（带真实类名）。
    pub fn register_class_with_name(
        &mut self,
        class: ClassId,
        name: &str,
        bases: Vec<ClassId>,
        virtual_bases: Vec<ClassId>,
        fields: Vec<(String, CxxType)>,
        has_virtual: bool,
    ) {
        self.register_class_name(class, name);
        self.register_class(class, bases, virtual_bases, fields, has_virtual);
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
                symbol_decl: HashMap::new(),
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
            if class_id == owner_class {
                def.symbol_decl.insert(symbol.clone(), class_id);
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
    /// - `class_id`: 方法登记所在的类 (可能是「继承同名虚槽」的派生类)
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
        self.register_method_with_decl(class_id, name, symbol, is_virtual, owner_class, None);
    }

    /// 同上, 额外登记「这个实现符号由哪个类声明」。
    ///
    /// 布局层已有信息区分不了二者: `class_id` 是槽位的登记地, `owner_class`
    /// 是被覆写的基方法所属类。声明类只能由调用方 (拿得到符号表的 native
    /// 组装层) 显式告知。
    pub fn register_method_with_decl(
        &mut self,
        class_id: ClassId,
        name: String,
        symbol: String,
        is_virtual: bool,
        owner_class: ClassId,
        declared_in: Option<ClassId>,
    ) {
        if let Some(def) = self.classes.get_mut(&class_id) {
            if is_virtual {
                def.has_virtual = true;
            }
            // 只有本类自己声明的实现才能登记为它的声明地: `class_id ==
            // owner_class` 说明该条目没被任何基类方法占位 (新虚函数或本类
            // 声明), 或者调用方直接给出了声明类。
            if let Some(d) = declared_in {
                if d == class_id {
                    def.symbol_decl.insert(symbol.clone(), d);
                }
            } else if class_id == owner_class {
                def.symbol_decl.insert(symbol.clone(), class_id);
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

    /// 判断类是否需要 vptr（自身或继承虚函数，或拥有虚基类）。
    ///
    /// Itanium: 虚继承类即使没有虚函数也需要 vbptr，它指向同一张 vtable 的
    /// address point，vbase offsets 存在 address point 之前的负偏移区。
    fn class_needs_vptr(&self, class_id: ClassId) -> bool {
        if self.class_has_virtual(class_id) {
            return true;
        }
        !self.collect_all_virtual_bases(class_id).is_empty()
    }

    /// 收集虚函数槽 (按 vtable 顺序展平)。
    ///
    /// 返回每一项是一条虚槽的【最终实现】与【槽身份】; 重写的虚函数
    /// `symbol`/`owner` 为派生类的, `declaring` 仍是被声明的那条槽。
    /// Stage 6: 支持虚继承场景。
    fn collect_virtual_methods(&self, class_id: ClassId) -> Vec<VSlot> {
        let def = match self.classes.get(&class_id) {
            Some(d) => d,
            None => return Vec::new(),
        };

        let mut methods: Vec<VSlot> = Vec::new();

        // 先收集非虚基类的虚函数（按基类声明顺序），再收集虚基类的
        // (按虚基类声明顺序) —— Itanium 的虚槽序就是这个次序。
        //
        // 去重键 = (名字, **声明该虚槽的类**)。不能只按名字: 同一个名字在
        // `X{virtual f}` 与 `Y{virtual f}` 里是两个独立槽; 也不能不兼面
        // 到共享: `Diamond : Left, Right` 且 Left/Right 都 `virtual V` 时,
        // V::fv 只会存在**一份** V 子对象, 只占一个槽 (按名字去重会
        // 丢掉 X/Y 两槽; 不去重则 `_ZTV7Diamond` 把 fv 发两遍, 槽号与
        // GCC 不一致, 虚调用绑到错的实现)。
        for &base in def.bases.iter().chain(def.virtual_bases.iter()) {
            let base_methods = self.collect_virtual_methods(base);
            for slot in base_methods {
                if methods
                    .iter()
                    .any(|s| s.name == slot.name && s.declaring == slot.declaring)
                {
                    continue;
                }
                // 检查派生类是否重写
                if let Some(m) = def
                    .methods
                    .iter()
                    .find(|m| m.is_virtual && m.name == slot.name)
                {
                    // 重写: 实现换成派生类的 symbol, 但**槽身份不变**
                    methods.push(VSlot {
                        name: slot.name.clone(),
                        symbol: m.symbol.clone(),
                        owner: class_id,
                        declaring: slot.declaring,
                    });
                } else {
                    // 未重写: 继承基类的
                    methods.push(slot);
                }
            }
        }

        // 再添加派生类自己新增的虚函数
        // 新增: owner_class == class_id 且未在基类虚函数中出现
        for m in &def.methods {
            if m.is_virtual
                && m.owner_class == class_id
                && !methods.iter().any(|s| s.name == m.name)
            {
                methods.push(VSlot {
                    name: m.name.clone(),
                    symbol: m.symbol.clone(),
                    owner: class_id,
                    declaring: class_id,
                });
            }
        }

        methods
    }

    /// 虚基子对象在最派生对象里使用的 secondary vftable 符号名。
    ///
    /// 与 GCC 写在 `_ZTV<D>` 里的 V 链那段对应 (本后端把它拆成独立符号,
    /// 因为发射层一个 `Decl::Var` 对应一个符号; 内容布局与 GCC 的 chain 段
    /// 逐字节一致: [offset-to-top] [typeinfo for D] [虚槽...])。
    fn vbase_vftable_symbol(&self, class_id: ClassId, vbase: ClassId) -> String {
        format!("_ZTV{}::vbase{}", self.class_name(class_id), self.class_name(vbase))
    }

    /// 在登记信息里找实现符号 `symbol` 的**声明类**。
    ///
    /// `MethodInfo::owner_class` 记的是「该虚槽实现被谁覆写」(供同槽识别),
    /// 不能拿来回答「函数体按哪个类的布局访问 this」。声明类才是后者: 它就是
    /// 发射这个符号的那个类, 由 `ClassDef::symbol_decl` 在登记时直接记下。
    pub fn declaring_class_of(&self, symbol: &str) -> Option<ClassId> {
        for def in self.classes.values() {
            if let Some(cid) = def.symbol_decl.get(symbol) {
                return Some(*cid);
            }
        }
        None
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

        // 只要有虚函数**或**有虚基类就要生成 vtable: 后者用于承载 vbtable。
        if !self.class_needs_vptr(class_id) {
            return Vec::new();
        }

        let class_name = self.class_name(class_id);
        let typeinfo_sym = self.typeinfo_symbol(class_id);
        let vtable_sym = self.vtable_symbol(class_id);
        let mut vtables = Vec::new();

        // 找到 primary base（第一个有虚函数的非虚基类）
        let primary_base = def
            .bases
            .iter()
            .copied()
            .find(|&b| self.class_has_virtual(b));

        // 虚基类列表（从 abi 获取，已经计算好偏移）
        let virtual_bases = &abi.virtual_bases;

        // --- Primary vtable ---
        let vmethods = self.collect_virtual_methods(class_id);

        let mut components = Vec::new();

        // vbase offset 条目（在 address point 之前，即负偏移位置）
        // 按 Itanium ABI: vbase offsets 在 offset-to-top 之前。
        // 偏移值直接取布局层 Step 2.5 已算好的静态偏移，不重算。
        for vb in virtual_bases {
            components.push(VTableComponent::VBaseOffset {
                target: vb.base,
                offset: vb.static_offset as i64,
            });
        }

        // offset-to-top (complete object: 0)
        components.push(VTableComponent::OffsetToTop(0));
        // typeinfo 指针
        components.push(VTableComponent::TypeInfo(typeinfo_sym.clone()));
        // 虚函数槽（从 address point 开始）
        for slot in &vmethods {
            components.push(VTableComponent::Function {
                name: slot.name.clone(),
                symbol: slot.symbol.clone(),
                adjustment: ThisAdjust::default(),
            });
        }

        // address_point = vbase_offsets + offset-to-top + typeinfo
        let address_point = (virtual_bases.len() as i64 + 2) * POINTER_SIZE as i64;

        vtables.push(VTableLayout {
            symbol: vtable_sym.clone(),
            address_point,
            chains: chain_tags(&components, None),
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

            // 计算从 base subobject 到 complete object 的 this 调整。
            // 直取 Step 1 已算好的 `abi.bases` 偏移: 早前这里调
            // `direct_base_offset` 走另一套估算 (`estimate_class_size`), 而它
            // 不知道「虚基子对象会被抬到最派生对象尾部」, 于是把 primary base
            // 的**整块**尺寸算进去, 得到 32 而不是真实的 16; secondary vbtable
            // 的 vbase offset 就变成 32-32=0 —— 经二级基指针取虚基直接落到
            // 子对象头部 (实测 off_right=16 / gcc 32)。
            let base_offset = abi
                .bases
                .iter()
                .find(|b| b.base == base_id)
                .map(|b| b.offset as i64)
                .unwrap_or(0);

            let base_methods = self.collect_virtual_methods(base_id);

            let mut components = Vec::new();
            // vbase offset 条目（secondary vtable 也需要 vbase offsets）
            for vb in virtual_bases {
                // 从 base subobject 视角看，vbase offset = 完整对象的 vbase offset - base_offset
                let adjusted_vbase_offset = vb.static_offset as i64 - base_offset;
                components.push(VTableComponent::VBaseOffset {
                    target: vb.base,
                    offset: adjusted_vbase_offset,
                });
            }

            // offset-to-top: 从 base subobject 到 complete object 的偏移（负值）
            components.push(VTableComponent::OffsetToTop(-base_offset));
            components.push(VTableComponent::TypeInfo(typeinfo_sym.clone()));

            for slot in &base_methods {
                let name = &slot.name;
                // 检查派生类是否重写
                if let Some(m) = def.methods.iter().find(|m| m.is_virtual && m.name == *name) {
                    // 派生类重写: 需要 thunk 调整 this 指针（从 base 到 complete object）
                    components.push(VTableComponent::Function {
                        name: name.clone(),
                        symbol: m.symbol.clone(),
                        adjustment: ThisAdjust::offset(-base_offset),
                    });
                } else {
                    // 未重写: 使用提供实现的类 (`slot.owner`) 的函数
                    let base_def = &self.classes[&slot.owner];
                    let sym = base_def
                        .methods
                        .iter()
                        .find(|bm| bm.is_virtual && bm.name == *name)
                        .map(|bm| bm.symbol.clone())
                        .unwrap_or_else(|| format!("{}::{}", self.class_name(slot.owner), name));
                    components.push(VTableComponent::Function {
                        name: name.clone(),
                        symbol: sym,
                        adjustment: ThisAdjust::default(),
                    });
                }
            }

            let address_point = (virtual_bases.len() as i64 + 2) * POINTER_SIZE as i64;

            vtables.push(VTableLayout {
                symbol: format!("_ZTV{}::base{}", class_name, self.class_name(base_id)),
                address_point,
                chains: chain_tags(&components, Some(base_id)),
                components,
                for_base: Some(base_id),
            });
        }

        // --- 虚基子对象的 secondary vbtable ---
        // 菱形虚继承下, 自己带虚基类的虚基子对象 (如 Final 里的 Mid1) 不能
        // 共用 complete object 的 primary vbtable: 它的 vbase offsets 必须相对该
        // 子对象, offset-to-top 必须为负的子对象偏移。这正是 Itanium 用 VTT
        // 传递、并在最派生构造函数末尾回写各子对象 vbptr 的那一步。
        for vb in virtual_bases {
            // 只为**直接**虚基类生成 secondary vbtable: 间接虚基子对象总是
            // 落在直接虚基子对象内部, 由它自己负责。
            if !def.virtual_bases.contains(&vb.base) {
                continue;
            }
            let sub_offset = vb.static_offset as i64;
            let sub_vbases = self.collect_all_virtual_bases(vb.base);
            // 子对象自己没有虚基类 → 不需要 vbtable (它只有普通 vptr,
            // 由该虚基类自己的构造函数写入)。
            if sub_vbases.is_empty() {
                continue;
            }

            let mut components = Vec::new();
            for &w in &sub_vbases {
                let w_abs = abi
                    .virtual_bases
                    .iter()
                    .find(|x| x.base == w)
                    .map(|x| x.static_offset as i64)
                    .unwrap_or(0);
                components.push(VTableComponent::VBaseOffset {
                    target: w,
                    offset: w_abs - sub_offset,
                });
            }
            components.push(VTableComponent::OffsetToTop(-sub_offset));
            components.push(VTableComponent::TypeInfo(typeinfo_sym.clone()));
            if self.class_has_virtual(vb.base) {
                for slot in self.collect_virtual_methods(vb.base) {
                    let name = &slot.name;
                    let m = def.methods.iter().find(|m| m.is_virtual && m.name == *name);
                    match m {
                        Some(m) => components.push(VTableComponent::Function {
                            name: name.clone(),
                            symbol: m.symbol.clone(),
                            adjustment: ThisAdjust::offset(-sub_offset),
                        }),
                        None => {
                            let owner_def = &self.classes[&slot.owner];
                            let sym = owner_def
                                .methods
                                .iter()
                                .find(|bm| bm.is_virtual && bm.name == *name)
                                .map(|bm| bm.symbol.clone())
                                .unwrap_or_else(|| {
                                    format!("{}::{}", self.class_name(slot.owner), name)
                                });
                            components.push(VTableComponent::Function {
                                name: name.clone(),
                                symbol: sym,
                                adjustment: ThisAdjust::default(),
                            });
                        },
                    }
                }
            }
            let address_point = (sub_vbases.len() as i64 + 2) * POINTER_SIZE as i64;
            vtables.push(VTableLayout {
                symbol: self.sub_vbtable_symbol(class_id, vb.base),
                address_point,
                chains: chain_tags(&components, Some(vb.base)),
                components,
                for_base: Some(vb.base),
            });
        }

        vtables
    }

    /// 非虚基类子对象在派生对象里占用的字节数。
    ///
    /// 与基类自己的 standalone `size` 不同, 两种情况下会变小 (GCC 实测):
    /// 1. **基类有虚基类**: 虚基子对象不属于该基类子对象, 它被抬到最派生
    ///    对象的尾部由所有派生者共享。`M : virtual V{fv,gv;int v} {int m}`
    ///    standalone 是 32 (nv 12 + V@16), 而 `E : M {int e1,e2,e3,e4}` 的
    ///    e1 在 **12**、V 在 **32**、sizeof(E)=48; `F : M {double e}` e@16、
    ///    V@24、sizeof(F)=40。只按 32 算就会把 V 复制两份 (实测 sizeof(D)=56
    ///    vs gcc 32), 且两份实例谁都不是「唯一共享的那一份」。
    /// 2. **多态 primary base 的尾部填充可复用**: `C{virtual ~C(); int i}`
    ///    size 16, 而 `D : C {char d}` 的 d 在 **12**、sizeof(D)=16。非多态
    ///    POD 基则保留填充: `A{int i; char c}` size 8, `B : A {char d}` 的 d
    ///    在 **8**。secondary (非 primary) 基一律不共享填充, 仍按整块排布
    ///    —— `C : A, B` 里 B 必须从 A 的整块之后开始 (offset-to-top = -16)。
    ///
    /// 「多态」在这里是「非 POD」的可检测代理: 布局层当前只掌握虚函数信息,
    /// 用户声明构造/析构带来的非平凡性未登记。判据集中在这一个函数里,
    /// 后续补平凡性跟踪时只需在这里扩展。
    fn embedded_base_footprint(&self, base_abi: &ClassAbi, is_primary: bool) -> u64 {
        if !base_abi.virtual_bases.is_empty() || (is_primary && base_abi.has_virtual_functions) {
            base_abi.non_virtual_size
        } else {
            base_abi.size
        }
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
    ///
    /// 单一真源: `compute_class` 的 Step 2.5 已经按传递闭包把**全部**虚基
    /// 子对象在最派生对象里的静态偏移登记进 `virtual_bases`, primary vftable
    /// 里的 `VBaseOffset` 槽写的就是同一个值。曾经这里有一套独立的
    /// `compute_vbase_offset_from_layout` 估算 (只会把虚基接在非虚部分后面),
    /// 不知道「非虚基类的虚基会被抬到本对象尾部」, 与真实布局分叉 ——
    /// 两套答案时谁对无法判定, 故删除。
    pub fn compute_vbase_offset(&self, derived: ClassId, vbase: ClassId) -> Option<i64> {
        let derived_abi = self.compute_class(derived);
        derived_abi
            .virtual_bases
            .iter()
            .find(|vb| vb.base == vbase)
            .map(|vb| vb.static_offset as i64)
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
    /// - `__si_class_type_info`: 单个非虚基类
    /// - `__vmi_class_type_info`: 多基类或有虚基类
    ///
    /// 基类 `__offset_flags` 低字节标志位 (与 libstdc++ `__base_info` 一致):
    /// - `__virtual_mask = 0x1`: 虚基类
    /// - `__public_mask  = 0x2`: public 继承
    pub fn compute_rtti(&self, class_id: ClassId, abi: &ClassAbi) -> RttiLayout {
        let typeinfo_symbol = self.typeinfo_symbol(class_id);
        let typeinfo_name_symbol = self.typeinfo_name_symbol(class_id);

        let has_virtual_bases = !abi.virtual_bases.is_empty();

        let mut base_classes = Vec::new();

        // 非虚基类
        for base in &abi.bases {
            // 简化：所有基类都视为 public（测试场景下）
            let flags: u32 = RTTI_BASE_PUBLIC_MASK; // public
            base_classes.push(RttiBaseClass {
                base: base.base,
                offset: base.offset as i64,
                is_virtual: false,
                flags,
            });
        }

        // 虚基类
        for vb in &abi.virtual_bases {
            // 偏移直取布局层 Step 2.5 的静态偏移（唯一真源，不重算）
            let offset = vb.static_offset as i64;
            // Itanium ABI `__base_info::__offset_flags`: virtual=0x1, public=0x2
            let flags: u32 = RTTI_BASE_PUBLIC_MASK | RTTI_BASE_VIRTUAL_MASK;
            base_classes.push(RttiBaseClass {
                base: vb.base,
                offset,
                is_virtual: true,
                flags,
            });
        }

        // `__base_info` 本应按直接基类的**声明顺序**排列; dcc 的 ClassDef 把
        // 非虚基与虚基分列登记, 这里按「非虚基在前、虚基在后」合并。动态转换
        // 的可达性/二义性判定与 `__base_info` 次序无关 (libstdc++ 会遍历全部
        // 基类), 故混序声明 (`struct D : virtual V, B`) 不影响 dynamic_cast 结果。
        let kind = if base_classes.is_empty() {
            TypeInfoKind::Class
        } else if base_classes.len() == 1 && !base_classes[0].is_virtual {
            TypeInfoKind::SingleInheritance
        } else {
            TypeInfoKind::Vmi
        };

        RttiLayout {
            typeinfo_symbol,
            typeinfo_name_symbol,
            has_virtual_bases,
            base_classes,
            kind,
        }
    }
}

/// 虚基子对象嵌入形态相比 standalone 布局需要**额外**预留的头部字节。
///
/// GCC 实测 (LP64):
/// - `V{virtual fv; int v}` standalone = [vptr][v] = 16; 作为虚基嵌入时
///   仍是 [vptr][v] (那个 vptr 由最派生类改写成链 vftable) → 额外 0 字节
/// - `Base{int b}` (非多态) standalone = 4; 被多态类当虚基时子对象是
///   [vptr][b] = 12 (`sizeof(Mid2{virtual Base; int m2; virtual f()}) == 16`)
///   → 额外 8 字节
///
/// 判据是「本类自己有没有 vptr 放在偏移 0」: 有则嵌入时复用同一个槽,
/// 没有才新留一个。误加 8 会把尾部所有子对象往后推 (实测 sizeof(M)=36 vs
/// gcc 32, 且多出的空间被当对象读)。
pub fn vb_embedded_extra_head(vb_abi: &ClassAbi) -> u64 {
    if vb_abi.vbase_embedded_vptr == 0 {
        return 0;
    }
    // 本类的 vptr 就在偏移 0 (无 primary base 共享、自身多态)
    let vptr_at_zero = vb_abi.vptrs.iter().any(|p| p.offset == 0);
    if vptr_at_zero {
        0
    } else {
        POINTER_SIZE
    }
}

/// 虚基子对象嵌入形态的头部指针槽大小 (相对字段起始地址之前)。
///
/// = 本类 standalone 已有的 vptr 槽 + 嵌入时额外预留的槽。
pub fn vb_embedded_head(vb_abi: &ClassAbi) -> u64 {
    let own_vptr = if vb_abi.vbase_embedded_vptr > 0 && vb_abi.vptrs.iter().any(|p| p.offset == 0) {
        POINTER_SIZE
    } else {
        0
    };
    own_vptr + vb_embedded_extra_head(vb_abi)
}

/// 虚基子对象嵌入形态的大小。
///
/// 两种情形:
/// - **复用 standalone 布局** (本类的 vptr 就在偏移 0): 大小 = standalone
///   全尺寸 (`size`, 含该类自己作为完整对象时的虚基尾部)。
///   GCC 实测: `V{virtual fv; int v}` = 16 → 嵌在 M 尾部也是 16
///   (sizeof(M)=32)。若错用 `non_virtual_size` (不含 V 的虚基尾),
///   嵌套虚基 (D→M→V 与 V→X) 的尾部会被挤掉, sizeof 直接少 8/16。
/// - **额外预留头部** (本类非多态但有链 vftable 要写): 大小 = 头部 + 非虚部分。
///   GCC 实测: `Base{int b}` 被多态 Mid2 当虚基 → 子对象 [vptr][b] = 12。
pub fn vb_embedded_size(vb_abi: &ClassAbi) -> u64 {
    let extra = vb_embedded_extra_head(vb_abi);
    if extra == 0 {
        return vb_abi.size;
    }
    let body = if vb_abi.non_virtual_size > 0 {
        vb_abi.non_virtual_size
    } else {
        vb_abi.size
    };
    align_up(extra + body, vb_abi.align.max(1))
}

/// 为一组 vtable 分量生成「虚槽 → chain」标签序列。
///
/// 只对 `Function` 分量计数 (offset-to-top / typeinfo / vcall / vbase offset
/// 不占虚槽序), 每一项都是 `chain`: `None` = primary chain, `Some(base)` =
/// 该 vtable 属于 `base` 子对象的 secondary chain。
///
/// 发射端 (`lower_ir::lower_virtual_call`) 根据 this 指针静态类型在这张表里
/// 选槽, 因此 chain 归属必须由布局层给出, 不能靠符号名前缀猜。
pub fn chain_tags(components: &[VTableComponent], chain: Option<ClassId>) -> Vec<Option<ClassId>> {
    components
        .iter()
        .filter(|c| matches!(c, VTableComponent::Function { .. }))
        .map(|_| chain)
        .collect()
}

/// 从已算好的 vtable 分量里提取需要发射的 this-adjust thunk 列表。
///
/// 单一真源: 偏移量只读 `VTableComponent::Function::adjustment` (布局层
/// 已在 secondary vtable / 虚基子对象 vbtable 上算好), 这里不再重算,
/// 只负责把「需要调整的槽」翻译成 thunk 描述符, 并按入口符号去重
/// (同一入口可能出现在多张 secondary vtable 里)。
pub fn collect_thunks_from_vtables(vtables: &[VTableLayout]) -> Vec<ThunkDesc> {
    let mut thunks: Vec<ThunkDesc> = Vec::new();
    for vt in vtables {
        for comp in &vt.components {
            if let VTableComponent::Function {
                symbol, adjustment, ..
            } = comp
            {
                if let Some(entry) =
                    thunk_symbol_for(symbol, adjustment.non_virtual, adjustment.vbase_index)
                {
                    if thunks.iter().any(|t| t.symbol == entry) {
                        continue;
                    }
                    thunks.push(ThunkDesc {
                        symbol: entry,
                        target_symbol: symbol.clone(),
                        this_adjust: *adjustment,
                        return_adjust: None,
                        kind: ThunkKind::Virtual,
                    });
                }
            }
        }
    }
    thunks
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
            },
        };

        let mut abi = ClassAbi {
            class_id: class,
            ..ClassAbi::default()
        };

        let has_virtual = self.class_has_virtual(class);
        let needs_vptr = self.class_needs_vptr(class);
        abi.needs_vptr = needs_vptr;
        abi.has_direct_virtual_bases = !def.virtual_bases.is_empty();
        abi.has_virtual_functions = has_virtual;
        // primary base: 第一个非虚基类（无论是否有虚函数都是 primary base
        // 如果有虚函数且基类也有虚函数，则共享 vptr
        let primary_base = def.bases.first().copied();

        let mut current_offset: u64 = 0;
        let mut max_align: u64 = 1;

        // --- Step 0: 分配 vptr / vbptr ---
        // 有虚函数就要 vptr; 有虚基类就要 vbptr (两者共用同一张 vtable 的
        // address point)。若 primary base 自己就需要 vptr, 则共享它的 vptr。
        let primary_has_virtual = primary_base
            .map(|b| self.class_needs_vptr(b))
            .unwrap_or(false);
        if needs_vptr && !primary_has_virtual {
            abi.vptrs.push(VPtrLayout {
                offset: 0,
                vtable_index: 0,
                complete_only: false,
                address_point_override: None,
            });
            current_offset = POINTER_SIZE;
            max_align = POINTER_ALIGN;
        }

        // --- Step 1: 布局非虚基类 ---
        for (i, &base_id) in def.bases.iter().enumerate() {
            let base_abi = self.compute_class(base_id);
            let base_align = base_abi.align.max(1);
            // 基类子对象在本对象里实际占用的字节数 (可能小于它的 standalone
            // 尺寸, 见 `embedded_base_footprint`)。
            let footprint = self.embedded_base_footprint(&base_abi, i == 0);

            // primary base 判定：第一个有虚函数的基类是 primary base
            // （如果基类有虚函数，则它作为 primary base 共享 vptr）
            let is_primary = i == 0;

            // 计算该基类的起始 offset
            let base_offset =
                if is_primary && primary_base.is_some() && Some(base_id) == primary_base {
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
            abi.this_adjust_map
                .insert(base_id, ThisAdjust::offset(base_offset as i64));

            if Some(base_id) == primary_base {
                abi.primary_base = Some(base_id);
                // primary base 共享 vptr（仅当类有虚函数时）
                if has_virtual
                    && primary_has_virtual
                    && !abi.vptrs.iter().any(|v| v.offset == base_offset)
                {
                    abi.vptrs.push(VPtrLayout {
                        offset: base_offset,
                        vtable_index: 0,
                        complete_only: false,
                        address_point_override: None,
                    });
                }
            } else if self.class_has_virtual(base_id) {
                // 非 primary base 但有虚函数，有自己的 vptr（对应 secondary vtable）
                // 找到对应的 secondary vtable index
                let vtable_idx = abi
                    .vtables
                    .iter()
                    .position(|v| v.for_base == Some(base_id))
                    .unwrap_or(0); // 暂时用 0，后面再修正
                abi.vptrs.push(VPtrLayout {
                    offset: base_offset,
                    vtable_index: vtable_idx,
                    complete_only: false,
                    address_point_override: None,
                });
            }

            // 更新最大对齐
            if base_align > max_align {
                max_align = base_align;
            }

            // 推进 offset
            let end_offset = base_offset + footprint;
            // 空基类（size 0）不推进 offset（避免无谓推进）
            if footprint > 0 {
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
        // 先快照非虚部分大小: 本类作为子对象嵌入其它对象时只占这段空间。
        //
        // 这里存的是**未取整**的已用字节数: GCC 实测 `M{virtual V; int m}` 的
        // nv_end = 12 (size 因 8 对齐取整到 32 含虚基), 而 `E : M {int e1..}`
        // 的 e1 落在 **12** —— 派生类从基类的已用字节末尾继续分配。
        let non_virtual_size = current_offset;
        let all_virtual_bases = self.collect_all_virtual_bases(class);
        for (vbase_idx, &vb_id) in all_virtual_bases.iter().enumerate() {
            let vb_abi = self.compute_class(vb_id);
            let vb_align = vb_abi.align.max(1);
            // 子对象尺寸 = 虚基类的**虚基嵌入形态**大小 (首 vptr 槽 + 非虚部分)。
            // GCC 实测 `M : virtual V`: V 子对象在 M 内是 [vptr][v], 那个 vptr
            // 由最派生构造函数写成 M 为 V 链准备的 secondary vftable; 若无此
            // 空间, 写入会踩到对象外的栈上相邻变量 (直接段错误)。
            let vb_size = vb_embedded_size(&vb_abi);

            // 对齐当前 offset
            let vb_offset = align_up(current_offset, vb_align);
            let subobject_field = vbase_subobject_field_name(&self.class_name(vb_id));

            // 添加虚基类布局
            abi.virtual_bases.push(VirtualBaseLayout {
                base: vb_id,
                vtable_offset: 0, // 后面在 vtable 计算时填充
                vbase_index: vbase_idx,
                static_offset: vb_offset,
                subobject_field: subobject_field.clone(),
            });

            // 为虚基子对象暴露具名句柄字段，字段展平与 `&o.__vb_Base` 均以此为地址。
            // 句柄 = 子对象**首地址** (含头部指针槽), 与 GCC 的 `&o.__vb_Base`
            // 语义一致; 大小用嵌入形态全尺寸。
            abi.fields.push(FieldLayout {
                name: subobject_field,
                offset: vb_offset,
                size: vb_size,
                align: vb_align,
            });
            // 记下子对象自己的 vptr 槽位置 (头部第一个指针槽就是 vptr;
            // 若子对象另有 vbptr, vbptr 在偏移 0, vptr 紧随其后)。
            if vb_abi.vbase_embedded_vptr > 0 {
                abi.vbase_own_vptr_slots.push((vb_id, vb_offset));
            }

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
        abi.non_virtual_size = non_virtual_size;
        // 多态类作为虚基子对象嵌入时要预留首 vptr 槽 (见 `vb_embedded_size`)。
        // 注意用**本类自己**是否多态 (`def.has_virtual`), 不能用继承来的
        // `has_virtual`: `struct Base { int b; }` 被多态类当虚基时, GCC 实测
        // `sizeof(Mid1{virtual Base; int m1;}) == 16` —— Base 子对象就是
        // `int` (4 字节), 头部没有任何指针槽; 误加 8 字节会把整个尾部
        // 往后推, 成员访问/子对象句柄全部错位。
        abi.vbase_embedded_vptr = if def.has_virtual { POINTER_SIZE } else { 0 };
        abi.size = if current_offset == 0 {
            // 空类（无基类无字段）: Itanium ABI 规定大小为 1
            1
        } else {
            align_up(current_offset, max_align)
        };

        // --- Step 4: 计算 vtable / vbtable 布局 ---
        if needs_vptr {
            abi.vtables = self.compute_vtables(class, &abi);
            // 从 primary vtable 中提取 vbase offset 值，更新到 virtual_bases
            if let Some(primary_vtable) = abi.vtables.first() {
                // vbase offset 槽相对 vptr (address point) 的字节偏移:
                //   slot 在 vtable 数组下标 `pos`, vptr = 数组起点 + address_point
                //   ⇒ vtable_offset = pos*8 - address_point (负值, 因为在 address point 之前)
                // 常见错误是写成 `-(idx+2)*8`: 那只在 **恰好 2 个虚基** 时碰巧正确,
                // 菱形虚继承 (3 个传递虚基) 下会读到错的 vbtable 槽。
                let ap = primary_vtable.address_point;
                let mut vbase_idx = 0;
                for (pos, comp) in primary_vtable.components.iter().enumerate() {
                    if let VTableComponent::VBaseOffset { target, offset } = comp {
                        if vbase_idx < abi.virtual_bases.len()
                            && abi.virtual_bases[vbase_idx].base == *target
                        {
                            let slot_offset = (pos as i64) * POINTER_SIZE as i64 - ap;
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

            // --- 虚基子对象自己的 vftable (Stage 6 chain 模型) ---
            //
            // GCC 实测 (`M : virtual V{fv,gv}; D : M{fv override}` 的 `_ZTV1D`):
            //   V 子对象在**最派生对象**里的 vptr 不指向 V 自己的 vtable,
            //   而指向最派生 vtable 里一段属于 V 这条 chain 的 secondary vftable:
            //     [offset-to-top = -(vbase 子对象偏移)] [typeinfo for D] [V 链的虚槽...]
            //   槽里放的是最终覆写者 (可能被 virtual thunk 包一层)。
            // 之前的实现不给虚基子对象写 vptr, 于是 V 子对象停留在 V 自己
            // 构造函数写的 `_ZTV1V` 上 —— `V* p; p->fv()` 静默调到 `V::fv`
            // (实测 gcc 107 / dcc 7)。
            for vb in &abi.virtual_bases {
                if !self.class_has_virtual(vb.base) {
                    continue;
                }
                let sym = self.vbase_vftable_symbol(class, vb.base);
                if abi.vtables.iter().any(|v| v.symbol == sym) {
                    continue;
                }
                let vbase_off = vb.static_offset as i64;
                let mut comps = vec![
                    VTableComponent::OffsetToTop(-vbase_off),
                    VTableComponent::TypeInfo(self.typeinfo_symbol(class)),
                ];
                // 该链的虚槽顺寻必须按「以 vb.base 为完整对象」的虚方法表算,
                // 再把实现换成本类 (最派生类) 的最终覆写者 —— 与 GCC 在
                // `_ZTV<D>` 里给 V 链放 D 覆写 (+thunk) 的行为一致。
                let chain_slots = self.collect_virtual_methods(vb.base);
                let overrides = self.collect_virtual_methods(class);
                for slot in chain_slots {
                    // 在本类的虚槽表里按【槽身份】(名字 + 声明类) 找最终实现。
                    // 不能只按名字: 多继承下同名虚函数可以属于两条不同的槽。
                    let resolved = overrides
                        .iter()
                        .find(|s| s.name == slot.name && s.declaring == slot.declaring)
                        .unwrap_or(&slot);
                    let impl_sym = resolved.symbol.clone();
                    // 调用时传入的是**虚基子对象**的 this, 而实现期望的是它
                    // 提供类 (`VSlot::owner`) 子对象的 this: 差值 = owner_off - vbase_off。
                    // 用 `owner` 而不是 `symbol_decl` 名字表: 后者只能回答「这个
                    // 符号在哪注册过」, 而 override 后的实现符号属于谁必须由虚槽
                    // 展开给出。误判会插一个莫须有的 -vbase_off thunk, vtable 里
                    // 留下未定义符号 (实测运行期跳空指针直接段错误)。
                    let owner_off = if resolved.owner == class {
                        // 实现就是本类 (完整对象) 自己的: this = 对象首地址
                        0
                    } else if resolved.owner == vb.base {
                        // 实现在虚基子对象自己 (或它的某个基子对象) 里:
                        // 从该虚基子对象进来的调用已经拿到正确的 this。
                        vbase_off
                    } else {
                        abi.this_adjust_map
                            .get(&resolved.owner)
                            .map(|a| a.non_virtual)
                            .unwrap_or(vbase_off)
                    };
                    let adj = if owner_off == vbase_off {
                        ThisAdjust::default()
                    } else {
                        ThisAdjust::offset(owner_off - vbase_off)
                    };
                    comps.push(VTableComponent::Function {
                        name: slot.name.clone(),
                        symbol: impl_sym,
                        adjustment: adj,
                    });
                }
                let address_point = 2 * POINTER_SIZE as i64;
                let n_slots = comps
                    .iter()
                    .filter(|c| matches!(c, VTableComponent::Function { .. }))
                    .count();
                let chains = vec![Some(vb.base); n_slots];
                abi.vtables.push(VTableLayout {
                    symbol: sym,
                    address_point,
                    components: comps,
                    for_base: Some(vb.base),
                    chains,
                });
            }
            // 为虚基子对象登记一个只在完整对象构造时写入的 vptr。
            let mut vbase_vptrs: Vec<VPtrLayout> = Vec::new();
            for &(vb_id, slot) in &abi.vbase_own_vptr_slots {
                let sym = self.vbase_vftable_symbol(class, vb_id);
                let idx = match abi.vtables.iter().position(|v| v.symbol == sym) {
                    Some(i) => i,
                    None => continue,
                };
                if vbase_vptrs.iter().any(|v| v.offset == slot)
                    || abi.vptrs.iter().any(|v| v.offset == slot)
                {
                    continue;
                }
                vbase_vptrs.push(VPtrLayout {
                    offset: slot,
                    vtable_index: idx,
                    complete_only: true,
                    address_point_override: None,
                });
            }
            abi.vptrs.extend(vbase_vptrs);

            // 虚基子对象自己的 vbptr: 最派生构造函数末尾必须把它指向本类的
            // secondary vbtable (否则从子对象视角读 vbtable 会拿到错位的
            // vbase offsets)。这些条目 offset > 0, 只在 complete-object
            // 构造的最后一步写入。
            let mut sub_vptrs: Vec<VPtrLayout> = Vec::new();
            for vb in &abi.virtual_bases {
                if !def.virtual_bases.contains(&vb.base) {
                    continue;
                }
                if self.collect_all_virtual_bases(vb.base).is_empty() {
                    continue;
                }
                if abi.vptrs.iter().any(|v| v.offset == vb.static_offset) {
                    continue;
                }
                let sym = self.sub_vbtable_symbol(class, vb.base);
                let idx = match abi.vtables.iter().position(|v| v.symbol == sym) {
                    Some(i) => i,
                    None => continue,
                };
                sub_vptrs.push(VPtrLayout {
                    offset: vb.static_offset,
                    vtable_index: idx,
                    complete_only: true,
                    address_point_override: None,
                });
            }
            abi.vptrs.extend(sub_vptrs);

            // thunk 登记: 任何「实现需要先调 this 才能进入最终覆写」的虚槽,
            // 入口必须切成 thunk 符号。偏移由 vtable 分量携带的
            // `ThisAdjust` 唯一决定, 这里只按它分配名字与目标。
            //
            // 必须放在**所有** vtable (primary / secondary / 虚基 chain) 都
            // 登记完之后: 早于 chain vftable 收集会让覆写虚基槽的 thunk 入口
            // 写进 vtable 却没有对应定义 (链接期 *ABS* 未定义, 运行期跳空)。
            abi.thunks = collect_thunks_from_vtables(&abi.vtables);
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
                (
                    "b".into(),
                    CxxType::Integer {
                        bits: 32,
                        signed: true,
                    },
                ),
                (
                    "c".into(),
                    CxxType::Integer {
                        bits: 8,
                        signed: false,
                    },
                ),
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
                (
                    "a".into(),
                    CxxType::Integer {
                        bits: 32,
                        signed: true,
                    },
                ),
                (
                    "x".into(),
                    CxxType::Integer {
                        bits: 8,
                        signed: false,
                    },
                ),
            ],
        );
        calc.register_simple_class(b_id, vec![("b".into(), CxxType::Float { bits: 64 })]);
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
                CxxType::Array(
                    Box::new(CxxType::Integer {
                        bits: 32,
                        signed: true,
                    }),
                    Some(5),
                ),
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
                (
                    "p".into(),
                    CxxType::Pointer(Box::new(CxxType::Integer {
                        bits: 32,
                        signed: true,
                    })),
                ),
                (
                    "x".into(),
                    CxxType::Integer {
                        bits: 32,
                        signed: true,
                    },
                ),
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
            vec![(
                "ref".into(),
                CxxType::LValueRef(Box::new(CxxType::Integer {
                    bits: 32,
                    signed: true,
                })),
            )],
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
                (
                    "a".into(),
                    CxxType::Integer {
                        bits: 32,
                        signed: true,
                    },
                ),
                (
                    "x".into(),
                    CxxType::Integer {
                        bits: 8,
                        signed: false,
                    },
                ),
            ],
        );
        calc.register_simple_class(b_id, vec![("b".into(), CxxType::Float { bits: 64 })]);
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            vec![(
                "a".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_simple_class(b_id, vec![("b".into(), CxxType::Float { bits: 64 })]);
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            vec![(
                "a".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_class(
            b_id,
            vec![a_id],
            vec![],
            vec![(
                "b".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false,
        );
        calc.register_class(
            c_id,
            vec![b_id],
            vec![],
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
                (
                    "a".into(),
                    CxxType::Integer {
                        bits: 32,
                        signed: true,
                    },
                ),
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
            vec![(
                "a".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_simple_class(b_id, vec![("b".into(), CxxType::Float { bits: 64 })]);
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![
                (
                    "c".into(),
                    CxxType::Integer {
                        bits: 32,
                        signed: true,
                    },
                ),
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(symbol.ends_with("::foo"));
                assert!(adjustment.is_zero());
            },
            _ => panic!("expected Function"),
        }

        // 第四个组件: bar (slot 1)
        match &vtable.components[3] {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(symbol.ends_with("::bar"));
                assert!(adjustment.is_zero());
            },
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(
                    symbol.contains("Class1") && symbol.ends_with("::foo"),
                    "expected Derived::foo, got {}",
                    symbol
                );
                assert!(adjustment.is_zero());
            },
            _ => panic!("expected Function for foo"),
        }

        // slot 1: bar (继承自 Base)
        match &vtable.components[3] {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(
                    symbol.contains("Class0") && symbol.ends_with("::bar"),
                    "expected Base::bar, got {}",
                    symbol
                );
                assert!(adjustment.is_zero());
            },
            _ => panic!("expected Function for bar"),
        }

        // slot 2: baz (Derived 新增)
        match &vtable.components[4] {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(
                    symbol.contains("Class1") && symbol.ends_with("::baz"),
                    "expected Derived::baz, got {}",
                    symbol
                );
                assert!(adjustment.is_zero());
            },
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
            vec![(
                "a".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(symbol.ends_with("::a_func"), "expected a_func, got {}", symbol);
                assert!(adjustment.is_zero());
            },
            _ => panic!("expected Function for a_func"),
        }

        // slot 1: C::b_func (在 primary vtable 中无调整)
        match &primary.components[3] {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(symbol.ends_with("::b_func"), "expected b_func, got {}", symbol);
                assert!(adjustment.is_zero());
            },
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
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert!(symbol.ends_with("::b_func"), "expected b_func, got {}", symbol);
                assert_eq!(adjustment.non_virtual, -16, "expected thunk adjust -16");
                assert!(adjustment.vbase_index.is_none());
            },
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert_eq!(symbol, "Class1::b");
                assert!(adjustment.is_zero(), "inherited method should have zero adjustment");
            },
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
        // 合成子对象句柄字段名用**源语言类名** (`__vb_Base`), 所以要先注册名字.
        calc.register_class_name(base_id, "Base");
        calc.register_class_name(derived_id, "Derived");

        // Base: 有虚函数
        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        // Derived: 虚继承 Base
        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false, // 自身没有新的虚函数，但继承了
        );

        let abi = calc.compute_class(derived_id);

        // 验证有虚函数（继承的）
        assert!(abi.is_polymorphic());
        // GCC 实测 (`Derived : virtual Base{virtual foo; int x;}`):
        //   Derived 不是 Base 的非虚派生者, 拿不到 primary base,
        //   所以自己有 vptr@0; 而虚基 Base 子对象在 +16 处**还有一份
        //   自己的 vptr**, 由最派生构造写成 Base 链的 secondary vftable。
        //   (只有 Derived 的完整对象构造才写后者, 故 complete_only)
        assert_eq!(abi.vptrs.len(), 2, "Derived.vptr@0 + Base 子对象 vptr@16");
        assert_eq!(abi.vptrs[0].offset, 0);
        let vb_vptr = abi
            .vptrs
            .iter()
            .find(|p| p.offset == 16)
            .expect("虚基子对象自己的 vptr 槽");
        assert!(vb_vptr.complete_only);

        // 验证非虚基类为空
        assert!(abi.bases.is_empty());

        // 验证虚基类
        assert_eq!(abi.virtual_bases.len(), 1);
        assert_eq!(abi.virtual_bases[0].base, base_id);
        assert_eq!(abi.virtual_bases[0].vbase_index, 0);

        // 验证字段 y 的偏移（在 vptr 之后）
        // 布局层会为每个虚基子对象合成一个具名句柄字段 `__vb_Base`,
        // 使「共享子对象的地址」在 IR 里可表达; 它不是用户字段。
        let user_fields: Vec<_> = abi
            .fields
            .iter()
            .filter(|f| !is_vbase_subobject_field(&f.name))
            .collect();
        assert_eq!(user_fields.len(), 1);
        assert_eq!(user_fields[0].name, "y");
        assert_eq!(user_fields[0].offset, 8); // vptr (8 bytes) 之后
                                              // 虚基子对象句柄字段存在且偏移与 vbtable 里的静态偏移一致
        let vb_field = abi
            .fields
            .iter()
            .find(|f| f.name == "__vb_Base")
            .expect("missing __vb_ subobject field");
        assert_eq!(vb_field.offset, abi.virtual_bases[0].static_offset);

        // 验证大小: vptr(8) + y(4) + padding(4) + Base(16) = 32
        assert_eq!(abi.size, 32);
        assert_eq!(abi.align, 8);

        // 验证 vtable 中有 VBaseOffset 条目
        // primary + 虚基子对象的 chain vftable (Stage 6)
        assert_eq!(abi.vtables.len(), 2);
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
            },
            _ => panic!("expected VBaseOffset"),
        }

        // address_point = vbase_offset(1) * 8 + offset-to-top(8) + typeinfo(8) = 8 + 16 = 24
        assert_eq!(vtable.address_point, 24);

        // 虚基子对象的 chain vftable: [offset-to-top = -16][typeinfo][foo 槽]
        let chain = abi
            .vtables
            .iter()
            .find(|v| v.for_base == Some(base_id) && v.symbol.contains("::vbase"))
            .expect("虚基子对象的 chain vftable");
        assert_eq!(chain.address_point, 16);
        assert!(matches!(chain.components[0], VTableComponent::OffsetToTop(-16)));
        assert_eq!(chain.vfunc_slots().len(), 1);
        assert!(abi
            .vbase_own_vptr_slots
            .iter()
            .any(|&(c, off)| c == base_id && off == 16),);
    }

    /// 虚基数量 ≠ 2 时校验 vbtable 槽偏移的**通用式** `pos*8 - address_point`。
    ///
    /// 闭式 `-(idx+2)*8` 只在恰好 2 个虚基时碰巧相等, 3 个虚基时给出
    /// -16/-24/-32 而正确值是 -40/-32/-24 —— 偏移错位会让成员访问经 vbtable
    /// 调整 this 时算到别的子对象上 (菱形虚继承下即 `ambiguous`/错值/崩溃根因之一)。
    #[test]
    fn three_virtual_bases_vbtable_slot_offsets() {
        let mut calc = ItaniumAbiCalculator::new();
        let v1 = ClassId(0);
        let v2 = ClassId(1);
        let v3 = ClassId(2);
        let derived = ClassId(3);
        for (id, name) in [(v1, "V1"), (v2, "V2"), (v3, "V3"), (derived, "D")] {
            calc.register_class_name(id, name);
        }
        let i32_ty = CxxType::Integer {
            bits: 32,
            signed: true,
        };
        for id in [v1, v2, v3] {
            calc.register_class(id, vec![], vec![], vec![("x".into(), i32_ty.clone())], true);
            calc.register_method(id, "foo".into(), true, id);
        }
        calc.register_class(derived, vec![], vec![v1, v2, v3], vec![("m".into(), i32_ty)], false);

        let abi = calc.compute_class(derived);
        assert_eq!(abi.virtual_bases.len(), 3);
        let vtable = abi.vtables.first().expect("primary vtable");
        // 3 个 VBaseOffset 槽排在 OffsetToTop + TypeInfo 之前
        assert_eq!(vtable.address_point, (3 + 2) * 8);
        // 每个虚基的 vtable_offset 必须等于其 VBaseOffset 组件在 vtable 数组里的
        // 槽位相对 address point 的字节偏移。
        for (idx, vb) in abi.virtual_bases.iter().enumerate() {
            let pos = vtable
                .components
                .iter()
                .position(|c| matches!(c, VTableComponent::VBaseOffset { target, .. } if *target == vb.base))
                .expect("vbase offset slot");
            assert_eq!(
                vb.vtable_offset,
                (pos as i64) * 8 - vtable.address_point,
                "vbase#{idx} (vbase_index={}) 的 vbtable 槽偏移错位",
                vb.vbase_index
            );
        }
        assert_eq!(
            abi.virtual_bases
                .iter()
                .map(|vb| vb.vtable_offset)
                .collect::<Vec<_>>(),
            vec![-40, -32, -24]
        );
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
        // 同上: `__vb_A` 句柄字段与 secondary vbtable 符号都依赖真实类名.
        calc.register_class_name(a_id, "A");
        calc.register_class_name(b_id, "B");
        calc.register_class_name(c_id, "C");
        calc.register_class_name(d_id, "D");

        // A
        calc.register_class(
            a_id,
            vec![],
            vec![],
            vec![(
                "a".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(a_id, "foo".into(), true, a_id);

        // B : virtual A
        calc.register_class(
            b_id,
            vec![],
            vec![a_id],
            vec![(
                "b".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false,
        );

        // C : virtual A
        calc.register_class(
            c_id,
            vec![],
            vec![a_id],
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false,
        );

        // D : B, C
        calc.register_class(
            d_id,
            vec![b_id, c_id],
            vec![],
            vec![(
                "d".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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

        // 验证字段 d 存在（用户字段 + 合成的 `__vb_A` 子对象句柄字段）
        let user_fields: Vec<_> = abi_d
            .fields
            .iter()
            .filter(|f| !is_vbase_subobject_field(&f.name))
            .collect();
        assert_eq!(user_fields.len(), 1);
        assert_eq!(user_fields[0].name, "d");
        assert!(abi_d.fields.iter().any(|f| f.name == "__vb_A"));

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
            },
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base1_id, "f1".into(), true, base1_id);

        // Base2
        calc.register_class(
            base2_id,
            vec![],
            vec![],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base2_id, "f2".into(), true, base2_id);

        // Derived : virtual Base1, virtual Base2
        calc.register_class(
            derived_id,
            vec![],
            vec![base1_id, base2_id],
            vec![(
                "z".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
                },
                VTableComponent::OffsetToTop(_) => {
                    assert!(vbase_seen, "OffsetToTop should come after VBaseOffsets");
                    offset_to_top_seen = true;
                },
                _ => {},
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
            vec![(
                "b".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base_id, "f".into(), true, base_id);

        calc.register_class(
            vbase_id,
            vec![],
            vec![],
            vec![(
                "v".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(vbase_id, "g".into(), true, vbase_id);

        calc.register_class(
            derived_id,
            vec![base_id],
            vec![vbase_id],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
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
        // 无基类的类：typeinfo 符号应为 _ZTI + mangled 类名
        let mut calc = ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        calc.register_class_name(class_id, "Foo");
        calc.register_simple_class(
            class_id,
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );

        let abi = calc.compute_class(class_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTI3Foo");
        assert_eq!(abi.rtti.typeinfo_name_symbol, "_ZTS3Foo");
        assert!(!abi.rtti.has_virtual_bases);
        assert!(abi.rtti.base_classes.is_empty());
    }

    #[test]
    fn rtti_single_inheritance_base_classes() {
        // 单继承：应该有一个非虚基类
        let mut calc = ItaniumAbiCalculator::new();
        let base_id = ClassId(0);
        let derived_id = ClassId(1);
        calc.register_class_name(base_id, "Base");
        calc.register_class_name(derived_id, "Derived");

        calc.register_simple_class(
            base_id,
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_class(
            derived_id,
            vec![base_id],
            vec![],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTI7Derived");
        assert!(!abi.rtti.has_virtual_bases);
        assert_eq!(abi.rtti.base_classes.len(), 1);

        let base = &abi.rtti.base_classes[0];
        assert_eq!(base.base, base_id);
        assert_eq!(base.offset, 0); // primary base
        assert!(!base.is_virtual);
        assert_eq!(base.flags & RTTI_BASE_PUBLIC_MASK, RTTI_BASE_PUBLIC_MASK); // public=0x2
                                                                               // g++ 真值对照 (`struct B : A` 且 A 为公有非虚基):
                                                                               //   objdump -r .data.rel.ro._ZTI1B 与 `__offset_flags` 均给 0x2。
        assert_eq!(base.offset_flags(), 0x2);
    }

    #[test]
    fn rtti_multiple_inheritance_base_classes() {
        // 多继承：应该有两个非虚基类
        let mut calc = ItaniumAbiCalculator::new();
        let a_id = ClassId(0);
        let b_id = ClassId(1);
        let c_id = ClassId(2);
        calc.register_class_name(a_id, "A");
        calc.register_class_name(b_id, "B");
        calc.register_class_name(c_id, "C");

        calc.register_simple_class(
            a_id,
            vec![(
                "a".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
        );
        calc.register_simple_class(b_id, vec![("b".into(), CxxType::Float { bits: 64 })]);
        calc.register_class(
            c_id,
            vec![a_id, b_id],
            vec![],
            vec![(
                "c".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false,
        );

        let abi = calc.compute_class(c_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTI1C");
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
        calc.register_class_name(base_id, "Base");
        calc.register_class_name(derived_id, "Derived");

        calc.register_class(
            base_id,
            vec![],
            vec![],
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(base_id, "foo".into(), true, base_id);

        calc.register_class(
            derived_id,
            vec![],
            vec![base_id],
            vec![(
                "y".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            false,
        );

        let abi = calc.compute_class(derived_id);
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTI7Derived");
        assert!(abi.rtti.has_virtual_bases);
        assert_eq!(abi.rtti.base_classes.len(), 1);

        let vb = &abi.rtti.base_classes[0];
        assert_eq!(vb.base, base_id);
        assert!(vb.is_virtual);
        // Itanium `__base_info::__offset_flags` 真值 (本机 g++ 实测
        // `struct D : virtual A` 的 _ZTI1D 倒数一个字 = 0xffffffffffffe003,
        // 即 -32<<8 | 0x3): virtual = 0x1, public = 0x2。
        assert_eq!(vb.flags & RTTI_BASE_VIRTUAL_MASK, RTTI_BASE_VIRTUAL_MASK); // virtual=0x1
        assert_eq!(vb.flags & RTTI_BASE_PUBLIC_MASK, RTTI_BASE_PUBLIC_MASK); // public=0x2
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

    #[test]
    fn rtti_symbols_are_derived_from_real_class_name_not_id() {
        // 语义需求: `_ZTV` / `_ZTI` / `_ZTS` 必须从**源语言类名**推导。
        // ClassId 是 TU 内符号表下标, 同一份 class 定义在不同 TU 会得到
        // 不同 id; 若符号名带 id, 多 TU 链接时既无法合并也不同 ABI。
        let mut calc = ItaniumAbiCalculator::new();
        let id = ClassId(42);
        calc.register_class_with_name(
            id,
            "Animals::Dog",
            vec![],
            vec![],
            vec![(
                "age".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            true,
        );
        calc.register_method(id, "speak".into(), true, id);

        let abi = calc.compute_class(id);
        assert_eq!(abi.vtables[0].symbol, "_ZTVN7Animals3DogE");
        assert_eq!(abi.rtti.typeinfo_symbol, "_ZTIN7Animals3DogE");
        assert_eq!(abi.rtti.typeinfo_name_symbol, "_ZTSN7Animals3DogE");
        // vtable 里的 typeinfo 条目与 rtti 一致
        assert!(abi.vtables[0]
            .components
            .iter()
            .any(|c| matches!(c, VTableComponent::TypeInfo(s) if s == "_ZTIN7Animals3DogE")));
    }

    // === 虚调用槽号单一真源 (`slot_of` / `vcall_slot*`) ===

    #[test]
    fn vcall_slot_follows_primary_vtable_order_for_multi_inheritance() {
        // class A { virtual void a_func(); };
        // class B { virtual void b_func(); };
        // class C : A, B { /* 重写两个基槽 */ virtual void c_func(); };
        // 语义需求: lower_ir 发射 `call *[vptr + 8*slot]` 时, 槽号必须来自**布局层**
        // 的 primary vtable 顺序, 而不是 sema 的登记顺序 (两者在多继承下不同序)。
        let mut calc = ItaniumAbiCalculator::new();
        let (a_id, b_id, c_id) = (ClassId(0), ClassId(1), ClassId(2));
        calc.register_class(a_id, vec![], vec![], vec![], true);
        calc.register_method(a_id, "a_func".into(), true, a_id);
        calc.register_class(b_id, vec![], vec![], vec![], true);
        calc.register_method(b_id, "b_func".into(), true, b_id);
        calc.register_class(c_id, vec![a_id, b_id], vec![], vec![], true);
        calc.register_method(c_id, "a_func".into(), true, a_id); // 重写 A::a_func
        calc.register_method(c_id, "b_func".into(), true, b_id); // 重写 B::b_func
        calc.register_method(c_id, "c_func".into(), true, c_id); // 本类新虚槽

        let abi = calc.compute_class(c_id);
        assert_eq!(
            abi.vtables[0].vfunc_slots(),
            vec![
                ("a_func", "Class2::a_func"),
                ("b_func", "Class2::b_func"),
                ("c_func", "Class2::c_func")
            ],
            "primary vtable 槽序应为 [重写 A 槽, 重写 B 槽, 本类新槽]"
        );
        assert_eq!(abi.vcall_slot("a_func"), Some(0));
        assert_eq!(abi.vcall_slot("b_func"), Some(1));
        assert_eq!(abi.vcall_slot("c_func"), Some(2));
        // 也可用实现符号名查询 (调用方两种标识都拿得到)
        assert_eq!(abi.vcall_slot("Class2::b_func"), Some(1));
        assert_eq!(abi.vcall_slot("missing"), None);

        // 通过 B 子对象的 secondary vtable 调用时, 槽号相对该 secondary 重新编号
        // (B 自己的 vptr 指向 `_ZTVClass2::baseClass1`, 其 address point 后第一项是 b_func)。
        assert_eq!(abi.vcall_slot_for_base(b_id, "b_func"), Some(0));
        assert_eq!(abi.vcall_slot_for_base(b_id, "a_func"), None);
        // primary vtable 的 `for_base` 是 None, 用本类 id 查不到 secondary
        assert_eq!(abi.vcall_slot_for_base(c_id, "a_func"), None);
        // get_vtable_slot 与 vcall_slot 同源 (同一真源, 不再各自实现)
        assert_eq!(abi.get_vtable_slot("b_func"), abi.vcall_slot("b_func"));
    }

    #[test]
    fn vcall_slot_maps_any_class_destructor_to_the_shared_dtor_slot() {
        // class B { virtual void f(); virtual ~B(); };   // 槽 0 = f, 槽 1 = ~B
        // class D : B { ~D() override; virtual void g(); } // ~D 复用槽 1, g 得新槽 2
        // Itanium 里基类与派生类析构是**同一条虚槽**; 布局层用规范名 `~` 登记
        // (见 native.rs), 而 lower_ir 侧的调用名是本类析构名 (`~D`), 故
        // `slot_of("~D")` 必须落到那条共用槽上, 不能按名字精确匹配失败。
        let mut calc = ItaniumAbiCalculator::new();
        let (b_id, d_id) = (ClassId(0), ClassId(1));
        calc.register_class(b_id, vec![], vec![], vec![], true);
        calc.register_method(b_id, "f".into(), true, b_id);
        calc.register_method_with_symbol(b_id, "~".into(), "Class0::~".into(), true, b_id);
        calc.register_class(d_id, vec![b_id], vec![], vec![], true);
        calc.register_method_with_symbol(d_id, "~".into(), "Class1::~".into(), true, d_id);
        calc.register_method(d_id, "g".into(), true, d_id);

        let b_abi = calc.compute_class(b_id);
        assert_eq!(b_abi.vcall_slot("~B"), Some(1));
        assert_eq!(b_abi.vcall_slot("~"), Some(1));
        assert_eq!(b_abi.vcall_slot("f"), Some(0));

        let d_abi = calc.compute_class(d_id);
        assert_eq!(
            d_abi.vtables[0].vfunc_slots(),
            vec![("f", "Class0::f"), ("~", "Class1::~"), ("g", "Class1::g")],
            "派生主 vtable 必须以基类槽序为前缀, 析构不换槽"
        );
        assert_eq!(d_abi.vcall_slot("~D"), Some(1), "本类析构名要能查到共用的析构槽");
        assert_eq!(d_abi.vcall_slot("f"), Some(0), "未被重写的基虚函数保持原槽");
        assert_eq!(d_abi.vcall_slot("g"), Some(2));
    }

    // === Stage 6: 虚基子对象自己的 vftable (chain 模型) ===

    /// GCC 实测真值来源 (`g++ -std=c++17 -O0 -S`, LP64):
    ///   V  = { vptr@0, v@8 }                     size 16
    ///   M  = { M.vptr@0, m@8, V.vptr@16, v@24 }  size 32
    ///   `_ZTV1M` = [vbase 16][ott 0][ti M][M::fm] | [ott -16][ti M][V::fv]
    ///   `_ZTV1M` 的 V 链段 address point = +56; M ctor 写 obj+16 = 该地址
    #[test]
    fn vbase_subobject_gets_its_own_vftable_and_slot() {
        let mut calc = ItaniumAbiCalculator::new();
        let (v_id, m_id) = (ClassId(5), ClassId(6));
        calc.register_class_name(v_id, "V");
        calc.register_class_name(m_id, "M");
        let i32 = CxxType::Integer {
            bits: 32,
            signed: true,
        };
        // V: 自己声明 fv (声明地 = V, 未被任何类覆写)
        calc.register_class_with_name(
            v_id,
            "V",
            vec![],
            vec![],
            vec![("v".into(), i32.clone())],
            true,
        );
        calc.register_method_with_decl(
            v_id,
            "fv".into(),
            "V_fv".to_string(),
            true,
            v_id,
            Some(v_id),
        );
        // M: 虚继 V, 自己新增 fm
        calc.register_class_with_name(
            m_id,
            "M",
            vec![],
            vec![v_id],
            vec![("m".into(), i32.clone())],
            true,
        );
        calc.register_method_with_decl(
            m_id,
            "fm".into(),
            "M_fm".to_string(),
            true,
            m_id,
            Some(m_id),
        );

        let m_abi = calc.compute_class(m_id);
        // 虚基尾部子对象 = [vptr][v] = 16 字节 → M 布局 vptr@0, m@8, V 子对象@16
        let vb = m_abi
            .virtual_bases
            .iter()
            .find(|x| x.base == v_id)
            .expect("V is virtual base of M");
        assert_eq!(vb.static_offset, 16, "V 子对象偏移: {vb:?}");

        // 为 V 子对象专供的 secondary vftable 必须存在, 且它的槽不调 this
        let chain = m_abi
            .vtables
            .iter()
            .find(|v| v.symbol == "_ZTVM::vbaseV")
            .expect("V 链的 secondary vftable 必须发射");
        assert!(
            matches!(chain.components[0], VTableComponent::OffsetToTop(-16)),
            "offset-to-top 是从 V 子对象回到完整对象的负偏移: {:?}",
            chain.components[0]
        );
        assert_eq!(chain.address_point, 16, "V 链的 vptr 指向其自己段的开头");
        match &chain.components[2] {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert_eq!(symbol, "V_fv");
                assert!(
                    adjustment.is_zero(),
                    "V::fv 没被覆写, 从 V 子对象进来时 this 已正确, 不该插 thunk: {adjustment:?}"
                );
            },
            other => panic!("expected Function, got {other:?}"),
        }

        // 完整对象构造必须把 V 子对象的 vptr 写成这张 chain vftable
        assert!(
            m_abi
                .vptrs
                .iter()
                .any(|p| p.offset == 16 && p.complete_only),
            "V 子对象的 vptr 槽没登记: {:?}",
            m_abi.vptrs
        );
    }

    /// 虚基被本类覆写时, V 链的槽必须指向**本类实现 + this-adjust thunk**。
    #[test]
    fn vbase_chain_overridden_slot_uses_this_adjust_thunk() {
        let mut calc = ItaniumAbiCalculator::new();
        let (v_id, m_id) = (ClassId(0), ClassId(1));
        calc.register_class_name(v_id, "V");
        calc.register_class_name(m_id, "M");
        let i32 = CxxType::Integer {
            bits: 32,
            signed: true,
        };
        calc.register_class_with_name(
            v_id,
            "V",
            vec![],
            vec![],
            vec![("v".into(), i32.clone())],
            true,
        );
        calc.register_method_with_decl(
            v_id,
            "fv".into(),
            "V_fv".to_string(),
            true,
            v_id,
            Some(v_id),
        );
        calc.register_class_with_name(m_id, "M", vec![], vec![v_id], vec![("m".into(), i32)], true);
        // M 覆写 fv: 实现符号属于 M, 登记的虚槽条目指向被覆写的 V::fv
        calc.register_method_with_decl(
            m_id,
            "fv".into(),
            "M_fv".to_string(),
            true,
            v_id,
            Some(m_id),
        );

        let m_abi = calc.compute_class(m_id);
        let v_off = m_abi
            .virtual_bases
            .iter()
            .find(|x| x.base == v_id)
            .map(|x| x.static_offset as i64)
            .unwrap();
        let chain = m_abi
            .vtables
            .iter()
            .find(|v| v.symbol == "_ZTVM::vbaseV")
            .expect("chain vftable");
        match &chain.components[2] {
            VTableComponent::Function {
                symbol, adjustment, ..
            } => {
                assert_eq!(symbol, "M_fv", "被覆写的槽必须绑到派生实现");
                assert_eq!(
                    adjustment.non_virtual,
                    -(v_off),
                    "覆写实现按完整对象 this 写, 从 V 子对象进来要调回对象开头"
                );
            },
            other => panic!("expected Function, got {other:?}"),
        }
        // thunk 入口必须同时出现在 vtable 引用与 thunk 登记表里 (不留未定义符号)
        let thunk = m_abi
            .thunks
            .iter()
            .find(|t| t.target_symbol == "M_fv")
            .expect("覆写槽的 thunk 必须由布局层分配");
        assert_eq!(thunk.symbol, chain.entry_symbol_at(2).unwrap());
    }
}
