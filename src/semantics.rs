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
    Scalar { size: usize },
    Array { elem_size: usize, count: Option<usize> },
    Pointer { pointee_size: usize },
    Aggregate { size: usize },
    Vector { elem_size: usize, count: usize },
    Complex { inner_size: usize },
    /// VLA: 元素大小已知, 但总大小/计数在运行时确定。
    Vla { elem_size: usize },
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
    /// 来自另一个表达式的地址 (递归, 如 (*p)[i], s.arr[i])
    ExprAddr { sub: Box<AddressDescriptor> },
}

/// 字节偏移: 可能是编译期常量, 也可能含运行时分量。
#[derive(Debug, Clone)]
pub enum ByteOffset {
    /// 编译期常量偏移
    Constant(i64),
    /// 运行时: index_reg * stride + constant_addend
    Runtime { stride: usize, addend: i64 },
    /// VLA 运行时 stride: stride 在运行时才知道 (需要从 VLA 元数据槽加载)
    VlaRuntime { addend: i64 },
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
    Array { elem_size: usize, count: Option<usize> },
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
pub fn compute_lvalue_address<R: ObjectResolver>(
    resolver: &R,
    expr: &Expr,
    ctx: &AddressContext,
) -> AddressDescriptor {
    match expr {
        Expr::Ident { name, ty, .. } => {
            let id = resolver.resolve_identity(&name.name);
            let storage = resolver.resolve_storage(&id, &name.name);
            address_from_storage(storage, ty)
        }

        Expr::Index { arr, idx, ty: elem_ty, .. } => {
            // arr[i] 的地址 = arr 的基址 + i * sizeof(elem_ty)
            let base_addr = compute_lvalue_address(resolver, arr, ctx);
            let idx_offset = compute_index_offset(idx, elem_ty);
            combine_address(base_addr, idx_offset, elem_ty)
        }

        Expr::Member {
            expr: base,
            field,
            is_arrow,
            ty: member_ty,
            ..
        } => {
            if *is_arrow {
                // ptr->field: 先加载指针值, 再加字段偏移
                let ptr_addr = compute_lvalue_address(resolver, base, ctx);
                let field_offset = member_field_offset(base, field);
                let combined = add_constant_offset(ptr_addr, field_offset);
                AddressDescriptor {
                    element_info: element_info_from_type(member_ty),
                    ..combined
                }
            } else {
                // s.field: 在结构体地址上加字段偏移
                let struct_addr = compute_lvalue_address(resolver, base, ctx);
                let field_offset = member_field_offset(base, field);
                let combined = add_constant_offset(struct_addr, field_offset);
                AddressDescriptor {
                    element_info: element_info_from_type(member_ty),
                    ..combined
                }
            }
        }

        Expr::Deref { e, ty, .. } => {
            // *p: p 的值就是目标地址。
            // 先计算 p 的地址, 然后加载指针值。
            let ptr_addr = compute_lvalue_address(resolver, e, ctx);
            AddressDescriptor {
                base: AddressBase::ExprAddr {
                    sub: Box::new(ptr_addr),
                },
                offset: ByteOffset::Constant(0),
                element_info: element_info_from_type(ty),
            }
        }

        Expr::Cast { e, ty, .. } => {
            // (T)e 作为 lvalue: 穿透 cast, 直接计算内层地址
            let inner = compute_lvalue_address(resolver, e, ctx);
            AddressDescriptor {
                element_info: element_info_from_type(ty),
                ..inner
            }
        }

        _ => {
            // 不应该到达这里: 只有 lvalue 表达式才会被传入。
            // 对于非 lvalue, 返回一个零偏移的占位描述符。
            AddressDescriptor {
                base: AddressBase::StackFrame { offset: 0 },
                offset: ByteOffset::Constant(0),
                element_info: ElementInfo {
                    size: expr.ty().size(),
                    kind: ElementKind::Scalar,
                },
            }
        }
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

    // 运行时偏移
    match elem_ty {
        // VLA 元素: stride 在运行时才知道
        _ if elem_ty.has_vla() => ByteOffset::VlaRuntime { addend: 0 },
        _ => ByteOffset::Runtime {
            stride,
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
        // 栈上数组: 取地址 (LEA FP+offset)
        (StorageClass::Stack, Type::Array(_, _)) | (StorageClass::Stack, Type::Vector(_, _)) => {
            AddressBase::StackFrame {
                offset: storage.stack_offset.unwrap_or(0),
            }
        }
        // 栈上标量/指针: 需要加载值 (LOAD [FP+offset])
        (StorageClass::Stack, _) => AddressBase::StackSlotLoad {
            offset: storage.stack_offset.unwrap_or(0),
        },
        // 全局数组: 取符号地址 (RIP-relative LEA)
        (StorageClass::GlobalData, Type::Array(_, _))
        | (StorageClass::GlobalData, Type::Vector(_, _))
        | (StorageClass::GlobalTLS, Type::Array(_, _))
        | (StorageClass::GlobalTLS, Type::Vector(_, _)) => AddressBase::GlobalSymbolAddr {
            symbol_key: storage.symbol_key.unwrap_or_default(),
        },
        // 全局标量/指针: 加载符号值
        (StorageClass::GlobalData, _) | (StorageClass::GlobalTLS, _) => {
            AddressBase::GlobalSymbolLoad {
                symbol_key: storage.symbol_key.unwrap_or_default(),
            }
        }
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

/// 两个字节偏移相加。
fn add_byte_offsets(a: ByteOffset, b: ByteOffset) -> ByteOffset {
    match (a, b) {
        (ByteOffset::Constant(x), ByteOffset::Constant(y)) => ByteOffset::Constant(x + y),
        (ByteOffset::Constant(c), ByteOffset::Runtime { stride, addend })
        | (ByteOffset::Runtime { stride, addend }, ByteOffset::Constant(c)) => {
            ByteOffset::Runtime {
                stride,
                addend: addend + c,
            }
        }
        (ByteOffset::Constant(c), ByteOffset::VlaRuntime { addend })
        | (ByteOffset::VlaRuntime { addend }, ByteOffset::Constant(c)) => {
            ByteOffset::VlaRuntime {
                addend: addend + c,
            }
        }
        // 两个运行时偏移不能简单合并 (需要两个寄存器)
        // 保留第一个, 第二个由物化阶段处理
        (other, _) => other,
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
        }
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
        }
        Type::Complex(_) => {
            if is_load {
                MaterializeAction::ComplexLoad
            } else {
                MaterializeAction::ComplexStore
            }
        }
        _ => {
            if is_load {
                MaterializeAction::LoadScalar
            } else {
                MaterializeAction::StoreScalar
            }
        }
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
        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => StorageType::Aggregate { size: ty.size() },
        _ => StorageType::Scalar { size: ty.size() },
    }
}
