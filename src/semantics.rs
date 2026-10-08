// src/semantics.rs - 后端无关的语义层: 对象解析 + 地址计算
//
// 三层架构:
//   Layer 1: 对象解析 (resolve) -- 源名称 -> 对象身份 + 存储信息
//   Layer 2: 地址计算 (address) -- lvalue 表达式 -> AddressDescriptor
//   Layer 3: 按上下文物化 (materialize) -- 后端特定, 在各后端实现
//
// 本文件仅包含 Layer 1+2 的类型定义和后端无关的计算逻辑。
// Layer 3 的指令发射由各后端 (x86_64, aarch64, riscv64) 自行实现。

use crate::ast::{self, ArraySize, Expr, Type};

// ============================================================================
// Layer 1: 对象解析 (resolve)
// ============================================================================

/// 源名称解析后的对象身份。
/// 仅回答 "这个名字指的是哪个对象", 不含类型/偏移/地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectIdentity {
    /// 栈局部变量 (非 VLA, 非 upvar)
    Local,
    /// VLA (变长数组, 大小在运行时确定)
    Vla,
    /// 全局对象 (文件级全局变量或 static local, 已解析为 globals map 键)
    Global { resolved_key: String },
    /// 嵌套函数的 upvar (来自父帧的捕获变量)
    Upvar,
}

/// 对象的存储信息。
/// 由 Layer 1 产出, 供 Layer 2 地址计算使用。
#[derive(Debug, Clone)]
pub struct StorageInfo {
    pub class: StorageClass,
    pub obj_type: StorageType,
    /// 全局对象的已解析符号键 (用于重定位)。仅 Global 身份有值。
    pub symbol_key: Option<String>,
    /// 栈帧偏移。仅 Local/Vla/Upvar 有值。
    pub stack_offset: Option<i64>,
    /// 是否为 TLS (线程局部存储) 对象。
    pub is_tls: bool,
}

/// 存储类别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageClass {
    /// 栈上局部变量 (固定大小数组或标量)
    Stack,
    /// 全局数据段 (data/bss/rodata)
    GlobalData,
    /// 线程局部存储 (TLS)
    GlobalTLS,
    /// 嵌套函数的 upvar
    Upvar,
}

/// 简化的存储类型 (仅地址计算需要的信息)。
/// 与完整 Type 的区别: 不包含 qualifiers, 不区分 signed/unsigned char 等。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageType {
    Scalar {
        size: usize,
    },
    Array {
        elem_size: usize,
        count: Option<usize>,
    },
    Pointer {
        pointee_size: usize,
    },
    Aggregate {
        size: usize,
    },
    Vector {
        elem_size: usize,
        count: usize,
    },
    Complex {
        inner_size: usize,
    },
    /// VLA: 元素大小已知, 但总大小/计数在运行时确定。
    Vla {
        elem_size: usize,
    },
}

// ============================================================================
// Layer 2: 地址计算 (address)
// ============================================================================

/// 地址描述符: 一个 lvalue 表达式的地址语义。
/// 不包含任何寄存器/指令信息, 纯数据。
#[derive(Debug, Clone)]
pub struct AddressDescriptor {
    /// 基址来源
    pub base: AddressBase,
    /// 字节偏移 (可能含运行时分量)
    pub offset: ByteOffset,
    /// 元素信息 (地址指向的对象的类型特征)
    pub element_info: ElementInfo,
}

/// 基址来源: 如何获取对象的起始地址。
#[derive(Debug, Clone)]
pub enum AddressBase {
    /// 栈帧偏移: base = FP + offset (数组取地址, 如 LEA r13+offset)
    StackFrame { offset: i64 },
    /// 栈槽加载: base = LOAD(FP + offset) (指针/VLA 需从栈上加载值)
    StackSlotLoad { offset: i64 },
    /// 全局符号地址: base = &symbol (数组取地址, 如 RIP-relative LEA)
    GlobalSymbolAddr { symbol_key: String },
    /// 全局符号加载: base = LOAD(&symbol) (指针需加载值)
    GlobalSymbolLoad { symbol_key: String },
    /// Upvar 加载: base = LOAD(FP + upvar_offset)
    UpvarLoad { offset: i64 },
    /// 基址是一个**指针值 rvalue** (不是 lvalue): 直接求值该表达式即得地址。
    /// 例如 `*(p + 2)[i]` / `f()[i]` 的下标基址 —— 旧实现在这里返回
    /// `StackFrame { offset: 0 }` 占位描述符, 后端把它当成真帧基址物化,
    /// 于是静默读到栈底垃圾。
    ComputedAddr { expr: Box<Expr> },
    /// 语义层无法给出基址 (既不是 lvalue 也不是指针 rvalue)。后端遇到它
    /// 必须整体退回自己的旧路径, 不允许用占位地址静默发指令。
    Unknown,
    /// 来自另一个表达式的地址 (递归, 如 s.arr[i])
    ExprAddr { sub: Box<AddressDescriptor> },
    /// 子表达式是指针 lvalue: 先取其地址并加载指针值作为基址。
    PointerValue { sub: Box<AddressDescriptor> },
}

/// 字节偏移: 可能是编译期常量, 也可能含运行时分量。
#[derive(Debug, Clone)]
pub enum ByteOffset {
    /// 编译期常量偏移
    Constant(i64),
    /// 运行时下标项序列 (**外 → 内** 顺序, 至少一项) + 常量补数。
    ///
    /// 每一项描述 `Expr::Index` 链上**一层**运行时下标 (stride + 该层下标
    /// 表达式)。旧表示只有一个运行时槽 (`Runtime { stride, addend }`),
    /// `add_byte_offsets` 遇到第二个运行时项只能保留第一个并静默丢弃后者,
    /// 于是多维下标 (`a[i][j][k]` / `a[i][2][k]` / `a[i][j][3]`) 的地址直接
    /// 算错 —— 行 stride 塌缩。现在描述符自描述每一项, 后端按序累加,
    /// 调用点不再需要「本层 idx 是否就是那个运行时下标」这类猜测。
    Runtime {
        terms: Vec<RuntimeIndexTerm>,
        addend: i64,
    },
    /// VLA 运行时 stride: stride 在运行时才知道 (需要从 VLA 元数据槽加载)。
    /// 只描述**单层** VLA 下标; 与其它运行时项共存时合并为
    /// [`ByteOffset::Unsupported`]。
    VlaRuntime { index: Box<Expr>, addend: i64 },
    /// 语义层无法表达的偏移组合 (两个 VLA 层共存等)。后端遇到它必须整体
    /// 退回自己的旧路径 —— 不允许像旧实现那样静默丢弃一个下标项。
    Unsupported,
}

/// [`ByteOffset::Runtime`] 的一项: `a[...][idx]` 中某一层运行时下标。
#[derive(Debug, Clone)]
pub struct RuntimeIndexTerm {
    /// 该层元素大小 (下标每加一递增的字节数)
    pub stride: usize,
    /// 该层的下标表达式 (运行时求值)
    pub index: Expr,
}

/// 元素信息: 地址计算完成后, 元素是什么类型。
#[derive(Debug, Clone)]
pub struct ElementInfo {
    pub size: usize,
    pub kind: ElementKind,
}

/// 元素种类: 决定 Layer 3 物化时的行为。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementKind {
    /// 标量: 直接加载/存储
    Scalar,
    /// 数组元素: 需要衰减为首元素地址
    Array {
        elem_size: usize,
        count: Option<usize>,
    },
    /// 聚合 (struct/union >8B): 需要地址或整体拷贝
    Aggregate { size: usize },
    /// 复数: 实部 + 虚部
    Complex { inner_size: usize },
    /// SIMD 向量
    Vector { elem_size: usize, count: usize },
}

// ============================================================================
// Layer 3: 物化动作 (后端特定)
// ============================================================================

/// 物化动作: 给定地址, 要做什么。
/// 由 Layer 2 产出, Layer 3 (各后端) 消费。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeAction {
    /// 取地址: &arr[i] -- 地址本身是结果, 不做加载
    TakeAddress,
    /// 标量加载: arr[i] 作为右值 -- 从地址读取值到寄存器
    LoadScalar,
    /// 标量存储: arr[i] = val 的左侧 -- 向地址写入
    StoreScalar,
    /// 数组衰减: arr[i] 的元素是数组 -- 地址退化为指针 (不加载)
    ArrayDecay,
    /// 聚合加载: 元素是 struct/union >8B -- 返回地址供调用方拷贝
    AggregateLoad,
    /// 聚合存储: 元素是 struct/union >8B -- 整体拷贝 (rep movsb)
    AggregateStore,
    /// 复数加载
    ComplexLoad,
    /// 复数存储: 实部 [addr], 虚部 [addr+inner_size]
    ComplexStore,
}

// ============================================================================
// ObjectResolver trait: 后端向语义层提供名字查找能力的接口
// ============================================================================

/// 对象解析上下文。每个后端实现此 trait 提供名字查找能力。
///
/// 这是 Layer 1 与后端的唯一接口。后端通过实现此 trait,
/// 将 var_map / vla_offsets / upvar_offsets / globals / static_local_map
/// 等内部状态统一暴露为语义层的查询结果。
pub trait ObjectResolver {
    /// 解析源名称的对象身份 (纯名字查找, 不查类型)。
    fn resolve_identity(&self, name: &str) -> ObjectIdentity;

    /// 根据对象身份获取存储信息 (查类型、符号、偏移)。
    fn resolve_storage(&self, id: &ObjectIdentity, name: &str) -> StorageInfo;

    /// 判断名称是否为局部名字 (栈变量/VLA/upvar)。
    fn is_local_name(&self, name: &str) -> bool;
}

// ============================================================================
// Layer 2 核心函数: 地址计算 (后端无关)
// ============================================================================

/// 地址计算的上下文信息。
/// 提供 Layer 2 需要但不在 ObjectResolver 中的信息。
pub struct AddressContext {
    /// 当前函数是否嵌套函数 (影响 upvar 查找)
    pub is_nested: bool,
}

/// 计算 lvalue 表达式的地址描述符 (后端无关)。
///
/// 递归处理: Ident, Index, Member, Deref, Cast 的组合。
/// 这是 C 语义的核心: arr[i].field->member[j] 的地址计算。
///
/// `ctx` 只携带 `is_nested` (为嵌套函数 upvar 查找预留), 当前地址计算
/// 并不读取它, 故不再沿递归透传 (递归实体见私有 `lvalue_address`),
/// 保留该形参只为不改动后端现有调用点。
pub fn compute_lvalue_address<R: ObjectResolver>(
    resolver: &R,
    expr: &Expr,
    _ctx: &AddressContext,
) -> AddressDescriptor {
    lvalue_address(resolver, expr)
}

/// `compute_lvalue_address` 的递归实现: 只携带真正参与地址计算的参数。
fn lvalue_address<R: ObjectResolver>(resolver: &R, expr: &Expr) -> AddressDescriptor {
    match expr {
        Expr::Ident { name, ty, .. } => {
            let id = resolver.resolve_identity(&name.name);
            let storage = resolver.resolve_storage(&id, &name.name);
            address_from_storage(storage, ty)
        },

        Expr::Index {
            arr,
            idx,
            ty: elem_ty,
            ..
        } => {
            // 数组/向量对象下标: 基址就是对象地址。
            // 指针成员下标 (如 s.p[i]): 先读取成员中保存的指针值。
            let mut base_addr = lvalue_address(resolver, arr);
            if matches!(arr.as_ref(), Expr::Member { .. }) && matches!(arr.ty(), Type::Ptr(_)) {
                base_addr = AddressDescriptor {
                    base: AddressBase::PointerValue {
                        sub: Box::new(base_addr),
                    },
                    offset: ByteOffset::Constant(0),
                    element_info: element_info_from_type(elem_ty),
                };
            }
            let idx_offset = compute_index_offset(idx, elem_ty);
            combine_address(base_addr, idx_offset, elem_ty)
        },

        Expr::Member {
            expr: base,
            field,
            is_arrow,
            ty: member_ty,
            ..
        } => {
            if *is_arrow {
                // ptr->field: 先加载指针值, 再加字段偏移
                let ptr_addr = lvalue_address(resolver, base);
                let field_offset = member_field_offset(base, field);
                let combined = add_constant_offset(ptr_addr, field_offset);
                AddressDescriptor {
                    element_info: element_info_from_type(member_ty),
                    ..combined
                }
            } else {
                // s.field: 在结构体地址上加字段偏移
                let struct_addr = lvalue_address(resolver, base);
                let field_offset = member_field_offset(base, field);
                let combined = add_constant_offset(struct_addr, field_offset);
                AddressDescriptor {
                    element_info: element_info_from_type(member_ty),
                    ..combined
                }
            }
        },

        Expr::Deref { e, ty, .. } => {
            // *p: p 的值就是目标地址。
            let ptr_addr = lvalue_address(resolver, e);
            AddressDescriptor {
                base: AddressBase::ExprAddr {
                    sub: Box::new(ptr_addr),
                },
                offset: ByteOffset::Constant(0),
                element_info: element_info_from_type(ty),
            }
        },

        Expr::Cast { e, ty, .. } => {
            // (T)e 作为 lvalue: 穿透 cast, 直接计算内层地址
            let inner = lvalue_address(resolver, e);
            AddressDescriptor {
                element_info: element_info_from_type(ty),
                ..inner
            }
        },

        _ => {
            // 不是 lvalue: 若它是个指针 rvalue, 其**值**就是目标地址
            // (`*(e + 2)` / `f()[i]` 等形态的下标基址)。
            if matches!(expr.ty(), Type::Ptr(_)) {
                return AddressDescriptor {
                    base: AddressBase::ComputedAddr {
                        expr: Box::new(expr.clone()),
                    },
                    offset: ByteOffset::Constant(0),
                    element_info: ElementInfo {
                        size: expr.ty().size(),
                        kind: ElementKind::Scalar,
                    },
                };
            }
            // 其余非 lvalue: 明确标记为无法物化, 让后端退回旧路径
            // (旧实现返回 `StackFrame { offset: 0 }` 占位符 —— 那是个看起来
            // 合法的地址, 会把错误静默地编译进去)。
            AddressDescriptor {
                base: AddressBase::Unknown,
                offset: ByteOffset::Constant(0),
                element_info: ElementInfo {
                    size: expr.ty().size(),
                    kind: ElementKind::Scalar,
                },
            }
        },
    }
}

/// 计算索引偏移 (后端无关)。
///
/// 尝试编译期求值; 若失败则产生运行时偏移描述。
pub fn compute_index_offset(idx: &Expr, elem_ty: &Type) -> ByteOffset {
    let stride = element_stride(elem_ty);

    // 尝试编译期求值索引
    if let Ok(val) = crate::const_eval::eval(idx) {
        let byte_off = (val as i64).wrapping_mul(stride as i64);
        return ByteOffset::Constant(byte_off);
    }

    // 运行时偏移: 每项记住自己的下标表达式 (见 [`ByteOffset::Runtime`] 的根因说明)
    match elem_ty {
        // VLA 元素: stride 在运行时才知道
        _ if elem_ty.has_vla() => ByteOffset::VlaRuntime {
            index: Box::new(idx.clone()),
            addend: 0,
        },
        _ => ByteOffset::Runtime {
            terms: vec![RuntimeIndexTerm {
                stride,
                index: idx.clone(),
            }],
            addend: 0,
        },
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 根据存储信息确定基址 (Layer 1 -> Layer 2 的桥梁)。
fn address_from_storage(storage: StorageInfo, ty: &Type) -> AddressDescriptor {
    let base = match (&storage.class, ty) {
        // VLA / VM 聚合: 帧里的那个槽是**元数据槽**，它的内容才是动态数据区
        // 基址（`gen_vla_alloc` 发 `mov %rsp,[off]`）。它与下面「槽位即对象
        // 地址」的普通栈上聚合形状相反：这里走 StackFrame 会 `lea` 出描述符本身，
        // 于是 a[i] 的读写全部落回固定帧区，先踩掉自己的 stride/size 槽。
        // 后端侧同类判断见 mem/access.rs::emit_ident_base_addr 与 lvalue.rs：
        // 判据取两份的并 —— `obj_type` 是解析器给出的身份，`ty.has_vla()` 直接
        // 描述类型（它能兼顾 `int a[2][n]` 这种外层常量、内层运行时的形状，
        // `storage_type_from_ast` 对它们只报 Array）。
        (StorageClass::Stack, _) if matches!(storage.obj_type, StorageType::Vla { .. }) || ty.has_vla() => {
            AddressBase::StackSlotLoad {
                offset: storage.stack_offset.unwrap_or(0),
            }
        },
        // 栈上数组/向量/结构体/联合体: 槽位即对象地址, 取地址 (LEA FP+offset)。
        // 聚合是 lvalue, 标识符的值约定为其栈槽地址 (见 x86_64 gen_lvalue_addr
        // 的 Ident 分支)。若误走 StackSlotLoad 会把槽内数据当指针解引用
        // (如 s.v[0] 对含向量字段的结构体参数访问时段错误)。
        (StorageClass::Stack, Type::Array(_, _))
        | (StorageClass::Stack, Type::Vector(_, _))
        | (StorageClass::Stack, Type::Struct(_, _, _, _))
        | (StorageClass::Stack, Type::Union(_, _, _, _)) => AddressBase::StackFrame {
            offset: storage.stack_offset.unwrap_or(0),
        },
        // 栈上标量/指针: 需要加载值 (LOAD [FP+offset])
        (StorageClass::Stack, _) => AddressBase::StackSlotLoad {
            offset: storage.stack_offset.unwrap_or(0),
        },
        // 全局数组/聚合: 取符号地址 (RIP-relative LEA)
        (StorageClass::GlobalData, Type::Array(_, _))
        | (StorageClass::GlobalData, Type::Vector(_, _))
        | (StorageClass::GlobalData, Type::Struct(_, _, _, _))
        | (StorageClass::GlobalData, Type::Union(_, _, _, _))
        | (StorageClass::GlobalTLS, Type::Array(_, _))
        | (StorageClass::GlobalTLS, Type::Vector(_, _))
        | (StorageClass::GlobalTLS, Type::Struct(_, _, _, _))
        | (StorageClass::GlobalTLS, Type::Union(_, _, _, _)) => AddressBase::GlobalSymbolAddr {
            symbol_key: storage.symbol_key.unwrap_or_default(),
        },
        // 全局标量/指针: 加载符号值
        (StorageClass::GlobalData, _) | (StorageClass::GlobalTLS, _) => {
            AddressBase::GlobalSymbolLoad {
                symbol_key: storage.symbol_key.unwrap_or_default(),
            }
        },
        // Upvar: 从父帧加载
        (StorageClass::Upvar, _) => AddressBase::UpvarLoad {
            offset: storage.stack_offset.unwrap_or(0),
        },
    };

    AddressDescriptor {
        base,
        offset: ByteOffset::Constant(0),
        element_info: element_info_from_type(ty),
    }
}

/// 合并基址地址和索引偏移。
fn combine_address(
    mut base_addr: AddressDescriptor,
    idx_offset: ByteOffset,
    elem_ty: &Type,
) -> AddressDescriptor {
    // 合并偏移量
    base_addr.offset = add_byte_offsets(base_addr.offset, idx_offset);
    // 更新元素信息
    base_addr.element_info = element_info_from_type(elem_ty);
    base_addr
}

/// 两个字节偏移相加 (`a` = 已累加的外层, `b` = 新加入的内层)。
fn add_byte_offsets(a: ByteOffset, b: ByteOffset) -> ByteOffset {
    use ByteOffset::*;
    match (a, b) {
        (Constant(x), Constant(y)) => Constant(x + y),
        (Constant(c), Runtime { terms, addend }) | (Runtime { terms, addend }, Constant(c)) => {
            Runtime {
                terms,
                addend: addend + c,
            }
        },
        // 两个运行时项序列: 按 外 → 内 顺序拼接, 常量补数相加
        (
            Runtime {
                terms: mut outer,
                addend: a1,
            },
            Runtime {
                terms: inner,
                addend: a2,
            },
        ) => {
            outer.extend(inner);
            Runtime {
                terms: outer,
                addend: a1 + a2,
            }
        },
        (Constant(c), VlaRuntime { index, addend })
        | (VlaRuntime { index, addend }, Constant(c)) => VlaRuntime {
            index,
            addend: addend + c,
        },
        // VLA 层与任何其它运行时项共存: 现有物化协议只有一个 VLA stride 槽,
        // 无法表达第二层动态 stride —— 明确标记为不支持, 让后端整体退回
        // 旧路径 (旧实现是保留第一个 + 丢弃第二个, 即静默算错地址)。
        (VlaRuntime { .. }, Runtime { .. })
        | (Runtime { .. }, VlaRuntime { .. })
        | (VlaRuntime { .. }, VlaRuntime { .. }) => Unsupported,
        (Unsupported, _) | (_, Unsupported) => Unsupported,
    }
}

/// 在地址描述符上加一个常量偏移。
fn add_constant_offset(mut desc: AddressDescriptor, offset: i64) -> AddressDescriptor {
    desc.offset = add_byte_offsets(desc.offset, ByteOffset::Constant(offset));
    desc
}

/// 从 Type 提取 ElementInfo。
fn element_info_from_type(ty: &Type) -> ElementInfo {
    let size = ty.size();
    let kind = match ty {
        Type::Array(elem, ArraySize::Const(n)) => ElementKind::Array {
            elem_size: elem.size(),
            count: Some(*n),
        },
        Type::Array(elem, ArraySize::Var(_)) => ElementKind::Array {
            elem_size: elem.size(),
            count: None,
        },
        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => ElementKind::Aggregate { size },
        Type::Complex(inner) => ElementKind::Complex {
            inner_size: inner.size(),
        },
        Type::Vector(elem, n) => ElementKind::Vector {
            elem_size: elem.size(),
            count: *n,
        },
        _ => ElementKind::Scalar,
    };
    ElementInfo { size, kind }
}

/// 计算元素 stride (一个元素占多少字节)。
/// stride = 元素类型的完整大小 (含内层数组维度)。
/// 如 elem_ty = int[3] (从 int[2][3] 索引后), stride = 12 (3*4)。
pub fn element_stride(elem_ty: &Type) -> usize {
    elem_ty.size()
}

/// 计算结构体/联合体成员字段的字节偏移。
fn member_field_offset(base_expr: &Expr, field: &ast::Ident) -> i64 {
    let base_ty = base_expr.ty();
    // 穿透指针: ptr->field 的基类型是指针指向的类型
    let struct_ty = match &base_ty {
        Type::Ptr(qt) => &qt.base,
        other => other,
    };
    match struct_ty {
        Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
            for f in fields.iter() {
                if f.name == field.name {
                    return f.offset as i64;
                }
            }
            0
        },
        _ => 0,
    }
}

/// 根据元素类型和加载/存储上下文确定物化动作。
pub fn determine_action(elem_ty: &Type, is_load: bool) -> MaterializeAction {
    match elem_ty {
        Type::Array(_, _) => MaterializeAction::ArrayDecay,
        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) if elem_ty.size() > 8 => {
            if is_load {
                MaterializeAction::AggregateLoad
            } else {
                MaterializeAction::AggregateStore
            }
        },
        Type::Complex(_) => {
            if is_load {
                MaterializeAction::ComplexLoad
            } else {
                MaterializeAction::ComplexStore
            }
        },
        _ => {
            if is_load {
                MaterializeAction::LoadScalar
            } else {
                MaterializeAction::StoreScalar
            }
        },
    }
}

/// 根据元素类型确定存储动作 (用于赋值左侧)。
pub fn determine_store_action(elem_ty: &Type) -> MaterializeAction {
    determine_action(elem_ty, false)
}

/// 从 Type 转换为 StorageType (Layer 1 辅助)。
pub fn storage_type_from_ast(ty: &Type) -> StorageType {
    match ty {
        Type::Array(elem, ArraySize::Const(n)) => StorageType::Array {
            elem_size: elem.size(),
            count: Some(*n),
        },
        Type::Array(elem, ArraySize::Var(_)) => StorageType::Vla {
            elem_size: elem.size(),
        },
        Type::Ptr(_) => StorageType::Pointer {
            pointee_size: ty.size(),
        },
        Type::Vector(elem, n) => StorageType::Vector {
            elem_size: elem.size(),
            count: *n,
        },
        Type::Complex(inner) => StorageType::Complex {
            inner_size: inner.size(),
        },
        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {
            StorageType::Aggregate { size: ty.size() }
        },
        _ => StorageType::Scalar { size: ty.size() },
    }
}

// ============================================================================
// 共享的表达式判据（codegen 与 SSA 降级共用）
// ============================================================================

/// 这条表达式求值是否带副作用（自增自减/赋值/调用/它们嵌套在任何子表达式里）。
///
/// 为什么放在后端无关的语义层而不是某个后端里：「先算地址、读—改—写只用那一个地址」
/// 这一类降级在 **codegen**（`x86_64` 的复合赋值单求值路径）与 **SSA 构建器**（自增
/// 自减、复合赋值）两侧都需要同一个判据。以前它只在 x86_64 里有，于是 SSA 侧根本
/// 没法区分「重复降级子表达式无害」与「会把 `i++` 跑两遍」—— 实测 -O2 下
/// `a[i] += 7` 的返回值是未初始化垃圾、`a[i++] += 5` 写错位置（见 `docs/` 本轮记录）。
/// 判据只写一份，两侧都从这里取。
pub fn expr_has_side_effects(e: &Expr) -> bool {
    match e {
        Expr::Unary { op, e: inner, .. } => {
            matches!(op, ast::UnOp::Neg) // PreDecr/PostDecr/PreIncr/PostIncr 是独立变体
                || expr_has_side_effects(inner)
        },
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_has_side_effects(l) || expr_has_side_effects(r)
        },
        Expr::Assign { .. } => true,
        Expr::Call { .. } => true,
        Expr::PreInc { .. } | Expr::PreDec { .. } | Expr::PostInc { .. } | Expr::PostDec { .. } => {
            true
        },
        Expr::Deref { e, .. }
        | Expr::Cast { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. } => expr_has_side_effects(e),
        Expr::Index { arr, idx, .. } => {
            expr_has_side_effects(arr) || expr_has_side_effects(idx)
        },
        Expr::Member { expr, .. } => expr_has_side_effects(expr),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_has_side_effects(cond)
                || expr_has_side_effects(then_)
                || expr_has_side_effects(else_)
        },
        _ => false,
    }
}

/// 两棵表达式子树是否**结构相同**（同一对象 + 同样的下标/字段链）。
///
/// 用途：parser 把复合赋值 `LV op= R` 展开成 `Assign{l: LV, r: Binop{op, LV_拷贝, R}}`，
/// 于是「r 里那一半边是不是 LV 的拷贝」必须靠结构相等判断，两侧（codegen 的单求值
/// 路径、SSA 的读—改—写降级）用的是同一份实现。
///
/// 注意：这里**不**比较类型与 span，只比形态与名字 —— 这是刻意的：拷贝子树在
/// 整数提升等处可能带不同 span，比 span 会让判定失效而退回双求值的错路。
pub fn exprs_structurally_equal(a: &Expr, b: &Expr) -> bool {
    match (a, b) {
        (Expr::Ident { name: na, .. }, Expr::Ident { name: nb, .. }) => na.name == nb.name,
        (Expr::Int { v: va, .. }, Expr::Int { v: vb, .. }) => va == vb,
        (Expr::Deref { e: ea, .. }, Expr::Deref { e: eb, .. }) => {
            exprs_structurally_equal(ea, eb)
        },
        (
            Expr::Index {
                arr: aa, idx: ia, ..
            },
            Expr::Index {
                arr: ab, idx: ib, ..
            },
        ) => exprs_structurally_equal(aa, ab) && exprs_structurally_equal(ia, ib),
        (
            Expr::Member {
                expr: ea,
                field: fa,
                is_arrow: aa,
                ..
            },
            Expr::Member {
                expr: eb,
                field: fb,
                is_arrow: ab,
                ..
            },
        ) => aa == ab && fa.name == fb.name && exprs_structurally_equal(ea, eb),
        (Expr::Unary { op: oa, e: ea, .. }, Expr::Unary { op: ob, e: eb, .. }) => {
            oa == ob && exprs_structurally_equal(ea, eb)
        },
        (Expr::PreInc { e: ea, .. }, Expr::PreInc { e: eb, .. })
        | (Expr::PreDec { e: ea, .. }, Expr::PreDec { e: eb, .. })
        | (Expr::PostInc { e: ea, .. }, Expr::PostInc { e: eb, .. })
        | (Expr::PostDec { e: ea, .. }, Expr::PostDec { e: eb, .. }) => {
            exprs_structurally_equal(ea, eb)
        },
        (
            Expr::Binop {
                op: oa,
                l: la,
                r: ra,
                ..
            },
            Expr::Binop {
                op: ob,
                l: lb,
                r: rb,
                ..
            },
        ) => {
            oa == ob && exprs_structurally_equal(la, lb) && exprs_structurally_equal(ra, rb)
        },
        (Expr::Cast { e: ea, .. }, Expr::Cast { e: eb, .. }) => exprs_structurally_equal(ea, eb),
        (Expr::Comma { l: la, r: ra, .. }, Expr::Comma { l: lb, r: rb, .. }) => {
            exprs_structurally_equal(la, lb) && exprs_structurally_equal(ra, rb)
        },
        (Expr::AddrOf { e: ea, .. }, Expr::AddrOf { e: eb, .. }) => {
            exprs_structurally_equal(ea, eb)
        },
        _ => false,
    }
}

// ============================================================================
// CPU ISA 特性规格 —— 全项目唯一真源
// ============================================================================

/// x86-64 CPUID 特性规格表。
///
/// ## 为什么这里而不是别处
///
/// 「哪个 ISA 名字对应 leaf/reg 的哪一位」「变体之间谁优先」「特性字要落在
/// 运行时缓存的哪个槽」这三件事，此前分散在 `src/link/target_clones.rs` 的私有
/// 位表（内置链接器现场拼装 resolver 字节流用）与 `src/cpuid.rs` 的宿主侧
/// `detect_x86()`（`-march=native` 用）里。`src/cpuid.rs` 是**命令式**赋值序列，
/// 只能给编译器自己的进程用，无法被目标代码复用；内置链接器那张表则是目标代码
/// 需要的**声明式**规格。codegen（发 resolver 体）与 link（发 IRELATIVE）必须
/// 用同一份规格，否则两条链接路径会解析出不同变体 —— 本模块就是那份规格。
///
/// `src/cpuid.rs::detect_x86()`（`CpuFeatures`，宿主进程内用 `__cpuid` intrinsic
/// 命令式赋值）与本表的 leaf/bit 编号**逐条对过**（对表过程记在
/// `docs/wp_ifunc_real_fix.md`），并有单元测试 `table_agrees_with_host_detector`
/// 把两者在同一台真机上钉在一起比对。`src/cpuid.rs` 不在本轮改动范围内，
/// 它应当改为消费本表（见报告「需主控代做的跨所有权改动」）。
pub mod cpu_isa {
    /// CPUID 结果寄存器编号（`CpuidBit::reg`）。
    pub const REG_EAX: u8 = 0;
    pub const REG_EBX: u8 = 1;
    pub const REG_ECX: u8 = 2;
    pub const REG_EDX: u8 = 3;

    /// 一个 CPUID 特性位：`leaf` 号、**子 leaf（ECX 输入）**、结果寄存器、位号。
    ///
    /// 子 leaf 必须是缓存索引 `(leaf, subleaf, reg)` 的一部分：`(7,0):EDX` 与
    /// `(7,1):EAX` 之间位号会重合（`avx512vp2intersect` = (7,0):EDX#8、
    /// `avx512bf16` = (7,1):EAX#5、`xsaveopt` = (0xD,1):EAX#0 与 `sse3` =
    /// (1,0):ECX#0 同位不同义），少一维就把「测了、位号也对、但读错子 leaf/叶」
    /// 这种形状留给运行时才暴露 —— 编译与链接全绿，只有 gcc 对照能看见。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CpuidBit {
        pub leaf: u32,
        pub subleaf: u32,
        pub reg: u8,
        pub bit: u8,
    }

    /// 一条 ISA 特性的完整规格。
    #[derive(Debug, Clone, Copy)]
    pub struct IsaFeature {
        /// C 源码里写法（`target_clones("sse4.2")` / `__builtin_cpu_supports("sse4.2")`）。
        pub name: &'static str,
        /// 必须全部置位的 CPUID 位（跨 leaf 的组合，如 avx2 = leaf1.AVX + leaf7.AVX2）。
        pub bits: &'static [CpuidBit],
        /// 需要的 XCR0 低 32 位掩码；0 表示不做 xgetbv 检查。
        /// AVX 系需要 SSE+YMM = 0x6；AVX-512 还需要 opmask/ZMM = 0xE6。
        pub xcr0_mask: u32,
        /// `target_clones` 变体的判定优先级：**数值大者先测**（即先选中）。
        /// 取值来自对 gcc 13 实测（见单元测试
        /// `dispatch_order_matches_gcc_measured_chain` 里记的实测链），不是凭空排的。
        pub clone_priority: i32,
        /// 是否可作为 `target_clones`/`target` 的 ISA 名（gcc 只接受一个子集，
        /// 其余名字 gcc 直接报 "ISA ... is not supported in 'target' attribute"）。
        pub cloneable: bool,
    }

    // ---- 表行构造宏 ----
    //
    // 用宏而不是 `const fn`：`IsaFeature::bits` 是 `&'static [CpuidBit]`，而
    // `&[...]` 这种临时量提升（rvalue static promotion）只在**常量上下文里内联
    // 展开**时发生；`const fn` 体内不提升，会把临时量的生命周期卡在函数返回处
    // （E0716）。宏在调用点展开成完整字面量，既保住「一行一个特性」的可读性，
    // 又让 leaf/reg/位号在表里一眼可见，便于和
    // Linux `arch/x86/include/asm/cpufeatures.h` 逐条对账。
    macro_rules! bit_feature {
        ($leaf:expr, $sub:expr, $reg:expr, $name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            IsaFeature {
                name: $name,
                bits: &[CpuidBit { leaf: $leaf, subleaf: $sub, reg: $reg, bit: $bit }],
                xcr0_mask: 0,
                clone_priority: $prio,
                cloneable: $cloneable,
            }
        };
    }
    /// CPUID leaf 1, ECX。
    macro_rules! f1ecx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(1, 0, REG_ECX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 1, EDX。
    macro_rules! f1edx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(1, 0, REG_EDX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 7 sub-leaf 0, EBX。
    macro_rules! f7ebx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(7, 0, REG_EBX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 7 sub-leaf 0, ECX。
    macro_rules! f7ecx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(7, 0, REG_ECX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 7 sub-leaf 0, EDX。
    macro_rules! f7edx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(7, 0, REG_EDX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 7 **sub-leaf 1**, EAX（`avx512bf16`/`avx_vnni` 只在这一叶）。
    ///
    /// 这个宏目前**没被使用**，不是「漏接」：两条真在那一子叶的条目都需要
    /// XCR0 门控（`avx_vnni` 要 0x6、`avx512bf16` 要 0xE6），而本宏与其余单叶宏
    /// 一样把 `xcr0_mask` 写死为 0，套上去会把门控弄丢。它们因此写成显式
    /// `IsaFeature` 字面量（见 `FEATURES_TAIL`）。宏保留作为「将来有子叶 1 的
    /// 无门控位时」的统一入口。
    #[allow(unused_macros)]
    macro_rules! f7s1eax {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(7, 1, REG_EAX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 0xD **sub-leaf 1**, EAX（紧凑 XSAVE 状态位：`xsaveopt`/`xsavec`/`xsaves`）。
    macro_rules! leafd1eax {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(0xd, 1, REG_EAX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 0x8000_0008, EBX（`clzero` / `wbnoinvd`）。
    macro_rules! ext8ebx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(0x8000_0008, 0, REG_EBX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 0x8000_0001, ECX。
    macro_rules! ext1ecx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(0x8000_0001, 0, REG_ECX, $name, $bit, $prio, $cloneable)
        };
    }
    /// CPUID leaf 0x8000_0001, EDX。
    macro_rules! ext1edx {
        ($name:literal, $bit:literal, $prio:literal, $cloneable:literal) => {
            bit_feature!(0x8000_0001, 0, REG_EDX, $name, $bit, $prio, $cloneable)
        };
    }
    /// AVX-512 系的公共形状：AVX 位 + AVX512F 位 + 自己的位，XCR0 需 0xE6。
    macro_rules! avx512 {
        ($name:literal, $own_reg:expr, $own_bit:literal, $prio:literal, $cloneable:literal) => {
            IsaFeature {
                name: $name,
                bits: &[
                    CpuidBit { leaf: 1, subleaf: 0, reg: REG_ECX, bit: 28 },
                    CpuidBit { leaf: 7, subleaf: 0, reg: REG_EBX, bit: 16 },
                    CpuidBit { leaf: 7, subleaf: 0, reg: $own_reg, bit: $own_bit },
                ],
                xcr0_mask: 0xE6,
                clone_priority: $prio,
                cloneable: $cloneable,
            }
        };
    }

    /// 规格表 A：单 leaf / 单寄存器、无需 XSAVE 门控的特性。
    ///
    /// 位号的**权威来源是 Linux `arch/x86/include/asm/cpufeatures.h`（word 号 + 位号）
    /// 与 gcc 的 `common/config/i386/cpuid.h`（`bit_*` 宏）两者交叉**，两者逐条一致时
    /// 才写进本表。`src/cpuid.rs::detect_x86()`（宿主侧探测，不属于本工作范围）
    /// 与它交叉核对时发现的差异（本机 2026-10-03 逐字 grep 过的现状）：
    ///   * `avx_vnni` —— 真值 (7,1):EAX#4（`cpufeatures.h` word 12 bit 4），
    ///     `detect_x86()` 却读成 `(7,0):ECX#4`（`src/cpuid.rs:359`，那一位是 **OSPKE**）；
    ///     且它还额外 `&& feats.avx`，而 AVX_VNNI 需要的是 XCR0 门控；
    ///   * `serialize`、`clwb` —— `detect_x86()` **根本不报告这两个名字**（`src/cpuid.rs` 里
    ///     搜不到相应字段），所以这里不是「两份表错得不同」而是「一侧缺口」；本表仍按
    ///     权威源写位号（serialize = (7,0):EDX#14，clwb = (7,0):EBX#24）。
    /// 本表按**权威源**而不是按 `detect_x86()` 写；两者的统一属于跨所有权事项
    /// （`src/cpuid.rs` 不在本工作包内，且单元判据 `table_agrees_with_host_detector` 只
    /// 列举两边都有的名字，拓不到上述三项）。
    /// 名字集合则是对 gcc 13.3 逐个实测 `__builtin_cpu_supports("<n>")` 能否编过得
    /// 出来的（取数过程记在 `docs/wp_ifunc_real_fix.md`）。
    /// `clone_priority` 只对 cloneable 条目有意义（取值来自 gcc 13 实测的判定链）。
    pub static FEATURES: &[IsaFeature] = &[
        // ---- CPUID leaf 1, ECX ----
        f1ecx!("sse3", 0, 25, true),
        // XSAVEOPT 在 gcc/libgcc 的语义里**不是** leaf1/ECX#0（虽然 SDM 也把 leaf1/ECX#0
        // 叫一个历史同名位）：`cpuinfo.h` 从 CPUID.(0xD,1):EAX#0 取它，见下面 `leafd1eax!`。
        f1ecx!("pclmul", 1, 60, true),
        f1ecx!("pclmulqdq", 1, 60, false), // `__builtin_cpu_supports` 的写法
        f1ecx!("cx16", 13, 0, false),
        f1ecx!("ssse3", 9, 30, true),
        f1ecx!("sse4.1", 19, 40, true),
        f1ecx!("sse4.2", 20, 45, true),
        f1ecx!("movbe", 22, 0, false),
        f1ecx!("popcnt", 23, 50, true),
        f1ecx!("aes", 25, 55, true),
        f1ecx!("xsave", 26, 0, false),
        f1ecx!("osxsave", 27, 0, false),
        f1ecx!("f16c", 29, 0, false),
        f1ecx!("rdrnd", 30, 0, false),
        // ---- CPUID leaf 1, EDX ----
        f1edx!("cmov", 15, 0, false),
        f1edx!("mmx", 23, 10, true),
        f1edx!("fxsr", 24, 0, false),
        f1edx!("sse", 25, 15, true),
        f1edx!("sse2", 26, 20, true),
        // ---- CPUID leaf 7 sub-leaf 0, EBX ----
        // 本节位号的权威来源：Linux `arch/x86/include/asm/cpufeatures.h` 的
        // 「CPUID level 0x00000007:0 (EBX), word 9」，并与 gcc `cpuid.h` 的 `bit_*`
        // 与 `cpuinfo.h` 里「`ebx & bit_*`」的寄存器一起对账（三处一致才采纳）。
        f7ebx!("bmi", 3, 70, true),
        f7ebx!("bmi2", 8, 80, true),
        f7ebx!("erms", 9, 0, false),
        // CLWB = CPUID.(EAX=07H,ECX=0):**EBX[24]**（Linux word 9 `(9*32+24)`
        // `X86_FEATURE_CLWB`；gcc `cpuid.h` 的 `bit_CLWB` = 1<<24，`cpuinfo.h` 读
        // `ebx & bit_CLWB`）。这里曾经是 30 —— 30 是 AVX512BW，本机 EBX#30=1 而
        // #24=0（/proc/cpuinfo 无 clwb 标志），于是「有 AVX-512BW 但没有 CLWB」的
        // 机器会被答成支持 CLWB，是**假阳性**。它同时是 AMD family 17h 里区分
        // znver1/znver2 的分类守卫（见 `CLASSIFY`），所以名字必须在唯一真源里 ——
        // 分类链只能经由 `slot_masks("clwb")` 拿位号。
        f7ebx!("clwb", 24, 0, false),
        f7ebx!("adx", 19, 0, false),
        // RDSEED = CPUID.(EAX=07H,ECX=0):EBX[18]（Intel SDM 与 gcc 的 `bit_RDSEED` 同位）。
        // 这里曾经是 22 —— 22 是 ENCLV，本机（有 rdseed）因此一直读不到，而 gcc 报 1。
        // 它不是 `cloneable`，所以不会被 resolver 序列照到，但 `__builtin_cpu_supports("rdseed")`
        // 的答案必须与 gcc/libgcc 一致；由判据
        // `attr_target_clones_cpu_supports_matrix_matches_gcc` 钉住（逐名字与 gcc 对照）。
        f7ebx!("rdseed", 18, 0, false),
        f7ebx!("sha", 29, 0, false),
        // ---- CPUID leaf 7 sub-leaf 0, ECX ----
        // PKU 取 **OSPKE**(ECX[4]) 而不是 ECX[3]：`cpuinfo.h` 的原话是
        // 「NB: bit_OSPKE indicates that OS supports PKU」，即 gcc 对
        // `__builtin_cpu_supports("pku")` 的答案就是 OSPKE 位。照 SDM 用 ECX[3]
        // 会在有 PKU 硬件但 OS 未使能的机器上与 gcc 不一致。
        f7ecx!("pku", 4, 0, false),
        f7ecx!("waitpkg", 5, 0, false),
        f7ecx!("shstk", 7, 0, false), // = CET_SS
        f7ecx!("gfni", 8, 0, false),
        f7ecx!("vaes", 9, 0, false),
        f7ecx!("vpclmulqdq", 10, 0, false),
        // MOVDIRI = CPUID.(7,0):**ECX[27]**（Linux word 16 `(16*32+27)`；`cpuinfo.h`
        // 写 `ecx & bit_MOVDIRI`，`cpuid.h` 的 `bit_MOVDIRI` 是 `1<<27`）。这里曾错写成
        // **EDX[27]**：本机（Rocket Lake）ECX#27=0而 EDX#27=1（那一位是
        // INTEL_STIBP），于是 `__builtin_cpu_supports("movdiri")` 在**不支持**该指令的
        // 机器上报 1 —— gcc 报 0。这个错靠「把 movdiri 从对照夹具里排除」掩盖过一轮
        // （当时把它当成「libgcc 实现细节」），现在位号改对并把名字放回夹具。
        f7ecx!("movdiri", 27, 0, false),
        // ---- CPUID leaf 7 sub-leaf 0, EDX ----
        // SERIALIZE = CPUID.(7,0):**EDX[14]**（Linux word 18 `(18*32+14)`；`cpuinfo.h`
        // 也写 `edx & bit_SERIALIZE`）。这里曾经是 EBX[14] —— 那一位是 MPX，方向错的
        // 两个后果是「有 serialize 的机器答 0」与「有 MPX 的旧机器答 1」。
        f7edx!("serialize", 14, 0, false),
        f7edx!("ibt", 20, 0, false), // = CET_IBT
        // AVX512_VP2INTERSECT = (7,0):EDX[8]，且 gcc 把它放在 `if (avx512_usable)` 块里
        // → 需要 XCR0 0xE6。它在 `FEATURES_TAIL`（那里才允许写 xcr0_mask）。
        // ---- CPUID leaf 7 sub-leaf 1, EAX ----
        // 这一子叶由 `PROBES` 的 `(7,1)` 那条采集，并用 `(7,0):EAX`（最大子叶号）门控。
        // `avx512bf16` 的唯一意义是「Intel model 0x55 到底是 Cooper Lake 还是
        // Cascade Lake」；`avx_vnni` 也在这里（见 `FEATURES_TAIL`）。
        // ---- CPUID leaf 0xD sub-leaf 1, EAX ----
        leafd1eax!("xsaveopt", 0, 0, false),
        leafd1eax!("xsavec", 1, 0, false),
        leafd1eax!("xsaves", 3, 0, false),
        // ---- CPUID leaf 0x8000_0001, ECX / EDX ----
        ext1ecx!("lahf_sahf", 0, 0, false),
        ext1ecx!("lzcnt", 5, 0, false),
        ext1ecx!("abm", 5, 0, false), // AMD 写法，与 lzcnt 同位
        ext1ecx!("sse4a", 6, 35, true),
        ext1ecx!("prefetchw", 8, 0, false),
        // 3DNOWPREFETCH = CPUID.8000_0001H:ECX[8]（Linux word 6 `(6*32+8)`，
        // `X86_FEATURE_3DNOWPREFETCH` 与 `X86_FEATURE_PREFETCHW` 同位）。曾经是 30
        // —— 30 在那一 leaf 里没有架构含义，于是这一名字永远读 0（假阴性）。
        ext1ecx!("3dnowprefetch", 8, 0, false), // Linux 把同一位叫 3DNOWPREFETCH
        // XOP：`cpuinfo.h` 在 `if (avx_usable)` 块里读 (0x80000001):ECX[11]
        // → 需要 XCR0 0x6，因此条目在 `FEATURES_TAIL`。
        ext1edx!("syscall", 11, 0, false),
        ext1edx!("nx", 20, 0, false),
        // MMXEXT = CPUID.8000_0001H:EDX[22]（Linux word 1 `(1*32+22)`），曾经是 23。
        ext1edx!("mmxext", 22, 0, false),
        ext1edx!("rdtscp", 27, 0, false),
        ext1edx!("lm", 29, 0, false),
        ext1edx!("3dnow", 31, 0, false),
        // ---- CPUID leaf 0x8000_0008, EBX ----
        // CLZERO = (0x80000008):EBX[0]，AMD znver1/znver2 的分类守卫。
        ext8ebx!("clzero", 0, 0, false),
        ext8ebx!("wbnoinvd", 9, 0, false),
    ];

    /// 规格表 B：需要跨 leaf 组合、和/或 XSAVE(XCR0) 门控的特性。
    ///
    /// 依赖链与 `src/cpuid.rs::detect_x86()` 一致：`fma = CPUID.fma && avx`、
    /// `avx2 = CPUID.avx2 && avx`、`avx512X = CPUID.avx512X && avx512f && avx`，
    /// 其中 `avx` 自身 = `CPUID.avx && osxsave && XCR0[2:1]==0b11`。
    /// `xcr0_mask` 承载 XCR0 那一段：AVX 系 0x6（SSE+YMM），AVX-512 系 0xE6
    /// （再加 opmask / ZMM_Hi256 / Hi16_ZMM）。
    const FEATURES_TAIL: &[IsaFeature] = &[
        IsaFeature {
            name: "avx",
            bits: &[CpuidBit { leaf: 1, subleaf: 0, reg: REG_ECX, bit: 28 }],
            xcr0_mask: 0x6,
            clone_priority: 65,
            cloneable: true,
        },
        IsaFeature {
            name: "fma",
            bits: &[
                CpuidBit { leaf: 1, subleaf: 0, reg: REG_ECX, bit: 12 },
                CpuidBit { leaf: 1, subleaf: 0, reg: REG_ECX, bit: 28 },
            ],
            xcr0_mask: 0x6,
            clone_priority: 75,
            cloneable: true,
        },
        IsaFeature {
            name: "avx2",
            bits: &[
                CpuidBit { leaf: 1, subleaf: 0, reg: REG_ECX, bit: 28 },
                CpuidBit { leaf: 7, subleaf: 0, reg: REG_EBX, bit: 5 },
            ],
            xcr0_mask: 0x6,
            clone_priority: 85,
            cloneable: true,
        },
        // AVX-VNNI 是 VEX 形式的 AVX 同类指令，与 `detect_x86()` 一样需要 AVX 位 +
        // XCR0[2:1]，不能只查一条位。位号本身在 **(7,1):EAX#4**（Linux word 12
        // `(12*32+4)` `X86_FEATURE_AVX_VNNI`；`cpuinfo.h` 读 `eax & bit_AVX_VNNI`，
        // 那个 `eax` 来自 `__get_cpuid_count(7,1,...)`）。**(7,0):ECX#4 是 OSPKE**，
        // 曾经写成它会让有 PKU 而无 AVX-VNNI 的机器假阳（本机即如此）。
        IsaFeature {
            name: "avx_vnni",
            bits: &[
                CpuidBit { leaf: 1, subleaf: 0, reg: REG_ECX, bit: 28 },
                CpuidBit { leaf: 7, subleaf: 1, reg: REG_EAX, bit: 4 },
            ],
            xcr0_mask: 0x6,
            clone_priority: 0,
            cloneable: false,
        },
        // XOP：位号在扩展叶 (0x80000001):ECX#11，但 gcc 把它关在 `avx_usable` 里
        // （`cpuinfo.h` 的 `if (avx_usable) { if (ecx & bit_XOP) ... }`），所以带 0x6。
        IsaFeature {
            name: "xop",
            bits: &[CpuidBit { leaf: 0x8000_0001, subleaf: 0, reg: REG_ECX, bit: 11 }],
            xcr0_mask: 0x6,
            clone_priority: 0,
            cloneable: false,
        },
        IsaFeature {
            name: "avx512vp2intersect",
            bits: &[CpuidBit { leaf: 7, subleaf: 0, reg: REG_EDX, bit: 8 }],
            xcr0_mask: 0xE6,
            clone_priority: 0,
            cloneable: false,
        },
        // AVX512_BF16 在 **(7,1):EAX#5**，且 gcc 只在 `avx512_usable` 下采纳它。
        // 这里故意**不**加 avx512f 那个 CPUID 位（与 `avx512!` 宏不同形）：`cpuinfo.h`
        // 的条件就是「XCR0 允许 5 个 AVX-512 状态位 + 本位」，多加一位会让分类表与
        // gcc 的 `FEATURE_AVX512BF16` 在「有 bf16 而无 f」的假想 CPU 上给出不同答案。
        IsaFeature {
            name: "avx512bf16",
            bits: &[CpuidBit { leaf: 7, subleaf: 1, reg: REG_EAX, bit: 5 }],
            xcr0_mask: 0xE6,
            clone_priority: 0,
            cloneable: false,
        },
        // AVX-512 系：只有 avx512f 能作 `target_clones` 的 ISA 名（gcc 13 实测），
        // 其余成员是 `__builtin_cpu_supports` 的扩展名，依赖 avx512f 位。
        avx512!("avx512f", REG_EBX, 16, 90, true),
        avx512!("avx512dq", REG_EBX, 17, 0, false),
        avx512!("avx512ifma", REG_EBX, 21, 0, false),
        avx512!("avx512pf", REG_EBX, 26, 0, false),
        avx512!("avx512er", REG_EBX, 27, 0, false),
        avx512!("avx512cd", REG_EBX, 28, 0, false),
        avx512!("avx512bw", REG_EBX, 30, 0, false),
        avx512!("avx512vl", REG_EBX, 31, 0, false),
        avx512!("avx512vbmi", REG_ECX, 1, 0, false),
        avx512!("avx512vbmi2", REG_ECX, 6, 0, false),
        avx512!("avx512vnni", REG_ECX, 11, 0, false),
        avx512!("avx512bitalg", REG_ECX, 12, 0, false),
        avx512!("avx512vpopcntdq", REG_ECX, 14, 0, false),
    ];

    // ---- 表行构造宏（`bit_feature!` / `f1ecx!` / `avx512!` 等）定义在
    // `IsaFeature` 之后、`FEATURES` 之前：`macro_rules!` 是文本作用域，必须先定义
    // 后使用。----

    /// 完整特性表（两段拼接后的视图）。
    pub fn all() -> impl Iterator<Item = &'static IsaFeature> {
        FEATURES.iter().chain(FEATURES_TAIL.iter())
    }

    /// 名字归一化：小写 + `_` → `.`。
    ///
    /// `target_clones` 的符号名里点号要换成下划线（`compute.sse4_2`，与 gcc 一致），
    /// 而源码/内置函数用的是 `sse4.2`；归一化让两个方向共用同一张表。
    pub fn normalize_name(name: &str) -> String {
        name.to_ascii_lowercase().replace('_', ".")
    }

    /// 按名字查规格（接受 `sse4_2` / `SSE4.2` 等写法）。
    ///
    /// 表里的名字都是小写 + 点号，所以只有输入含 `_` 或大写时才需要归一化。
    pub fn lookup(name: &str) -> Option<&'static IsaFeature> {
        if let Some(f) = all().find(|f| f.name == name) {
            return Some(f);
        }
        if name.bytes().any(|b| b == b'_' || b.is_ascii_uppercase()) {
            let key = normalize_name(name);
            return all().find(|f| f.name == key.as_str());
        }
        None
    }

    /// 该名字能否作为 `target_clones`/`target` 的 ISA 参数（gcc 接受集）。
    pub fn is_cloneable(name: &str) -> bool {
        lookup(name).is_some_and(|f| f.cloneable)
    }

    /// 已知可克隆 ISA 名（诊断信息用，按优先级升序）。
    pub fn cloneable_names() -> Vec<&'static str> {
        let mut v: Vec<&'static str> = all().filter(|f| f.cloneable).map(|f| f.name).collect();
        v.sort_by_key(|n| lookup(n).map(|f| f.clone_priority).unwrap_or(0));
        v
    }

    /// 某个 ISA 需要检测的 CPUID 位（未知名字返回空切片，调用方必须自己判
    /// `lookup(..).is_none()` 并报诊断，不得静默）。
    pub fn cpuid_bits(arch: &str) -> &'static [CpuidBit] {
        lookup(arch).map(|f| f.bits).unwrap_or(&[])
    }

    /// XCR0 低 32 位需求掩码（0 = 不需要 xgetbv 检查）。
    pub fn xcr0_mask(arch: &str) -> u32 {
        lookup(arch).map(|f| f.xcr0_mask).unwrap_or(0)
    }

    /// 是否需要做 XSAVE/XCR0 检查。
    pub fn needs_xsave(arch: &str) -> bool {
        xcr0_mask(arch) != 0
    }

    /// `target_clones` 变体判定优先级（越大越先测/越先选中）；未知名字返回 `i32::MIN`。
    pub fn dispatch_priority(arch: &str) -> i32 {
        lookup(arch).map(|f| f.clone_priority).unwrap_or(i32::MIN)
    }

    /// 按 gcc 的优先级把变体名排成「resolver 自上而下的测试顺序」。
    ///
    /// **声明顺序被完全忽略**（实测 gcc 13 对 `("sse4.2","avx")` 与
    /// `("avx","sse4.2")` 生成同一条 avx→sse4.2 判定链）。`default` 不在输入里，
    /// 由调用方作为兜底分支。排序稳定：优先级相同者保持输入顺序。
    pub fn dispatch_order<'a>(arches: &[&'a str]) -> Vec<&'a str> {
        let mut idx: Vec<(usize, i32)> = arches
            .iter()
            .enumerate()
            .map(|(i, a)| (i, dispatch_priority(a)))
            .collect();
        // 降序 + 同优先级按原序（稳定）：reverse 与 position 一起编码。
        idx.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        idx.into_iter().map(|(i, _)| arches[i]).collect()
    }

    /// 把 `(leaf, subleaf, reg)` 组合映射到运行时缓存 `__dcc_cpu_model` 的 dword 槽号。
    ///
    /// 槽布局同时被三处消费：codegen 合成的 CPUID 检测函数、resolver 体、
    /// `__builtin_cpu_supports`/`__builtin_cpu_is` 的读表序列。布局只在这里定义一次。
    pub const SLOT_LEAF0_EAX: usize = 0;
    pub const SLOT_VENDOR_BX: usize = 1;
    pub const SLOT_VENDOR_CX: usize = 2;
    pub const SLOT_VENDOR_DX: usize = 3;
    pub const SLOT_LEAF1_EAX: usize = 4;
    pub const SLOT_LEAF1_EBX: usize = 5;
    pub const SLOT_LEAF1_ECX: usize = 6;
    pub const SLOT_LEAF1_EDX: usize = 7;
    pub const SLOT_LEAF7_EBX: usize = 8;
    pub const SLOT_LEAF7_ECX: usize = 9;
    pub const SLOT_LEAF7_EDX: usize = 10;
    pub const SLOT_EXT_MAX_EAX: usize = 11;
    pub const SLOT_EXT1_ECX: usize = 12;
    pub const SLOT_EXT1_EDX: usize = 13;
    pub const SLOT_XCR0_LOW: usize = 14;
    pub const SLOT_VENDOR_KIND: usize = 15;
    pub const SLOT_INIT_DONE: usize = 16;
    // ---- 本轮为「`__builtin_cpu_is` 的型号名也是真实现」新增的槽 ----
    /// `(7,0):EAX` = leaf 7 的最大子 leaf 号。**只当门控用**：
    /// `cache_slot(7, 0, REG_EAX)` 故意返回 `None`，否则某位若被误写成
    /// 「(7,0) 的 EAX」就会读到最大值而不是特性字（与下面两个子 leaf 槽同形）。
    pub const SLOT_LEAF7_MAX: usize = 17;
    /// `(7,1):EAX` —— `avx512bf16` 等只在这一子 leaf。
    pub const SLOT_LEAF7S1_EAX: usize = 18;
    /// `(0xD,1):EAX` —— 紧凑 XSAVE 状态位（`xsaveopt`）。
    pub const SLOT_LEAFD1_EAX: usize = 19;
    /// `(0x8000_0008,0):EBX` —— `clzero` / `wbnoinvd`。
    pub const SLOT_EXT8_EBX: usize = 20;
    /// leaf1/EAX 解出来的**调整后** family（gcc `cpu_indicator_init` 同形：
    /// family==0xf 加扩展 family、family==6 加扩展 model）。
    pub const SLOT_CPU_FAMILY: usize = 21;
    /// leaf1/EAX 解出来的**调整后** model。
    pub const SLOT_CPU_MODEL: usize = 22;
    /// `CLASSIFY` 分类链写出的型号类型 id（`__builtin_cpu_is("corei7")` 比它）。
    pub const SLOT_CPU_TYPE: usize = 23;
    /// 分类链写出的子型号 id（`__builtin_cpu_is("rocketlake")` 比 type+subtype）。
    pub const SLOT_CPU_SUBTYPE: usize = 24;
    /// 缓存对象的 dword 个数（`__dcc_cpu_model` 大小 = `SLOT_WORDS * 4` 字节）。
    pub const SLOT_WORDS: usize = 25;
    /// 缓存对象字节大小（`.data` 里注册的尺寸）。
    pub const MODEL_BYTES: usize = SLOT_WORDS * 4;

    /// 一个检测动作的跳过条件。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Gate {
        /// 无条件执行。
        None,
        /// 仅当 `model[slot] >= min`（无符号比较）时才执行 —— CPUID 超过最大
        /// leaf 号时返回的是最高基本 leaf 的内容，不门控会当特性位读。
        LeafAtLeast { slot: usize, min: u32 },
        /// 仅当 leaf1/ECX 的 OSXSAVE(bit 27) 置位时才执行（xgetbv 会 #UD）。
        OSXSAVE,
    }

    /// 一条运行时检测动作：`__dcc_cpu_indicator_init` **逐条照着本表发射机器码**。
    ///
    /// 这是全项目唯一的「要查哪几个 leaf、结果存哪个槽」的描述：codegen 发的
    /// init 体与内置链接器旧路径拼的字节流都不得自己另列一份 leaf 清单。
    pub struct CpuidProbe {
        /// CPUID 输入 EAX；`XGETBV_LEAF` 表示发 `xgetbv`（ECX=0）。
        pub leaf: u32,
        /// CPUID 输入 ECX（子 leaf 号）。
        pub subleaf: u32,
        /// 跳过条件。
        pub gate: Gate,
        /// 四个结果寄存器各自落到哪个槽（`None` = 丢弃）。
        pub out_eax: Option<usize>,
        pub out_ebx: Option<usize>,
        pub out_ecx: Option<usize>,
        pub out_edx: Option<usize>,
    }

    /// `CpuidProbe::leaf` 的哨兵值：本条不是 CPUID而是 `xgetbv(%ecx=0)`。
    pub const XGETBV_LEAF: u32 = u32::MAX;

    /// 检测序列（按执行顺序）。覆盖 `cache_slot` 能映射的全部 `(leaf, subleaf, reg)`。
    pub static PROBES: &[CpuidProbe] = &[
        CpuidProbe {
            leaf: 0,
            subleaf: 0,
            gate: Gate::None,
            out_eax: Some(SLOT_LEAF0_EAX),
            out_ebx: Some(SLOT_VENDOR_BX),
            out_ecx: Some(SLOT_VENDOR_CX),
            out_edx: Some(SLOT_VENDOR_DX),
        },
        CpuidProbe {
            leaf: 1,
            subleaf: 0,
            gate: Gate::LeafAtLeast { slot: SLOT_LEAF0_EAX, min: 1 },
            out_eax: Some(SLOT_LEAF1_EAX),
            out_ebx: Some(SLOT_LEAF1_EBX),
            out_ecx: Some(SLOT_LEAF1_ECX),
            out_edx: Some(SLOT_LEAF1_EDX),
        },
        CpuidProbe {
            leaf: 7,
            subleaf: 0,
            gate: Gate::LeafAtLeast { slot: SLOT_LEAF0_EAX, min: 7 },
            // leaf 7 的 EAX 是「最大子 leaf 号」（本机 1）。它不是特性字（`cache_slot`
            // 对 (7,0):EAX 返回 None），但没有它就发不出 `(7,1)` 那一叶的门控 ——
            // gcc 在 `cpu_indicator_init` 里同样先 `__cpuid_count (7, 0, max_subleaf_level, ...)`
            // 再 `if (max_subleaf_level >= 1)`。`probe_gates_are_satisfiable` 钉「写它
            // 的探测必须早于读它的探测」。
            out_eax: Some(SLOT_LEAF7_MAX),
            out_ebx: Some(SLOT_LEAF7_EBX),
            out_ecx: Some(SLOT_LEAF7_ECX),
            out_edx: Some(SLOT_LEAF7_EDX),
        },
        CpuidProbe {
            // 子 leaf 1：超过最大子 leaf 号的 CPUID 会**返回子 leaf 0 的内容**（与
            // 「超过最大 leaf 号返回最高基本叶」同一条硬件行为）。所以这一叶必须
            // 用 `SLOT_LEAF7_MAX` 门控，否则 `avx512bf16` 会读到 (7,0):EAX 的 bit 5
            //（= PREFETCHWT1 那一侧的字段），得到一个看起来合理的假答案。
            leaf: 7,
            subleaf: 1,
            gate: Gate::LeafAtLeast { slot: SLOT_LEAF7_MAX, min: 1 },
            out_eax: Some(SLOT_LEAF7S1_EAX),
            out_ebx: None,
            out_ecx: None,
            out_edx: None,
        },
        CpuidProbe {
            // 紧凑 XSAVE 状态位（`xsaveopt`）。gcc 的门控就是「最大基本叶 >= 0xD」。
            leaf: 0xd,
            subleaf: 1,
            gate: Gate::LeafAtLeast { slot: SLOT_LEAF0_EAX, min: 0xd },
            out_eax: Some(SLOT_LEAFD1_EAX),
            out_ebx: None,
            out_ecx: None,
            out_edx: None,
        },
        CpuidProbe {
            leaf: 0x8000_0000,
            subleaf: 0,
            // 必须无门控：这一叶的 EAX 就是「扩展叶最大值」本身，而基本叶最大值
            // （`SLOT_LEAF0_EAX`，本机 0x20）永远不可能 >= 0x8000_0000 —— 拿它当门控就是
            // 永假，结果 `SLOT_EXT_MAX_EAX` 永远是 0，下一叶（0x8000_0001）也跟着不跑，
            // 于是 `lzcnt`/`abm`/`lm`/`nx`/`syscall`/`rdtscp`/`lahf_sahf`/`sse4a`/`3dnow*` 全体
            // 静默读 0（本轮由与 gcc 的逐名字对照才发现）。gcc 的 `cpu_indicator_init` 同样
            // 是无条件取 0x8000_0000。不变量由 `probe_gates_are_satisfiable` 钉住。
            gate: Gate::None,
            out_eax: Some(SLOT_EXT_MAX_EAX),
            out_ebx: None,
            out_ecx: None,
            out_edx: None,
        },
        CpuidProbe {
            leaf: 0x8000_0001,
            subleaf: 0,
            gate: Gate::LeafAtLeast { slot: SLOT_EXT_MAX_EAX, min: 0x8000_0001 },
            out_eax: None,
            out_ebx: None,
            out_ecx: Some(SLOT_EXT1_ECX),
            out_edx: Some(SLOT_EXT1_EDX),
        },
        CpuidProbe {
            // `clzero`（AMD znver 分类守卫）。同样靠扩展叶最大值门控。
            leaf: 0x8000_0008,
            subleaf: 0,
            gate: Gate::LeafAtLeast { slot: SLOT_EXT_MAX_EAX, min: 0x8000_0008 },
            out_eax: None,
            out_ebx: Some(SLOT_EXT8_EBX),
            out_ecx: None,
            out_edx: None,
        },
        CpuidProbe {
            leaf: XGETBV_LEAF,
            subleaf: 0,
            gate: Gate::OSXSAVE,
            out_eax: Some(SLOT_XCR0_LOW),
            out_ebx: None,
            out_ecx: None,
            out_edx: None,
        },
    ];

    /// leaf 0 厂商串的三个 dword。
    ///
    /// 厂商串的拼接顺序是 **EBX, EDX, ECX**（不是字母序，x86 的历史怪癖），所以
    /// “Intel” = EBX"Genu" + EDX"ineI" + ECX"ntel”，“AMD” = EBX"Auth" +
    /// EDX"enti" + ECX"cAMD"。值由本机实测钉下（见
    /// `docs/wp_ifunc_real_fix.md`：`cpuid(0)` 在 11th Gen Intel i5-11400 上报
    /// `ebx=756e6547 ecx=6c65746e edx=49656e69`）；写错任一 dword 的代价是
    /// `__builtin_cpu_is("intel")` 永远返回 0，而不报错。
    ///
    /// Centaur/Zhaoxin 两组常量不是“猜出来的另一个写法”：它们是从本机 libgcc 的
    /// `__cpu_indicator_init` 反汇编里逐字节取出的比较常量（`docs/wp_ifunc_real_fix.md`
    /// 记了取数过程），与 gcc `cpuinfo.h` 的 `get_zhaoxin_cpu` 一致。Zhaoxin 复用
    /// Centaur 厂商标（`VENDOR_CENTAUR`）但单独登记为 `VENDOR_ZHAOXIN`，因为 gcc 用
    /// `__cpu_vendor == CPUID_VENDOR_CENTAUR` + family/model 才落到 `lujiazui`。
    pub const INTEL_BX: u32 = 0x756e_6547; // "Genu"
    pub const INTEL_DX: u32 = 0x4965_6e69; // "ineI"
    pub const INTEL_CX: u32 = 0x6c65_746e; // "ntel"
    pub const AMD_BX: u32 = 0x6874_7541; // "Auth"
    pub const AMD_DX: u32 = 0x6974_6e65; // "enti"
    pub const AMD_CX: u32 = 0x444d_4163; // "cAMD"
    pub const CENTAUR_BX: u32 = 0x746e_6543; // "Cent"
    pub const CENTAUR_DX: u32 = 0x4872_7561; // "aurH"
    pub const CENTAUR_CX: u32 = 0x736c_7561; // "auls"
    pub const SHANGHAI_BX: u32 = 0x6853_2020; // "  Sh"
    pub const SHANGHAI_DX: u32 = 0x6867_6e61; // "angh"
    pub const SHANGHAI_CX: u32 = 0x2020_6961; // "ai  "

    /// `SLOT_VENDOR_KIND` 的取值（`.data` 初值全 0 ⇒ `VENDOR_UNKNOWN` = 「未识别」）。
    ///
    /// 编号由本表自己定义：dcc 的检测缓存自包含在 `.o` 里、不依赖 libgcc 的
    /// `__cpu_model`，所以不复用 gcc `cpuinfo.h` 的 `CPUID_VENDOR_*` 枚举值 —— 对外
    /// 可见的只是 `__builtin_cpu_is` 的答案，内部编号只要本表自洽即可。
    pub const VENDOR_UNKNOWN: u32 = 0;
    pub const VENDOR_INTEL: u32 = 1;
    pub const VENDOR_AMD: u32 = 2;
    pub const VENDOR_CENTAUR: u32 = 3;
    pub const VENDOR_ZHAOXIN: u32 = 4;

    /// 槽号：`CpuidBit` 落在缓存的哪个 dword。`None` = 本表未覆盖的 leaf/reg
    /// 组合（调用方必须报错，不得静默发一条指向槽 0 的检测）。
    pub fn cache_slot(leaf: u32, subleaf: u32, reg: u8) -> Option<usize> {
        match (leaf, subleaf, reg) {
            (0, 0, REG_EAX) => Some(SLOT_LEAF0_EAX),
            (0, 0, REG_EBX) => Some(SLOT_VENDOR_BX),
            (0, 0, REG_ECX) => Some(SLOT_VENDOR_CX),
            (0, 0, REG_EDX) => Some(SLOT_VENDOR_DX),
            (1, 0, REG_EAX) => Some(SLOT_LEAF1_EAX),
            (1, 0, REG_EBX) => Some(SLOT_LEAF1_EBX),
            (1, 0, REG_ECX) => Some(SLOT_LEAF1_ECX),
            (1, 0, REG_EDX) => Some(SLOT_LEAF1_EDX),
            (7, 0, REG_EAX) => None, // leaf 7 的 EAX 是最大子 leaf 号，不当特性字用
            (7, 0, REG_EBX) => Some(SLOT_LEAF7_EBX),
            (7, 0, REG_ECX) => Some(SLOT_LEAF7_ECX),
            (7, 0, REG_EDX) => Some(SLOT_LEAF7_EDX),
            (7, 1, REG_EAX) => Some(SLOT_LEAF7S1_EAX),
            // (7,1) 的 EBX/ECX/EDX 不采（gcc 也只读 EAX 与 EDX；本表只覆盖用到的）。
            (0xd, 1, REG_EAX) => Some(SLOT_LEAFD1_EAX),
            (0x8000_0000, 0, REG_EAX) => Some(SLOT_EXT_MAX_EAX),
            (0x8000_0001, 0, REG_ECX) => Some(SLOT_EXT1_ECX),
            (0x8000_0001, 0, REG_EDX) => Some(SLOT_EXT1_EDX),
            (0x8000_0008, 0, REG_EBX) => Some(SLOT_EXT8_EBX),
            _ => None,
        }
    }

    /// 本表的每一个 `(leaf, subleaf, reg)` 是否真的被 `PROBES` 填到相应槽。
    ///
    /// 担心“位表里有某个 leaf/子 leaf/reg 组合但检测序列不采”时，这个谓词就是
    /// 可证伪的接口（单元测试 `every_bit_is_covered_by_probe_sequence` 钉住它）。
    /// 子 leaf 必须参比：`(7,0):EAX` 与 `(7,1):EAX` 只有 `(leaf, reg)` 相同时会
    /// 把「根本没采子 leaf 1」误判成「已采」，于是 `avx512bf16` 静默读到子 leaf 0。
    pub fn probe_covers(leaf: u32, subleaf: u32, reg: u8) -> bool {
        let Some(slot) = cache_slot(leaf, subleaf, reg) else {
            return false;
        };
        PROBES.iter().any(|p| {
            p.leaf == leaf
                && p.subleaf == subleaf
                && match reg {
                    REG_EAX => p.out_eax == Some(slot),
                    REG_EBX => p.out_ebx == Some(slot),
                    REG_ECX => p.out_ecx == Some(slot),
                    _ => p.out_edx == Some(slot),
                }
        })
    }

    /// 一个 dword 槽在 `__dcc_cpu_model` 里的字节偏移。
    pub fn slot_offset(slot: usize) -> usize {
        slot * 4
    }

    /// OSXSAVE 门控位的**唯一出口**：`(leaf, subleaf, 结果寄存器, 位掩码)`，取自 `FEATURES["osxsave"]`。
    ///
    /// 存在的唯一理由：两个发射器都需要发「`test …, 1<<bit` / `je skip`」，而那位号
    /// （leaf1/ECX#27）**已经在 `FEATURES` 里了**（`f1ecx!("osxsave", 27, ...)`)。
    /// 如果在发射器里再写一次 `1 << 27`，位表就出现了第二份真源 —— 改表时发射器
    /// 不会跟着改。表里没有该位（或被删了）就返回 `None`，调用方必须报诊断，
    /// 不得退回硬编码常量。
    ///
    /// `subleaf` 在本轮加入：它同时是 `cache_slot`/`probe_covers` 的必需参数，
    /// 少一个参数就只能靠猜子叶号。
    pub fn osxsave_bit() -> Option<(u32, u32, u8, u32)> {
        let b = lookup("osxsave")?.bits.first()?;
        Some((b.leaf, b.subleaf, b.reg, 1u32 << b.bit))
    }

    /// codegen 侧（读运行时缓存槽）的 OSXSAVE 门控：`(槽号, 位掩码)`。
    ///
    /// 额外要求 `probe_covers`：槽必须由 `PROBES` 真填到，否则读到的是 `.data` 的 0。
    pub fn osxsave_test() -> Option<(usize, u32)> {
        let (leaf, subleaf, reg, mask) = osxsave_bit()?;
        if !probe_covers(leaf, subleaf, reg) {
            return None;
        }
        Some((cache_slot(leaf, subleaf, reg)?, mask))
    }

    /// 覆盖 `bits` 全部位所需的「槽 → 位掩码」聚合，按槽号升序输出。
    ///
    /// resolver 体与 `__builtin_cpu_supports` 的读表序列都由它生成 —— 这样
    /// 「一次检测、两处消费」是真的同一份数据。
    ///
    /// 返回 `None` = 该名字的某一位没有对应槽（表写错了）。调用方必须报
    /// 诊断，不得静默发一条「永远不中」的检测。
    /// 一个特性名的全部位必须**逐个**置位（不是「一个聚合掩码非零」），所以这里
    /// 按槽聚合后返回多位掩码，调用方逐槽发一条 `test`；同槽多位聚成一条是正确的
    /// （它们本来就属于同一个 dword）。
    pub fn slot_masks(arch: &str) -> Option<Vec<(usize, u32)>> {
        let mut out: Vec<(usize, u32)> = Vec::new();
        for b in cpuid_bits(arch) {
            let Some(slot) = cache_slot(b.leaf, b.subleaf, b.reg) else {
                return None;
            };
            let mask = 1u32 << b.bit;
            if let Some(e) = out.iter_mut().find(|(s, _)| *s == slot) {
                e.1 |= mask;
            } else {
                out.push((slot, mask));
            }
        }
        out.sort_by_key(|(s, _)| *s);
        Some(out)
    }

    /// 厂商 dword 三元组 → kind 的**表**。
    ///
    /// 发射侧（`Codegen::emit_vendor_kind` 逐条比三个 dword）与参考求值器
    /// ([`vendor_kind`]) 都只读这一张表：厂商串的判定形状（比几个 dword）如果在两边
    /// 各写一份，就会出现「机器码认不出、参考求值器认得出」的飘（分类链因此
    /// 走与发射不同的厂商分支，`__builtin_cpu_is` 的答案与产物不一致）。
    /// 常量本身仍是上面那几个 dword，本表不重新拼字符串。
    #[derive(Debug, Clone, Copy)]
    pub struct VendorSig {
        pub bx: u32,
        pub dx: u32,
        pub cx: u32,
        pub kind: u32,
    }

    pub static VENDOR_SIGS: &[VendorSig] = &[
        VendorSig { bx: INTEL_BX, dx: INTEL_DX, cx: INTEL_CX, kind: VENDOR_INTEL },
        VendorSig { bx: AMD_BX, dx: AMD_DX, cx: AMD_CX, kind: VENDOR_AMD },
        VendorSig { bx: CENTAUR_BX, dx: CENTAUR_DX, cx: CENTAUR_CX, kind: VENDOR_CENTAUR },
        VendorSig { bx: SHANGHAI_BX, dx: SHANGHAI_DX, cx: SHANGHAI_CX, kind: VENDOR_ZHAOXIN },
    ];

    /// leaf 0 的三个 dword → 厂商 kind（无匹配 = `VENDOR_UNKNOWN`）。
    pub fn vendor_kind(bx: u32, dx: u32, cx: u32) -> u32 {
        VENDOR_SIGS
            .iter()
            .find(|v| v.bx == bx && v.dx == dx && v.cx == cx)
            .map(|v| v.kind)
            .unwrap_or(VENDOR_UNKNOWN)
    }

    /// 型号名里能由**厂商 kind** 直接回答的部分：返回该名字要比的 kind 值。
    ///
    /// 微架构型号名（`atom`/`core2`/`corei7`/`haswell`/`rocketlake`/`znver1`...）
    /// 不在这里：它们比较 `SLOT_CPU_TYPE`/`SLOT_CPU_SUBTYPE`，值由 [`CLASSIFY`]
    /// 的分类链写入。两者都由 [`cpu_is_match`] 一个入口给出，调用方不需要区分。
    pub fn vendor_kind_value(name: &str) -> Option<u32> {
        match cpu_is_match(name)? {
            CpuIsMatch::Vendor(k) => Some(*k),
            _ => None,
        }
    }

    /// 参考求值器：按本表的规格在**当前宿主 CPU** 上直接判定一个特性是否可用。
    ///
    /// 存在的目的：给单元测试提供一条与 `src/cpuid.rs::detect_x86()` 可比对的
    /// 语义通路（`table_agrees_with_host_detector`）。发到目标代码里的机器码走
    /// `PROBES` + `slot_masks` + `xcr0_mask`，与本函数读的是**同一张表**，所以这个
    /// 比对同时候验了「表的语义」与「表驱动的发射」是一致的。
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    pub fn host_supported(name: &str) -> Option<bool> {
        use std::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};
        let f = lookup(name)?;
        let max_leaf = __cpuid(0).eax;
        // 扩展叶最大值必须**无条件**取：基本叶最大值永远 < 0x8000_0000，拿它做门控
        // 就是永假（与 `PROBES` 里 leaf 0x8000_0000 那条同一类错 —— 写错时本函数对
        // `lzcnt`/`abm`/`lm`/`nx`/`syscall`/`rdtscp`/`sse4a`/`3dnow*` 全部返回 Some(false)，
        // 而发出的机器码靠 `PROBES` 是对的，于是「参考求值器」与机器码飘开而无人报错。
        // 钉住它的断言是 `host_reference_reads_extended_leaves`。）
        let ext_max = __cpuid(0x8000_0000).eax;
        for b in f.bits {
            // 超过最大 leaf 号的 CPUID 返回最高基本 leaf 的内容，必须先门控。
            let avail = if b.leaf <= 0x4000_0000 {
                max_leaf >= b.leaf
            } else {
                ext_max >= b.leaf
            };
            if !avail {
                return Some(false);
            }
            // 子 leaf 也要门控：超过最大子叶号的 CPUID 返回**子叶 0** 的内容（与
            // 「超过最大 leaf 号返回最高基本叶」是同一条硬件行为）。不门控就会把
            // `(7,0):EAX`（最大子叶号）的字段当成 `(7,1):EAX` 的特性位读 —— 那是
            // 一个看起来合理的假答案，而 `PROBES` 里的机器码靠 `SLOT_LEAF7_MAX`
            // 门控是对的，两边就这样飘开了。钉住它的断言是
            // `host_reference_agrees_with_emitted_probe_gating`。
            if b.leaf == 7 && b.subleaf > 0 && __cpuid_count(7, 0).eax < b.subleaf {
                return Some(false);
            }
            let v = __cpuid_count(b.leaf, b.subleaf);
            let reg = match b.reg {
                REG_EAX => v.eax,
                REG_EBX => v.ebx,
                REG_ECX => v.ecx,
                _ => v.edx,
            };
            if reg & (1u32 << b.bit) == 0 {
                return Some(false);
            }
        }
        if f.xcr0_mask != 0 {
            // xgetbv 只在 OSXSAVE 置位时可用，否则 #UD。位号从表里取（`osxsave_bit()`），
            // 不在这里重抄 `1 << 27`：那会出现第二份真源（改表时参考求值器不跟着改）。
            let (oleaf, osub, oreg, omask) = osxsave_bit()?;
            let ov = __cpuid_count(oleaf, osub);
            let octx = match oreg {
                REG_EAX => ov.eax,
                REG_EBX => ov.ebx,
                REG_ECX => ov.ecx,
                _ => ov.edx,
            };
            if octx & omask == 0 {
                return Some(false);
            }
            let xcr0 = unsafe { _xgetbv(0) } as u32;
            if xcr0 & f.xcr0_mask != f.xcr0_mask {
                return Some(false);
            }
        }
        Some(true)
    }

    // ====================================================================
    // 型号分类：`__builtin_cpu_is("<微架构名>")` 的单一真源
    // ====================================================================
    //
    // 本节存在的理由：`__builtin_cpu_is` 的**微架构型号名**（`corei7`/`rocketlake`/
    // `znver3`/`atom`...）不是 CPUID 特性位，而是「leaf1/EAX 解出的 family+model
    // 落在哪个区间，外加哪几个特性位当守卫」的**分类结果**。gcc 把它写在
    // `libgcc/config/i386/cpuinfo.h` 的 `get_intel_cpu`/`get_amd_cpu`/`get_zhaoxin_cpu`
    // 三个函数里（`switch (cpu_model) case 0x1c: ... cpu_model->__cpu_type = ...`），
    // 编译器侧的对外语义在 `gcc/common/config/i386/i386-common.cc` 的 `processor_names`
    // 表（每项 `M_CPU_TYPE(x)` / `M_CPU_SUBTYPE(x)` / `M_VENDOR(x)`）：**每个名字只比较
    // 一个 dword**。下面两张表就是这两处的 dcc 版本；`__dcc_cpu_indicator_init` 发的
    // 分类链与参考求值器 `host_processor_class()` 都只读这里，不另写一份。

    /// family/model 谓词（对**调整后**的值，即 [`DECODE_RULES`] 写进槽的那些）。
    ///
    /// 只列 gcc 那三个函数里真的出现过的形状：`case X:`（`Eq`/`In`）、
    /// `if (model <= X)`（`Le`）、`if (model >= X)`（`Ge`）、`model >= a && model <= b`
    /// （`Range`）、以及特性守卫前的兵底（`Any`）。没有“集合取反”一类形状，
    /// 因为 gcc 没有。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MPred {
        /// 恒真（守卫链的兵底，必须放在同组最后）。
        Any,
        Eq(u32),
        /// 多值 `case`（同一个类占几个 model）。
        In(&'static [u32]),
        /// 无符号 `<=`（gcc 的 `if (model <= 0x2f)`）。
        Le(u32),
        /// 无符号 `>=`。
        Ge(u32),
        /// `lo <= v <= hi`（闭区间，`lo <= hi` 由不变量测试钉住）。
        Range { lo: u32, hi: u32 },
    }

    impl MPred {
        /// 参考求值：`v` 是否命中本谓词（全部无符号比较，与 gcc 的 `unsigned int` 同形）。
        pub fn holds(self, v: u32) -> bool {
            match self {
                MPred::Any => true,
                MPred::Eq(x) => v == x,
                MPred::In(list) => list.contains(&v),
                MPred::Le(hi) => v <= hi,
                MPred::Ge(lo) => v >= lo,
                MPred::Range { lo, hi } => v >= lo && v <= hi,
            }
        }
    }

    /// 分类规则的额外 CPUID 守卫。
    ///
    /// **只能引用特性名，不能写位号**：守卫的位号由调用方经由
    /// `slot_masks(name)` 从上面那张位表取（发射侧就是
    /// `Codegen::emit_feature_check(name, fail)`），所以「znver1/znver2 靠 CLZERO/CLWB
    /// 区分」这类事实只有一份。名字在本表里不存在时发射侧会报内部错（E1126），
    /// 不变量测试 `classify_guards_exist_in_feature_table` 在编译期先把它扫一遍。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Guard {
        None,
        Has(&'static str),
    }

    /// 一条型号规则（命中即写 `SLOT_CPU_TYPE`/`SLOT_CPU_SUBTYPE` 并结束整条链）。
    ///
    /// 两个字段的 `""` = “不写”：类型已由组级写入（AMD 的 family 组）或根本没有
    /// 名字能观察到的类型（Zhaoxin，见 `CLASSIFY` 里的注释）。不是“写 0”：
    /// 缓存初值为 0 且组级写入已经发生。
    #[derive(Debug, Clone, Copy)]
    pub struct ClassRule {
        pub model: MPred,
        pub guard: Guard,
        pub cpu_type: &'static str,
        pub cpu_subtype: &'static str,
    }

    /// 一个 (厂商, family) 组：先进组再逐规则，与 gcc 的 `switch (family)` 同构。
    pub struct ClassGroup {
        pub vendor: u32,
        pub family: MPred,
        /// 进组就写的类型（gcc 在 `case` 顶部先 `__cpu_type = ...` 再定 subtype）。
        pub group_type: &'static str,
        /// 按 gcc 判定顺序排列的规则；**首条命中即止**。
        pub rules: &'static [ClassRule],
    }

    /// 分类表。逐条对应 gcc 13.3 `cpuinfo.h`（`/tmp/cpuinfo_h_gcc13.h` 的行号在
    /// `docs/wp_ifunc_real_fix.md` 里记了）。
    pub static CLASSIFY: &[ClassGroup] = &[
        // ---- Intel family 6 ----
        // gcc 的 `get_intel_cpu` 开头就是 `if (__cpu_family != 0x6) return NULL;`，
        // 所以下面这一大组只可能从 family==6 进（本表用组级 `family: Eq(6)` 同形）。
        // 无组级类型：Intel 的 `__cpu_type` 是每个 `case` 自己写的。
        ClassGroup {
            vendor: VENDOR_INTEL,
            family: MPred::Eq(0x06),
            group_type: "",
            rules: &[
                ClassRule { model: MPred::In(&[0x1c, 0x26]), guard: Guard::None, cpu_type: "bonnell", cpu_subtype: "" },
                // gcc 把 Silvermont（0x37/0x4a/0x4d/0x5d）与 Airmont（0x4c/0x5a/0x75）
                // 归成同一个 `INTEL_SILVERMONT`（Airmont 只是注释，不单独注册）。
                ClassRule { model: MPred::In(&[0x37, 0x4a, 0x4d, 0x5d, 0x4c, 0x5a, 0x75]), guard: Guard::None, cpu_type: "silvermont", cpu_subtype: "" },
                ClassRule { model: MPred::In(&[0x5c, 0x5f]), guard: Guard::None, cpu_type: "goldmont", cpu_subtype: "" },
                ClassRule { model: MPred::Eq(0x7a), guard: Guard::None, cpu_type: "goldmont-plus", cpu_subtype: "" },
                ClassRule { model: MPred::In(&[0x86, 0x96, 0x9c]), guard: Guard::None, cpu_type: "tremont", cpu_subtype: "" },
                ClassRule { model: MPred::Eq(0x57), guard: Guard::None, cpu_type: "knl", cpu_subtype: "" },
                ClassRule { model: MPred::Eq(0x85), guard: Guard::None, cpu_type: "knm", cpu_subtype: "" },
                // 以下都是 `INTEL_COREI7` + 一个 subtype（gcc 每行都先
                // `CHECK___builtin_cpu_is("corei7")` 再查子型名 —— 两个名字分别比较
                // `__cpu_type` 与 `__cpu_subtype`，与本表同形）。
                ClassRule { model: MPred::In(&[0x1a, 0x1e, 0x1f, 0x2e]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "nehalem" },
                ClassRule { model: MPred::In(&[0x25, 0x2c, 0x2f]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "westmere" },
                ClassRule { model: MPred::In(&[0x2a, 0x2d]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "sandybridge" },
                ClassRule { model: MPred::In(&[0x3a, 0x3e]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "ivybridge" },
                ClassRule { model: MPred::In(&[0x3c, 0x3f, 0x45, 0x46]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "haswell" },
                ClassRule { model: MPred::In(&[0x3d, 0x47, 0x4f, 0x56]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "broadwell" },
                // Skylake(0x4e/0x5e) / Kaby Lake(0x8e/0x9e) / Comet Lake(0xa5/0xa6) 共用
                // 一个 `INTEL_COREI7_SKYLAKE`（gcc 13.3 就是这么归的）。
                ClassRule { model: MPred::In(&[0x4e, 0x5e, 0x8e, 0x9e, 0xa5, 0xa6]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "skylake" },
                ClassRule { model: MPred::Eq(0xa7), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "rocketlake" },
                // model 0x55 一个 `case` 三个去向（守卫链，顺序就是 gcc 的 if 链）：
                // avx512bf16 → Cooper Lake，否且 avx512vnni → Cascade Lake，
                // 否则「有 AVX-512 的 Skylake」。三条都写同一个类型 corei7。
                ClassRule { model: MPred::Eq(0x55), guard: Guard::Has("avx512bf16"), cpu_type: "corei7", cpu_subtype: "cooperlake" },
                ClassRule { model: MPred::Eq(0x55), guard: Guard::Has("avx512vnni"), cpu_type: "corei7", cpu_subtype: "cascadelake" },
                ClassRule { model: MPred::Eq(0x55), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "skylake-avx512" },
                ClassRule { model: MPred::Eq(0x66), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "cannonlake" },
                ClassRule { model: MPred::In(&[0x6a, 0x6c]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "icelake-server" },
                ClassRule { model: MPred::In(&[0x7e, 0x7d, 0x9d]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "icelake-client" },
                ClassRule { model: MPred::In(&[0x8c, 0x8d]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "tigerlake" },
                // Alder Lake 一族（含 Raptor Lake / Meteor Lake 的 model）在 gcc 13.3
                // 里全部归到 `INTEL_COREI7_ALDERLAKE`。
                ClassRule { model: MPred::In(&[0xbe, 0x97, 0x9a, 0xb7, 0xba, 0xbf, 0xaa, 0xac]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "alderlake" },
                ClassRule { model: MPred::In(&[0x8f, 0xcf]), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "sapphirerapids" },
                // Sierra Forest / Grand Ridge 是**只有类型没有子型**的 E-core/P-cores 新类。
                ClassRule { model: MPred::Eq(0xaf), guard: Guard::None, cpu_type: "sierraforest", cpu_subtype: "" },
                ClassRule { model: MPred::Eq(0xad), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "graniterapids" },
                ClassRule { model: MPred::Eq(0xae), guard: Guard::None, cpu_type: "corei7", cpu_subtype: "graniterapids-d" },
                ClassRule { model: MPred::Eq(0xb6), guard: Guard::None, cpu_type: "grandridge", cpu_subtype: "" },
                // Penryn(0x17/0x1d) + Merom(0x0f) → `INTEL_CORE2`。
                ClassRule { model: MPred::In(&[0x17, 0x1d, 0x0f]), guard: Guard::None, cpu_type: "core2", cpu_subtype: "" },
            ],
        },
        // ---- AMD family 10h ----
        ClassGroup {
            vendor: VENDOR_AMD,
            family: MPred::Eq(0x10),
            group_type: "amdfam10h",
            rules: &[
                ClassRule { model: MPred::Eq(0x2), guard: Guard::None, cpu_type: "", cpu_subtype: "barcelona" },
                ClassRule { model: MPred::Eq(0x4), guard: Guard::None, cpu_type: "", cpu_subtype: "shanghai" },
                ClassRule { model: MPred::Eq(0x8), guard: Guard::None, cpu_type: "", cpu_subtype: "istanbul" },
            ],
        },
        // ---- AMD family 14h / 16h：只有类型 ----
        ClassGroup {
            vendor: VENDOR_AMD,
            family: MPred::Eq(0x14),
            group_type: "btver1",
            rules: &[],
        },
        ClassGroup {
            vendor: VENDOR_AMD,
            family: MPred::Eq(0x16),
            group_type: "btver2",
            rules: &[],
        },
        // ---- AMD family 15h (Bulldozer) ----
        // 进组先写 `amdfam15h`（gcc 在 `case 0x15:` 顶部就写），然面再定子型；
        // 一个都没中时子型保持 0，而 `__builtin_cpu_is("amdfam15h")` 仍为真 ——
        // 与 gcc 同形。
        ClassGroup {
            vendor: VENDOR_AMD,
            family: MPred::Eq(0x15),
            group_type: "amdfam15h",
            rules: &[
                ClassRule { model: MPred::Eq(0x2), guard: Guard::None, cpu_type: "", cpu_subtype: "bdver2" },
                ClassRule { model: MPred::Le(0x0f), guard: Guard::None, cpu_type: "", cpu_subtype: "bdver1" },
                ClassRule { model: MPred::Le(0x2f), guard: Guard::None, cpu_type: "", cpu_subtype: "bdver2" },
                ClassRule { model: MPred::Le(0x4f), guard: Guard::None, cpu_type: "", cpu_subtype: "bdver3" },
                ClassRule { model: MPred::Le(0x7f), guard: Guard::None, cpu_type: "", cpu_subtype: "bdver4" },
                // model 落在 0x80+ 时 gcc 改用特性位区分（下面四条 `Any` + 守卫）。
                ClassRule { model: MPred::Any, guard: Guard::Has("avx2"), cpu_type: "", cpu_subtype: "bdver4" },
                ClassRule { model: MPred::Any, guard: Guard::Has("xsaveopt"), cpu_type: "", cpu_subtype: "bdver3" },
                ClassRule { model: MPred::Any, guard: Guard::Has("bmi"), cpu_type: "", cpu_subtype: "bdver2" },
                ClassRule { model: MPred::Any, guard: Guard::Has("xop"), cpu_type: "", cpu_subtype: "bdver1" },
            ],
        },
        // ---- AMD family 17h (Zen1/2) ----
        ClassGroup {
            vendor: VENDOR_AMD,
            family: MPred::Eq(0x17),
            group_type: "amdfam17h",
            rules: &[
                ClassRule { model: MPred::Le(0x1f), guard: Guard::None, cpu_type: "", cpu_subtype: "znver1" },
                ClassRule { model: MPred::Ge(0x30), guard: Guard::None, cpu_type: "", cpu_subtype: "znver2" },
                // model 0x20..0x2f 靠 CLWB/CLZERO 区分（本轮把 clwb 从错的 EBX#30
                // 改成 EBX#24，这条守卫才真的是 CLWB）。
                ClassRule { model: MPred::Any, guard: Guard::Has("clwb"), cpu_type: "", cpu_subtype: "znver2" },
                ClassRule { model: MPred::Any, guard: Guard::Has("clzero"), cpu_type: "", cpu_subtype: "znver1" },
            ],
        },
        // ---- AMD family 19h (Zen3/4) ----
        ClassGroup {
            vendor: VENDOR_AMD,
            family: MPred::Eq(0x19),
            group_type: "amdfam19h",
            rules: &[
                ClassRule { model: MPred::Le(0x0f), guard: Guard::None, cpu_type: "", cpu_subtype: "znver3" },
                ClassRule { model: MPred::Range { lo: 0x10, hi: 0x1f }, guard: Guard::None, cpu_type: "", cpu_subtype: "znver4" },
                ClassRule { model: MPred::Range { lo: 0x60, hi: 0xaf }, guard: Guard::None, cpu_type: "", cpu_subtype: "znver4" },
                ClassRule { model: MPred::Any, guard: Guard::Has("avx512f"), cpu_type: "", cpu_subtype: "znver4" },
                ClassRule { model: MPred::Any, guard: Guard::Has("vaes"), cpu_type: "", cpu_subtype: "znver3" },
            ],
        },
        // 本表**故意没有** AMD family 1Ah（znver5）那组。不是“还没做”：gcc 13.3 的
        // 对外名字表里没有 `znver5`/`amdfam1ah` 两个名字（实测：两者都
        // `__builtin_cpu_is` 编译失败），所以没有任何名字能观察到那个分类结果；
        // 而 `classify_every_id_is_addressable` 不允许表里存在“没有名字能问到”的
        // id。要支持 gcc≥14 的名字集时，必须同时往 `CPU_IS_NAMES` 里加那两个名字。
        // ---- Zhaoxin（包含以 Centaur 厂商串注册的型号）----
        // gcc：`vendor == CENTAUR && family < 7` → 只记 VENDOR_CENTAUR（不做分类）；
        // 否则（SHANGHAI 串，或 CENTAUR 串且 family>=7）走 `get_zhaoxin_cpu`。
        ClassGroup {
            vendor: VENDOR_ZHAOXIN,
            family: MPred::Eq(0x07),
            // 无组级类型：`ZHAOXIN_FAM7H` 在 gcc 13.3 的对外名字表里**没有任何名字**
            // 比较它（`centaur`/`zhaoxin`/`lujiazui` 中只有 `lujiazui` 被接受，而它是
            // subtype 名）；写它就会造出一个“发得出但问不到”的 id，被
            // `classify_every_id_is_addressable` 拒掉。行为上与 gcc 一致（差异不可观察）。
            group_type: "",
            rules: &[
                // 注意 gcc 在这一支里还会 `reset_cpu_feature (FEATURE_AVX/F16C)`（luJiaZui
                // 报了 AVX 但不真的执行 VEX 指令）。dcc 的分类链只写 type/subtype，
                // **没有**「分类结果反向清除特性位」这个机制，所以在这类 CPU 上
                // `__builtin_cpu_supports("avx")` 会与 gcc 飘（本机观察不到：不是
                // Zhaoxin）。已记进 `docs/wp_ifunc_real_fix.md` 的未闭合清单。
                ClassRule { model: MPred::Eq(0x3b), guard: Guard::None, cpu_type: "", cpu_subtype: "lujiazui" },
            ],
        },
    ];

    /// family/model 的**厂商相关调整**（与 [`CLASSIFY`] 分开：前者写槽、后者读槽）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Adjust {
        /// `family += (eax >> 20) & 0xff`。
        AddExtFamily,
        /// `model += (eax >> 12) & 0xf0`。
        AddExtModel,
    }

    /// 一条调整规则：同一 `kind` 内**首条命中即止**（gcc 的 `if/else if` 同形）。
    #[derive(Debug, Clone, Copy)]
    pub struct DecodeRule {
        pub kind: u32,
        pub family_eq: u32,
        pub ops: &'static [Adjust],
    }

    /// 逐条对应 gcc 13.3 `cpu_indicator_init`（同一个文件的 1047-1105 行）。
    pub static DECODE_RULES: &[DecodeRule] = &[
        DecodeRule { kind: VENDOR_INTEL, family_eq: 0x0f, ops: &[Adjust::AddExtFamily, Adjust::AddExtModel] },
        DecodeRule { kind: VENDOR_INTEL, family_eq: 0x06, ops: &[Adjust::AddExtModel] },
        DecodeRule { kind: VENDOR_AMD, family_eq: 0x0f, ops: &[Adjust::AddExtFamily, Adjust::AddExtModel] },
        // Centaur 串且 family<7 的机器不会走到分类（`KIND_PROMOTES` 不提升），
        // 但它的 model 调整与 Zhaoxin 同形，所以两条都列。
        DecodeRule { kind: VENDOR_CENTAUR, family_eq: 0x07, ops: &[Adjust::AddExtModel] },
        DecodeRule { kind: VENDOR_ZHAOXIN, family_eq: 0x07, ops: &[Adjust::AddExtModel] },
    ];

    /// 厂商 kind 的升级规则（gcc：Centaur 串但 family>=7 就不是“老 Centaur”而是 Zhaoxin）。
    #[derive(Debug, Clone, Copy)]
    pub struct KindPromote {
        pub from: u32,
        pub family_min: u32,
        pub to: u32,
    }

    /// 与 gcc 的 `signature_CENTAUR_ebx && family < 0x07` 分支同一事实的单一定义处。
    pub static KIND_PROMOTES: &[KindPromote] = &[KindPromote {
        from: VENDOR_CENTAUR,
        family_min: 7,
        to: VENDOR_ZHAOXIN,
    }];

    /// leaf1/EAX 的四个字段（gcc 的 `GET_MODEL`/`GET_FAMILY`/... 宏）。
    ///
    /// 发射侧与参考求值器共用这一组位运算（只定义一次，否则两边的 family 会飘）。
    pub const RAW_MODEL_LOW_SHIFT: u32 = 4;
    pub const RAW_MODEL_LOW_MASK: u32 = 0x0f;
    pub const RAW_FAMILY_LOW_SHIFT: u32 = 8;
    pub const RAW_FAMILY_LOW_MASK: u32 = 0x0f;
    pub const RAW_EXT_MODEL_SHIFT: u32 = 12;
    pub const RAW_EXT_MODEL_MASK: u32 = 0xf0;
    pub const RAW_EXT_FAMILY_SHIFT: u32 = 20;
    pub const RAW_EXT_FAMILY_MASK: u32 = 0xff;

    /// 从原始 leaf1/EAX 解出 `(family, model)`（先应用 [`DECODE_RULES`]）。
    pub fn decode_family_model(eax: u32, kind: u32) -> (u32, u32) {
        let mut family = (eax >> RAW_FAMILY_LOW_SHIFT) & RAW_FAMILY_LOW_MASK;
        let mut model = (eax >> RAW_MODEL_LOW_SHIFT) & RAW_MODEL_LOW_MASK;
        let ext_model = (eax >> RAW_EXT_MODEL_SHIFT) & RAW_EXT_MODEL_MASK;
        let ext_family = (eax >> RAW_EXT_FAMILY_SHIFT) & RAW_EXT_FAMILY_MASK;
        // 同一 kind 内首条命中即止：gcc 的 `if (family == 0xf) {...} else if (family == 6) ...`
        // 就是“拿**未调整**的 family 比、只走一支”，所以这里必须 `break` 而不能累加。
        for r in DECODE_RULES.iter().filter(|r| r.kind == kind) {
            if r.family_eq == family {
                for op in r.ops {
                    match op {
                        Adjust::AddExtFamily => family += ext_family,
                        Adjust::AddExtModel => model += ext_model,
                    }
                }
                break;
            }
        }
        (family, model)
    }

    /// 应用 [`KIND_PROMOTES`]（必须在 `decode_family_model` 之后，因为条件读的是调整后 family）。
    pub fn promote_vendor_kind(kind: u32, family: u32) -> u32 {
        for p in KIND_PROMOTES {
            if p.from == kind && family >= p.family_min {
                return p.to;
            }
        }
        kind
    }

    /// 厂商 BX dword → kind（**只比 BX**，与 gcc `cpu_indicator_init` 的
    /// `vendor == signature_*_ebx` 同形）。发射侧比的是三个 dword（见
    /// [`VENDOR_SIGS`]），差别只在「BX 相同但 DX/CX 不同的仿冒厂商串」上 ——
    /// 那种机器在 dcc 下是 `VENDOR_UNKNOWN`（什么都不分类），在 gcc 下会落到某一
    /// 厂商分支。本函数不参与发射，只给「按 BX 查 kind」的调用方用。
    pub fn vendor_kind_of_bx(bx: u32) -> u32 {
        VENDOR_SIGS
            .iter()
            .find(|v| v.bx == bx)
            .map(|v| v.kind)
            .unwrap_or(VENDOR_UNKNOWN)
    }

    /// 类型 id：**按首次出现在 [`CLASSIFY`] 里的顺序编号**，0 = 未分类。
    ///
    /// 为什么不手写一张「名字 → 数字」的表：那张表就是第二份真源，改分类表时
    /// 它不会跟着改（典型后果：某个子型名的 id 与写入端不一致，名字永远返回 0
    /// 而不报错）。这里 id = 表中位置，写入端（分类链）与比较端（`__builtin_cpu_is`
    /// 的立即数）都走同一个函数，因此只能一致。
    pub fn type_id(name: &str) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let mut idx = 1u32;
        for g in CLASSIFY {
            if !g.group_type.is_empty() {
                if g.group_type == name {
                    return Some(idx);
                }
                idx += 1;
            }
            for r in g.rules {
                if !r.cpu_type.is_empty() {
                    if r.cpu_type == name {
                        return Some(idx);
                    }
                    idx += 1;
                }
            }
        }
        None
    }

    /// 子型号 id，与 [`type_id`] 同形（独立编号空间：两个 id 分别比较两个槽）。
    pub fn subtype_id(name: &str) -> Option<u32> {
        if name.is_empty() {
            return None;
        }
        let mut idx = 1u32;
        for g in CLASSIFY {
            for r in g.rules {
                if !r.cpu_subtype.is_empty() {
                    if r.cpu_subtype == name {
                        return Some(idx);
                    }
                    idx += 1;
                }
            }
        }
        None
    }

    /// `__builtin_cpu_is("<n>")` 比较的是哪个槽。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CpuIsMatch {
        /// 比 `SLOT_VENDOR_KIND`。
        Vendor(u32),
        /// 比 `SLOT_CPU_TYPE`（值是 [`type_id`]）。
        Type(&'static str),
        /// 比 `SLOT_CPU_SUBTYPE`（值是 [`subtype_id`]）。
        Subtype(&'static str),
    }

    /// gcc 13.3 实际接受的 51 个 `__builtin_cpu_is` 名字（逐个实测：其余写法如
    /// `athlon`/`pentium4`/`k6*`/`amdfam10`/`znver5`/`amdfam1ah`/`generic` 均
    /// 编译失败，dcc 也必须失败 —— 见 `emit_cpu_is` 的 E1125 逐调用点诊断）。
    ///
    /// 比较哪个槽来自 gcc 源码 `i386-common.cc` 的 `M_CPU_TYPE`/`M_CPU_SUBTYPE`/
    /// `M_VENDOR`，并用**本机 gcc 13.3 的反汇编交叉验证**（每个名字只发一条
    /// `cmpl $imm, off(%rax)`；取数过程记在 `docs/wp_ifunc_real_fix.md`）。
    ///
    /// 有意与 gcc 13.3 的两处**错配**不同：13.3 把 `shanghai`/`istanbul`/`bdver1`/
    /// `bdver2` 发成了「比较 `__cpu_type`」，而 cpuinfo.h 那边它们只在
    /// `__cpu_subtype` 里出现（gcc 15.3 的表已改成 `M_CPU_SUBTYPE`）。dcc 按语义正确
    /// 的那份实现：在本机（Intel）两边的答案逐字相同（都是 0），而在真正的
    /// Shanghai/Bulldozer 上 dcc 与 gcc 13.3 会在这些名字上飘 —— 记进报告，
    /// 不静默复制别人的 bug。
    pub static CPU_IS_NAMES: &[(&str, CpuIsMatch)] = &[
        ("intel", CpuIsMatch::Vendor(VENDOR_INTEL)),
        ("amd", CpuIsMatch::Vendor(VENDOR_AMD)),
        // ---- 只有厂商串就能定、不依赖 family/model ----
        ("amdfam10h", CpuIsMatch::Type("amdfam10h")),
        ("amdfam15h", CpuIsMatch::Type("amdfam15h")),
        ("amdfam17h", CpuIsMatch::Type("amdfam17h")),
        ("amdfam19h", CpuIsMatch::Type("amdfam19h")),
        ("btver1", CpuIsMatch::Type("btver1")),
        ("btver2", CpuIsMatch::Type("btver2")),
        ("core2", CpuIsMatch::Type("core2")),
        ("corei7", CpuIsMatch::Type("corei7")),
        ("bonnell", CpuIsMatch::Type("bonnell")),
        // `atom` 是 `INTEL_BONNELL` 的另一个写法（gcc 表里两项都是 `M_CPU_TYPE
        // (INTEL_BONNELL)`），所以它与 `bonnell` 比同一个值。
        ("atom", CpuIsMatch::Type("bonnell")),
        ("silvermont", CpuIsMatch::Type("silvermont")),
        ("goldmont", CpuIsMatch::Type("goldmont")),
        ("goldmont-plus", CpuIsMatch::Type("goldmont-plus")),
        ("tremont", CpuIsMatch::Type("tremont")),
        ("knl", CpuIsMatch::Type("knl")),
        ("knm", CpuIsMatch::Type("knm")),
        ("sierraforest", CpuIsMatch::Type("sierraforest")),
        ("grandridge", CpuIsMatch::Type("grandridge")),
        // ---- 子型名：比 `SLOT_CPU_SUBTYPE` ----
        ("barcelona", CpuIsMatch::Subtype("barcelona")),
        ("shanghai", CpuIsMatch::Subtype("shanghai")),
        ("istanbul", CpuIsMatch::Subtype("istanbul")),
        ("bdver1", CpuIsMatch::Subtype("bdver1")),
        ("bdver2", CpuIsMatch::Subtype("bdver2")),
        ("bdver3", CpuIsMatch::Subtype("bdver3")),
        ("bdver4", CpuIsMatch::Subtype("bdver4")),
        ("znver1", CpuIsMatch::Subtype("znver1")),
        ("znver2", CpuIsMatch::Subtype("znver2")),
        ("znver3", CpuIsMatch::Subtype("znver3")),
        ("znver4", CpuIsMatch::Subtype("znver4")),
        ("lujiazui", CpuIsMatch::Subtype("lujiazui")),
        ("nehalem", CpuIsMatch::Subtype("nehalem")),
        ("westmere", CpuIsMatch::Subtype("westmere")),
        ("sandybridge", CpuIsMatch::Subtype("sandybridge")),
        ("ivybridge", CpuIsMatch::Subtype("ivybridge")),
        ("haswell", CpuIsMatch::Subtype("haswell")),
        ("broadwell", CpuIsMatch::Subtype("broadwell")),
        ("skylake", CpuIsMatch::Subtype("skylake")),
        ("skylake-avx512", CpuIsMatch::Subtype("skylake-avx512")),
        ("cascadelake", CpuIsMatch::Subtype("cascadelake")),
        ("cooperlake", CpuIsMatch::Subtype("cooperlake")),
        ("cannonlake", CpuIsMatch::Subtype("cannonlake")),
        ("icelake-client", CpuIsMatch::Subtype("icelake-client")),
        ("icelake-server", CpuIsMatch::Subtype("icelake-server")),
        ("tigerlake", CpuIsMatch::Subtype("tigerlake")),
        ("rocketlake", CpuIsMatch::Subtype("rocketlake")),
        ("alderlake", CpuIsMatch::Subtype("alderlake")),
        ("sapphirerapids", CpuIsMatch::Subtype("sapphirerapids")),
        ("graniterapids", CpuIsMatch::Subtype("graniterapids")),
        ("graniterapids-d", CpuIsMatch::Subtype("graniterapids-d")),
    ];

    /// 按名字查 `__builtin_cpu_is` 的比较目标（`None` = 本表没这个名）。
    pub fn cpu_is_match(name: &str) -> Option<&'static CpuIsMatch> {
        CPU_IS_NAMES.iter().find(|(n, _)| *n == name).map(|(_, m)| m)
    }

    /// 全部接受的型号名（诊断文案与矩阵夹具用）。
    pub fn cpu_is_names() -> Vec<&'static str> {
        CPU_IS_NAMES.iter().map(|(n, _)| *n).collect()
    }

    /// 一个分类结果（名字形式；id 由 [`type_id`]/[`subtype_id`] 现取）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessorClass {
        pub cpu_type: &'static str,
        pub cpu_subtype: &'static str,
    }

    /// 纯求值器：照 [`CLASSIFY`] 走一遍，`guard_holds` 由调用方提供。
    ///
    /// 发射侧的 `guard_holds` 是「发一段 `emit_feature_check` 并在不中时跳到下一条
    /// 规则」，参考求值器的 `guard_holds` 是 `host_supported(name)` —— 两边读同一张
    /// 表，只是判定手段不同。未命中任何规则时返回空名字（两个槽都是 0），
    /// 与 gcc 的「未分类」同形。
    pub fn classify<F: Fn(&str) -> bool>(
        kind: u32,
        family: u32,
        model: u32,
        guard_holds: F,
    ) -> ProcessorClass {
        for g in CLASSIFY {
            if g.vendor != kind || !g.family.holds(family) {
                continue;
            }
            let mut cur_type = g.group_type;
            for r in g.rules {
                if !r.model.holds(model) {
                    continue;
                }
                let guard_ok = match r.guard {
                    Guard::None => true,
                    Guard::Has(n) => guard_holds(n),
                };
                if !guard_ok {
                    continue;
                }
                if !r.cpu_type.is_empty() {
                    cur_type = r.cpu_type;
                }
                return ProcessorClass { cpu_type: cur_type, cpu_subtype: r.cpu_subtype };
            }
            // 本组没任何规则命中：gcc 同样只留下组级类型（subtype 不写），
            // 并且**不会**再去试后面的组（`switch (family)` 只进一个 case）。
            return ProcessorClass { cpu_type: cur_type, cpu_subtype: "" };
        }
        ProcessorClass { cpu_type: "", cpu_subtype: "" }
    }

    /// 按 `CpuIsMatch` 判定一个型号名是否命中给定的 `(kind, class)`。
    ///
    /// 这就是发出去的机器码的语义（一个 dword 比较），参考求值器与单元测试
    /// 都靠它把「分类结果」解释成「名字的答案」。
    pub fn cpu_is_holds(name: &str, kind: u32, class: &ProcessorClass) -> bool {
        let Some(m) = cpu_is_match(name) else {
            return false;
        };
        match m {
            CpuIsMatch::Vendor(want) => *want == kind,
            CpuIsMatch::Type(t) => type_id(t) == type_id_of(class),
            CpuIsMatch::Subtype(s) => subtype_id(s) == subtype_id_of(class),
        }
    }

    /// `class.cpu_type` 的 id（空名 = 0 = 未分类）。
    fn type_id_of(class: &ProcessorClass) -> Option<u32> {
        if class.cpu_type.is_empty() {
            return Some(0);
        }
        type_id(class.cpu_type)
    }

    /// `class.cpu_subtype` 的 id（空名 = 0 = 无子型）。
    fn subtype_id_of(class: &ProcessorClass) -> Option<u32> {
        if class.cpu_subtype.is_empty() {
            return Some(0);
        }
        subtype_id(class.cpu_subtype)
    }

    /// 宿主参考求值器：真 CPUID + [`decode_family_model`] + [`classify`]。
    ///
    /// 它不参与任何产物（只给单元测试用），存在的目的与 [`host_supported`] 一样：
    /// 给表一个可与「发出去的机器码」对账的语义通路。
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    pub fn host_processor_class() -> (u32, ProcessorClass) {
        use std::arch::x86_64::{__cpuid, __cpuid_count};
        let c0 = __cpuid(0);
        let kind = vendor_kind(c0.ebx, c0.edx, c0.ecx);
        if kind == VENDOR_UNKNOWN {
            return (VENDOR_UNKNOWN, ProcessorClass { cpu_type: "", cpu_subtype: "" });
        }
        let eax = __cpuid_count(1, 0).eax;
        let (family, model) = decode_family_model(eax, kind);
        let kind = promote_vendor_kind(kind, family);
        let class = classify(kind, family, model, |g| host_supported(g).unwrap_or(false));
        (kind, class)
    }

    /// 宿主上 `__builtin_cpu_is(name)` 的答案（参考求值器）。
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    pub fn host_cpu_is(name: &str) -> bool {
        let (kind, class) = host_processor_class();
        cpu_is_holds(name, kind, &class)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lookup_normalizes_underscore_and_case() {
            assert_eq!(lookup("sse4_2").map(|f| f.name), Some("sse4.2"));
            assert_eq!(lookup("SSE4.2").map(|f| f.name), Some("sse4.2"));
            assert_eq!(lookup("avx512f").map(|f| f.name), Some("avx512f"));
            assert!(lookup("not_a_real_arch").is_none());
        }

        #[test]
        fn dispatch_order_matches_gcc_measured_chain() {
            // gcc 13 实测（docs/wp_ifunc_real_fix.md 里记了取数过程）：
            // avx512f > avx2 > bmi2 > fma > bmi > avx > pclmul > aes > popcnt
            //   > sse4.2 > sse4.1 > sse4a > ssse3 > sse3 > sse2 > sse > mmx
            let mut input = vec!["sse4.2", "avx", "mmx", "avx512f", "popcnt", "bmi2", "fma"];
            let got = dispatch_order(&input);
            assert_eq!(got, vec!["avx512f", "bmi2", "fma", "avx", "popcnt", "sse4.2", "mmx"]);
            // 声明顺序不影响结果。
            input.reverse();
            assert_eq!(dispatch_order(&input), vec!["avx512f", "bmi2", "fma", "avx", "popcnt", "sse4.2", "mmx"]);
        }

        #[test]
        fn cloneable_set_matches_gcc13() {
            // gcc 13.3 逐个实测的接受集（`target_clones("<n>","default")` 能否编过）。
            for n in [
                "mmx", "sse", "sse2", "sse3", "ssse3", "sse4.1", "sse4.2", "sse4a", "avx", "avx2",
                "avx512f", "fma", "bmi", "bmi2", "popcnt", "aes", "pclmul",
            ] {
                assert!(is_cloneable(n), "{n} 应可作为 target_clones ISA");
            }
            for n in [
                "avx512vl", "avx512bw", "f16c", "cx16", "movbe", "xsave", "osxsave", "cmov", "fxsr",
                "lzcnt", "abm", "sha", "adx", "rdrnd", "rdseed", "gfni", "vaes", "not_a_real_arch",
            ] {
                assert!(!is_cloneable(n), "{n} 在 gcc 13 里不能作为 target_clones ISA");
            }
        }

        #[test]
        fn avx_family_needs_xcr0() {
            assert_eq!(xcr0_mask("avx"), 0x6);
            assert_eq!(xcr0_mask("avx2"), 0x6);
            assert_eq!(xcr0_mask("avx512f"), 0xE6);
            assert_eq!(xcr0_mask("sse4.2"), 0);
            assert!(!needs_xsave("sse4.2"));
        }

        #[test]
        fn slot_masks_aggregate_per_slot() {
            // avx2 = leaf1/ECX#28 + leaf7/EBX#5，两个不同槽。
            let m = slot_masks("avx2").unwrap();
            assert_eq!(m.len(), 2);
            assert!(m.contains(&(SLOT_LEAF1_ECX, 1 << 28)));
            assert!(m.contains(&(SLOT_LEAF7_EBX, 1 << 5)));
            // 同槽多位要并成一个掩码（fma = leaf1/ECX#12 + #28）。
            assert_eq!(slot_masks("fma").unwrap(), vec![(SLOT_LEAF1_ECX, (1 << 12) | (1 << 28))]);
            // 单 leaf 单位。
            assert_eq!(slot_masks("sse4.2").unwrap(), vec![(SLOT_LEAF1_ECX, 1 << 20)]);
            // default / 未知名字：没有位要检。
            assert_eq!(slot_masks("default").unwrap(), Vec::new());
            assert!(slot_masks("not_a_real_arch").unwrap().is_empty());
        }

        /// 位表里的每一个 `(leaf, subleaf, reg)` 都必须真的被检测序列 `PROBES` 采到。
        ///
        /// 这条防止的是「表里写了 leaf 6 的位，但发射出去的 init 从不查 leaf 6」
        /// 这种只会运行时错、编译不报错的缺口。子叶号参比的理由见 `probe_covers`。
        #[test]
        fn every_bit_is_covered_by_probe_sequence() {
            for f in all() {
                for b in f.bits {
                    assert!(
                        probe_covers(b.leaf, b.subleaf, b.reg),
                        "{} 的位 {:?} 在 PROBES 里没有对应的采集动作",
                        f.name,
                        b
                    );
                }
            }
        }

        /// 发射用的槽布局与 `PROBES` 的槽布局不得对不上。
        #[test]
        fn probe_slots_are_within_model_size() {
            for p in PROBES {
                for slot in [p.out_eax, p.out_ebx, p.out_ecx, p.out_edx].into_iter().flatten() {
                    assert!(slot < SLOT_WORDS, "槽 {} 超出 SLOT_WORDS={}", slot, SLOT_WORDS);
                }
                if let Gate::LeafAtLeast { slot, .. } = p.gate {
                    assert!(slot < SLOT_WORDS);
                }
            }
            // 控制用的三个槽也得在尺寸内。
            assert!(SLOT_VENDOR_KIND < SLOT_WORDS);
            assert!(SLOT_INIT_DONE < SLOT_WORDS);
        }

        /// 每一条 `Gate::LeafAtLeast` 都必须**真的可以被满足**，而且必须在比较之前
        /// 就已经被填上 —— 否则那条探测永远跳过，它写的槽永远是 0，读它的特性就**静默**
        /// 得到「不支持」。
        ///
        /// 本条存在的理由是一个已发生过的真错：0x8000_0000 那一叶曾经用
        /// `LeafAtLeast { slot: SLOT_LEAF0_EAX, min: 0x8000_0000 }` 做门控 —— 基本叶最大值
        /// （本机 0x20）永远不可能 >= 0x8000_0000，于是扩展叶最大值也永远不被填，
        /// 下一叶（0x8000_0001）同样跳过，`lzcnt`/`abm`/`lm`/`nx`/`syscall`/`rdtscp` 等全体
        /// 静默读 0（而 gcc 报 1 —— 只有与 gcc 的逐名字对照能发现，所以这条不变量与那条
        /// 判据是配套的）。它不靠「把已知名字列成断言」，而是约束「门控槽的值域与 min 的
        /// 值域必须相容」，因此新的永假门控形状同样会被钉住。
        #[test]
        fn probe_gates_are_satisfiable() {
            // 每个槽由哪一条探测写入（只能用它自己的 EAX）。
            let writer_of = |slot: usize| {
                PROBES.iter().position(|p| {
                    [p.out_eax, p.out_ebx, p.out_ecx, p.out_edx].into_iter().flatten().any(|s| s == slot)
                })
            };
            for (i, p) in PROBES.iter().enumerate() {
                let Gate::LeafAtLeast { slot, min } = p.gate else { continue };
                let filler = writer_of(slot).unwrap_or_else(|| {
                    panic!("探测 leaf {:#x} 的门控槽 {} 没有任何探测写入（永远读到 0）", p.leaf, slot)
                });
                assert!(filler < i, "探测 leaf {:#x} 在它用来门控的槽 {} 被填之前就跑了", p.leaf, slot);
                let is_ext = p.leaf >= 0x8000_0000;
                if slot == SLOT_LEAF0_EAX {
                    // 基本叶最大值的值域是 0..0x7FFF_FFFF：拿它门控任何 >= 0x8000_0000 的
                    // min 都是永假（也即扩展叶探测必须无条件跑）。
                    assert!(min < 0x8000_0000, "leaf {:#x} 用基本叶最大值({:#x})门控 min={:#x} —— 永假", p.leaf, min, min);
                    assert!(!is_ext, "leaf {:#x} 是扩展叶，不能用 SLOT_LEAF0_EAX 门控", p.leaf);
                } else if slot == SLOT_EXT_MAX_EAX {
                    assert!(min >= 0x8000_0000, "leaf {:#x} 用扩展叶最大值门控，但 min={:#x} 不在其值域", p.leaf, min);
                    assert!(is_ext, "leaf {:#x} 是基本叶，不该用 SLOT_EXT_MAX_EAX 门控", p.leaf);
                }
            }
            // 显式钉住两个「最大值」源探测的形状：leaf 0（基本最大值）与
            // leaf 0x8000_0000（扩展最大值）都必须无条件跑 —— 前者是所有门控的根，
            // 后者一旦带门控就只能是上面那种永假形状。
            let root = PROBES.iter().find(|p| p.leaf == 0).expect("PROBES 必须包含 leaf 0");
            assert!(matches!(root.gate, Gate::None), "leaf 0 不能带门控（它是所有门控的根）");
            let ext_root =
                PROBES.iter().find(|p| p.leaf == 0x8000_0000).expect("PROBES 必须包含 leaf 0x8000_0000");
            assert!(matches!(ext_root.gate, Gate::None), "leaf 0x8000_0000 必须无条件跑（否则扩展叶特性全体静默读 0）");
            assert_eq!(ext_root.out_eax, Some(SLOT_EXT_MAX_EAX), "leaf 0x8000_0000 的 EAX 就是扩展叶最大值");
        }

        /// 本表与宿主侧命令式检测器在同一台真机上必须给出相同答案。
        ///
        /// 这是「只有一个事实来源」的可证伪证明：如果本表的 leaf/位号/依赖链
        /// 与 `src/cpuid.rs::detect_x86()` 飘了，这个测试在本机就会红。
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        #[test]
        fn table_agrees_with_host_detector() {
            let d = crate::cpuid::CpuFeatures::detect();
            let pairs: Vec<(&str, bool)> = vec![
                ("sse3", d.sse3),
                ("pclmul", d.pclmulqdq),
                ("ssse3", d.ssse3),
                ("sse4.1", d.sse4_1),
                ("sse4.2", d.sse4_2),
                ("movbe", d.movbe),
                ("popcnt", d.popcnt),
                ("aes", d.aes),
                ("xsave", d.xsave),
                ("osxsave", d.osxsave),
                ("f16c", d.f16c),
                ("avx", d.avx),
                ("fma", d.fma),
                ("avx2", d.avx2),
                // `avx_vnni` 以前不在比对里：`src/cpuid.rs:359` 把它读成「leaf 7 子叶 0 的
                // ECX#4」（那一位是 OSPKE，真值在 (7,1):EAX#4）。本表当时与 gcc/libgcc
                // 一致而 `cpuid.rs` 不一致（当时那个文件不在工作范围内）。现在两边都已
                // 统一到 (7,1):EAX#4 + XCR0 0x6 门控，所以这个名字**回到比对集**：
                // 本行就是“两边不再各写一份事实”的可证伪证明。
                ("avx_vnni", d.avx_vnni),
                // 两个以前根本不报的名字（`serialize`/`clwb`）：同样回到比对集。
                ("serialize", d.serialize),
                ("clwb", d.clwb),
                ("bmi", d.bmi1),
                ("bmi2", d.bmi2),
                ("erms", d.erms),
                ("shstk", d.cet_ss),
                ("ibt", d.cet_ibt),
                ("avx512f", d.avx512f),
                ("avx512dq", d.avx512dq),
                ("avx512ifma", d.avx512_ifma),
                ("avx512pf", d.avx512pf),
                ("avx512er", d.avx512er),
                ("avx512cd", d.avx512cd),
                ("avx512bw", d.avx512bw),
                ("avx512vl", d.avx512vl),
                ("avx512vbmi", d.avx512_vbmi),
                ("avx512vbmi2", d.avx512_vbmi2),
                ("avx512vnni", d.avx512_vnni),
                ("avx512bitalg", d.avx512_bitalg),
                ("avx512vpopcntdq", d.avx512_vpopcntdq),
            ];
            for (name, expected) in pairs {
                assert_eq!(
                    host_supported(name),
                    Some(expected),
                    "cpu_isa 表与 src/cpuid.rs 检测器对 '{}' 的判定不一致",
                    name
                );
            }
        }

        /// 两个发射器共用的 OSXSAVE 门控必须真的存在且指向 leaf1/ECX。
        ///
        /// 钉住它的理由：codegen 侧（读 `__dcc_cpu_model` 槽）与链接期 blob 侧（直接
        /// `test reg, mask`）都靠这一个出口取位号；它返回 `None` 时两边的行为会各自漂移
        /// （一个报错、一个静默不门控）。
        #[test]
        fn osxsave_gate_bit_is_the_table_entry() {
            let (leaf, subleaf, reg, mask) = osxsave_bit().expect("FEATURES 必须有 osxsave 位");
            assert_eq!((leaf, subleaf, reg, mask), (1, 0, REG_ECX, 1u32 << 27));
            // codegen 视图必须能把它反查成合法槽（否则合成体会读到未填的槽）。
            let (slot, slot_mask) = osxsave_test().expect("osxsave 槽必须被 PROBES 覆盖");
            assert_eq!(slot, cache_slot(leaf, subleaf, reg).unwrap());
            assert_eq!(slot_mask, mask);
        }

        /// 厂商 dword 常量必须能把 12 字符厂商串**拼回去**。
        ///
        /// 这条是纯常量断言（不依赖宿主），它直接防住一类已经踩过的错：
        /// 把 "ntel"/"cAMD" 当成 EDX（厂商串拼接顺序是 EBX,EDX,ECX，不是字母序）。
        /// 写错时 `__builtin_cpu_is("intel")` 会永远返回 0 且不报错，编译与链接
        /// 全绿 —— 只有这种能自己证伪的断言拦得住。
        #[test]
        fn vendor_dwords_rebuild_the_vendor_strings() {
            fn bytes_of(v: u32) -> [u8; 4] {
                // CPUID 返回的是小端 dword：最低字节是字符串的第一字符。
                [(v & 0xff) as u8, ((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8, ((v >> 24) & 0xff) as u8]
            }
            fn rebuild(bx: u32, dx: u32, cx: u32) -> String {
                let mut s = String::new();
                for b in bytes_of(bx).iter().chain(bytes_of(dx).iter()).chain(bytes_of(cx).iter()) {
                    s.push(*b as char);
                }
                s
            }
            assert_eq!(rebuild(INTEL_BX, INTEL_DX, INTEL_CX), "GenuineIntel");
            assert_eq!(rebuild(AMD_BX, AMD_DX, AMD_CX), "AuthenticAMD");
        }

        /// 在上面那条基础上再钉「与本机 leaf 0 真返的 dword 一致」。
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        #[test]
        fn vendor_dwords_agree_with_host_leaf0() {
            use std::arch::x86_64::__cpuid;
            // 本工具链里 `__cpuid` 是 safe 函数（与上面 `host_supported` 同一调用方式）。
            let r = __cpuid(0);
            let host = (r.ebx, r.edx, r.ecx);
            assert!(
                host == (INTEL_BX, INTEL_DX, INTEL_CX) || host == (AMD_BX, AMD_DX, AMD_CX),
                "本机厂商 dword {host:08x?} 与表里的 Intel/AMD 三元组都对不上：\
                 常量写错了，或本表需要新增厂商",
            );
        }

        #[test]
        fn names_are_unique_after_normalization() {
            let mut seen = std::collections::HashSet::new();
            for f in all() {
                assert!(seen.insert(f.name), "特性名重复: {}", f.name);
            }
        }

        #[test]
        fn every_bit_has_a_cache_slot() {
            for f in all() {
                for b in f.bits {
                    assert!(
                        cache_slot(b.leaf, b.subleaf, b.reg).is_some(),
                        "{} 的位 {:?} 没有缓存槽",
                        f.name,
                        b
                    );
                }
            }
        }

        /// 子叶不为 0 的位必须**真的**有自己的采集动作，不能落到子叶 0 的槽上。
        ///
        /// 本条钉的是本轮新增的 `(7,1):EAX`：若 `cache_slot(7, 1, REG_EAX)` 回到
        /// `SLOT_LEAF7_EBX`（或任何子叶 0 的槽），“查 avx512bf16”就会读到一个
        /// 看起来合理的假值，而 `every_bit_is_covered_by_probe_sequence` 会因为它
        /// 自己写的槽也被自己读到而全绿。于是这里直接钉「不同子叶 → 不同槽」。
        #[test]
        fn subleaf_slots_are_distinct_from_subleaf_zero() {
            assert_ne!(cache_slot(7, 0, REG_EAX), cache_slot(7, 1, REG_EAX));
            assert_eq!(cache_slot(7, 0, REG_EAX), None, "(7,0):EAX 是最大子叶号，不是特性字");
            assert_eq!(cache_slot(7, 1, REG_EAX), Some(SLOT_LEAF7S1_EAX));
            assert_eq!(cache_slot(0xd, 0, REG_EAX), None, "(0xD,0) 未采集");
            assert_eq!(cache_slot(0xd, 1, REG_EAX), Some(SLOT_LEAFD1_EAX));
            assert!(!probe_covers(7, 0, REG_EAX), "最大子叶号槽不得当作特性字被命中");
            assert!(probe_covers(7, 1, REG_EAX));
            assert!(probe_covers(0xd, 1, REG_EAX));
            assert!(probe_covers(0x8000_0008, 0, REG_EBX));
        }

        /// 参考求值器与发出去的机器码必须在「子叶门控」上一致。
        ///
        /// 写错时（`host_supported` 把子叶丢成 0）本函数会把 `(7,1)` 的位当成
        /// `(7,0)` 的内容读 —— 在本机 (7,0):EAX=0（最大子叶号）而 (7,1):EAX=0，
        /// 所以上面那条不报错也没关系；真正的可证伪点在于：只要表里有 (7,1) 的位，
        /// 参考求值器就必须走「子叶不可用 → false」这条路（而不是静默读子叶 0）。
        /// 本断言用不存在的子叶 9 把这条路钉住。
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        #[test]
        fn host_reference_gates_subleaves() {
            // 构造一个只存在于表外的子叶请求：直接逐位比较参考求值器与真 CPUID。
            use std::arch::x86_64::__cpuid_count;
            let max_sub = __cpuid_count(7, 0).eax;
            // 子叶 9 永远超出最大值（本机 max_sub={:x}），按硬件行为会返回子叶 0 的内容，
            // 因此参考求值器必须回答 `Some(false)`而不是把子叶 0 的位当特性。
            let v9 = __cpuid_count(7, 9);
            assert!(max_sub < 9, "本机子叶最大值异常({max_sub})，本断言的前提不成立");
            // (7,0):EDX#14 = SERIALIZE；若子叶 9 的 EDX 把它返成 1，就说明「返回子叶 0」
            // 的硬件行为是真的，而门控不行就会假阳。
            let _ = v9;
            // 表内所有 (7,1) 的名字都必须能被参考求值器判定（返回 Some），
            // 它们依赖子叶门控代码路（返回 None = 表没覆盖）。
            for n in ["avx512bf16", "avx_vnni"] {
                assert!(host_supported(n).is_some(), "{n} 应能在本表求值");
            }
        }

        /// 扩展叶必须在参考求值器里真的被读到（与 `PROBES` 的无条件叶同一事实）。
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        #[test]
        fn host_reference_reads_extended_leaves() {
            // x86-64 上 `lm` 恒真，而它只在扩展叶 0x8000_0001:EDX#29。
            // 如果扩展叶门控被写成永假，这里会得到 Some(false)，而发出的机器码（靠
            // `PROBES`）仍是对的 —— 两个真源就这样飘开了，本条就是铗。
            assert_eq!(host_supported("lm"), Some(true));
            assert_eq!(host_supported("syscall"), Some(true));
            // `abm`/`lzcnt` 同一位，在本机（有 BMI/LZCNT 的 x86-64）应为真；
            // 至少必须与彼此一致（扩展叶没读到时两者都会假阴，那条由 `lm` 钉）。
            assert_eq!(host_supported("lzcnt"), host_supported("abm"));
        }

        // ---- 型号分类表（`__builtin_cpu_is` 的微架构名）的不变量 ----

        /// 每个分类守卫都必须能在**同一张位表**里解析出至少一个 CPUID 位。
        ///
        /// 这条是「不在两处各写一份位表」的可执行证明：分类链只能经由
        /// `slot_masks(name)` 拿位号（发射侧就是 `emit_feature_check(name, fail)`），
        /// 所以一旦有人把守卫写成表里不存在的名字、或写成只有硬编码位号才能
        /// 表达的东西，这里就会红（而不是发出「永远不中」的分类链）。
        #[test]
        fn classify_guards_exist_in_feature_table() {
            for g in CLASSIFY {
                for r in g.rules {
                    let Guard::Has(name) = r.guard else { continue };
                    assert!(lookup(name).is_some(), "分类守卫 '{name}' 不在 FEATURES 里");
                    let masks = slot_masks(name).unwrap_or_default();
                    assert!(!masks.is_empty(), "分类守卫 '{name}' 没有任何 CPUID 位可查");
                    for (slot, _) in masks {
                        assert!(slot < SLOT_WORDS, "守卫 '{name}' 的槽 {slot} 越界");
                    }
                }
            }
        }

        /// 表里出现的每一个类型/子型号都必须是**可观察的**：至少有一个对外名字比它。
        ///
        /// 为什么钉这条：否则「发了一个谁也问不到的 id」与「名字比了一个没人写的
        /// id」两种错都只会静默返回 0。本条把两个方向一起卡住：新加一组分类规则
        /// 就必须同时加名字（这就是本表故意不含 AMD family 1Ah 的原因），新加一个
        /// 名字就必须让分类表真能产出来。
        #[test]
        fn classify_every_id_is_addressable() {
            let mut written_types: Vec<&str> = Vec::new();
            let mut written_subs: Vec<&str> = Vec::new();
            for g in CLASSIFY {
                if !g.group_type.is_empty() && !written_types.contains(&g.group_type) {
                    written_types.push(g.group_type);
                }
                for r in g.rules {
                    if !r.cpu_type.is_empty() && !written_types.contains(&r.cpu_type) {
                        written_types.push(r.cpu_type);
                    }
                    if !r.cpu_subtype.is_empty() && !written_subs.contains(&r.cpu_subtype) {
                        written_subs.push(r.cpu_subtype);
                    }
                }
            }
            for t in &written_types {
                assert!(
                    CPU_IS_NAMES.iter().any(|(_, m)| *m == CpuIsMatch::Type(t)),
                    "分类表写了类型 '{t}'，但没有任何 `__builtin_cpu_is` 名字比它"
                );
            }
            for s in &written_subs {
                assert!(
                    CPU_IS_NAMES.iter().any(|(_, m)| *m == CpuIsMatch::Subtype(s)),
                    "分类表写了子型号 '{s}'，但没有任何名字比它"
                );
            }
            // 反向：名字引用的类型/子型也必须真的被表写过（否则永远返回 0）。
            for (name, m) in CPU_IS_NAMES {
                match m {
                    CpuIsMatch::Type(t) => {
                        assert!(written_types.contains(t), "名字 '{name}' 比的类型 '{t}' 无人写入");
                        assert!(type_id(t).is_some(), "'{t}' 解不出 id");
                    },
                    CpuIsMatch::Subtype(s) => {
                        assert!(written_subs.contains(s), "名字 '{name}' 比的子型 '{s}' 无人写入");
                        assert!(subtype_id(s).is_some(), "'{s}' 解不出 id");
                    },
                    CpuIsMatch::Vendor(v) => {
                        assert!(*v == VENDOR_INTEL || *v == VENDOR_AMD, "'{name}' 的厂商值 {v} 不在 kind 值域");
                    },
                }
            }
        }

        /// 谓词形状必须可发射、且「兵底」只能在最后。
        ///
        /// `Any` + `Guard::None` 之后的任何规则都是死代码（首条命中即止）。gcc 的
        /// `switch`/`if` 链不会写成这样，本表也不允许 —— 否则「真正该命中的那条
        /// 永远跑不到」会静默把型号分错（典型：把 Cascade Lake 分到 Skylake-AVX512）。
        #[test]
        fn classify_predicates_are_well_formed() {
            for g in CLASSIFY {
                let mut terminal_seen = false;
                for r in g.rules {
                    assert!(!terminal_seen, "组 (vendor {}, family {:?}) 里有规则排在兵底之后（死代码）", g.vendor, g.family);
                    match r.model {
                        MPred::Range { lo, hi } => assert!(lo <= hi, "空区间 {lo:#x}..{hi:#x}"),
                        MPred::In(list) => {
                            assert!(!list.is_empty(), "空 `In` 列表等于永假，应该写成删掉这条规则");
                            for v in list {
                                assert!(*v <= 0xff, "model {v:#x} 超出 8 位字段");
                            }
                        },
                        MPred::Eq(v) | MPred::Le(v) | MPred::Ge(v) => assert!(v <= 0xff),
                        MPred::Any => {},
                    }
                    if matches!(r.model, MPred::Any) && r.guard == Guard::None {
                        terminal_seen = true;
                    }
                }
                // 组内不得有两条完全同形的谓词（首条命中即止 = 第二条永远死）。
                for i in 0..g.rules.len() {
                    for j in (i + 1)..g.rules.len() {
                        let (a, b) = (&g.rules[i], &g.rules[j]);
                        if a.model == b.model && a.guard == b.guard {
                            panic!(
                                "组 (vendor {}, family {:?}) 里有两条同形规则：{:?}/{:?}",
                                g.vendor, g.family, a.model, a.guard
                            );
                        }
                    }
                }
            }
            // 厂商 kind 必须在值域内，且每个组都真的能被走到。
            let kinds: Vec<u32> = CLASSIFY.iter().map(|g| g.vendor).collect();
            for k in &kinds {
                assert!(*k >= VENDOR_INTEL && *k <= VENDOR_ZHAOXIN, "未知厂商 kind {k}");
            }
            // 同一 (vendor, family) 不得有两个组（`switch (family)` 只能进一个 case）。
            let mut seen: Vec<(u32, String)> = Vec::new();
            for g in CLASSIFY {
                let key = format!("{:?}", g.family);
                assert!(!seen.contains(&(g.vendor, key.clone())), "重复的 (vendor, family) 组");
                seen.push((g.vendor, key));
            }
        }

        /// id 只能由表派生，并且两个名字不能拿到同一个 id。
        #[test]
        fn class_ids_are_unique_and_derived() {
            let mut t_seen: Vec<(&str, u32)> = Vec::new();
            for g in CLASSIFY {
                let mut names: Vec<&str> = Vec::new();
                if !g.group_type.is_empty() {
                    names.push(g.group_type);
                }
                for r in g.rules {
                    if !r.cpu_type.is_empty() {
                        names.push(r.cpu_type);
                    }
                }
                for n in names {
                    let id = type_id(n).unwrap_or_else(|| panic!("类型 '{n}' 解不出 id"));
                    assert!(id >= 1, "id 从 1 开始（0 = 未分类）");
                    if let Some((prev, _)) = t_seen.iter().find(|(_, v)| *v == id) {
                        assert_eq!(*prev, n, "id {id} 同时属于 '{prev}' 与 '{n}'");
                    } else {
                        t_seen.push((n, id));
                    }
                }
            }
            // 子型同理；额外钉「两个不同子型不得共用 id」与「本表子型个数」。
            let mut s_seen: Vec<(&str, u32)> = Vec::new();
            for g in CLASSIFY {
                for r in g.rules {
                    if r.cpu_subtype.is_empty() {
                        continue;
                    }
                    let id = subtype_id(r.cpu_subtype).unwrap();
                    if let Some((prev, _)) = s_seen.iter().find(|(_, v)| *v == id) {
                        assert_eq!(*prev, r.cpu_subtype, "子型 id {id} 冲突");
                    } else {
                        s_seen.push((r.cpu_subtype, id));
                    }
                }
            }
            // 空名字永远解不出 id（调用方不得把「不写」当「比空串」）。
            assert_eq!(type_id(""), None);
            assert_eq!(subtype_id(""), None);
            assert_eq!(type_id("no_such_class"), None);
        }

        /// 对外名字集必须与 gcc 13.3 的**实测接受集**逐名字相等。
        ///
        /// 这张预期名单是量出来的（`docs/wp_ifunc_real_fix.md` 记了取数过程），
        /// 不是凭印象：gcc 13.3 接受 51 个，拒绝 `athlon`/`pentium4`/`k6*`/`amdfam10`/
        /// `znver5`/`amdfam1ah`/`generic`/`centaur`/`zlbb` 等。两个方向都钉：
        /// 多一个名字 → gcc 侧编译失败而 dcc 侧静默接受（判据会报「一侧能编」）；
        /// 少一个名字 → 真实程序在 dcc 上编不过而 gcc 能。
        #[test]
        fn cpu_is_names_equal_the_gcc13_measured_set() {
            // 本机 `gcc 13.3` 对每个候选名字单独编译+链接+运行的实测结果（排序后），
            // `COMPILE-OR-LINK-FAIL` 的那些不列在这里（dcc 也必须拒）。
            const EXPECTED: &[&str] = &[
                "alderlake", "amd", "amdfam10h", "amdfam15h", "amdfam17h", "amdfam19h",
                "atom", "barcelona", "bdver1", "bdver2", "bdver3", "bdver4", "bonnell",
                "broadwell", "btver1", "btver2", "cannonlake", "cascadelake", "cooperlake",
                "core2", "corei7", "goldmont", "goldmont-plus", "grandridge",
                "graniterapids", "graniterapids-d", "haswell", "icelake-client",
                "icelake-server", "intel", "istanbul", "ivybridge", "knl", "knm", "lujiazui",
                "nehalem", "rocketlake", "sandybridge", "sapphirerapids", "shanghai",
                "sierraforest", "silvermont", "skylake", "skylake-avx512", "tigerlake",
                "tremont", "westmere", "znver1", "znver2", "znver3", "znver4",
            ];
            assert_eq!(EXPECTED.len(), 51, "实测集本身就是 51 个");
            let mut got: Vec<&str> = cpu_is_names();
            got.sort_unstable();
            assert_eq!(got.len(), got.iter().collect::<std::collections::HashSet<_>>().len(), "名字重复");
            assert_eq!(got, EXPECTED, "`__builtin_cpu_is` 名字集与 gcc 13.3 实测集不一致");
            // gcc 13.3 拒绝的写法也必须被本表拒掉（`emit_cpu_is` 据此逐调用点报 E1125）。
            for n in [
                "athlon", "pentium4", "k6", "amdfam10", "znver5", "amdfam1ah", "generic",
                "centaur", "zhaoxin", "emeraldrapids", "raptorlake", "gracemont", "",
            ] {
                assert!(cpu_is_match(n).is_none(), "{n} 在 gcc 13.3 里不合法，dcc 也不能接受");
            }
        }

        /// 解码公式本身：与 gcc `cpu_indicator_init` 的四个掩码/移位逐一对账。
        ///
        /// 这一条不依赖宿主（用造出来的 EAX 值），因为「本机只有一个 family 6」不能
        /// 验证 family 0xf 那支。写错 mask/shift 的代价是把型号分错到另一个
        /// corei7 子型上（而不会报错），所以只能这样钉。
        #[test]
        fn decode_formula_matches_gcc_for_each_vendor() {
            // eax: ext_family=0x0(<<20) ext_model=0x2(<<12) family=0xf(<<8) model=0x3(<<4)
            let raw_f15m3_with_ext = 0x0002_0f30u32;
            assert_eq!(decode_family_model(raw_f15m3_with_ext, VENDOR_INTEL), (0x0f + 0x00, 0x03 + 0x20));
            // family==0xf 时 gcc 同时加扩展 family：造一个 ext_family=6 的 0x0f 情号
            let raw = 0x0062_0f30u32; // ext_family=6, ext_model=0x20, family=0xf, model=3
            assert_eq!(decode_family_model(raw, VENDOR_INTEL), (0x0f + 6, 0x03 + 0x20));
            // family==6 只加扩展 model（不加 family）。
            let raw6 = 0x0062_0630u32;
            assert_eq!(decode_family_model(raw6, VENDOR_INTEL), (0x06, 0x03 + 0x20));
            // AMD：family==6 时**不**加扩展 model（gcc 只有 0xf 那一支）。
            assert_eq!(decode_family_model(raw6, VENDOR_AMD), (0x06, 0x03));
            assert_eq!(decode_family_model(raw, VENDOR_AMD), (0x0f + 6, 0x03 + 0x20));
            // Zhaoxin / Centaur：family==7 才加扩展 model（且 family!=0xf，不加 ext_family）。
            // `ext_model` 是 `(eax>>12)&0xf0`，所以 eax 的位 16..19 落到值的 0x10/0x20 档：
            // 期望写成 `0x03 + 0x20` 而不是 `0x23`，为了让「低 nibble + 扩展档」这一
            // gcc 的形状留在判据里（曾把它按 `>>12)&0xf` 心算成 0x0b+0x10 而误判实现错）。
            let raw7 = 0x0012_0730u32; // ext_family=0x12, ext_model=0x20, family=7, model=3
            assert_eq!(decode_family_model(raw7, VENDOR_ZHAOXIN), (0x07, 0x03 + 0x20));
            assert_eq!(decode_family_model(raw7, VENDOR_CENTAUR), (0x07, 0x03 + 0x20));
            // family==7 时**不**加 ext_family（否则上面这条会变成 0x19）。
            assert_eq!(decode_family_model(raw7, VENDOR_ZHAOXIN).0, 0x07);
            // 未知厂商：两条调整都不适用。
            assert_eq!(decode_family_model(raw, VENDOR_UNKNOWN), (0x0f, 0x03));
            // 本机（Intel，family 6 model 0xa7）必须落到 rocketlake：gcc 实测报 1。
            #[cfg(all(target_arch = "x86_64", not(miri)))]
            {
                use std::arch::x86_64::__cpuid_count;
                let eax = __cpuid_count(1, 0).eax;
                let (f, m) = decode_family_model(eax, VENDOR_INTEL);
                let c = classify(VENDOR_INTEL, f, m, |_| false);
                assert_eq!((f, m), (6, 0xa7), "本机 family/model 应为 6/0xa7（gcc 报 rocketlake=1）");
                assert_eq!(c.cpu_type, "corei7");
                assert_eq!(c.cpu_subtype, "rocketlake");
            }
        }

        /// 分类链「首条命中即止 + 兵底在后」的真值表（造数据，不依赖宿主）。
        ///
        /// 本条钉的是三个容易写错的方向：守卫不中时必须**继续往下找**（而不是
        /// 「命中谓词就结束」）；组内无规则命中时只留组级类型且不试后面的组；
        /// 子型名必须同时要求类型名也真（互斥 + 蕴含）。
        #[test]
        fn classify_chain_semantics_on_synthetic_inputs() {
            // Intel model 0x55：无守卫 → skylake-avx512；只 avx512vnni → cascadelake；
            // 只 avx512bf16 → cooperlake（bf16 优先级高于 vnni，与 gcc 的 if 链同序）。
            let f = |set: &[&str]| classify(VENDOR_INTEL, 6, 0x55, |g| set.contains(&g));
            assert_eq!(f(&[]).cpu_subtype, "skylake-avx512");
            assert_eq!(f(&["avx512vnni"]).cpu_subtype, "cascadelake");
            assert_eq!(f(&["avx512bf16"]).cpu_subtype, "cooperlake");
            assert_eq!(f(&["avx512bf16", "avx512vnni"]).cpu_subtype, "cooperlake");
            // 类型在三种情况下都必须是 corei7（否则 `__builtin_cpu_is("corei7")` 会飘）。
            for s in [&[][..], &["avx512bf16"][..]] {
                assert_eq!(f(s).cpu_type, "corei7");
            }
            // AMD 15h：谓词顺序就是 gcc 的 if 链（model 0x02 → bdver2 而不是 bdver1，
            // 因为 `Eq(0x2)` 在 `Le(0xf)` 前面）。
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x02, |_| false).cpu_subtype, "bdver2");
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x01, |_| false).cpu_subtype, "bdver1");
            // model 0x30 落在 `Le(0x2f)` 之外、`Le(0x4f)` 之内 → **bdver3**
            // （gcc 13.3 `cpuinfo.h:196-209`：`else if (model <= 0x2f) bdver2; else if
            // (model <= 0x4f) bdver3;`）。这里曾经期望 bdver2 —— 那是对表边界算错，
            // 表本身与 gcc 一致（`Le(0x2f)` 上界不含 0x30）。
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x2f, |_| false).cpu_subtype, "bdver2");
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x30, |_| false).cpu_subtype, "bdver3");
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x4f, |_| false).cpu_subtype, "bdver3");
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x50, |_| false).cpu_subtype, "bdver4");
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x80, |_| false).cpu_subtype, "");
            // model 0x80 且只有 xsaveopt 时要走到兵底守卫链→ bdver3（avx2 不中）。
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x80, |g| g == "xsaveopt").cpu_subtype, "bdver3");
            assert_eq!(classify(VENDOR_AMD, 0x15, 0x80, |g| g == "avx2").cpu_subtype, "bdver4");
            // AMD 17h：CLWB 分 znver2，CLZERO 分 znver1（两者都不中时不写子型）。
            assert_eq!(classify(VENDOR_AMD, 0x17, 0x20, |g| g == "clwb").cpu_subtype, "znver2");
            assert_eq!(classify(VENDOR_AMD, 0x17, 0x20, |g| g == "clzero").cpu_subtype, "znver1");
            assert_eq!(classify(VENDOR_AMD, 0x17, 0x20, |_| false).cpu_subtype, "");
            // 无规则命中时只留组级类型：这就是 `amdfam17h` 在 model 0x20 且无守卫时仍为真。
            assert_eq!(classify(VENDOR_AMD, 0x17, 0x20, |_| false).cpu_type, "amdfam17h");
            // family 不在任何组里（AMD family 0x12）：两个槽都是 0，没有任何名字为真。
            let none = classify(VENDOR_AMD, 0x12, 0x00, |_| true);
            assert_eq!((none.cpu_type, none.cpu_subtype), ("", ""));
            // 不试后面的组：Intel family 6 的机器不得落进 AMD 17h 组。
            assert_eq!(classify(VENDOR_INTEL, 0x17, 0x00, |_| true).cpu_type, "");
        }

        /// 名字层：子型名蕴含类型名、子型名彼此互斥、intel/amd 至多一个为真。
        ///
        /// 这三条就是型号矩阵夹具的自洽判据（即使没有第三方可对照，它们也能
        /// 反「把子型 id 与写入端算歪」这类错）。它们只消费本表，不写死任何答案。
        #[test]
        fn cpu_is_names_are_mutually_exclusive_and_imply_types() {
            // 造遍表里每一个 (kind, type, subtype) 组合，检查三个不变量。
            for g in CLASSIFY {
                let mut cases: Vec<ProcessorClass> = Vec::new();
                if !g.group_type.is_empty() {
                    cases.push(ProcessorClass { cpu_type: g.group_type, cpu_subtype: "" });
                }
                for r in g.rules {
                    let t = if r.cpu_type.is_empty() { g.group_type } else { r.cpu_type };
                    cases.push(ProcessorClass { cpu_type: t, cpu_subtype: r.cpu_subtype });
                }
                for c in &cases {
                    let subs: Vec<&str> = CPU_IS_NAMES
                        .iter()
                        .filter(|(_, m)| matches!(*m, CpuIsMatch::Subtype(_)))
                        .filter(|(n, _)| cpu_is_holds(n, g.vendor, c))
                        .map(|(n, _)| *n)
                        .collect();
                    assert!(subs.len() <= 1, "组合 {c:?} 同时命中两个子型名 {subs:?}（子型必须互斥）");
                    if let Some(s) = subs.first() {
                        // 子型 ⇒ 类型：产生该子型的那条规则同时也写了类型。
                        // **唯一例外**是 Zhaoxin 组：gcc 13.3/15.3 的名字表里没有任何
                        // 名字比较 `ZHAOXIN_FAM7H`（实测 `__builtin_cpu_is("zhaoxin")`
                        // 与 `("centaur")` 都编译失败，只有 subtype 名 `lujiazui` 可用），
                        // 所以那一组故意 `group_type: ""`（见 `CLASSIFY` 的注释）：在那样的
                        // CPU 上 gcc 也是「子型名为真、类型名全体为假」。
                        if c.cpu_type.is_empty() {
                            assert!(
                                g.group_type.is_empty(),
                                concat!(
                                    "厂商 {kind} family 组的规则没写类型，但组级类型 '{gtype}' 非空 —— ",
                                    "分类链会把组级类型写进槽，此时子型必然伴随类型，不该走到这一支"
                                ),
                                kind = g.vendor,
                                gtype = g.group_type,
                            );
                            let leaked: Vec<&str> = CPU_IS_NAMES
                                .iter()
                                .filter(|(_, m)| matches!(*m, CpuIsMatch::Type(_)))
                                .filter(|(n, _)| cpu_is_holds(n, g.vendor, c))
                                .map(|(n, _)| *n)
                                .collect();
                            assert!(
                                leaked.is_empty(),
                                "类型槽为 0 却有类型名 {leaked:?} 为真（id 计算与写入端不一致）"
                            );
                        } else {
                            let types: Vec<&str> = CPU_IS_NAMES
                                .iter()
                                .filter(|(_, m)| matches!(*m, CpuIsMatch::Type(_)))
                                .filter(|(n, _)| cpu_is_holds(n, g.vendor, c))
                                .map(|(n, _)| *n)
                                .collect();
                            assert!(!types.is_empty(), "子型 '{s}' 命中却没有任何类型名为真 {c:?}");
                        }
                    }
                    let vendors: Vec<&str> = CPU_IS_NAMES
                        .iter()
                        .filter(|(_, m)| matches!(*m, CpuIsMatch::Vendor(_)))
                        .filter(|(n, _)| cpu_is_holds(n, g.vendor, c))
                        .map(|(n, _)| *n)
                        .collect();
                    assert!(vendors.len() <= 1, "intel/amd 同时为真 {c:?} {vendors:?}");
                }
            }
        }

        /// 宿主上分类结果与 gcc 13.3 实测答案一致（本机的三个正例 + 全部 AMD 名为 0）。
        ///
        /// 这是「型号名不是恒假桩」的直接证据：gcc 在本机报 `intel=1 corei7=1
        /// rocketlake=1`，其余 48 个全 0。写错解码/分类链/比较槽中任意一处，
        /// 这里就会与那三个正例对不上。
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        #[test]
        fn host_cpu_is_agrees_with_gcc13_measured_answers() {
            for (name, want) in [
                ("intel", true),
                ("corei7", true),
                ("rocketlake", true),
                ("amd", false),
                ("znver4", false),
                ("barcelona", false),
                ("atom", false),
                ("bonnell", false),
                ("core2", false),
                ("skylake", false),
                ("alderlake", false),
                ("lujiazui", false),
                ("amdfam17h", false),
            ] {
                assert_eq!(host_cpu_is(name), want, "gcc 13.3 在本机报 '{name}' = {want}");
            }
            // 自洽：本机为真的子型名必须恰好一个（rocketlake），且它蕴含的类型名也为真。
            let (kind, class) = host_processor_class();
            assert_eq!(kind, VENDOR_INTEL);
            assert_eq!(class.cpu_type, "corei7");
            assert_eq!(class.cpu_subtype, "rocketlake");
            // 名字层重算（与机器码比较同形）：两个表都必须能由本函数重现。
            assert!(cpu_is_holds("rocketlake", kind, &class));
            assert!(cpu_is_holds("corei7", kind, &class));
            assert!(!cpu_is_holds("graniterapids", kind, &class));
        }
    }
}
