//! C++ HIR → dcc typed IR lowering (native path)
//!
//! 职责:
//! - 把 HIR 表达式/语句降到 dcc SSA IR
//! - 使用 abi_layout 提供的布局描述符
//! - 不生成临时 C 文件，不走 C parser
//!
//! 当前阶段: Stage 3 基础 lowering。
//! 支持: 基本类型、变量、二元运算、赋值、调用、成员访问、
//! 取址/解引用、类型转换、控制流 (if/while)。

use std::collections::HashMap;

use crate::ast::{BinOp, FloatConst, QualType, QualifierFlags, Type, UnOp};
use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, ThisAdjust, VTableComponent};
use crate::cpp::hir::*;
use crate::cpp::sema::{Access, ClassId, CxxSymbol, CxxSymbolKind, CxxType, SymbolId};
use crate::diagnostics::Span;
use crate::ssa::{BasicBlock, Function, Inst, InstKind, Operand, Phi, SsaVar, Terminator};

/// `switch` 语句的标签收集结果: `(常量 case 值 → 目标 BB 名, default 目标 BB 名)`。
type SwitchTargets = (Vec<(i128, String)>, Option<String>);

// === LowerIrOutput ===

/// vtable 数据条目
#[derive(Debug, Clone)]
pub enum VTableEntry {
    /// i64 整数（offset-to-top 等）
    Int(i64),
    /// 全局符号地址（typeinfo、函数指针）
    SymAddr(String),
}

/// 需要发射**定义**的静态存储对象 (函数局部 `static` 对象 + 它的
/// thread-safe 初始化 guard 变量)。
///
/// 根因: `LowerIrOutput::globals` 只是一个名字列表, `native::build_program`
/// 从来不为它生成任何 `Decl::Var` —— native 路径因此根本没有全局/静态存储。
/// 后果不是编译失败而是**静默错值/段错误**: SSA→AST 重建出的
/// `__dcc_static_guard_0` 是个未声明标识符, C 前端按隐式 int **局部**处理,
/// `__cxa_guard_acquire` 于是收到一个未初始化的栈槽内容 (实测 iostream
/// 夹具在 `std::cout()` 里 SIGSEGV)。这里带上类型, 让发射端能分配真实存储。
#[derive(Debug, Clone)]
pub struct StaticObjectDef {
    /// 链接名 (`__dcc_static_<n>` / `__dcc_static_guard_<n>`)
    pub name: String,
    /// 已 ABI 降级的对象类型
    pub ty: Type,
}

/// Lowering 结果
#[derive(Debug, Default)]
pub struct LowerIrOutput {
    /// 生成的 SSA 函数
    pub functions: Vec<Function>,
    /// 生成的全局变量
    pub globals: Vec<String>,
    /// 需要发射定义的静态存储对象 (见 `StaticObjectDef`)
    pub static_objects: Vec<StaticObjectDef>,
    /// 生成的 vtable / RTTI 等只读数据（符号名列表）
    pub rodata: Vec<String>,
    /// vtable 全局数据（符号名 + 条目列表）
    pub vtables: Vec<(String, Vec<VTableEntry>)>,
    /// 需要导出的外部 runtime symbol（如 dccpp_*）
    pub external_symbols: Vec<String>,
}

/// 指针大小（x86_64 / LP64）
const POINTER_SIZE: u64 = 8;

/// 构造函数 variant 的链接名约定: `<class>_ctor_{complete,base,allocating}`.
///
/// 定义侧 (`IrLowerer::symbol_name` 对 `CxxSymbolKind::Constructor`) 与所有
/// 调用侧 (`lower_construct` / new 表达式 / 基类与成员初始化) 共用此函数,
/// 避免命名漂移导致 `undefined reference to <class>_ctor_complete`.
pub fn ctor_variant_name(class: &str, variant: CtorVariant) -> String {
    match variant {
        CtorVariant::Complete => format!("{}_ctor_complete", class),
        CtorVariant::Base => format!("{}_ctor_base", class),
        CtorVariant::Allocating => format!("{}_ctor_allocating", class),
    }
}

/// 成员函数 (非虚/虚) 的链接名约定: `<class>_<method>`.
///
/// 定义侧 (`symbol_link_name`)、调用侧 (`lower_call`) 与 vtable slot
/// (`native.rs` 注册方法时) 必须用同一个名字, 否则:
///   * 不同类的同名方法 (`PolyAnimal::speak` / `Dog::speak`) 会撞成同一个
///     符号 —— 多个 strong 定义 (multiple definition) 或 weak 合并后
///     vtable 全部指向其中一个实现;
///   * slot 里的地址与实际发射的函数符号对不上。
pub fn member_func_symbol(class: &str, method: &str) -> String {
    format!("{}_{}", class, method)
}

/// 析构函数 variant 的链接名约定: `<class>_dtor_{complete,base,deleting}`.
/// 与 `lower_destroy` / `lower_dtor_epilogue` 的调用侧同一约定。
pub fn dtor_variant_name(class: &str, variant: DtorVariant) -> String {
    match variant {
        DtorVariant::Complete => format!("{}_dtor_complete", class),
        DtorVariant::Base => format!("{}_dtor_base", class),
        DtorVariant::Deleting => format!("{}_dtor_deleting", class),
    }
}

/// 类符号的链接名 (命名空间内的类带 `ns_` 前缀, 全局类就是源名)。
fn class_symbol_link_name(symbols: &[CxxSymbol], id: SymbolId) -> Option<String> {
    symbols
        .get(id.0)
        .map(|c| c.linkage_name.clone().unwrap_or_else(|| c.name.clone()))
}

/// 一个符号在目标文件里实际使用的链接名 —— **唯一定义点**。
///
/// `IrLowerer::symbol_name` (定义/调用侧) 与 `native.rs` (vtable slot /
/// struct 字段 / 外部声明原型查找) 都走这里, 避免命名漂移。
pub fn symbol_link_name(symbols: &[CxxSymbol], sym: &CxxSymbol) -> String {
    // 显式 linkage_name 优先: 构造/析构的多个 ABI 变体 (C1/C2) 共享同一个
    // 所属类, 只能靠符号上的明确名字区分。
    if let Some(ln) = &sym.linkage_name {
        return ln.clone();
    }
    let base = sym.name.clone();
    let owner = sym.owner.and_then(|o| class_symbol_link_name(symbols, o));
    match sym.kind {
        CxxSymbolKind::Constructor => {
            ctor_variant_name(owner.as_deref().unwrap_or(&base), CtorVariant::Complete)
        },
        CxxSymbolKind::Destructor => {
            // sema 用 `~Class` 作为析构符号名; 调用侧约定是 `<class>_dtor_*`.
            let cls = owner
                .unwrap_or_else(|| base.strip_prefix('~').unwrap_or(base.as_str()).to_string());
            dtor_variant_name(&cls, DtorVariant::Complete)
        },
        CxxSymbolKind::Method => match owner {
            Some(c) => member_func_symbol(&c, &base),
            None => base,
        },
        _ => base,
    }
}

// === LowerIrError ===

/// Lowering 错误
#[derive(Debug, Clone)]
pub enum LowerIrError {
    /// 不支持的 HIR 节点（auto 模式下可 fallback 到 legacy）
    Unsupported(String),
    /// 内部错误
    Internal(String),
}

impl std::fmt::Display for LowerIrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerIrError::Unsupported(s) => write!(f, "unsupported HIR feature: {}", s),
            LowerIrError::Internal(s) => write!(f, "internal lowering error: {}", s),
        }
    }
}

impl std::error::Error for LowerIrError {}

// === IrLowerer ===

/// IR Lower
pub struct IrLowerer<'a> {
    /// 输入 HIR
    pub hir: &'a HirTranslationUnit,
    /// 符号表（从 Sema 提供）
    pub symbols: &'a [CxxSymbol],
    /// ABI 描述符表（class_id → ClassAbi）
    pub abis: HashMap<ClassId, ClassAbi>,
    /// 输出
    pub output: LowerIrOutput,
}

impl<'a> IrLowerer<'a> {
    pub fn new(hir: &'a HirTranslationUnit, symbols: &'a [CxxSymbol]) -> Self {
        Self {
            hir,
            symbols,
            abis: HashMap::new(),
            output: LowerIrOutput::default(),
        }
    }

    /// 注册一个类的 ABI 描述符
    pub fn register_abi(&mut self, class_id: ClassId, abi: ClassAbi) {
        self.abis.insert(class_id, abi);
    }

    /// vptr 的类型：*const u8（指向 vtable entry 的指针）
    pub fn vptr_ptr_type(&self) -> Type {
        // vptr 存储的是指向 vtable entry 的指针
        // 简化为 *const u8（字节指针），使用时再 cast
        Type::Ptr(Box::new(QualType::new(Type::UChar)))
    }

    /// vtable 条目的类型：*const u8（函数指针或其他 vtable 槽）
    pub fn vtable_entry_type(&self) -> Type {
        Type::Ptr(Box::new(QualType::new(Type::UChar)))
    }

    /// 执行 lowering
    pub fn lower(&mut self) -> Result<(), LowerIrError> {
        // 先收集所有 vtable 数据（从已注册的 ABI 描述符）
        self.emit_vtables();

        // 收集全局变量 (HIR 顶层声明中的 Variable)
        //
        // 其中带 `linkage_name` 的是**静态存储对象** (函数局部 static 与其
        // thread-safe guard): 除了登记名字, 还必须带上 ABI 降级后的类型, 由
        // native 发射端分配真实存储, 否则它们在目标文件里根本不存在。
        for decl in &self.hir.top_level {
            if let HirTopLevelDecl::Variable(sym_id) = decl {
                let sym = self.get_symbol(*sym_id)?;
                let name = self.symbol_name(sym);
                let need_definition =
                    matches!(sym.kind, CxxSymbolKind::Variable) && sym.linkage_name.is_some();
                let obj_ty = if need_definition {
                    Some(self.lower_type(&sym.ty)?)
                } else {
                    None
                };
                drop(sym);
                if !self.output.globals.contains(&name) {
                    self.output.globals.push(name.clone());
                }
                if let Some(ty) = obj_ty {
                    if !self.output.static_objects.iter().any(|o| o.name == name) {
                        self.output
                            .static_objects
                            .push(StaticObjectDef { name, ty });
                    }
                }
            }
        }

        for func in &self.hir.functions {
            let ssa_func = self.lower_function(func)?;
            self.output.functions.push(ssa_func);
        }
        Ok(())
    }

    /// 根据已注册的 ABI 描述符生成 vtable 数据。
    fn emit_vtables(&mut self) {
        // 按 ClassId 顺序收集（避免 HashMap 迭代顺序不确定）
        let mut class_ids: Vec<ClassId> = self.abis.keys().copied().collect();
        class_ids.sort_by_key(|c| c.0);

        for class_id in class_ids {
            let abi = match self.abis.get(&class_id) {
                Some(a) => a.clone(),
                None => continue,
            };

            for vtable in &abi.vtables {
                let mut entries = Vec::new();
                for comp in &vtable.components {
                    match comp {
                        VTableComponent::OffsetToTop(v) => entries.push(VTableEntry::Int(*v)),
                        VTableComponent::TypeInfo(sym) => {
                            entries.push(VTableEntry::SymAddr(sym.clone()));
                        },
                        VTableComponent::Function { symbol, .. } => {
                            entries.push(VTableEntry::SymAddr(symbol.clone()));
                        },
                        VTableComponent::VCallOffset(v) => entries.push(VTableEntry::Int(*v)),
                        VTableComponent::VBaseOffset { offset, .. } => {
                            entries.push(VTableEntry::Int(*offset));
                        },
                        VTableComponent::Unused => entries.push(VTableEntry::Int(0)),
                    }
                }
                self.output.vtables.push((vtable.symbol.clone(), entries));
                if !self.output.rodata.contains(&vtable.symbol) {
                    self.output.rodata.push(vtable.symbol.clone());
                }
            }
        }
    }

    /// 消费 lowerer，返回输出
    pub fn finish(self) -> LowerIrOutput {
        // 在所有 lowering 完成后，统一注册 external symbols
        // (实际上已经在调用过程中注册了)
        self.output
    }

    // --- helpers ---

    fn get_symbol(&self, id: SymbolId) -> Result<&CxxSymbol, LowerIrError> {
        self.symbols
            .get(id.0)
            .ok_or_else(|| LowerIrError::Internal(format!("symbol {} not found", id.0)))
    }

    /// 获取符号的有效名称（优先使用 linkage_name，否则用 name）
    ///
    /// 构造函数特殊处理: 用户写出的 ctor 定义在 dcc native ABI 里就是
    /// complete-object 构造, 其链接名为 `<class>_ctor_complete` —— 与所有
    /// 调用侧 (lower_construct / new 表达式 / 成员默认构造) 使用同一个约定,
    /// 否则调用侧引用 `<class>_ctor_complete` 而定义侧发 `<class>`.
    fn symbol_name(&self, sym: &CxxSymbol) -> String {
        symbol_link_name(self.symbols, sym)
    }

    /// 函数类型出现在**值**上下文（[conv.func] 退化后）时的 `ast::Type` 形态。
    ///
    /// C 前端把 `Expr::Ident{ty: Type::Func(..)}` 认定为「函数名退化成的地址」
    /// （`gen_expr_ident_rvalue` / `gen_expr_addrof` 都按 `Type::Func` 判定并
    /// 发 `lea sym(%rip)`）；而 `Type::Ptr(Void)` 会被当成普通标量变量 —— 名字
    /// 既不在 `var_map` 也不在 `globals_raw` 时退化成「隐式局部变量」栈槽，
    /// 读到未初始化内存。`lower_type(CxxType::Function)` 为了 ABI 尺寸统一
    /// 降成 `void*`，所以值上下文必须用这个与形式不同、尺寸相同（8 字节，
    /// 见 `size_of_type` 的 `Type::Func => POINTER_SIZE`）的形态。
    /// 形参/返回不可降级时退化为 `void`（只影响类型标注，不影响地址发射）。
    pub fn lower_func_value_type(&self, ty: &CxxType) -> Type {
        let (ret, params) = match ty {
            CxxType::Function { ret, params, .. } => (ret, params),
            _ => return Type::Func(Box::new(QualType::new(Type::Void)), Vec::new()),
        };
        Type::Func(
            Box::new(QualType::new(self.lower_type(ret).unwrap_or(Type::Void))),
            params
                .iter()
                .map(|p| QualType::new(self.lower_type(p).unwrap_or(Type::Void)))
                .collect(),
        )
    }

    /// 符号是否是「函数名可退化为函数指针」的函数（[conv.func] 的定义域）。
    ///
    /// 真源仍是 sema 的符号表（kind + 类型），不按符号名/夹具名匹配。
    pub fn is_function_value_symbol(sym: &CxxSymbol) -> bool {
        matches!(sym.kind, CxxSymbolKind::Function | CxxSymbolKind::Method)
            || matches!(sym.ty, CxxType::Function { .. })
    }

    // === type conversion: CxxType -> ast::Type ===

    /// 把 C++ 类型降到 ast::Type（ABI 降级后）。
    pub fn lower_type(&self, ty: &CxxType) -> Result<Type, LowerIrError> {
        match ty {
            CxxType::Void => Ok(Type::Void),
            CxxType::Bool => Ok(Type::Bool),
            CxxType::Integer { bits, signed } => self.lower_int_type(*bits, *signed),
            CxxType::Float { bits } => match bits {
                32 => Ok(Type::Float),
                64 => Ok(Type::Double),
                _ => Err(LowerIrError::Unsupported(format!("float with {} bits", bits))),
            },
            CxxType::Pointer(inner) => {
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Ptr(Box::new(QualType::new(inner_ty))))
            },
            CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                // ABI 降级：引用 → 指针
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Ptr(Box::new(QualType::new(inner_ty))))
            },
            CxxType::Array(inner, Some(n)) => {
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Array(Box::new(QualType::new(inner_ty)), crate::ast::ArraySize::Const(*n)))
            },
            CxxType::Array(inner, None) => {
                // 未指定大小的数组 → 指针（C 语义退化）
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Ptr(Box::new(QualType::new(inner_ty))))
            },
            CxxType::Function { ret: _, params, .. } => {
                // 函数类型 → 函数指针
                for _p in params {
                    // 验证所有参数类型都可降级（确保类型有意义）
                }
                // dcc ast::Type 没有单独的函数类型，用指针表示
                // 这里降级为 void* 指针（函数指针的 ABI 表示）
                // TODO(Stage 4): 引入 Proper function type
                Ok(Type::Ptr(Box::new(QualType::new(Type::Void))))
            },
            CxxType::Class(id) => {
                let abi = self.abis.get(id).ok_or_else(|| {
                    eprintln!(
                        "ERROR: ABI not found for class {:?}. Available ABIs: {:?}",
                        id,
                        self.abis.keys()
                    );
                    LowerIrError::Internal(format!("ABI for class {} not found", id.0))
                })?;
                self.lower_class_type(id, abi)
            },
            CxxType::Enum(_id) => {
                // 枚举 → int
                // TODO(Stage 4): 根据枚举底层类型选择
                Ok(Type::Int)
            },
            CxxType::MemberPointer { .. } => {
                Err(LowerIrError::Unsupported("member pointer type".to_string()))
            },
            CxxType::Dependent(s) => {
                Err(LowerIrError::Unsupported(format!("dependent type: {}", s)))
            },
            CxxType::Alias(id) => {
                let sym = self.get_symbol(*id)?;
                self.lower_type(&sym.ty)
            },
        }
    }

    fn lower_int_type(&self, bits: u16, signed: bool) -> Result<Type, LowerIrError> {
        match (bits, signed) {
            (8, true) => Ok(Type::SChar),
            (8, false) => Ok(Type::UChar),
            (16, true) => Ok(Type::Short),
            (16, false) => Ok(Type::UShort),
            (32, true) => Ok(Type::Int),
            (32, false) => Ok(Type::UInt),
            (64, true) => Ok(Type::LongLong),
            (64, false) => Ok(Type::ULongLong),
            (128, true) => Ok(Type::Int128),
            (128, false) => Ok(Type::UInt128),
            _ => Err(LowerIrError::Unsupported(format!(
                "integer with {} bits (signed={})",
                bits, signed
            ))),
        }
    }

    fn lower_class_type(&self, id: &ClassId, abi: &ClassAbi) -> Result<Type, LowerIrError> {
        let is_union = is_union_layout(abi);

        let sym_id = id.as_symbol();
        let sym = self.get_symbol(sym_id)?;
        let name = sym.name.clone();
        let align = abi.align as usize;

        // 收集所有字段（包括从基类继承的，展平到一个 struct 中）
        let mut fields = Vec::new();
        self.collect_flattened_fields(id, abi, 0, &mut fields)?;

        use std::sync::Arc;
        let fields_arc = Arc::new(fields);

        if is_union {
            Ok(Type::Union(name, fields_arc, align, false))
        } else {
            Ok(Type::Struct(name, fields_arc, align, false))
        }
    }

    /// 递归收集类及其所有基类的字段，展平成单一字段列表。
    /// 基类字段按基类偏移重新计算最终偏移。
    /// Stage 6: 也包含虚基类字段。
    ///
    /// 展平顺序与偏移全部由布局层 ([`ClassAbi::flatten_fields`]) 决定，
    /// 这里只把 `FlatField` 翻译成 `ast::Field`（补上类型）。
    fn collect_flattened_fields(
        &self,
        _class_id: &ClassId,
        abi: &ClassAbi,
        base_offset: u64,
        fields: &mut Vec<crate::ast::Field>,
    ) -> Result<(), LowerIrError> {
        for f in abi.flatten_fields(&self.abis) {
            let field_ty = self.field_type_by_name(f.owner, f.name.as_str())?;
            fields.push(crate::ast::Field {
                name: f.name.clone(),
                ty: field_ty,
                offset: (base_offset + f.offset) as usize,
                bit_width: None,
                bit_offset: 0,
                quals: crate::ast::QualifierFlags::NONE,
                ptr_quals: Vec::new(),
                field_align: None,
                field_packed: false,
            });
        }
        Ok(())
    }

    /// 在类中查找基类布局（支持直接基类和间接基类）。
    pub fn find_base_layout(
        &self,
        class_id: ClassId,
        base_id: ClassId,
    ) -> Result<Option<BaseLayout>, LowerIrError> {
        let abi = self.abis.get(&class_id).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", class_id.0))
        })?;
        // 直接基类
        for base in &abi.bases {
            if base.base == base_id {
                return Ok(Some(base.clone()));
            }
        }
        // 递归查找间接基类
        for base in &abi.bases {
            if let Some(indirect) = self.find_base_layout(base.base, base_id)? {
                // 累积偏移
                return Ok(Some(BaseLayout {
                    base: base_id,
                    offset: base.offset + indirect.offset,
                    is_primary: base.is_primary && indirect.is_primary,
                }));
            }
        }
        Ok(None)
    }

    /// 基类子对象在 `class_id` 对象里的字节偏移（包括**虚基类**）。
    ///
    /// 与 `find_base_layout` 的区别: 后者只回答非虚继承链上的固定偏移（用于
    /// 非虚基的 this-adjust，虚基必须走运行期 vbtable），而构造函数里
    /// 「最派生对象直接构造它的虚基子对象」用的就是布局层 Step 2.5 算好的
    /// 尾部静态偏移 (`VirtualBaseLayout::static_offset`)，所以这里也查它。
    pub fn base_subobject_offset(
        &self,
        class_id: ClassId,
        base_id: ClassId,
    ) -> Result<Option<u64>, LowerIrError> {
        if let Some(b) = self.find_base_layout(class_id, base_id)? {
            return Ok(Some(b.offset));
        }
        let abi = self.abis.get(&class_id).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", class_id.0))
        })?;
        Ok(abi
            .virtual_bases
            .iter()
            .find(|vb| vb.base == base_id)
            .map(|vb| vb.static_offset))
    }

    /// 检查字段是否属于某个基类（非当前类自身字段）。
    fn field_belongs_to_base(&self, _class_id: ClassId, _field_name: &str) -> Option<ClassId> {
        // Stage 4 简化：字段展平后统一通过 MemberLoad 访问，
        // 不需要区分是否属于基类。
        None
    }

    /// 查字段在对象里的偏移 (字节)。布局层是唯一真源。
    fn field_offset_in_class(&self, class_id: ClassId, name: &str) -> Result<u64, LowerIrError> {
        let abi = self.abis.get(&class_id).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", class_id.0))
        })?;
        abi.flatten_fields(&self.abis)
            .into_iter()
            .find(|f| f.name == name)
            .map(|f| f.offset)
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "field '{}' not in layout of class {}",
                    name, class_id.0
                ))
            })
    }

    fn field_type_by_name(&self, class_id: ClassId, name: &str) -> Result<Type, LowerIrError> {
        // 通过符号表查询字段类型，而不是用 Int 占位
        let owner_sym = class_id.as_symbol();
        for sym in self.symbols {
            if sym.owner == Some(owner_sym)
                && sym.name == name
                && matches!(sym.kind, CxxSymbolKind::Field | CxxSymbolKind::Variable)
            {
                return self.lower_type(&sym.ty);
            }
        }
        // 兜底：字段不在符号表中时用 Int（保持旧行为）
        Ok(Type::Int)
    }
}

/// 具名引用变量 (`int& r = x;` / 形参 `T& p`) 的**所指标量类型**。
///
/// 返回 `Some` 的条件刻意限制在非聚合所指类型: 聚合 (class/数组) 引用已经有
/// 一套按「地址」使用成员访问的约定 (`deref_object_for_member` 走 arrow 基址),
/// 在那里插入整聚合载入会回归。见 `lower_expr` / `lower_assign` 的调用点。
fn scalar_ref_referent(ty: &CxxType) -> Option<CxxType> {
    match ty {
        CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => match inner.as_ref() {
            CxxType::Class(_)
            | CxxType::Array(..)
            | CxxType::Function { .. }
            | CxxType::MemberPointer { .. }
            | CxxType::Dependent(_)
            | CxxType::Alias(_)
            | CxxType::Void => None,
            other => Some(other.clone()),
        },
        _ => None,
    }
}

/// 取成员访问基对象类型里的类 id: `Class(C)` / `C*` / `C&` 都归到 `C`。
fn class_of_object_type(ty: &CxxType) -> Option<ClassId> {
    match ty {
        CxxType::Class(id) => Some(*id),
        CxxType::Pointer(inner) | CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
            match inner.as_ref() {
                CxxType::Class(id) => Some(*id),
                other => class_of_object_type(other),
            }
        },
        _ => None,
    }
}

fn is_union_layout(abi: &ClassAbi) -> bool {
    if abi.fields.is_empty() {
        return false;
    }
    abi.fields.iter().all(|f| f.offset == 0)
}

/// 计算 ast::Type 的大小（字节），用于异常分配等场景。
fn type_size(ty: &Type) -> u64 {
    match ty {
        Type::Void => 0,
        Type::Bool => 1,
        Type::Char | Type::SChar | Type::UChar | Type::Char8 => 1,
        Type::Char16 => 2,
        Type::Char32 => 4,
        Type::Short | Type::UShort => 2,
        Type::Int | Type::UInt => 4,
        Type::Long | Type::ULong => 8,
        Type::LongLong | Type::ULongLong => 8,
        Type::Int128 | Type::UInt128 => 16,
        Type::Float => 4,
        Type::Double => 8,
        Type::LongDouble => 16,
        Type::Fract | Type::Accum => 4, // 简化
        Type::Ptr(_) => POINTER_SIZE,
        Type::Array(inner, crate::ast::ArraySize::Const(n)) => type_size(&inner.base) * (*n as u64),
        Type::Array(_, _) => POINTER_SIZE,
        Type::Vector(inner, n) => type_size(&inner.base) * (*n as u64),
        Type::Struct(_name, fields, _align, _) => {
            let mut max_end: u64 = 0;
            for f in fields.iter() {
                let fsize = type_size(&f.ty);
                let end = f.offset as u64 + fsize;
                if end > max_end {
                    max_end = end;
                }
            }
            max_end.max(1)
        },
        Type::Union(_name, fields, _align, _) => {
            let mut max_size: u64 = 0;
            for f in fields.iter() {
                let fsize = type_size(&f.ty);
                if fsize > max_size {
                    max_size = fsize;
                }
            }
            max_size.max(1)
        },
        Type::Enum(_, _, Some(underlying)) => type_size(&underlying.base),
        Type::Enum(_, _, None) => 4,
        Type::Func(_, _) => POINTER_SIZE,
        Type::Named(_) => POINTER_SIZE, // 简化
        Type::Atomic(inner) => type_size(&inner.base),
        Type::Complex(inner) => type_size(&inner.base) * 2,
        Type::Imaginary(inner) => type_size(&inner.base),
    }
}

// === Function lowerer ===

/// 一个 `switch` 的 `case`/`default` 标签 → 入口块名 **精确** 映射。
///
/// 为什么需要它 (根因修复): 块名由
/// [`FuncLowerer::unique_bb`] 生成为 `case_<值>.<计数器>`,
/// 而旧实现在下跳阶段用 `ssa.blocks.keys().find(|k|
/// k.starts_with("case_<值>"))` 反查块。`blocks` 是
/// `HashMap` (迭代序随进程随机), 匹配又是 **前缀** 匹配, 于是
/// `case_1` 会命中 `case_11.x`: 含 ≥ 10 个 case 的函数每次编译可能把
/// 不同的 case 接到同一入口 —— 静默错编译 (无诊断、可重现率随机)。
///
/// 现在反查全部取消: `collect_switch_cases` 在建块时就记下
/// 值 → 块名, 下跳阶段只直查本表。嵌套 `switch` 各自持有自己的
/// 一张表 (作为参数下传), 内层不会误命中外层标签。
pub struct SwitchLabels {
    /// `case` 常量值 → 入口块名 (重复值按源顺序保留首个)
    cases: HashMap<i128, String>,
    /// `default` 入口块名
    default_bb: Option<String>,
}

impl SwitchLabels {
    fn new(cases: Vec<(i128, String)>, default_bb: Option<String>) -> Self {
        let mut map: HashMap<i128, String> = HashMap::with_capacity(cases.len());
        for (v, bb) in cases {
            map.entry(v).or_insert(bb);
        }
        Self {
            cases: map,
            default_bb,
        }
    }

    fn lookup(&self, value: i128) -> Option<&String> {
        self.cases.get(&value)
    }
}

/// 单个函数的 lowering 上下文
struct FuncLowerer<'a, 'b> {
    lowerer: &'a mut IrLowerer<'b>,
    func: &'a HirFunction,
    ssa: Function,
    /// 当前正在构建的基本块
    current_bb: String,
    /// 局部变量名 -> 当前 SSA 变量版本
    locals: HashMap<String, SsaVar>,
    /// 返回块名称
    return_bb: String,
    /// SSA 版本计数器（每个基名一个）
    versions: HashMap<String, u32>,
    /// 基名 -> 该基名已承载过的类型形状。
    ///
    /// SSA 临时值以 **base 名** 为单位共享一个 C 局部变量
    /// (`ssa::rebuild_block` 的 `all_def_order`/`all_def_types`, 首个定义的类型胜出),
    /// 所以同一 base 下混入两种不同 `ty` 会让后一种被前一种的存储宽度截断。
    /// 见 `dedup_base` 的详细说明。
    base_types: HashMap<String, Vec<Type>>,
    /// BB 计数器
    bb_counter: u32,
    /// break 目标栈（循环/switch 退出块）
    break_stack: Vec<String>,
    /// continue 目标栈（循环继续块）
    continue_stack: Vec<String>,
    /// label 名 -> BB 名映射（支持 goto 前向引用）
    label_map: HashMap<String, String>,
    /// init_list 临时变量计数器
    init_list_counter: u32,
    /// M53: 引用形参绑定右值时物化的临时量计数器 (每个函数独立)
    ref_temp_counter: u32,
    /// 合成临时值 (`fresh_syn`) 的全局序号, 每个函数独立
    temp_seq: u32,
    /// 短路求值 (`&&` / `||`) 结果变量的全局序号, 每个函数独立
    logical_seq: u32,
}

impl<'a, 'b> FuncLowerer<'a, 'b> {
    fn new(lowerer: &'a mut IrLowerer<'b>, func: &'a HirFunction) -> Result<Self, LowerIrError> {
        let sym = lowerer.get_symbol(func.symbol)?;
        let fn_name = lowerer.symbol_name(sym);
        let ret_ty = lowerer.lower_type(&func.ret_ty)?;
        let span = dummy_span();

        let mut ssa = Function::new(fn_name, ret_ty.clone(), span);

        // 返回块 (void 函数或空函数收尾用)
        let return_bb = ssa.unique_bb("return");

        let mut locals = HashMap::new();
        let mut versions = HashMap::new();

        // 成员函数/构造函数/析构函数：在 ssa.params 前注入隐式 this 形参
        // (sema.build_function 仅把显式形参放入 HirFunction.params,
        // 但 lower_ir 的 this 访问和成员调用都期望 ssa.params[0] == this).
        let needs_this = matches!(
            sym.kind,
            CxxSymbolKind::Method | CxxSymbolKind::Constructor | CxxSymbolKind::Destructor
        ) && sym.owner.is_some();
        if needs_this {
            let this_class = ClassId::from(sym.owner.unwrap());
            // 如果 ABI 不存在，尝试从第一个参数（this 指针）的类型中获取类 ID
            let actual_this_class = if !lowerer.abis.contains_key(&this_class) {
                // 寻找类型为 Pointer(Class(_)) 的参数作为 this
                let mut found_class_id = None;
                for param_sym_id in &func.params {
                    let param_sym = lowerer.get_symbol(*param_sym_id)?;
                    if let CxxType::Pointer(inner) = &param_sym.ty {
                        if let CxxType::Class(cid) = inner.as_ref() {
                            found_class_id = Some(*cid);
                            break;
                        }
                    }
                }
                match found_class_id {
                    Some(cid) => cid,
                    None => {
                        return Err(LowerIrError::Internal(
                            format!(
                                "Cannot determine class for constructor/method: owner {:?} has no ABI, and no Class pointer param found",
                                sym.owner
                            )
                        ));
                    },
                }
            } else {
                this_class
            };

            let this_ty = lowerer
                .lower_type(&CxxType::Pointer(Box::new(CxxType::Class(actual_this_class))))?;
            let this_var = SsaVar {
                base: "this".to_string(),
                version: 0,
                ty: this_ty.clone(),
                span: dummy_span(),
            };
            ssa.params.push(this_var.clone());
            ssa.local_types.insert("this".to_string(), this_ty);
            ssa.original_locals.insert("this".to_string());
            ssa.local_quals
                .insert("this".to_string(), QualifierFlags::NONE);
            locals.insert("this".to_string(), this_var);
            versions.insert("this".to_string(), 0);
        }

        // 参数映射
        for (i, param_sym_id) in func.params.iter().enumerate() {
            let param_sym = lowerer.get_symbol(*param_sym_id)?;
            let param_ty = lowerer.lower_type(&param_sym.ty)?;
            let var = SsaVar {
                base: param_sym.name.clone(),
                version: 0,
                ty: param_ty.clone(),
                span: dummy_span(),
            };
            ssa.params.push(var.clone());
            ssa.local_types.insert(param_sym.name.clone(), param_ty);
            ssa.original_locals.insert(param_sym.name.clone());
            let param_quals = if param_sym.is_volatile {
                QualifierFlags::VOLATILE
            } else {
                QualifierFlags::NONE
            };
            ssa.local_quals.insert(param_sym.name.clone(), param_quals);
            locals.insert(param_sym.name.clone(), var);
            versions.insert(param_sym.name.clone(), 0);
            let _ = i;
        }

        let current_bb = ssa.entry.clone();

        // 预登记所有已有明确类型的局部名 (this / 形参 / original locals):
        // `rebuild_block` 取声明类型时 `local_types` 优先于指令推导, 因此这些
        // 名字一旦被合成临时值复用而又形状不同, 就会产生静默截断。预登记后
        // `dedup_base` 会让合成临时值改用别的 base。
        let mut base_types: HashMap<String, Vec<Type>> = HashMap::new();
        for (n, t) in ssa.local_types.iter() {
            base_types.insert(n.clone(), vec![t.clone()]);
        }

        Ok(Self {
            lowerer,
            func,
            ssa,
            current_bb,
            locals,
            return_bb,
            versions,
            base_types,
            bb_counter: 0,
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            label_map: HashMap::new(),
            init_list_counter: 0,
            ref_temp_counter: 0,
            temp_seq: 0,
            logical_seq: 0,
        })
    }

    fn finish(mut self) -> Result<Function, LowerIrError> {
        // 如果 entry 块还没有 terminator（比如空函数），跳到 return
        let entry_term = self.ssa.blocks[&self.ssa.entry].term.clone();
        if matches!(entry_term, Terminator::Unreachable) {
            let ret_bb = self.return_bb.clone();
            self.set_term(Terminator::Jump(ret_bb));
        }

        // 构建 return 块
        self.build_return_block();

        // 计算 CFG 边
        self.compute_cfg_edges();

        Ok(self.ssa)
    }

    fn build_return_block(&mut self) {
        let bb_name = self.return_bb.clone();
        // ret_val 已移除: 非 void 函数的 return 语句直接生成 Ret terminator,
        // return_bb 仅用于 void 函数/隐式 fall-through.
        let term = Terminator::Ret { val: None };
        self.ssa.blocks.get_mut(&bb_name).unwrap().term = term;
    }

    fn compute_cfg_edges(&mut self) {
        // 清空旧的 preds/succs
        let bb_names: Vec<String> = self.ssa.blocks.keys().cloned().collect();
        for bb in &bb_names {
            self.ssa.blocks.get_mut(bb).unwrap().preds.clear();
            self.ssa.blocks.get_mut(bb).unwrap().succs.clear();
        }

        for bb in bb_names {
            let term = self.ssa.blocks[&bb].term.clone();
            let succs: Vec<String> = match &term {
                Terminator::Ret { .. } | Terminator::Unreachable => Vec::new(),
                Terminator::Jump(t) => vec![t.clone()],
                Terminator::Branch { t, f, .. } => vec![t.clone(), f.clone()],
                Terminator::SwitchTable {
                    idx: _,
                    values: _,
                    targets,
                    default,
                } => {
                    let mut v = targets.clone();
                    v.push(default.clone());
                    v
                },
            };

            let bb_clone = bb.clone();
            self.ssa.blocks.get_mut(&bb).unwrap().succs = succs.clone();

            for succ in succs {
                if self.ssa.blocks.contains_key(&succ) {
                    self.ssa
                        .blocks
                        .get_mut(&succ)
                        .unwrap()
                        .preds
                        .push(bb_clone.clone());
                }
            }
        }
    }

    // === BB helpers ===

    fn unique_bb(&mut self, prefix: &str) -> String {
        let name = format!("{}.{}", prefix, self.bb_counter);
        self.bb_counter += 1;
        self.ssa.add_block(name.clone());
        name
    }

    fn set_term(&mut self, term: Terminator) {
        let bb = self.current_bb.clone();
        self.ssa.blocks.get_mut(&bb).unwrap().term = term;
    }

    fn current_block(&self) -> &BasicBlock {
        &self.ssa.blocks[&self.current_bb]
    }

    /// Collect all blocks whose terminator is Unreachable.
    /// Used to find implicit fall-through exits after lowering a statement.
    fn unreachable_blocks(&self) -> Vec<String> {
        let mut result = Vec::new();
        for (name, bb) in &self.ssa.blocks {
            if matches!(bb.term, Terminator::Unreachable) {
                result.push(name.clone());
            }
        }
        result
    }

    fn push_inst(&mut self, inst: Inst) {
        let bb = self.current_bb.clone();
        self.ssa.blocks.get_mut(&bb).unwrap().insts.push(inst);
    }

    // === SSA var helpers ===

    fn fresh_var(&mut self, base: &str, ty: Type) -> SsaVar {
        let base = self.dedup_base(base, &ty);
        let version = self.versions.get(&base).copied().unwrap_or(0);
        self.versions.insert(base.clone(), version + 1);
        SsaVar {
            base,
            version,
            ty,
            span: dummy_span(),
        }
    }

    /// 生产一个**合成临时值**的 SSA 变量 (与 `fresh_var` 相对)。
    ///
    /// 根因 (三个叠加, 都来自 `ssa::rebuild_block` 的 SSA->C 局部坍缩规则):
    /// 1. 回写 C AST 时只取 `base` 名 (`resolve_operand` / `emit_inst_resolved`
    ///    用 `d.base` 而不是 `d.name()`), 因此同一 base 的不同 SSA 版本会**坍缩
    ///    成同一个 C 变量**。这对源码局部变量 (需要"破坏性更新"语义) 是对的,
    ///    但合成临时值经常有多个同 base 版本同时活跃 —— 二元运算的两个操作数
    ///    就是典型:
    ///    ```text
    ///    field = p->a; field = p->b;   // 坍缩后同一个局部
    ///    if (field <= field)           // => 恒真 (实测 p->a <= p->b 算错)
    ///    ```
    /// 2. 同一 base 的存储类型取首个定义 (`all_def_types` 首值胜出), 于是指针
    ///    会被前面的 int 定义按 4 字节截断 (见 `dedup_base`)。
    /// 3. `rebuild_block` 对 `_` 开头的名字**跳过声明** (假定它们一定被单次使用
    ///    内联掉), 一旦没被内联就变成隐式量, 读值/回边全部错乱。
    ///
    /// 所以合成临时值的 base 必须**全局唯一且不带下划线前缀**: 三个问题一次消除,
    /// 而且每个临时值都有独立栈槽与独立类型。重名候选 (例如用户变量恰好叫
    /// `dcc_t1_field`) 直接跳过。用户局部变量仍走 `fresh_var`, 命名与栈布局不变。
    fn fresh_syn(&mut self, base: &str, ty: Type) -> SsaVar {
        loop {
            self.temp_seq += 1;
            let cand = format!("dcc_t{}_{}", self.temp_seq, base);
            if !self.base_types.contains_key(&cand) {
                self.base_types.insert(cand.clone(), vec![ty.clone()]);
                return SsaVar {
                    base: cand,
                    version: 0,
                    ty,
                    span: dummy_span(),
                };
            }
        }
    }

    /// 求一个**全局符号的地址** (vtable / typeinfo / 静态局部等)。
    ///
    /// 不能用 `Operand::SymAddr`: 它在 SSA→AST 重建时生成 `Expr::Ident{ty}`,
    /// codegen 按标量全局**取值** (`mov sym(%rip),%rax`)。而 native 发射的
    /// vtable/typeinfo 都是 `void*[]` **数组**全局, 数组名在表达式里应衰减为
    /// 地址; codegen 不看声明类型只看 `ty` 标注, 于是取到第 0 个槽的内容。
    /// `InstKind::AddrOf` 重建为 `&sym` → `lea sym(%rip)`, 才是符号地址。
    fn lower_sym_addr(&mut self, sym: String, ptr_ty: Type) -> Operand {
        let dest = self.fresh_syn("sym_addr", ptr_ty);
        self.push_inst(Inst {
            dest: Some(dest.clone()),
            kind: InstKind::AddrOf { base: sym },
            span: dummy_span(),
        });
        Operand::Var(dest)
    }

    /// 把值写回一个**静态存储对象** (`lname` 为其链接名)。
    ///
    /// 形状与 volatile 标量完全一致 (`AddrOf → Store`), 只是基址是全局符号而不是
    /// 栈上局部: `InstKind::AddrOf` 重建为 `&sym` => `lea sym(%rip)`。
    /// 聚合类型 (class/union/数组) 的整体赋值需要结构体拷贝, 本后端的 `Store`
    /// 不携带结构体宽度语义, 因此明确报 unsupported (不静默降级为按标量存)。
    fn lower_static_storage_store(
        &mut self,
        lname: String,
        ty: Type,
        src: Operand,
    ) -> Result<Operand, LowerIrError> {
        if matches!(ty, Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
            return Err(LowerIrError::Unsupported(format!(
                "assignment to static aggregate object '{}' by value",
                lname
            )));
        }
        let ptr_ty = Type::Ptr(Box::new(QualType::new(ty.clone())));
        let addr = self.lower_sym_addr(lname, ptr_ty);
        let val = self.fresh_syn("static_store", ty.clone());
        self.push_inst(Inst {
            dest: Some(val.clone()),
            kind: InstKind::Cast {
                ty: ty.clone(),
                e: src,
            },
            span: dummy_span(),
        });
        self.push_inst(Inst {
            dest: None,
            kind: InstKind::Store {
                e: addr,
                src: Operand::Var(val.clone()),
                is_volatile: false,
            },
            span: dummy_span(),
        });
        Ok(Operand::Var(val))
    }

    /// 静态存储对象上的自增/自减: `AddrOf → Load → Bin → Store`。
    ///
    /// 与 `lower_incdec_local` 的 volatile 分支同构, 但必须走全局符号:
    /// 静态存储的旧值不在本函数的 SSA 里 (也不应在 —— 它要能跳调用/跳函数
    /// 保持), 旧实现只查 `self.locals` 于是直接 `local variable not found`.
    fn lower_incdec_static_storage(
        &mut self,
        lname: String,
        op: UnaryOp,
        ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use UnaryOp::*;
        if matches!(ty, Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
            return Err(LowerIrError::Unsupported(
                "increment/decrement of static aggregate object".to_string(),
            ));
        }
        let ptr_ty = Type::Ptr(Box::new(QualType::new(ty.clone())));
        let addr = self.lower_sym_addr(lname, ptr_ty);
        let old = self.fresh_syn("static_load", ty.clone());
        self.push_inst(Inst {
            dest: Some(old.clone()),
            kind: InstKind::Load {
                e: addr.clone(),
                ty: ty.clone(),
                is_volatile: false,
            },
            span: dummy_span(),
        });
        let ast_op = match op {
            PreInc | PostInc => BinOp::Add,
            PreDec | PostDec => BinOp::Sub,
            _ => unreachable!(),
        };
        let new_val = self.fresh_syn("static_inc", ty.clone());
        self.push_inst(Inst {
            dest: Some(new_val.clone()),
            kind: InstKind::Bin {
                op: ast_op,
                l: Operand::Var(old.clone()),
                r: Operand::Int(1),
            },
            span: dummy_span(),
        });
        self.push_inst(Inst {
            dest: None,
            kind: InstKind::Store {
                e: addr,
                src: Operand::Var(new_val.clone()),
                is_volatile: false,
            },
            span: dummy_span(),
        });
        if matches!(op, PreInc | PreDec) {
            Ok(Operand::Var(new_val))
        } else {
            // 后置形态的值是**修改前**的旧值
            let post = self.fresh_syn("static_postinc", ty.clone());
            self.push_inst(Inst {
                dest: Some(post.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(old),
                    r: Operand::Int(0),
                },
                span: dummy_span(),
            });
            Ok(Operand::Var(post))
        }
    }

    /// 为 `base` 选一个与 `ty` 形状相容的基名。
    ///
    /// 背景 (根因): SSA -> C 局部变量的重建在 `ssa::rebuild_block` 里按
    /// **base 名** 收集 `all_def_order`/`all_def_types`, 同一 base 的多个 SSA
    /// 版本共用一个 C 局部变量, 其类型取首个定义 (`or_insert` 语义, 首个胜出)。
    /// 而 `fresh_var` 的合成基名 (`call_result` / `field` / `cast` / `deref` …)
    /// 在同一函数里会被反复使用且类型各不相同, 于是后面的定义被前面的存储
    /// 宽度截断。实测:
    /// ```text
    /// int v = f();                        // call_result  : int (4B)
    /// G* p = (G*)malloc(sizeof(G));       // 同一个 int 局部 => 指针只留低 32 位
    /// p->a = 5;                           // movslq 符号扩展 => 野指针 => SIGSEGV
    /// ```
    /// 即 `mov %eax,-0x58(%rbp)` + `movslq -0x58(%rbp),%rax`, 160(0xa0) 之类的
    /// 截断地址写回, 直接段错误。
    ///
    /// 这里在生产端保证「一个 base 只承载一种形状」: 形状相同照旧复用 base,
    /// 形状不同则改用 `base_t1`/`base_t2`… 作为新 base。用户局部变量类型唯一,
    /// base 名保持不变 => 现有代码的局部变量命名、声明序与栈槽偏移都不受影响。
    fn dedup_base(&mut self, base: &str, ty: &Type) -> String {
        // 先登记/比对原 base
        match self.base_types.get_mut(base) {
            None => {
                self.base_types.insert(base.to_string(), vec![ty.clone()]);
                return base.to_string();
            },
            Some(list) => {
                if list.iter().any(|t| t == ty) {
                    return base.to_string();
                }
            },
        }
        let mut idx = 1usize;
        loop {
            let cand = format!("{}_t{}", base, idx);
            match self.base_types.get_mut(&cand) {
                None => {
                    self.base_types.insert(cand.clone(), vec![ty.clone()]);
                    return cand;
                },
                Some(list) => {
                    if list.iter().any(|t| t == ty) {
                        return cand;
                    }
                },
            }
            idx += 1;
        }
    }

    // === expression lowering ===

    /// 降低表达式，返回一个 Operand（如果表达式有值）。
    fn lower_expr(&mut self, expr_id: HirExprId) -> Result<Operand, LowerIrError> {
        let expr = self.get_expr(expr_id)?;
        let expr_ty = self.lowerer.lower_type(&expr.ty)?;

        match &expr.kind {
            HirExprKind::IntLiteral(v) => Ok(Operand::Int(*v)),

            // 字符串字面量: 解码转义 + 补 [lex.string] 终止符后交给
            // `Operand::Str`, 它在 SSA→AST 重建里变回 `Expr::String`, 由
            // codegen 落 .rodata 并发 `lea .Lstr(%rip)`。
            //
            // 以前 sema 把字面量占位成 `IntLiteral(0)`, 于是 `strlen("abc")`
            // 实际传空指针 (ranges 夹具运行期 SIGSEGV), `puts("...")` 之类
            // 同理不可用。
            HirExprKind::StringLiteral(text) => {
                let mut bytes = crate::cpp::const_text::decode_string_escapes(text);
                bytes.push(0);
                Ok(Operand::Str(bytes))
            },

            HirExprKind::FloatLiteral(s) => {
                // C++ HIR 浮点字面量按 double 建模（显式 f 后缀在 HIR
                // 侧已处理类型；此处不存在 80-bit long double 通道）。
                let val: f64 = s.parse().unwrap_or(0.0);
                Ok(Operand::Float(FloatConst::F64(val), Type::Double))
            },

            HirExprKind::BoolLiteral(b) => Ok(Operand::Int(if *b { 1 } else { 0 })),

            HirExprKind::NullPtr => Ok(Operand::Int(0)),

            HirExprKind::Local(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = sym.name.clone();
                let sym_is_volatile = sym.is_volatile;
                let sym_ty = sym.ty.clone();
                let linkage_name = sym.linkage_name.clone();
                drop(sym);

                // 有 linkage_name => **静态存储对象** (函数局部 static 及其 guard).
                //
                // 一律经地址访问, 三条规则都建立在「引用表达式的 ABI 表示就是
                // 所指对象的地址」(与 lower_object_lvalue_addr / lower_address_of
                // 的判定同源) 上:
                //   1. 表达式类型是 `T&`/`T&&` (如 `ostream& cout(){ static ostream
                //      os; return os; }`): 值就是对象地址, 发 `&lname`;
                //   2. 聚合 (class/union/数组) 按对象使用: 直接以全局符号名充当
                //      对象, 重建出 `__dcc_static_<n>.field` 形态, codegen 按已
                //      声明的全局 struct 解算 `lea sym(%rip)+off`;
                //   3. 标量: 从 `&lname` load。
                //
                // 原先统一用 `Operand::SymAddr(lname, Ptr)`: 它重建为
                // `Expr::Ident{ty: Ptr}` 而非 `&sym`, codegen 按标量全局「取值」
                // (`mov sym(%rip),%rax`); 再加上这些对象从未被发射定义, 名字退化成
                // 隐式 int 局部量, 读到的就是未初始化栈槽 —— `__cxa_guard_acquire`
                // 收到垃圾指针, <iostream> 夹具一调用 std::cout() 即 SIGSEGV。
                if let Some(lname) = linkage_name {
                    let lowered_ty = self.lowerer.lower_type(&sym_ty)?;
                    let ptr_ty = Type::Ptr(Box::new(QualType::new(lowered_ty.clone())));
                    if matches!(expr.ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
                        return Ok(self.lower_sym_addr(lname, ptr_ty));
                    }
                    match lowered_ty {
                        Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {
                            // 聚合对象本体: 名字即全局符号 (见上面规则 2)
                            return Ok(Operand::Var(SsaVar {
                                base: lname,
                                version: 0,
                                ty: lowered_ty,
                                span: dummy_span(),
                            }));
                        },
                        _ => {
                            let addr = self.lower_sym_addr(lname, ptr_ty);
                            // 加载值
                            let dest = self.fresh_syn("static_load", lowered_ty.clone());
                            let load_inst = Inst {
                                dest: Some(dest.clone()),
                                kind: InstKind::Load {
                                    e: addr,
                                    ty: lowered_ty,
                                    is_volatile: sym_is_volatile,
                                },
                                span: dummy_span(),
                            };
                            self.push_inst(load_inst);
                            return Ok(Operand::Var(dest));
                        },
                    }
                }

                if let Some(var) = self.locals.get(&name) {
                    // volatile 标量局部变量: 从栈内存加载, 不走 SSA 版本
                    // 复合类型(数组/结构体/联合体)直接返回 SSA 变量:
                    // 它们的子访问 (MemberLoad/IndexLoad) 已带 is_volatile 标志
                    if sym_is_volatile {
                        let ty = self.lowerer.lower_type(&sym_ty)?;
                        match ty {
                            Type::Array(_, _)
                            | Type::Struct(_, _, _, _)
                            | Type::Union(_, _, _, _) => {
                                return Ok(Operand::Var(var.clone()));
                            },
                            _ => {},
                        }
                        let addr =
                            self.fresh_syn("vaddr", Type::Ptr(Box::new(QualType::new(ty.clone()))));
                        let addr_inst = Inst {
                            dest: Some(addr.clone()),
                            kind: InstKind::AddrOf { base: name.clone() },
                            span: dummy_span(),
                        };
                        self.push_inst(addr_inst);
                        let dest = self.fresh_syn("vload", ty);
                        let load_inst = Inst {
                            dest: Some(dest.clone()),
                            kind: InstKind::Load {
                                e: Operand::Var(addr),
                                ty: dest.ty.clone(),
                                is_volatile: true,
                            },
                            span: dummy_span(),
                        };
                        self.push_inst(load_inst);
                        return Ok(Operand::Var(dest));
                    }
                    // **引用变量的 lvalue→rvalue 转换** (`int& r = gx; if (r == 9)`
                    // / `int v = r;` / `f(r)` 按值形参 / `return r;`):
                    // 它在栈上存放的是被引用对象的**地址**, 出现在值位置时必须
                    // 经由该地址间接读。直接返回 SSA 变量等于把地址当值 —— 实测
                    // `int& r = gx; return r == 9 ? 0 : 1;` 恒为假 (静默错值)。
                    let var_owned = var.clone();
                    if let Some(referent) = scalar_ref_referent(&sym_ty) {
                        let val_ty = self.lowerer.lower_type(&referent)?;
                        let dest = self.fresh_syn("ref_rvalue", val_ty.clone());
                        self.push_inst(Inst {
                            dest: Some(dest.clone()),
                            kind: InstKind::Load {
                                e: Operand::Var(var_owned),
                                ty: val_ty,
                                is_volatile: sym_is_volatile,
                            },
                            span: dummy_span(),
                        });
                        return Ok(Operand::Var(dest));
                    }
                    Ok(Operand::Var(var_owned))
                } else {
                    Err(LowerIrError::Internal(format!("local variable '{}' not found", name)))
                }
            },

            HirExprKind::Global(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = self.lowerer.symbol_name(sym);
                // [conv.func]: 函数名出现在**值**位置 (作实参 / 初始化或赋值给
                // 函数指针 / return ...) 时, 值就是函数自身地址。必须用
                // `Operand::FnAddr(name, Type::Func(..))`: SSA→AST 重建把它变成
                // `Expr::Ident{ty: Func}`, C 前端据此发 `lea sym(%rip)`。
                //
                // 原先落回 `Operand::SymAddr(name, Ptr(Void))` 时, codegen 认不出
                // 这个自由符号, 名字退化成「隐式 int 局部量」—— 实参搬运变成无
                // 重定位的 `movslq slot(%r13)` 读未初始化栈槽。ranges 夹具把
                // `is_even`/`triple` 传给 filter/transform view 即按此垃圾指针
                // 调用而 SIGSEGV。`lower_sym_addr`(AddrOf) 也不行: 它重建出的
                // `Ident` 类型被硬编码成 `Void`, `gen_expr_addrof` 的 `Type::Func`
                // 判定不命中, 同样落到栈槽 (实测 `lea -0xb8(%r13)`)。
                let is_func_value = IrLowerer::is_function_value_symbol(sym);
                let func_val_ty = if is_func_value {
                    self.lowerer.lower_func_value_type(&sym.ty)
                } else {
                    Type::Void
                };
                let ty = if is_func_value {
                    func_val_ty
                } else {
                    self.lowerer.lower_type(&sym.ty)?
                };
                if is_func_value {
                    return Ok(Operand::SymAddr(name, ty));
                }
                // 全局变量: 值语义 = 按符号取值
                Ok(Operand::SymAddr(name, ty))
            },

            HirExprKind::This => {
                // this 参数是函数的第一个隐式参数
                if self.ssa.params.is_empty() {
                    return Err(LowerIrError::Internal(
                        "this used in non-member function".to_string(),
                    ));
                }
                Ok(Operand::Var(self.ssa.params[0].clone()))
            },

            HirExprKind::BinaryOp { op, lhs, rhs } => {
                self.lower_binary_op(*op, *lhs, *rhs, &expr_ty)
            },

            HirExprKind::UnaryOp { op, operand } => self.lower_unary_op(*op, *operand, &expr_ty),

            HirExprKind::Index { array, index } => self.lower_index(*array, *index, &expr_ty),

            HirExprKind::Assign { lhs, rhs } => {
                let rhs_val = self.lower_expr(*rhs)?;
                self.lower_assign(*lhs, rhs_val)
            },

            HirExprKind::CompoundAssign { op, lhs, rhs } => {
                self.lower_compound_assign(*op, *lhs, *rhs)
            },

            HirExprKind::Call { callee, args } => self.lower_call(callee, args, &expr_ty),

            HirExprKind::FieldAccess { object, field } => {
                self.lower_field_access(*object, *field, &expr_ty)
            },

            HirExprKind::AddressOf(inner) => self.lower_address_of(*inner, &expr_ty),

            HirExprKind::Dereference(inner) => self.lower_dereference(*inner, &expr_ty),

            HirExprKind::Cast {
                expr,
                target,
                kind: _,
            } => self.lower_cast(*expr, target),

            HirExprKind::BaseCast { object, from, to } => {
                self.lower_base_cast(*object, *from, *to, &expr_ty)
            },

            HirExprKind::VBaseAdjust { object, from, to } => {
                self.lower_vbase_adjust(*object, *from, *to, &expr_ty)
            },

            HirExprKind::TypeInfoRef(class_id) => {
                // typeid 表达式：返回 typeinfo 符号的地址
                // type_info* 类型简化为 *const u8（不透明指针）
                let abi = self.lowerer.abis.get(class_id).ok_or_else(|| {
                    LowerIrError::Internal(format!(
                        "ABI not found for class {} in TypeInfoRef",
                        class_id.0
                    ))
                })?;
                let sym_name = abi.rtti.typeinfo_symbol.clone();
                let ty = Type::Ptr(Box::new(QualType::new(Type::UChar)));
                // 将 typeinfo 符号加入 rodata 列表
                if !self.lowerer.output.rodata.contains(&sym_name) {
                    self.lowerer.output.rodata.push(sym_name.clone());
                }
                // typeid 的结果是 `const std::type_info*`, 即 typeinfo **对象的地址**
                Ok(self.lower_sym_addr(sym_name, ty))
            },

            HirExprKind::TypeInfoDyn(object) => self.lower_typeinfo_dyn(*object),

            HirExprKind::DynamicCast { object, src, dst } => {
                self.lower_dynamic_cast(*object, src, dst, &expr_ty)
            },

            HirExprKind::Construct {
                class,
                place,
                args,
                variant,
            } => self.lower_construct(*class, *place, args, *variant),

            HirExprKind::Destroy {
                class,
                place,
                variant,
            } => self.lower_destroy(*class, *place, *variant),

            HirExprKind::Throw { object, ty } => self.lower_throw(*object, ty),

            HirExprKind::New {
                ty,
                args,
                placement,
                is_array,
            } => self.lower_new(ty, args, placement.as_deref(), *is_array),

            HirExprKind::Delete {
                ptr,
                is_array,
                dtor,
            } => self.lower_delete(*ptr, *is_array, *dtor),

            HirExprKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                // 条件表达式: dest = cond ? then : else
                let cond = self.lower_expr(*cond)?;
                let then_val = self.lower_expr(*then_expr)?;
                let else_val = self.lower_expr(*else_expr)?;
                let result = self.fresh_syn("__cond", expr_ty.clone());
                self.push_inst(Inst {
                    dest: Some(result.clone()),
                    kind: InstKind::Select {
                        cond,
                        then_: then_val,
                        else_: else_val,
                    },
                    span: dummy_span(),
                });
                Ok(Operand::Var(result))
            },

            HirExprKind::Comma { lhs, rhs } => {
                // 逗号表达式: 先求 lhs (丢弃结果), 再求 rhs (作为结果)
                let _ = self.lower_expr(*lhs)?;
                self.lower_expr(*rhs)
            },

            HirExprKind::InitList {
                elems,
                target_ty: _,
            } => self.lower_init_list(elems, &expr_ty),

            HirExprKind::SizeOf(cxx_ty) => {
                // sizeof(T): 降级为 ast::Type 后取字节大小, 作为编译期常量
                let lowered = self.lowerer.lower_type(cxx_ty)?;
                Ok(Operand::Int(type_size(&lowered) as i128))
            },

            _ => Err(LowerIrError::Unsupported(format!(
                "HIR expression kind: {:?}",
                expr_kind_name(&expr.kind)
            ))),
        }
    }

    fn get_expr(&self, id: HirExprId) -> Result<HirExpr, LowerIrError> {
        self.lowerer
            .hir
            .exprs
            .get(id.0)
            .cloned()
            .ok_or_else(|| LowerIrError::Internal(format!("expr {} not found", id.0)))
    }

    fn get_stmt(&self, id: HirStmtId) -> Result<HirStmt, LowerIrError> {
        self.lowerer
            .hir
            .stmts
            .get(id.0)
            .cloned()
            .ok_or_else(|| LowerIrError::Internal(format!("stmt {} not found", id.0)))
    }

    fn get_body(&self, id: HirBodyId) -> Result<HirBody, LowerIrError> {
        self.lowerer
            .hir
            .bodies
            .get(id.0)
            .cloned()
            .ok_or_else(|| LowerIrError::Internal(format!("body {} not found", id.0)))
    }

    // --- binary ops ---

    fn lower_binary_op(
        &mut self,
        op: BinaryOp,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use BinaryOp::*;

        // 短路逻辑：&& 和 || 需要特殊处理
        match op {
            LogicalAnd => return self.lower_logical_and(lhs_id, rhs_id),
            LogicalOr => return self.lower_logical_or(lhs_id, rhs_id),
            _ => {},
        }

        let lhs = self.lower_expr(lhs_id)?;
        let rhs = self.lower_expr(rhs_id)?;

        let ast_op = match op {
            Add => BinOp::Add,
            Sub => BinOp::Sub,
            Mul => BinOp::Mul,
            Div => BinOp::Div,
            Rem => BinOp::Mod,
            BitAnd => BinOp::And,
            BitOr => BinOp::Or,
            BitXor => BinOp::Xor,
            Shl => BinOp::Shl,
            Shr => BinOp::Shr,
            Eq => BinOp::EqEq,
            Ne => BinOp::Ne,
            Lt => BinOp::Lt,
            Le => BinOp::Le,
            Gt => BinOp::Gt,
            Ge => BinOp::Ge,
            LogicalAnd | LogicalOr => unreachable!(),
            _ => return Err(LowerIrError::Unsupported(format!("binary op {:?}", op))),
        };

        let dest = self.fresh_syn("_tmp", result_ty.clone());
        let inst = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Bin {
                op: ast_op,
                l: lhs,
                r: rhs,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(Operand::Var(dest))
    }

    /// 降低一元运算。Deref 复用 lower_dereference (Load)；AddrOf 由
    /// AddressOf 分支处理；自增/自减目前支持局部变量 (SSA 重绑定)。
    fn lower_unary_op(
        &mut self,
        op: UnaryOp,
        operand_id: HirExprId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use UnaryOp::*;
        match op {
            Deref => self.lower_dereference(operand_id, result_ty),
            AddrOf => self.lower_address_of(operand_id, result_ty),
            Plus => self.lower_expr(operand_id),
            Minus | BitNot | LogicalNot => {
                let operand = self.lower_expr(operand_id)?;
                let ast_op = match op {
                    Minus => UnOp::Neg,
                    BitNot => UnOp::BitNot,
                    LogicalNot => UnOp::Not,
                    _ => unreachable!(),
                };
                let dest = self.fresh_syn("unary", result_ty.clone());
                self.push_inst(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Un {
                        op: ast_op,
                        e: operand,
                    },
                    span: dummy_span(),
                });
                Ok(Operand::Var(dest))
            },
            PreInc | PreDec | PostInc | PostDec => {
                let operand_expr = self.get_expr(operand_id)?;
                match &operand_expr.kind {
                    HirExprKind::Local(sid) => {
                        // 局部变量: 走完整的 SSA 重绑定或 volatile 路径
                        self.lower_incdec_local(*sid, op, result_ty)
                    },
                    HirExprKind::FieldAccess { object, field } => {
                        // 成员字段: 走 load-op-store 路径 (总是内存路径,
                        // 因为对象可能是 lvalue/const/pointer 等任意形态)
                        self.lower_incdec_field(*object, *field, op, result_ty)
                    },
                    _ => Err(LowerIrError::Unsupported(
                        "increment/decrement of non-local object".to_string(),
                    )),
                }
            },
        }
    }

    /// lower Inc/Dec 对局部变量 (Local HirExprKind): 保持原有 SSA 重绑定
    /// 逻辑, volatile 标量仍走内存路径。
    fn lower_incdec_local(
        &mut self,
        sym_id: SymbolId,
        op: UnaryOp,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use UnaryOp::*;
        let sym = self.lowerer.get_symbol(sym_id)?;
        let name = sym.name.clone();
        let sym_is_volatile = sym.is_volatile;
        let sym_ty = sym.ty.clone();
        let linkage_name = sym.linkage_name.clone();
        let ty = self.lowerer.lower_type(&sym_ty)?;

        // 静态存储对象 (`static int n; ++n;`): 走内存读-改-写。
        // 下面的 `self.locals` 重绑定模对它是错的: 名字根本不在 self.locals 里。
        if let Some(lname) = linkage_name {
            return self.lower_incdec_static_storage(lname, op, &ty);
        }

        // volatile 标量局部变量: 走内存路径 (load -> op -> store)
        if sym_is_volatile {
            match &ty {
                Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {},
                _ => {
                    let addr =
                        self.fresh_syn("vaddr", Type::Ptr(Box::new(QualType::new(ty.clone()))));
                    let addr_inst = Inst {
                        dest: Some(addr.clone()),
                        kind: InstKind::AddrOf { base: name.clone() },
                        span: dummy_span(),
                    };
                    self.push_inst(addr_inst);
                    let load_dest = self.fresh_syn("vload", ty.clone());
                    let load_inst = Inst {
                        dest: Some(load_dest.clone()),
                        kind: InstKind::Load {
                            e: Operand::Var(addr.clone()),
                            ty: ty.clone(),
                            is_volatile: true,
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(load_inst);
                    let ast_op = match op {
                        PreInc | PostInc => BinOp::Add,
                        PreDec | PostDec => BinOp::Sub,
                        _ => unreachable!(),
                    };
                    let new_val = self.fresh_var(&name, ty.clone());
                    self.push_inst(Inst {
                        dest: Some(new_val.clone()),
                        kind: InstKind::Bin {
                            op: ast_op,
                            l: Operand::Var(load_dest.clone()),
                            r: Operand::Int(1),
                        },
                        span: dummy_span(),
                    });
                    let store_inst = Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: Operand::Var(addr),
                            src: Operand::Var(new_val.clone()),
                            is_volatile: true,
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(store_inst);
                    self.locals.insert(name, new_val.clone());
                    return Ok(Operand::Var(if matches!(op, PreInc | PreDec) {
                        new_val
                    } else {
                        let post = self.fresh_tmp("postinc", ty.clone());
                        self.push_inst(Inst {
                            dest: Some(post.clone()),
                            kind: InstKind::Bin {
                                op: BinOp::Add,
                                l: Operand::Var(load_dest),
                                r: Operand::Int(0),
                            },
                            span: dummy_span(),
                        });
                        post
                    }));
                },
            }
        }

        // 非 volatile: 纯 SSA 版本重绑定
        let old = match self.locals.get(&name) {
            Some(v) => v.clone(),
            None => {
                return Err(LowerIrError::Internal(format!("local variable '{}' not found", name)))
            },
        };
        let ast_op = match op {
            PreInc | PostInc => BinOp::Add,
            PreDec | PostDec => BinOp::Sub,
            _ => unreachable!(),
        };
        let new_var = self.fresh_var(&name, result_ty.clone());
        self.push_inst(Inst {
            dest: Some(new_var.clone()),
            kind: InstKind::Bin {
                op: ast_op,
                l: Operand::Var(old.clone()),
                r: Operand::Int(1),
            },
            span: dummy_span(),
        });
        self.locals.insert(name, new_var.clone());
        if matches!(op, PreInc | PreDec) {
            Ok(Operand::Var(new_var))
        } else {
            let post = self.fresh_tmp("postinc", result_ty.clone());
            self.push_inst(Inst {
                dest: Some(post.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(old),
                    r: Operand::Int(0),
                },
                span: dummy_span(),
            });
            Ok(Operand::Var(post))
        }
    }

    /// lower Inc/Dec 对成员字段 (FieldAccess HirExprKind):
    /// 总是走内存 load-op-store 路径, 因为对象可以是任意形态
    /// (lvalue/rvalue/pointer/const method 里的 mutable 成员 等)。
    fn lower_incdec_field(
        &mut self,
        object_id: HirExprId,
        field_id: SymbolId,
        op: UnaryOp,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use UnaryOp::*;
        // 1. load old value (via lower_field_access → MemberLoad)
        let old_val = self.lower_field_access(object_id, field_id, result_ty)?;

        // 2. compute new = old ± 1
        let ast_op = match op {
            PreInc | PostInc => BinOp::Add,
            PreDec | PostDec => BinOp::Sub,
            _ => unreachable!(),
        };
        let new_var = self.fresh_syn("field_inc", result_ty.clone());
        self.push_inst(Inst {
            dest: Some(new_var.clone()),
            kind: InstKind::Bin {
                op: ast_op,
                l: old_val.clone(),
                r: Operand::Int(1),
            },
            span: dummy_span(),
        });

        // 3. store new value back (via lower_field_store → MemberStore)
        self.lower_field_store(object_id, field_id, Operand::Var(new_var.clone()))?;

        // 4. prefix 返回新值; suffix 返回旧值 (固化到独立临时)
        if matches!(op, PreInc | PreDec) {
            Ok(Operand::Var(new_var))
        } else {
            let post = self.fresh_tmp("postinc", result_ty.clone());
            self.push_inst(Inst {
                dest: Some(post.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: old_val,
                    r: Operand::Int(0),
                },
                span: dummy_span(),
            });
            Ok(Operand::Var(post))
        }
    }

    /// 生产一个 **`&&` / `||` 短路结果变量** 的基名.
    ///
    /// 不能用 `fresh_syn` (`dcc_t<N>_` 前缀): 该前缀在 `ssa::rebuild_block` 里被
    /// 识别为「合成临时值」, 单次使用就在 merge 处被内联成一个表达式; 而短路结果
    /// 必然在**两条路径里各写一次**, 内联后只剩一条路径的写入, 另一条路径落到未
    /// 初始化的栈槽. 也不能用 `_` 前缀: 那些名字根本不会被声明.
    /// 因此用 `dcc_land<N>` / `dcc_lor<N>` —— 重建后是一个真实的 C 局部变量,
    /// 两条路径各赋值一次, 语义与源码的短路求值逐位对应.
    fn fresh_logical_base(&mut self, tag: &str) -> String {
        loop {
            self.logical_seq += 1;
            let cand = format!("dcc_{}{}", tag, self.logical_seq);
            if !self.base_types.contains_key(&cand) {
                return cand;
            }
        }
    }

    /// 把短路结果写进基名为 `base` 的结果变量 (两条路径各调用一次).
    fn push_logical_result(&mut self, base: &str, e: Operand) -> SsaVar {
        let dest = self.fresh_var(base, Type::Bool);
        self.push_inst(Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast { ty: Type::Bool, e },
            span: dummy_span(),
        });
        dest
    }

    fn lower_logical_and(
        &mut self,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        // a && b: if a { res = (bool)b } else { res = false }
        let then_bb = self.unique_bb("and_then");
        let else_bb = self.unique_bb("and_else");
        let merge_bb = self.unique_bb("and_merge");

        // 先求 lhs
        let lhs = self.lower_expr(lhs_id)?;
        self.set_term(Terminator::Branch {
            cond: lhs,
            t: then_bb.clone(),
            f: else_bb.clone(),
        });

        let result_base = self.fresh_logical_base("land");

        // then 块：计算 rhs 并写入结果
        self.current_bb = then_bb.clone();
        let rhs = self.lower_expr(rhs_id)?;
        self.push_logical_result(&result_base, rhs);
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // else 块：短路, 结果 false
        self.current_bb = else_bb.clone();
        self.push_logical_result(&result_base, Operand::Int(0));
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // merge 块：读结果变量. 这里的 SsaVar 只是**名字载体** (version 0 不
        // 对应任何定义): 两条路径的写入在 SSA→AST 重建时都会坍缩到同一个 C
        // 局部 `base`, 读取按 base 落回它, 因此不需要 phi.
        self.current_bb = merge_bb;
        Ok(Operand::Var(SsaVar {
            base: result_base,
            version: 0,
            ty: Type::Bool,
            span: dummy_span(),
        }))
    }

    fn lower_logical_or(
        &mut self,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        // a || b: if a { res = true } else { res = (bool)b }
        let then_bb = self.unique_bb("or_then");
        let else_bb = self.unique_bb("or_else");
        let merge_bb = self.unique_bb("or_merge");

        let lhs = self.lower_expr(lhs_id)?;
        self.set_term(Terminator::Branch {
            cond: lhs,
            t: then_bb.clone(),
            f: else_bb.clone(),
        });

        let result_base = self.fresh_logical_base("lor");

        // then 块：短路, 结果 true
        self.current_bb = then_bb.clone();
        self.push_logical_result(&result_base, Operand::Int(1));
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // else 块：计算 rhs 并写入结果
        self.current_bb = else_bb.clone();
        let rhs = self.lower_expr(rhs_id)?;
        self.push_logical_result(&result_base, rhs);
        self.set_term(Terminator::Jump(merge_bb.clone()));

        self.current_bb = merge_bb;
        Ok(Operand::Var(SsaVar {
            base: result_base,
            version: 0,
            ty: Type::Bool,
            span: dummy_span(),
        }))
    }

    // --- assignment ---

    fn lower_assign(&mut self, lhs_id: HirExprId, rhs: Operand) -> Result<Operand, LowerIrError> {
        let lhs_expr = self.get_expr(lhs_id)?;

        match &lhs_expr.kind {
            HirExprKind::Local(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = sym.name.clone();
                let sym_is_volatile = sym.is_volatile;
                let sym_ty = sym.ty.clone();
                let linkage_name = sym.linkage_name.clone();
                let ty = self.lowerer.lower_type(&sym_ty)?;

                // 静态存储对象 (`static int n; n = 5;`): 必须写回它的全局存储。
                // 这里不能走「SSA 版本重绑定 + self.locals.insert」: 名字根本不在
                // self.locals 里 (它不是自动局部变量), 而且同一符号在其他函数 /
                // 其他调用点看到的是同一块存储, 版本重绑定只会把新值留在栈上,
                // 跨调用丢失持久性 (静默错值)。
                if let Some(lname) = linkage_name {
                    return self.lower_static_storage_store(lname, ty, rhs);
                }

                // **对引用变量赋值** (`int& r = x; r = 9;` / `void f(int& p){ p = 9; }`):
                // 引用不可 reseated, 赋值的语义是「写它所指的对象」。上面的 SSA
                // 版本重绑定会把 9 直接写进存放地址的那个槽 —— 实测 `setref(r)`
                // 里 `p = 9` 之后全局 `gx` 仍是 1 (静默错值), 且指针槽被踩成 9。
                // 与 `lower_expr` 的 `ref_rvalue` 读路径严格对称。
                if let Some(referent) = scalar_ref_referent(&sym_ty) {
                    let val_ty = self.lowerer.lower_type(&referent)?;
                    let slot = self.locals.get(&name).cloned().ok_or_else(|| {
                        LowerIrError::Internal(format!("local variable '{}' not found", name))
                    })?;
                    let casted = self.fresh_syn("ref_store_val", val_ty.clone());
                    self.push_inst(Inst {
                        dest: Some(casted.clone()),
                        kind: InstKind::Cast { ty: val_ty, e: rhs },
                        span: dummy_span(),
                    });
                    self.push_inst(Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: Operand::Var(slot),
                            src: Operand::Var(casted.clone()),
                            is_volatile: sym_is_volatile,
                        },
                        span: dummy_span(),
                    });
                    return Ok(Operand::Var(casted));
                }

                let new_var = self.fresh_var(&name, ty.clone());

                let inst = Inst {
                    dest: Some(new_var.clone()),
                    kind: InstKind::Cast {
                        ty: ty.clone(),
                        e: rhs,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);

                // volatile 标量局部变量: 赋值后写回内存
                if sym_is_volatile {
                    match &ty {
                        Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {
                        },
                        _ => {
                            let addr = self
                                .fresh_var("vaddr", Type::Ptr(Box::new(QualType::new(ty.clone()))));
                            let addr_inst = Inst {
                                dest: Some(addr.clone()),
                                kind: InstKind::AddrOf { base: name.clone() },
                                span: dummy_span(),
                            };
                            self.push_inst(addr_inst);
                            let store_inst = Inst {
                                dest: None,
                                kind: InstKind::Store {
                                    e: Operand::Var(addr),
                                    src: Operand::Var(new_var.clone()),
                                    is_volatile: true,
                                },
                                span: dummy_span(),
                            };
                            self.push_inst(store_inst);
                        },
                    }
                }

                self.locals.insert(name, new_var.clone());
                Ok(Operand::Var(new_var))
            },
            HirExprKind::Dereference(inner_id) => {
                // *ptr = val → Store
                let ptr = self.lower_expr(*inner_id)?;
                let inner_expr = self.get_expr(*inner_id)?;
                let is_volatile = inner_expr.ty.is_volatile();
                let inst = Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: ptr,
                        src: rhs,
                        is_volatile,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                Ok(Operand::Void)
            },
            HirExprKind::FieldAccess { object, field } => {
                self.lower_field_store(*object, *field, rhs)
            },
            HirExprKind::Index { array, index } => {
                // a[i] = v → IndexStore
                let arr = self.lower_expr(*array)?;
                let idx = self.lower_expr(*index)?;
                let is_volatile = self.index_is_volatile(*array);
                // 元素类型 = `a[i]` 这个左值自身的类型 (不是 `a` 的类型):
                // 它决定 C 后端的元素字节步长与存宽。以前丢给 SSA 后重建为
                // `Expr::Index{ty: Type::Int}` 硬编码, `char[]` 下标会按 4 字节发射。
                let lhs_expr = self.get_expr(lhs_id)?;
                let elem_ty = self.lowerer.lower_type(&lhs_expr.ty)?;
                let inst = Inst {
                    dest: None,
                    kind: InstKind::IndexStore {
                        arr,
                        idx,
                        src: rhs,
                        ty: elem_ty,
                        is_volatile,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                Ok(Operand::Void)
            },
            // sema 把 `*p` 统一表示为 `UnaryOp { op: Deref }`, 因此
            // `*p = v` 会走到这里而不是上面的 Dereference 分支。
            HirExprKind::UnaryOp {
                op: crate::cpp::hir::UnaryOp::Deref,
                operand,
            } => {
                let ptr = self.lower_expr(*operand)?;
                let inner_expr = self.get_expr(*operand)?;
                let is_volatile = inner_expr.ty.is_volatile();
                let inst = Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: ptr,
                        src: rhs,
                        is_volatile,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                Ok(Operand::Void)
            },
            _ => Err(LowerIrError::Unsupported(format!(
                "assignment to lvalue kind: {:?}",
                expr_kind_name(&lhs_expr.kind)
            ))),
        }
    }

    // --- compound assignment ---

    fn lower_compound_assign(
        &mut self,
        op: BinaryOp,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        // 分解为: tmp = lhs op rhs; lhs = tmp
        let lhs_expr = self.get_expr(lhs_id)?;
        let result_ty = self.lowerer.lower_type(&lhs_expr.ty)?;
        let value = self.lower_binary_op(op, lhs_id, rhs_id, &result_ty)?;
        self.lower_assign(lhs_id, value)
    }

    // --- call ---

    /// 取函数符号的**显式形参类型表** (单一真源: sema 的 `CxxType::Function.params`)。
    ///
    /// 实参的 ABI 适配必须以它为依据, 而不能依赖夹具名/函数名特例。
    fn function_params(&mut self, sym_id: SymbolId) -> Vec<CxxType> {
        match self.lowerer.symbols.get(sym_id.0) {
            Some(s) => match &s.ty {
                CxxType::Function { params, .. } => params.clone(),
                _ => vec![],
            },
            None => vec![],
        }
    }

    /// 表达式是否是**可取地址的具名左值** (glvalue 且存放位置是别的对象)。
    fn is_addressable_glvalue(kind: &HirExprKind) -> bool {
        matches!(
            kind,
            HirExprKind::Local(_)
                | HirExprKind::Global(_)
                | HirExprKind::FieldAccess { .. }
                | HirExprKind::Dereference(_)
                | HirExprKind::UnaryOp {
                    op: UnaryOp::Deref,
                    ..
                }
        )
    }

    /// 按**被调形参类型**降低一个实参。
    ///
    /// 引用形参 (`T&` / `T&&`) 的 ABI 表示就是所指对象的**地址** —— 与
    /// `IrLowerer::lower_type` 把 `LValueRef` 降成 `Ptr` 同源。sema 传进来的 HIR
    /// 实参表达式类型仍是被引用对象的值类型 (`Class` / `Integer`, 不插隐式取址),
    /// 若这里一律 `lower_expr`, 就会把整个对象**按值**塞进一个指针槽:
    /// 被调函数解引用到的"地址"其实是对象的头几个字节 —— 实测
    /// `void set(P& a){ a.x = 9; } set(p);` 直接 SIGSEGV。
    ///
    /// 只对具名左值做隐式取址; 字面量/临时量绑定 `const T&` 需要临时量物化
    /// (见 `materialize_ref_temp`), 类类型的临时量需拷贝构造, 保持原路径不新增硬错。
    fn lower_call_arg(
        &mut self,
        param_ty: Option<&CxxType>,
        arg_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        if matches!(param_ty, Some(CxxType::LValueRef(_)) | Some(CxxType::RValueRef(_))) {
            let arg_expr = self.get_expr(arg_id)?;
            // 实参**自身是引用变量** (`int& r = x; f(r)`): 它已经按 ABI 存放
            // 为地址, 直接传那个存放值 —— 走 `lower_expr` 会插入一次载入
            // (引用现在被建模为「按地址间接访问的对象'), 被调函数收到
            // 对象值当指针用 —— 实测 `ti.before(ti)` SIGSEGV。
            if let Some(addr) = self.named_ref_slot_addr(arg_id) {
                return Ok(addr);
            }
            if Self::is_addressable_glvalue(&arg_expr.kind) {
                return self.lower_object_lvalue_addr(arg_id);
            }
            // M53: 走到这里 = 实参是**右值** (字面量 / 表达式结果), 而形参是引用.
            // C++ 要在调用方的栈上物化一个临时对象再把它的地址传过去.
            let inner = match param_ty {
                Some(CxxType::LValueRef(t)) | Some(CxxType::RValueRef(t)) => t.clone(),
                _ => unreachable!(),
            };
            if matches!(&*inner, CxxType::Class(_)) {
                // 类类型临时量要跑拷贝构造 (对象模型侧的事), 不在此跨界
                return self.lower_expr(arg_id);
            }
            let ir_ty = self.lowerer.lower_type(&inner)?;
            let value = self.lower_expr(arg_id)?;
            return self.materialize_ref_temp(value, &ir_ty);
        }
        self.lower_expr(arg_id)
    }

    /// M53: 把一个**已算好的标量值**存进一个新物化的局部临时量, 返回它的地址.
    ///
    /// 这是 `f(7)` 绑定 `const int&` 缺的那一步: 之前直接把值 7 塞进指针槽,
    /// 被调函数解引用地址 7 -> SIGSEGV (`std::flat_map` 夹具里每条
    /// `m.insert(3, 30)` 全踩这条)。栈槽只经 `AddrOf` 取址 + `Store` 写入,
    /// 不注册进 `self.locals` (它根本不参与名字查找)。
    fn materialize_ref_temp(
        &mut self,
        value: Operand,
        ir_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let name = format!("__ref_temp_{}", self.ref_temp_counter);
        self.ref_temp_counter += 1;
        self.ssa.local_types.insert(name.clone(), ir_ty.clone());
        self.ssa.original_locals.insert(name.clone());
        let ptr_ty = Type::Ptr(Box::new(QualType::new(ir_ty.clone())));
        let addr = self.fresh_syn("ref_temp_addr", ptr_ty);
        self.push_inst(Inst {
            dest: Some(addr.clone()),
            kind: InstKind::AddrOf { base: name },
            span: dummy_span(),
        });
        self.push_inst(Inst {
            dest: None,
            kind: InstKind::Store {
                e: Operand::Var(addr.clone()),
                src: value,
                is_volatile: false,
            },
            span: dummy_span(),
        });
        Ok(Operand::Var(addr))
    }

    fn lower_call(
        &mut self,
        callee: &HirCallee,
        args: &[HirExprId],
        ret_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        match callee {
            HirCallee::Direct(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let fn_name = self.lowerer.symbol_name(sym);
                let fn_ty = self.lowerer.lower_type(&sym.ty)?;

                // 判断是否为成员方法（有 owner class）
                let is_member = matches!(
                    sym.kind,
                    CxxSymbolKind::Method | CxxSymbolKind::Constructor | CxxSymbolKind::Destructor
                ) && sym.owner.is_some();
                let this_owner = sym.owner.unwrap_or(SymbolId(0));
                // 形参类型表必须在 `sym` 借用结束后再取 (它要 &mut self)。
                let param_tys = self.function_params(*sym_id);

                let mut lowered_args = Vec::with_capacity(args.len());

                if is_member {
                    // 成员函数调用：第一个参数是 this 指针
                    // 需要判断 this 是否需要调整（调用基类方法时）
                    let this_owner_class = ClassId::from(this_owner);

                    // 当前函数的 owner class（如果有）
                    let current_owner_class = {
                        let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
                        fn_sym.owner.map(ClassId::from)
                    };

                    if args.is_empty() {
                        return Err(LowerIrError::Internal(
                            "member function call missing this argument".to_string(),
                        ));
                    }

                    // 第一个参数是对象指针（this）
                    let object_expr = args[0];
                    let object = self.lower_expr(object_expr)?;

                    // 获取对象类型的 class
                    let object_expr_data = self.get_expr(object_expr)?;
                    let _object_class = match &object_expr_data.ty {
                        CxxType::Pointer(inner) => match inner.as_ref() {
                            CxxType::Class(cid) => Some(*cid),
                            _ => None,
                        },
                        CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                            match inner.as_ref() {
                                CxxType::Class(cid) => Some(*cid),
                                _ => None,
                            }
                        },
                        CxxType::Class(cid) => Some(*cid),
                        _ => None,
                    };

                    let adjusted_this = if let (Some(obj_class), Some(_cur_class)) =
                        (_object_class, current_owner_class)
                    {
                        // 如果对象的类 != 方法所在的类，说明需要做基类转换
                        if obj_class != this_owner_class {
                            // 用 base cast 方式调整 this 指针
                            let target_ty = self.lowerer.lower_type(&CxxType::Pointer(
                                Box::new(CxxType::Class(this_owner_class)),
                            ))?;
                            self.adjust_pointer_to_base(
                                object,
                                obj_class,
                                this_owner_class,
                                &target_ty,
                            )?
                        } else {
                            object
                        }
                    } else {
                        object
                    };

                    lowered_args.push(adjusted_this);

                    // 其余参数: 按形参类型做 ABI 适配。
                    // sema 的 `Function.params` 通常**不**含 this (lower_ir 在
                    // `FuncLowerer::new` 里注入), 但运行时/合成符号可能含;
                    // 用长度关系判定偏移, 不靠符号名猜。
                    let param_offset = if param_tys.len() + 1 == args.len() {
                        0
                    } else if param_tys.len() == args.len() {
                        1
                    } else {
                        0
                    };
                    for (i, arg_id) in args[1..].iter().enumerate() {
                        let pt = param_tys.get(i + param_offset);
                        lowered_args.push(self.lower_call_arg(pt, *arg_id)?);
                    }
                } else {
                    // 非成员函数
                    for (i, arg_id) in args.iter().enumerate() {
                        let pt = param_tys.get(i);
                        lowered_args.push(self.lower_call_arg(pt, *arg_id)?);
                    }
                }

                // 如果符号是 dccpp_* runtime symbol，需要注册为 external
                if (fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_"))
                    && !self.lowerer.output.external_symbols.contains(&fn_name)
                {
                    self.lowerer.output.external_symbols.push(fn_name.clone());
                }

                let callee_op = Operand::FnAddr(fn_name, fn_ty);

                if ret_ty == &Type::Void {
                    let inst = Inst {
                        dest: None,
                        kind: InstKind::Call {
                            callee: callee_op,
                            args: lowered_args,
                            ret_ty: ret_ty.clone(),
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(inst);
                    Ok(Operand::Void)
                } else {
                    let dest = self.fresh_syn("call_result", ret_ty.clone());
                    let inst = Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::Call {
                            callee: callee_op,
                            args: lowered_args,
                            ret_ty: ret_ty.clone(),
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(inst);
                    Ok(Operand::Var(dest))
                }
            },
            HirCallee::Virtual {
                method,
                slot,
                this_adjust,
            } => self.lower_virtual_call(*method, *slot, this_adjust, args, ret_ty),
            _ => Err(LowerIrError::Unsupported("indirect/ctor/dtor calls".to_string())),
        }
    }

    /// 从「已调整到 vptr 所在子对象」的指针出发, 取 `vtable[slot]` 的函数指针。
    ///
    /// `vptr_ptr` 须已是 `vptr_ptr_type()` (字节指针) 形态。槽号由调用方向布局层
    /// (`ClassAbi::vcall_slot`) 查得 —— 发射侧不自己数槽, 保证 vtable 布局与
    /// 调用点只有一个真源。虚函数调用与 `delete` 的虚析构派发共用本链。
    fn load_vtable_entry(
        &mut self,
        vptr_ptr: &Operand,
        slot: usize,
    ) -> Result<SsaVar, LowerIrError> {
        // 1. 从 vptr 加载 vtable 指针（vptr 类型为 *const *const u8）
        let vtable_ptr_ty = self.lowerer.vtable_entry_type();
        let vtable_ptr = self.fresh_syn("vtable_ptr", vtable_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(vtable_ptr.clone()),
            kind: InstKind::Load {
                e: vptr_ptr.clone(),
                ty: vtable_ptr_ty,
                is_volatile: false,
            },
            span: dummy_span(),
        });

        // 2. vtable_ptr + slot * sizeof(pointer) -> 槽地址
        let slot_offset = (slot as i128) * (POINTER_SIZE as i128);
        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
        let vt_byte_ptr = self.fresh_syn("vt_byte_ptr", byte_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(vt_byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: Operand::Var(vtable_ptr),
            },
            span: dummy_span(),
        });
        let slot_byte_ptr = self.fresh_syn("vt_slot_ptr", byte_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(slot_byte_ptr.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(vt_byte_ptr),
                r: Operand::Int(slot_offset),
            },
            span: dummy_span(),
        });

        // 3. 加载函数指针（类型为 void*，后续通过调用使用）
        let fn_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let fn_ptr_var = self.fresh_syn("fn_ptr", fn_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(fn_ptr_var.clone()),
            kind: InstKind::Cast {
                ty: fn_ptr_ty.clone(),
                e: Operand::Var(slot_byte_ptr),
            },
            span: dummy_span(),
        });
        let fp_var = self.fresh_syn("vcall_fp", fn_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(fp_var.clone()),
            kind: InstKind::Load {
                e: Operand::Var(fn_ptr_var),
                ty: fn_ptr_ty,
                is_volatile: false,
            },
            span: dummy_span(),
        });
        Ok(fp_var)
    }

    /// 虚函数调用 lowering：通过 vtable 间接调用。
    fn lower_virtual_call(
        &mut self,
        method: SymbolId,
        slot: usize,
        this_adjust: &crate::cpp::hir::ThisAdjust,
        args: &[HirExprId],
        ret_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        // 虚基调整不支持（Stage 5 仅非虚继承）
        if this_adjust.vbase_index.is_some() {
            return Err(LowerIrError::Unsupported(
                "virtual base adjustment in virtual call".to_string(),
            ));
        }

        if args.is_empty() {
            return Err(LowerIrError::Internal("virtual call missing this argument".to_string()));
        }

        // 1. 获取 this 指针对象
        let object_expr = args[0];
        let object = self.lower_expr(object_expr)?;

        // 获取对象类型的 class（用于确定 vptr 类型与虚槽号）
        let object_expr_data = self.get_expr(object_expr)?;
        let object_class = match &object_expr_data.ty {
            CxxType::Pointer(inner) => match inner.as_ref() {
                CxxType::Class(cid) => Some(*cid),
                _ => None,
            },
            CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => match inner.as_ref() {
                CxxType::Class(cid) => Some(*cid),
                _ => None,
            },
            CxxType::Class(cid) => Some(*cid),
            _ => None,
        };

        // 2. 应用 this_adjust（非虚偏移）：调整到对应 vptr 所在的子对象
        let adjusted_this = if this_adjust.non_virtual != 0 {
            let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
            let byte_ptr = self.fresh_syn("vcall_byte_ptr", byte_ptr_ty.clone());
            let cast_to_byte = Inst {
                dest: Some(byte_ptr.clone()),
                kind: InstKind::Cast {
                    ty: byte_ptr_ty.clone(),
                    e: object.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(cast_to_byte);

            let adj_byte = self.fresh_syn("vcall_adj_byte", byte_ptr_ty.clone());
            let gep = Inst {
                dest: Some(adj_byte.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(byte_ptr),
                    r: Operand::Int(this_adjust.non_virtual as i128),
                },
                span: dummy_span(),
            };
            self.push_inst(gep);

            // 转回 void* 用于 vptr 加载
            let vptr_ptr_ty = self.lowerer.vptr_ptr_type();
            let vptr_ptr = self.fresh_syn("vcall_vptr_ptr", vptr_ptr_ty.clone());
            let cast_back = Inst {
                dest: Some(vptr_ptr.clone()),
                kind: InstKind::Cast {
                    ty: vptr_ptr_ty,
                    e: Operand::Var(adj_byte),
                },
                span: dummy_span(),
            };
            self.push_inst(cast_back);
            Operand::Var(vptr_ptr)
        } else {
            // 零偏移：直接 cast 到 vptr 类型
            let vptr_ptr_ty = self.lowerer.vptr_ptr_type();
            let obj_ty = match &object {
                Operand::Var(v) => v.ty.clone(),
                _ => {
                    return Err(LowerIrError::Internal(
                        "virtual call object is not a var".to_string(),
                    ));
                },
            };
            if obj_ty == vptr_ptr_ty {
                object.clone()
            } else {
                let vptr_ptr = self.fresh_syn("vcall_vptr_ptr", vptr_ptr_ty.clone());
                let cast_inst = Inst {
                    dest: Some(vptr_ptr.clone()),
                    kind: InstKind::Cast {
                        ty: vptr_ptr_ty,
                        e: object.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_inst);
                Operand::Var(vptr_ptr)
            }
        };

        // 3~4. 取 `vtable[slot]` 的函数指针。抽成 `load_vtable_entry` 后, 虚函数
        // 调用与 `delete` 的虚析构派发共用同一条发射链。
        //
        // **槽号以布局层为单一真源**: `ClassAbi::vcall_slot` 按 this 指针静态类型
        // 类的主 vtable 函数序返号。sema 解析期的槽号由「本类 virtual_methods
        // 登记顺序」给出 (继承未重写的虚函数不进入该表), 与真实布局系统性
        // 错位 —— 实测 `M : B{virtual ~B(); id;} { bar; }` 里 `m->bar()` 拿到
        // slot 0, 而 slot 0 是虚析构槽。查不到时 (单测里手搓 ClassAbi、
        // 非多态类等) 回退 sema 槽号, 保持旧行为。
        let method_name = self
            .lowerer
            .get_symbol(method)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let slot = object_class
            .and_then(|cid| self.lowerer.abis.get(&cid))
            .and_then(|abi| abi.vcall_slot(&method_name))
            .unwrap_or(slot);
        let fp_var = self.load_vtable_entry(&adjusted_this, slot)?;

        // 5. 构造参数列表：第一个参数是 this（调整后）
        let method_sym = self.lowerer.get_symbol(method)?;
        let method_owner_class = method_sym.owner.map(ClassId::from);
        drop(method_sym);

        // 构造 this 参数：使用原始对象（非 vptr 指针类型），
        // 因为被调用的虚函数期望接收正确类型的 this 指针
        let this_arg = if this_adjust.non_virtual != 0 {
            // 非零偏移：adjusted_this 指向 vptr 子对象，但函数期望的 this 是
            // 该子对象的正确类型指针。我们用 adjusted_this 转成正确类型。
            // 这里简化：直接用 adjusted_this 作为 this 参数（ABI 层已经是正确偏移）
            // 但类型需要匹配：转成 method 所在类的指针类型
            if let Some(owner_class) = method_owner_class {
                let owner_ptr_ty = self
                    .lowerer
                    .lower_type(&CxxType::Pointer(Box::new(CxxType::Class(owner_class))))?;
                let this_var = self.fresh_syn("vcall_this", owner_ptr_ty.clone());
                let cast_this = Inst {
                    dest: Some(this_var.clone()),
                    kind: InstKind::Cast {
                        ty: owner_ptr_ty,
                        e: adjusted_this.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_this);
                Operand::Var(this_var)
            } else {
                adjusted_this.clone()
            }
        } else {
            // 零偏移：原始对象就是 this
            object.clone()
        };

        let mut lowered_args = Vec::with_capacity(args.len());
        lowered_args.push(this_arg);

        // 其余参数: 与直接调用同源, 按形参类型做 ABI 适配 (引用形参取实参地址)。
        let param_tys = self.function_params(method);
        let param_offset = if param_tys.len() + 1 == args.len() {
            0
        } else if param_tys.len() == args.len() {
            1
        } else {
            0
        };
        for (i, arg_id) in args[1..].iter().enumerate() {
            let pt = param_tys.get(i + param_offset);
            lowered_args.push(self.lower_call_arg(pt, *arg_id)?);
        }

        // 6. 通过函数指针间接调用
        let callee_op = Operand::Var(fp_var);

        if ret_ty == &Type::Void {
            let inst = Inst {
                dest: None,
                kind: InstKind::Call {
                    callee: callee_op,
                    args: lowered_args,
                    ret_ty: ret_ty.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(inst);
            Ok(Operand::Void)
        } else {
            let dest = self.fresh_syn("vcall_result", ret_ty.clone());
            let inst = Inst {
                dest: Some(dest.clone()),
                kind: InstKind::Call {
                    callee: callee_op,
                    args: lowered_args,
                    ret_ty: ret_ty.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(inst);
            Ok(Operand::Var(dest))
        }
    }

    /// 将指针调整到基类子对象（加偏移）。
    fn adjust_pointer_to_base(
        &mut self,
        ptr: Operand,
        from_class: ClassId,
        to_class: ClassId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        if from_class == to_class {
            return Ok(ptr);
        }

        let base_layout = self
            .lowerer
            .find_base_layout(from_class, to_class)?
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "base class {} not found in class {}",
                    to_class.0, from_class.0
                ))
            })?;

        let offset = base_layout.offset;
        self.compute_member_ptr(ptr, offset, result_ty)
    }

    // --- field access ---

    fn lower_field_access(
        &mut self,
        object_id: HirExprId,
        field_id: SymbolId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let (object, obj_ty, object_is_arrow) = self.lower_member_object(object_id)?;
        let field_sym = self.lowerer.get_symbol(field_id)?;
        let field_name = field_sym.name.clone();
        let obj_is_volatile = self.obj_base_is_volatile(object_id);

        let dest = self.fresh_syn("field", result_ty.clone());
        let inst = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::MemberLoad {
                obj: object,
                obj_ty,
                field: field_name,
                ty: result_ty.clone(),
                is_volatile: obj_is_volatile,
                is_arrow: object_is_arrow,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(Operand::Var(dest))
    }

    fn lower_field_store(
        &mut self,
        object_id: HirExprId,
        field_id: SymbolId,
        src: Operand,
    ) -> Result<Operand, LowerIrError> {
        let (object, obj_ty, object_is_arrow) = self.lower_member_object(object_id)?;
        let field_sym = self.lowerer.get_symbol(field_id)?;
        let field_name = field_sym.name.clone();
        // 字段类型 = 符号表里声明的类型 (单一真源): MemberStore 携带它,
        // SSA→AST 重建出的 `Expr::Member{ty}` 才是真实形态, 后端据此判定
        // 「聚合字段按值整体拷贝」。
        let field_ty = self.lowerer.lower_type(&field_sym.ty)?;
        drop(field_sym);
        let obj_is_volatile = self.obj_base_is_volatile(object_id);

        let inst = Inst {
            dest: None,
            kind: InstKind::MemberStore {
                obj: object,
                obj_ty,
                field: field_name,
                src,
                ty: field_ty,
                is_volatile: obj_is_volatile,
                is_arrow: object_is_arrow,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(Operand::Void)
    }

    /// 降级成员访问的基对象: 返回 `(obj operand, 结构体类型, obj 是否为箭头基址)`.
    ///
    /// 关键: C++ 的 `p->m` 在 sema 里被合成为 `Member(UnaryOp{Deref, p}, m)`,
    /// 于是对象表达式的类型已经是 pointee class 而不是指针, 仅靠
    /// `deref_object_for_member` 的类型判定会把它当成"类值"处理 ——
    /// `lower_expr` 对 `UnaryOp{Deref}` 发射 `InstKind::Load`, 即**整结构体按值
    /// 载入**, 随后的 MemberStore 便写到那个临时副本上, 写入丢失
    /// (`p->field = v` 完全不生效, 跨函数也不可见)。
    ///
    /// 这里在求值之前先识别解引用包装: 若被解引用的操作数本身是
    /// "class 指针", 就直接把指针交出去并标记 is_arrow = true, 让成员访问
    /// 走 `ptr->field` 的内存路径 (与 `this->field` 一致)。引用成员
    /// (`T&` 的 Deref) 与其它形态仍走原有语义。
    fn lower_member_object(
        &mut self,
        object_id: HirExprId,
    ) -> Result<(Operand, Type, bool), LowerIrError> {
        let obj_expr = self.get_expr(object_id)?;
        let inner_id = match &obj_expr.kind {
            HirExprKind::UnaryOp {
                op: UnaryOp::Deref,
                operand,
            } => *operand,
            HirExprKind::Dereference(inner) => *inner,
            _ => {
                // 非解引用包装: 按对象表达式自身形态处理
                let object = self.lower_expr(object_id)?;
                return self.deref_object_for_member(object_id, object);
            },
        };
        let inner_expr = self.get_expr(inner_id)?;
        if let CxxType::Pointer(pointee) = &inner_expr.ty {
            if matches!(pointee.as_ref(), CxxType::Class(_)) {
                let pointee_ty = self.lowerer.lower_type(pointee)?;
                let ptr = self.lower_expr(inner_id)?;
                return Ok((ptr, pointee_ty, true));
            }
        }
        // 解引用的不是 class 指针 (例如引用类型的隐式解引用): 保持原行为
        let object = self.lower_expr(object_id)?;
        self.deref_object_for_member(object_id, object)
    }

    /// 成员访问的对象若是 class 指针 (如 this), MemberLoad 的 obj
    /// 保留指针地址, 但 obj_ty 必须是指向的类类型; 对象为类值时原样返回.
    /// 返回 `(obj operand, 结构体类型, obj 是否为箭头基址)`.
    fn deref_object_for_member(
        &mut self,
        object_id: HirExprId,
        object: Operand,
    ) -> Result<(Operand, Type, bool), LowerIrError> {
        let obj_expr = self.get_expr(object_id)?;
        match &obj_expr.ty {
            CxxType::Pointer(inner) if matches!(inner.as_ref(), CxxType::Class(_)) => {
                let pointee_ty = self.lowerer.lower_type(inner)?;
                Ok((object, pointee_ty, true))
            },
            // 具名引用变量 (`P& r = ...; r.x`): 引用在 ABI 层的表示就是所指对象的
            // 地址, 所以成员访问必须走 `r->x` 形态。归到下面的 `_` 分支会把
            // `obj_ty` 降为 `Ptr(Struct)` 而 `is_arrow=false`, codegen 于是把**存放
            // 地址的栈槽**当作对象本体 (`lea -0x48(%r13)`), 读写都落在局部上
            // (实测 `P& r = ref(); r.x = 7;` 不生效)。
            CxxType::LValueRef(inner) | CxxType::RValueRef(inner)
                if matches!(inner.as_ref(), CxxType::Class(_)) =>
            {
                let pointee_ty = self.lowerer.lower_type(inner)?;
                Ok((object, pointee_ty, true))
            },
            _ => {
                let obj_ty = self.lowerer.lower_type(&obj_expr.ty)?;
                Ok((object, obj_ty, false))
            },
        }
    }

    /// 若 `expr_id` 是**自身即引用**的具名局部变量 (`int& r = x;` / 形参 `T& p`),
    /// 返回它所存放的那个地址 (不做间接访问)。否则 None。
    ///
    /// 判定基于**符号类型**而不是表达式类型: dcc 的 HIR 里引用变量的每次使用
    /// 都带 `T&`, 而「要值」(lvalue→rvalue, 见 `lower_expr`) 与「要地址」
    /// (绑定到引用、取 `&r`、作引用实参) 是两种不同的降级。单一真源在此。
    fn named_ref_slot_addr(&mut self, expr_id: HirExprId) -> Option<Operand> {
        let expr = self.get_expr(expr_id).ok()?;
        let sym_id = match &expr.kind {
            HirExprKind::Local(sym_id) => *sym_id,
            _ => return None,
        };
        let sym_ty = self.lowerer.get_symbol(sym_id).ok()?.ty.clone();
        if !matches!(sym_ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
            return None;
        }
        let name = self.lowerer.get_symbol(sym_id).ok()?.name.clone();
        let var = self.locals.get(&name).cloned()?;
        Some(Operand::Var(var))
    }

    /// 按**被初始化对象的类型**降低初始化式。
    ///
    /// 引用 (`T& r = obj;` / `T& ri = x;`) 绑定的是对象的**地址**, 不能按值拷贝:
    ///   - 初始化式本身已是引用 (如 `P& r2 = r;`、`P& r = f();`): 它的 ABI 表示就是
    ///     地址, 直接 `lower_expr`;
    ///   - 初始化式是一个左值 (具名对象 / 成员 / 解引用): 取它的左值地址。
    ///
    /// 缺这个判定时 `P& r = p;` 会把整个 struct 按值写到一个指针槽里 (实测段错误)。
    fn lower_init_value(
        &mut self,
        sym_ty: &CxxType,
        init_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        if matches!(sym_ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
            let init_expr = self.get_expr(init_id)?;
            if !matches!(init_expr.ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
                return self.lower_object_lvalue_addr(init_id);
            }
        }
        self.lower_expr(init_id)
    }

    /// 求 class **左值对象**的地址 (用于 `&obj.field`)。
    ///
    /// 与 `lower_member_object` 的区别: 后者面向 `MemberLoad`/`MemberStore`,
    /// 对 class 值对象返回结构体值操作数 (codegen 在成员访问里隐式取其地址);
    /// 而这里要的是可参与指针算术的**地址值**，必须自己发射 `&sym`。
    fn lower_object_lvalue_addr(&mut self, object_id: HirExprId) -> Result<Operand, LowerIrError> {
        let obj_expr = self.get_expr(object_id)?;
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        match &obj_expr.kind {
            // `p->m` / `(*p).m`: 对象地址就是指针本身 (引用 `T&` 在 ABI 层也是指针)
            HirExprKind::Dereference(inner) => self.lower_expr(*inner),
            HirExprKind::UnaryOp {
                op: UnaryOp::Deref,
                operand,
            } => self.lower_expr(*operand),
            // `this->m`: this 形参已是指针
            HirExprKind::This => {
                if self.ssa.params.is_empty() {
                    return Err(LowerIrError::Internal(
                        "this used in non-member function".to_string(),
                    ));
                }
                Ok(Operand::Var(self.ssa.params[0].clone()))
            },
            // 具名左值: 局部变量 / 静态局部 (linkage_name 指向其真名) / 全局对象
            HirExprKind::Local(sym_id) | HirExprKind::Global(sym_id) => {
                // 具名**引用**变量 (`P& a` 形参 / `int& r = x;` 局部): 它的存放
                // 就是被引用对象的地址, 取它的左值地址 = 读那个存放值。不能走
                // `lower_expr`: 引用变量的值语义现在是「载入所指对象」(见
                // `ref_rvalue` 分支), 两者必须分开。
                if let Some(addr) = self.named_ref_slot_addr(object_id) {
                    return Ok(addr);
                }
                // 具名**引用** (`P& a` 形参 / `P& r = x;` 局部): 它在 ABI 层已经是
                // 地址, 再发 `&a` 会取到存放地址的那个**槽** (双重间接) —— 实测
                // `P& pick(P& a, P& b){ return a; }` 因此返回栈槽地址。
                if matches!(obj_expr.ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
                    return self.lower_expr(object_id);
                }
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = match obj_expr.kind {
                    HirExprKind::Local(_) => {
                        sym.linkage_name.clone().unwrap_or_else(|| sym.name.clone())
                    },
                    _ => self.lowerer.symbol_name(sym),
                };
                Ok(self.lower_sym_addr(name, void_ptr_ty))
            },
            // 嵌套成员 `&a.b.c`: 先递归求 `&a.b`, 再加 `c` 在 `b` 类里的偏移
            HirExprKind::FieldAccess { object, field } => {
                let obj_expr = self.get_expr(*object)?;
                let class_id = class_of_object_type(&obj_expr.ty).ok_or_else(|| {
                    LowerIrError::Unsupported("address of field of non-class object".to_string())
                })?;
                let field_name = self.lowerer.get_symbol(*field)?.name.clone();
                let offset = self.lowerer.field_offset_in_class(class_id, &field_name)?;
                let base = self.lower_object_lvalue_addr(*object)?;
                self.compute_member_ptr(base, offset, &void_ptr_ty)
            },
            // 其余额式类型是引用: 它的 ABI 表示就 是地址
            _ => {
                if matches!(obj_expr.ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
                    return self.lower_expr(object_id);
                }
                Err(LowerIrError::Unsupported("address of non-addressable object".to_string()))
            },
        }
    }

    /// 成员访问的基对象是否是 volatile 限定的。
    /// 对于 `obj.field` 检查 obj 的类型是否 volatile；
    /// 对于 `ptr->field`（或指针基址）检查被指向的类型是否 volatile。
    fn obj_base_is_volatile(&self, object_id: HirExprId) -> bool {
        match self.get_expr(object_id) {
            Ok(expr) => expr.ty.is_volatile(),
            Err(_) => false,
        }
    }

    // --- address of / dereference ---

    fn lower_address_of(
        &mut self,
        inner_id: HirExprId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let inner_expr = self.get_expr(inner_id)?;

        match &inner_expr.kind {
            HirExprKind::Local(sym_id) => {
                // `&r` (r 是引用变量): 结果就是被引用对象的地址 —— 即 r 存放的
                // 那个值。发 `AddrOf{base: r}` 会取到**栈槽** (双重间接)。
                if let Some(addr) = self.named_ref_slot_addr(inner_id) {
                    return Ok(addr);
                }
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = sym.name.clone();
                // 局部变量地址 → AddrOf 指令
                let dest = self.fresh_syn("addr", result_ty.clone());
                let inst = Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::AddrOf { base: name },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                Ok(Operand::Var(dest))
            },
            HirExprKind::Global(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = self.lowerer.symbol_name(sym);
                // `&fn` 与 `fn` 是同一个值 ([conv.func]): 走与 `lower_expr` 的
                // Global 分支同一真源。不能用 `lower_sym_addr`: AddrOf 重建出的
                // `Ident` 类型硬编码为 `Void`, `gen_expr_addrof` 只认
                // `Type::Func` 才发 `lea sym(%rip)`。标量全局对象仍按 `SymAddr`
                // 占位 (codegen 按全局名解析)。
                if IrLowerer::is_function_value_symbol(sym) {
                    let fty = self.lowerer.lower_func_value_type(&sym.ty);
                    return Ok(Operand::SymAddr(name, fty));
                }
                Ok(Operand::SymAddr(name, result_ty.clone()))
            },
            HirExprKind::Dereference(inner_inner) => {
                // &*e → e (恒等)
                self.lower_expr(*inner_inner)
            },
            // &(*p) 的另一种编码: sema 把 `p->m()` 合成为
            // `Member(UnaryOp(Deref, p), m)`, 取 this 时得到
            // `AddressOf(UnaryOp{Deref})`. 语义与上面 Dereference 分支一致:
            // 地址即指针本身 (否则 `p->method()` 无法降级).
            HirExprKind::UnaryOp {
                op: UnaryOp::Deref,
                operand,
            } => self.lower_expr(*operand),
            // `&obj.field` / `&p->field`: 对象基址 + 字段偏移。
            // 偏移只从布局层查询 (ClassAbi::flatten_fields), 不在这里重算，
            // 因而虚基共享子对象的 `&o.__vb_Base` 与同类成员访问用的是
            // 同一个真值。
            //
            // 基址必须走 `lower_object_lvalue_addr` 而不是 `lower_member_object`:
            // 后者对 class **值**对象返回结构体值操作数 (MemberLoad 会隐式
            // 取地址), 直接当指针做 `+ off` 会先载入结构体首 8 字节,
            // 得到 vptr 而不是对象地址。
            HirExprKind::FieldAccess { object, field } => {
                let obj_expr = self.get_expr(*object)?;
                let class_id = class_of_object_type(&obj_expr.ty).ok_or_else(|| {
                    LowerIrError::Unsupported("address of field of non-class object".to_string())
                })?;
                let field_name = self.lowerer.get_symbol(*field)?.name.clone();
                let offset = self.lowerer.field_offset_in_class(class_id, &field_name)?;
                let object_op = self.lower_object_lvalue_addr(*object)?;
                self.compute_member_ptr(object_op, offset, result_ty)
            },
            // `&f()` / `&a[i]` 等: 只要表达式的**静态类型是左值引用**, 它在 ABI 层
            // 就已经是所指对象的地址, 取地址是恒等. 这一条让
            // `get_stream().method()` (sema 为 `this` 生成 AddressOf) 能降级,
            // 否则整个链式成员调用报 "address of complex expression".
            _ => {
                if matches!(inner_expr.ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
                    return self.lower_expr(inner_id);
                }
                Err(LowerIrError::Unsupported("address of complex expression".to_string()))
            },
        }
    }

    /// 生成唯一命名的临时 SSA 变量。
    ///
    /// `fresh_var` 用 (base, version) 区分同名变量, 而同一个 base 的所有
    /// version 共享同一个栈槽 —— 这对"真正的局部变量"是对的, 但用于表达式
    /// 临时值时会让同时存活的两个临时值互相覆盖 (如 `a[0] + a[1]` 的两次
    /// 下标读取都写到同一个槽)。临时值因此必须带自增序号, 与 C 前端
    /// `Builder::fresh` 的 `_t<n>_<base>` 命名保持一致。
    fn fresh_tmp(&mut self, base: &str, ty: Type) -> SsaVar {
        self.fresh_syn(base, ty)
    }

    /// 判断下标访问 `a[i]` 是否为 volatile 访问。
    ///
    /// `CxxType` 目前不在标量类型上携带 cv 限定符, 因此 volatile 信息挂在
    /// 符号上 (`CxxSymbol::is_volatile`)。这里对数组基址回溯到它对应的
    /// 局部/全局符号来判定, 保证 `volatile int a[2]` 的元素读写都带
    /// `is_volatile`, 不会被 DCE/DSE 消除。
    fn index_is_volatile(&self, array_id: HirExprId) -> bool {
        let expr = match self.get_expr(array_id) {
            Ok(e) => e,
            Err(_) => return false,
        };
        // 指针/数组元素本身带 volatile (如 `volatile int *p`)
        if expr.ty.is_volatile() {
            return true;
        }
        match &expr.kind {
            HirExprKind::Local(sym_id) | HirExprKind::Global(sym_id) => self
                .lowerer
                .get_symbol(*sym_id)
                .map(|s| s.is_volatile)
                .unwrap_or(false),
            HirExprKind::FieldAccess { object, field } => {
                let _ = field;
                self.index_is_volatile(*object)
            },
            _ => false,
        }
    }

    /// 降低下标访问 `a[i]`（读）→ `IndexLoad`。
    ///
    /// `result_ty` 就是**元素类型**（`HirExprKind::Index` 节点的 `ty`），必须
    /// 随 `InstKind::IndexLoad{ty}` 一路带到 SSA→AST 重建，否则元素内存步长会
    /// 默认按 `int` (4 字节) 计算。
    fn lower_index(
        &mut self,
        array_id: HirExprId,
        index_id: HirExprId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let arr = self.lower_expr(array_id)?;
        let idx = self.lower_expr(index_id)?;
        let is_volatile = self.index_is_volatile(array_id);
        let dest = self.fresh_tmp("idx", result_ty.clone());
        let inst = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::IndexLoad {
                arr,
                idx,
                ty: result_ty.clone(),
                is_volatile,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);
        Ok(Operand::Var(dest))
    }

    fn lower_dereference(
        &mut self,
        inner_id: HirExprId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let ptr = self.lower_expr(inner_id)?;
        let inner_expr = self.get_expr(inner_id)?;
        let is_volatile = inner_expr.ty.is_volatile();
        let dest = self.fresh_syn("deref", result_ty.clone());
        let inst = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Load {
                e: ptr,
                ty: result_ty.clone(),
                is_volatile,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);
        Ok(Operand::Var(dest))
    }

    // --- cast ---

    fn lower_cast(
        &mut self,
        expr_id: HirExprId,
        target: &CxxType,
    ) -> Result<Operand, LowerIrError> {
        let target_ty = self.lowerer.lower_type(target)?;
        let src = self.lower_expr(expr_id)?;
        let src_expr = self.get_expr(expr_id)?;
        let src_ty = self.lowerer.lower_type(&src_expr.ty)?;

        if src_ty == target_ty {
            return Ok(src);
        }

        let dest = self.fresh_syn("cast", target_ty.clone());
        let inst = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast {
                ty: target_ty,
                e: src,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);
        Ok(Operand::Var(dest))
    }

    // --- base cast (non-virtual inheritance) ---

    /// 非虚基类转换 lowering。
    /// 派生类指针/引用 → 基类指针/引用：加上 base offset。
    /// 若目标是 primary base（offset 0），则是 no-op 转换。
    fn lower_base_cast(
        &mut self,
        object_id: HirExprId,
        from_class: ClassId,
        to_class: ClassId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let object = self.lower_expr(object_id)?;

        // 同类转换：直接返回
        if from_class == to_class {
            return Ok(object);
        }

        // 查找基类偏移
        let base_layout = self
            .lowerer
            .find_base_layout(from_class, to_class)?
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "base class {} not found in class {}",
                    to_class.0, from_class.0
                ))
            })?;

        let offset = base_layout.offset as i64;

        // primary base (offset 0)：no-op，仅类型转换
        if offset == 0 {
            let src_expr = self.get_expr(object_id)?;
            let src_ty = self.lowerer.lower_type(&src_expr.ty)?;
            if src_ty == *result_ty {
                return Ok(object);
            }
            let dest = self.fresh_syn("base_cast", result_ty.clone());
            let inst = Inst {
                dest: Some(dest.clone()),
                kind: InstKind::Cast {
                    ty: result_ty.clone(),
                    e: object,
                },
                span: dummy_span(),
            };
            self.push_inst(inst);
            return Ok(Operand::Var(dest));
        }

        // 非 primary base：需要加偏移
        // 1. 转成 i8* 指针
        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
        let byte_ptr = self.fresh_syn("base_byte_ptr", byte_ptr_ty.clone());
        let cast_to_byte = Inst {
            dest: Some(byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: object,
            },
            span: dummy_span(),
        };
        self.push_inst(cast_to_byte);

        // 2. 加上偏移
        let offset_op = Operand::Int(offset as i128);
        let adjusted_byte_ptr = self.fresh_syn("base_adj_byte", byte_ptr_ty.clone());
        let gep_inst = Inst {
            dest: Some(adjusted_byte_ptr.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(byte_ptr),
                r: offset_op,
            },
            span: dummy_span(),
        };
        self.push_inst(gep_inst);

        // 3. 转回目标指针类型
        let dest = self.fresh_syn("base_cast", result_ty.clone());
        let cast_back = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast {
                ty: result_ty.clone(),
                e: Operand::Var(adjusted_byte_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_back);

        Ok(Operand::Var(dest))
    }

    // --- vbase adjust (virtual inheritance) ---

    /// 虚基类调整 lowering。
    /// 从派生类指针调整到虚基类指针：从 vtable 中读取 vbase offset，然后加到 this 指针上。
    ///
    /// 实现步骤:
    /// 1. 获取对象指针
    /// 2. 转换为 vptr 类型 (*const *const u8)
    /// 3. 从 vptr 加载 vtable 指针
    /// 4. 计算 vbase offset 槽的地址 (vtable_ptr + vtable_offset)
    ///    注意: vbase offset 在 address point 之前（负偏移）
    /// 5. 从 vtable 加载 vbase offset (i64)
    /// 6. 将 vbase offset 加到对象指针上
    /// 7. 转换为结果类型
    fn lower_vbase_adjust(
        &mut self,
        object_id: HirExprId,
        from_class: ClassId,
        to_class: ClassId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let object = self.lower_expr(object_id)?;

        // 同类转换：直接返回
        if from_class == to_class {
            return Ok(object);
        }

        // 获取 from_class 的 ABI
        let from_abi = self.lowerer.abis.get(&from_class).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", from_class.0))
        })?;

        // 虚基子对象的 vbtable 布局数据由布局层唯一给定, 这里只查询不重算:
        //   vbase_index    —— 该虚基在 vbtable 里的槽序
        //   vtable_offset  —— 该槽相对 vptr (address point) 的字节偏移 (负值)
        // vtable 布局（从低地址到高地址）:
        //   [vbase_offset_0] ... [vbase_offset_n-1] [offset_to_top] [typeinfo] [vfunc_0] ...
        //                                                                     ^ address_point
        let vbase = from_abi
            .virtual_bases
            .iter()
            .find(|vb| vb.base == to_class)
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "virtual base class {} not found in class {}",
                    to_class.0, from_class.0
                ))
            })?;
        let vtable_offset_bytes = vbase.vtable_offset;

        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));

        // 1. 将对象指针转换为 byte 指针（保存一份用于后面加偏移）
        let obj_byte_ptr = self.fresh_syn("obj_byte_ptr", byte_ptr_ty.clone());
        let cast_obj_to_byte = Inst {
            dest: Some(obj_byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: object.clone(),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_obj_to_byte);

        // 2. 将对象指针转换为 *const *const u8 (vptr 的类型)
        let vptr_ptr_ty =
            Type::Ptr(Box::new(QualType::new(Type::Ptr(Box::new(QualType::new(Type::UChar))))));
        let vptr_ptr = self.fresh_syn("vptr_ptr", vptr_ptr_ty.clone());
        let cast_to_vptr_ptr = Inst {
            dest: Some(vptr_ptr.clone()),
            kind: InstKind::Cast {
                ty: vptr_ptr_ty,
                e: object,
            },
            span: dummy_span(),
        };
        self.push_inst(cast_to_vptr_ptr);

        // 3. 从 vptr 加载 vtable 指针
        let vtable_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::UChar)));
        let vtable_ptr = self.fresh_syn("vtable_ptr", vtable_ptr_ty.clone());
        let load_vtable = Inst {
            dest: Some(vtable_ptr.clone()),
            kind: InstKind::Load {
                e: Operand::Var(vptr_ptr),
                ty: vtable_ptr_ty,
                is_volatile: false,
            },
            span: dummy_span(),
        };
        self.push_inst(load_vtable);

        // 4. 计算 vbase offset 槽的地址
        let vtable_byte_ptr = self.fresh_syn("vtable_byte", byte_ptr_ty.clone());
        let cast_vtable_to_byte = Inst {
            dest: Some(vtable_byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: Operand::Var(vtable_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_vtable_to_byte);

        let vbase_slot_ptr = self.fresh_syn("vbase_slot", byte_ptr_ty.clone());
        let gep_vbase_slot = Inst {
            dest: Some(vbase_slot_ptr.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(vtable_byte_ptr),
                r: Operand::Int(vtable_offset_bytes as i128),
            },
            span: dummy_span(),
        };
        self.push_inst(gep_vbase_slot);

        // 5. 从 vtable 加载 vbase offset (i64)
        let vbase_offset_ty = Type::Long; // i64
        let vbase_offset_ptr_ty = Type::Ptr(Box::new(QualType::new(vbase_offset_ty.clone())));
        let vbase_offset_ptr = self.fresh_syn("vbase_offset_ptr", vbase_offset_ptr_ty.clone());
        let cast_to_vbase_offset_ptr = Inst {
            dest: Some(vbase_offset_ptr.clone()),
            kind: InstKind::Cast {
                ty: vbase_offset_ptr_ty,
                e: Operand::Var(vbase_slot_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_to_vbase_offset_ptr);

        let vbase_offset = self.fresh_syn("vbase_offset", vbase_offset_ty.clone());
        let load_vbase_offset = Inst {
            dest: Some(vbase_offset.clone()),
            kind: InstKind::Load {
                e: Operand::Var(vbase_offset_ptr),
                ty: vbase_offset_ty,
                is_volatile: false,
            },
            span: dummy_span(),
        };
        self.push_inst(load_vbase_offset);

        // 6. 将 vbase offset 加到对象指针上
        let adjusted_byte_ptr = self.fresh_syn("vbase_adj_byte", byte_ptr_ty.clone());
        let add_vbase_offset = Inst {
            dest: Some(adjusted_byte_ptr.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(obj_byte_ptr),
                r: Operand::Var(vbase_offset),
            },
            span: dummy_span(),
        };
        self.push_inst(add_vbase_offset);

        // 7. 转换为结果类型
        let dest = self.fresh_syn("vbase_adjust", result_ty.clone());
        let cast_back = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast {
                ty: result_ty.clone(),
                e: Operand::Var(adjusted_byte_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_back);

        Ok(Operand::Var(dest))
    }

    // --- dynamic_cast ---

    /// `typeid(e)` 的动态路径 (Itanium ABI)。
    ///
    /// vtable 布局: `[VBaseOffset×n][offset_to_top][typeinfo][vfunc_0]...`，
    /// vptr 指向 address point (即 typeinfo 后一个slot)，所以
    /// **typeinfo 槽就在 `vptr - 8`**。读取它得到的就是最派生对象的
    /// `type_info` 对象地址，与 `typeid(T)` 静态路径的 `_ZTI{T}` 同一符号
    /// ——这是 typeid 比较能成立的根。
    fn lower_typeinfo_dyn(&mut self, object_id: HirExprId) -> Result<Operand, LowerIrError> {
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
        let vptr_ty = Type::Ptr(Box::new(QualType::new(Type::UChar)));

        // 1. 对象左值地址 → `(uchar**)&obj`
        let obj_addr = self.lower_object_lvalue_addr(object_id)?;
        let vptr_pp_ty = Type::Ptr(Box::new(QualType::new(vptr_ty.clone())));
        let vptr_pp = self.fresh_syn("ti_vptr_pp", vptr_pp_ty.clone());
        self.push_inst(Inst {
            dest: Some(vptr_pp.clone()),
            kind: InstKind::Cast {
                ty: vptr_pp_ty,
                e: obj_addr,
            },
            span: dummy_span(),
        });

        // 2. vptr = *vptr_pp
        let vptr = self.fresh_syn("ti_vptr", vptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(vptr.clone()),
            kind: InstKind::Load {
                e: Operand::Var(vptr_pp),
                ty: vptr_ty.clone(),
                is_volatile: false,
            },
            span: dummy_span(),
        });

        // 3. typeinfo 槽地址 = (char*)vptr - 8
        let vptr_byte = self.fresh_syn("ti_vptr_byte", byte_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(vptr_byte.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: Operand::Var(vptr),
            },
            span: dummy_span(),
        });
        let ti_slot = self.fresh_syn("ti_slot", byte_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(ti_slot.clone()),
            kind: InstKind::Bin {
                op: BinOp::Sub,
                l: Operand::Var(vptr_byte),
                r: Operand::Int(POINTER_SIZE as i128),
            },
            span: dummy_span(),
        });

        // 4. ti = *(void**)ti_slot
        let ti_slot_ptr_ty = Type::Ptr(Box::new(QualType::new(void_ptr_ty.clone())));
        let ti_slot_ptr = self.fresh_syn("ti_slot_ptr", ti_slot_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(ti_slot_ptr.clone()),
            kind: InstKind::Cast {
                ty: ti_slot_ptr_ty,
                e: Operand::Var(ti_slot),
            },
            span: dummy_span(),
        });
        let ti = self.fresh_syn("ti_dyn", void_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(ti.clone()),
            kind: InstKind::Load {
                e: Operand::Var(ti_slot_ptr),
                ty: void_ptr_ty,
                is_volatile: false,
            },
            span: dummy_span(),
        });
        Ok(Operand::Var(ti))
    }

    /// dynamic_cast lowering。
    ///
    /// 调用 __dynamic_cast runtime 函数:
    ///   void* __dynamic_cast(const void* sub,
    ///                        const __class_type_info* src,
    ///                        const __class_type_info* dst,
    ///                        ptrdiff_t src2dst);
    ///
    /// src2dst 是提示参数:
    ///   -1: 向上转型（src 是派生类，dst 是基类）
    ///    0: 无提示
    ///    2: 向下转型到公有基类
    ///
    /// 对于非多态类型，dynamic_cast 等价于 static_cast（编译期确定）。
    fn lower_dynamic_cast(
        &mut self,
        object_id: HirExprId,
        src_ty: &CxxType,
        dst_ty: &CxxType,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use crate::cpp::runtime::CxxRuntimeSymbol;

        let object = self.lower_expr(object_id)?;

        // 提取源和目标的类类型（从指针类型中）
        let src_class = match src_ty {
            CxxType::Pointer(inner) => match inner.as_ref() {
                CxxType::Class(cid) => Some(*cid),
                _ => None,
            },
            CxxType::Class(cid) => Some(*cid),
            _ => None,
        };

        let dst_class = match dst_ty {
            CxxType::Pointer(inner) => match inner.as_ref() {
                CxxType::Class(cid) => Some(*cid),
                _ => None,
            },
            CxxType::Class(cid) => Some(*cid),
            _ => None,
        };

        let (src_class, dst_class) = match (src_class, dst_class) {
            (Some(s), Some(d)) => (s, d),
            _ => {
                return Err(LowerIrError::Unsupported("dynamic_cast on non-class types".into()));
            },
        };

        // 同类转换：直接返回
        if src_class == dst_class {
            return Ok(object);
        }

        // 获取源类的 ABI，判断是否为多态类
        let src_abi = self.lowerer.abis.get(&src_class).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", src_class.0))
        })?;

        // 非多态类型：dynamic_cast 等价于 static_cast（只能向上转型）
        if !src_abi.is_polymorphic() {
            // 简化：直接做指针 cast（非多态类型只能向上转型，且偏移为 0 或已知）
            // 这里我们直接返回 cast 后的结果
            let dest = self.fresh_syn("dyn_cast_static", result_ty.clone());
            let inst = Inst {
                dest: Some(dest.clone()),
                kind: InstKind::Cast {
                    ty: result_ty.clone(),
                    e: object,
                },
                span: dummy_span(),
            };
            self.push_inst(inst);
            return Ok(Operand::Var(dest));
        }

        // 多态类型：调用 __dynamic_cast runtime
        // 获取 src 和 dst 的 typeinfo 符号
        let src_typeinfo = src_abi.rtti.typeinfo_symbol.clone();
        let dst_abi = self.lowerer.abis.get(&dst_class).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", dst_class.0))
        })?;
        let dst_typeinfo = dst_abi.rtti.typeinfo_symbol.clone();

        // 将 typeinfo 符号加入 rodata
        if !self.lowerer.output.rodata.contains(&src_typeinfo) {
            self.lowerer.output.rodata.push(src_typeinfo.clone());
        }
        if !self.lowerer.output.rodata.contains(&dst_typeinfo) {
            self.lowerer.output.rodata.push(dst_typeinfo.clone());
        }

        // type_info* 类型：*const u8
        let typeinfo_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::UChar)));

        // __dynamic_cast 函数类型：void* (*)(const void*, const type_info*, const type_info*, ptrdiff_t)
        let dyn_cast_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Func(
            Box::new(QualType::new(Type::Ptr(Box::new(QualType::new(Type::Void))))),
            vec![
                QualType::new(Type::Ptr(Box::new(QualType::new(Type::Void)))),
                QualType::new(typeinfo_ptr_ty.clone()),
                QualType::new(typeinfo_ptr_ty.clone()),
                QualType::new(Type::Long), // ptrdiff_t
            ],
        ))));

        // 构造参数
        // 1. object 指针（转为 void*）
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let obj_void_ptr = self.fresh_syn("dyn_cast_obj", void_ptr_ty.clone());
        let cast_obj = Inst {
            dest: Some(obj_void_ptr.clone()),
            kind: InstKind::Cast {
                ty: void_ptr_ty.clone(),
                e: object,
            },
            span: dummy_span(),
        };
        self.push_inst(cast_obj);

        // 2. src typeinfo
        let src_ti_op = self.lower_sym_addr(src_typeinfo, typeinfo_ptr_ty.clone());

        // 3. dst typeinfo
        let dst_ti_op = self.lower_sym_addr(dst_typeinfo, typeinfo_ptr_ty.clone());

        // 4. src2dst hint (简化为 0，表示无提示)
        let hint_op = Operand::Int(0);

        // 调用 __dynamic_cast
        let fn_name = CxxRuntimeSymbol::DynamicCast.name().to_string();

        // 如果是 runtime symbol，需要注册为 external
        if (fn_name.starts_with("dccpp_")
            || fn_name.starts_with("__dcc_")
            || fn_name.starts_with("__cxa_")
            || fn_name == "__dynamic_cast")
            && !self.lowerer.output.external_symbols.contains(&fn_name)
        {
            self.lowerer.output.external_symbols.push(fn_name.clone());
        }

        let fn_addr = Operand::FnAddr(fn_name, dyn_cast_fn_ty);

        let result_void_ptr = self.fresh_syn("dyn_cast_result", void_ptr_ty.clone());
        let call_inst = Inst {
            dest: Some(result_void_ptr.clone()),
            kind: InstKind::Call {
                callee: fn_addr,
                args: vec![Operand::Var(obj_void_ptr), src_ti_op, dst_ti_op, hint_op],
                ret_ty: void_ptr_ty.clone(),
            },
            span: dummy_span(),
        };
        self.push_inst(call_inst);

        // 将结果转换为目标类型
        let dest = self.fresh_syn("dyn_cast_final", result_ty.clone());
        let cast_result = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast {
                ty: result_ty.clone(),
                e: Operand::Var(result_void_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_result);

        Ok(Operand::Var(dest))
    }

    // --- construct / destroy ---

    /// 构造函数调用 lowering（placement new 风格）。
    fn lower_construct(
        &mut self,
        class: ClassId,
        place_id: HirExprId,
        args: &[HirExprId],
        variant: CtorVariant,
    ) -> Result<Operand, LowerIrError> {
        let place = self.lower_expr(place_id)?;
        let class_sym = self.lowerer.get_symbol(class.as_symbol())?;

        // 构造函数名：<class>_<variant>
        let class_name = self.lowerer.symbol_name(class_sym);
        let ctor_name = ctor_variant_name(&class_name, variant);

        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));

        let mut lowered_args = Vec::with_capacity(1 + args.len());
        // this 指针作为第一个参数
        lowered_args.push(place);
        for arg_id in args {
            lowered_args.push(self.lower_expr(*arg_id)?);
        }

        let callee_op = Operand::FnAddr(ctor_name, fn_ty);

        let inst = Inst {
            dest: None,
            kind: InstKind::Call {
                callee: callee_op,
                args: lowered_args,
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(Operand::Void)
    }

    /// 析构函数调用 lowering。
    fn lower_destroy(
        &mut self,
        class: ClassId,
        place_id: HirExprId,
        variant: DtorVariant,
    ) -> Result<Operand, LowerIrError> {
        let place = self.lower_expr(place_id)?;
        let class_sym = self.lowerer.get_symbol(class.as_symbol())?;

        // 析构函数名：<class>_dtor_<variant>
        let class_name = self.lowerer.symbol_name(class_sym);
        let dtor_name = match variant {
            DtorVariant::Complete => format!("{}_dtor_complete", class_name),
            DtorVariant::Base => format!("{}_dtor_base", class_name),
            DtorVariant::Deleting => format!("{}_dtor_deleting", class_name),
        };

        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));

        let callee_op = Operand::FnAddr(dtor_name, fn_ty);

        let inst = Inst {
            dest: None,
            kind: InstKind::Call {
                callee: callee_op,
                args: vec![place],
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(Operand::Void)
    }

    /// 降低 throw 表达式（SJLJ 风格异常处理）
    ///
    /// 流程:
    /// 1. rethrow (throw;)：调用 __dcc_eh_rethrow
    /// 2. throw expr：
    ///    - 计算抛出对象大小
    ///    - 调用 __cxa_allocate_exception 分配异常内存
    ///    - 把对象拷贝到异常内存
    ///    - 调用 __dcc_eh_throw 抛出异常（SJLJ 风格，内部 longjmp）
    fn lower_throw(
        &mut self,
        object: Option<HirExprId>,
        ty: &CxxType,
    ) -> Result<Operand, LowerIrError> {
        use crate::cpp::runtime::CxxRuntimeSymbol;

        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));

        match object {
            None => {
                // rethrow: 调用 __dcc_eh_rethrow
                let rethrow_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                let callee_op = Operand::FnAddr("__dcc_eh_rethrow".to_string(), rethrow_fn_ty);
                let inst = Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: callee_op,
                        args: vec![],
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                // throw 不返回，标记当前块为不可达
                self.set_term(Terminator::Unreachable);
                Ok(Operand::Void)
            },
            Some(obj_id) => {
                // 计算抛出对象大小
                let obj_size = self.compute_type_size(ty)?;
                let size_op = Operand::Int(obj_size as i128);

                // 调用 __cxa_allocate_exception(size)
                let alloc_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                let alloc_callee = Operand::FnAddr(
                    CxxRuntimeSymbol::AllocException.name().to_string(),
                    alloc_fn_ty.clone(),
                );
                let exn_ptr_var = self.fresh_syn("exn_ptr", void_ptr_ty.clone());
                let alloc_call = Inst {
                    dest: Some(exn_ptr_var.clone()),
                    kind: InstKind::Call {
                        callee: alloc_callee,
                        args: vec![size_op],
                        ret_ty: void_ptr_ty.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(alloc_call);

                // 把抛出对象的值拷贝到异常内存
                let obj_val = self.lower_expr(obj_id)?;
                let obj_ir_ty = self.lowerer.lower_type(ty)?;
                // 把 void* 转为目标类型指针
                let typed_ptr_var = self
                    .fresh_var("exn_typed", Type::Ptr(Box::new(QualType::new(obj_ir_ty.clone()))));
                let cast_inst = Inst {
                    dest: Some(typed_ptr_var.clone()),
                    kind: InstKind::Cast {
                        ty: Type::Ptr(Box::new(QualType::new(obj_ir_ty.clone()))),
                        e: Operand::Var(exn_ptr_var.clone()),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_inst);

                // 存储值到异常内存
                let store_inst = Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: Operand::Var(typed_ptr_var),
                        src: obj_val,
                        is_volatile: false,
                    },
                    span: dummy_span(),
                };
                self.push_inst(store_inst);

                // 调用 __dcc_eh_throw(exn_ptr, typeinfo)
                // 简化：typeinfo 暂时传 null，SJLJ 运行时用类型名匹配
                // 先获取 typeinfo（如果是类类型）
                let typeinfo_op = self.get_typeinfo_for(ty)?;

                let throw_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                let fn_name = "__dcc_eh_throw".to_string();

                // 如果是 runtime symbol，需要注册为 external
                if (fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_"))
                    && !self.lowerer.output.external_symbols.contains(&fn_name)
                {
                    self.lowerer.output.external_symbols.push(fn_name.clone());
                }

                let throw_callee = Operand::FnAddr(fn_name, throw_fn_ty);
                let throw_call = Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: throw_callee,
                        args: vec![Operand::Var(exn_ptr_var), typeinfo_op],
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                };
                self.push_inst(throw_call);

                // throw 不返回
                self.set_term(Terminator::Unreachable);
                Ok(Operand::Void)
            },
        }
    }

    // --- new / delete ---

    /// new 表达式 lowering。
    ///
    /// - scalar new: 调用 operator new(size_t) + 可选构造函数
    /// - array new: 调用 operator new[](size_t)
    /// - placement new: 不分配内存，直接在指定地址构造
    fn lower_new(
        &mut self,
        ty: &CxxType,
        args: &[HirExprId],
        placement: Option<&[HirExprId]>,
        is_array: bool,
    ) -> Result<Operand, LowerIrError> {
        use crate::cpp::runtime::CxxRuntimeSymbol;

        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let ret_ty = self
            .lowerer
            .lower_type(&CxxType::Pointer(Box::new(ty.clone())))?;

        if let Some(place_args) = placement {
            // placement new: 不分配内存，直接在 ptr 上构造
            // 返回值就是 placement 地址（cast 到目标类型指针）
            let ptr_val = self.lower_expr(place_args[0])?;

            // 如果是类类型则调用构造函数。早期只在 `!args.is_empty()` 时构造,
            // 于是 `new (buf) T()` 不跑默认构造 (vptr/成员都不初始化)。
            if let CxxType::Class(class_id) = ty {
                if !args.is_empty() || self.has_ctor_definition(*class_id, CtorVariant::Complete) {
                    // 构造函数需要 this 指针，placement ptr 就是 this
                    self.lower_construct(*class_id, place_args[0], args, CtorVariant::Complete)?;
                }
            }

            // 把 void* / 其他指针类型 cast 到目标类型指针
            let typed_ptr_var = self.fresh_syn("new_result", ret_ty.clone());
            let cast_inst = Inst {
                dest: Some(typed_ptr_var.clone()),
                kind: InstKind::Cast {
                    ty: ret_ty,
                    e: ptr_val,
                },
                span: dummy_span(),
            };
            self.push_inst(cast_inst);
            return Ok(Operand::Var(typed_ptr_var));
        }

        // 计算分配大小
        let alloc_size = if is_array {
            // array new: size = sizeof(T) * count
            // args[0] 是数组大小表达式
            if args.is_empty() {
                return Err(LowerIrError::Internal("array new missing size argument".to_string()));
            }
            let elem_size = self.compute_type_size(ty)?;
            let count_val = self.lower_expr(args[0])?;
            let elem_size_op = Operand::Int(elem_size as i128);
            let size_var = self.fresh_syn("alloc_size", Type::ULong);
            let mul_inst = Inst {
                dest: Some(size_var.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Mul,
                    l: elem_size_op,
                    r: count_val,
                },
                span: dummy_span(),
            };
            self.push_inst(mul_inst);
            Operand::Var(size_var)
        } else {
            // scalar new: size = sizeof(T)
            let sz = self.compute_type_size(ty)?;
            Operand::Int(sz as i128)
        };

        // 调用 operator new / operator new[]
        let op_sym = if is_array {
            CxxRuntimeSymbol::NewArray
        } else {
            CxxRuntimeSymbol::New
        };
        let new_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let fn_name = op_sym.name().to_string();

        // 如果是 runtime symbol，需要注册为 external
        if (fn_name.starts_with("dccpp_")
            || fn_name.starts_with("__dcc_")
            || fn_name.starts_with("__cxa_")
            || fn_name == "__dynamic_cast"
            || fn_name.starts_with("_Z"))
            && !self.lowerer.output.external_symbols.contains(&fn_name)
        {
            self.lowerer.output.external_symbols.push(fn_name.clone());
        }

        let new_callee = Operand::FnAddr(fn_name, new_fn_ty.clone());
        let raw_ptr_var = self.fresh_syn("raw_new", void_ptr_ty.clone());
        let new_call = Inst {
            dest: Some(raw_ptr_var.clone()),
            kind: InstKind::Call {
                callee: new_callee,
                args: vec![alloc_size],
                ret_ty: void_ptr_ty.clone(),
            },
            span: dummy_span(),
        };
        self.push_inst(new_call);

        // 如果是类类型则调用构造函数（scalar new only）。
        //
        // 早期只在 `!args.is_empty()` 时构造: `new D` / `new D()` 连 vptr 都不写,
        // 对象处于未构造状态 —— 之后 `delete (Base*)d` 读 vptr 即段错误。
        // 现在只要该类确有已登记的 `<class>_ctor_complete` 定义就调用; 没有定义
        // (纯聚合 `new P`) 保持只分配, 不凭空引入 undefined symbol。
        if !is_array {
            if let CxxType::Class(class_id) = ty {
                let has_ctor_definition =
                    self.has_ctor_definition(*class_id, CtorVariant::Complete);
                if !args.is_empty() || has_ctor_definition {
                    // 构造函数需要 this 指针
                    // 先把 raw_ptr 转成目标类型指针
                    let target_ptr_ty =
                        Type::Ptr(Box::new(QualType::new(self.lowerer.lower_type(ty)?)));
                    let typed_ptr_var = self.fresh_syn("typed_new", target_ptr_ty.clone());
                    let cast_inst = Inst {
                        dest: Some(typed_ptr_var.clone()),
                        kind: InstKind::Cast {
                            ty: target_ptr_ty,
                            e: Operand::Var(raw_ptr_var.clone()),
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(cast_inst);

                    // 调用构造函数
                    // 注意：这里需要用 HirExprId 来表示指针，但我们只有 Operand
                    // 简化：直接生成构造函数调用，this 指针就是 typed_ptr_var
                    let class_sym = self.lowerer.get_symbol(class_id.as_symbol())?;
                    let ctor_name = ctor_variant_name(
                        &self.lowerer.symbol_name(class_sym),
                        CtorVariant::Complete,
                    );
                    let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                    let mut ctor_args = vec![Operand::Var(typed_ptr_var)];
                    for arg_id in args {
                        ctor_args.push(self.lower_expr(*arg_id)?);
                    }
                    let ctor_callee = Operand::FnAddr(ctor_name, fn_ty);
                    let ctor_call = Inst {
                        dest: None,
                        kind: InstKind::Call {
                            callee: ctor_callee,
                            args: ctor_args,
                            ret_ty: Type::Void,
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(ctor_call);
                }
            }
        }

        // 把 void* 转成目标类型指针
        let result_var = self.fresh_syn("new_result", ret_ty.clone());
        let cast_inst = Inst {
            dest: Some(result_var.clone()),
            kind: InstKind::Cast {
                ty: ret_ty,
                e: Operand::Var(raw_ptr_var),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_inst);
        Ok(Operand::Var(result_var))
    }

    /// delete 表达式 lowering。
    ///
    /// - scalar delete: 调用 operator delete(void*)
    /// - array delete: 调用 operator delete[](void*)
    fn lower_delete(
        &mut self,
        ptr: HirExprId,
        is_array: bool,
        dtor: Option<crate::cpp::hir::HirDeleteDtor>,
    ) -> Result<Operand, LowerIrError> {
        // 指针表达式**只求值一次**: 析构与释放共用同一个值。
        let ptr_val = self.lower_expr(ptr)?;

        // C++ [expr.delete]/3: 先跑析构, 再调 operator delete。
        if let Some(dtor) = dtor {
            self.emit_delete_dtor(dtor, &ptr_val)?;
        }

        self.emit_delete_free(ptr_val, is_array)?;

        Ok(Operand::Void)
    }

    /// 发射 `delete` 前的析构调用 (与析构后的 `operator delete` 共用同一 this 值)。
    fn emit_delete_dtor(
        &mut self,
        dtor: crate::cpp::hir::HirDeleteDtor,
        ptr_val: &Operand,
    ) -> Result<(), LowerIrError> {
        use crate::cpp::hir::HirDeleteDtor;

        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let callee = match dtor {
            HirDeleteDtor::Direct { class } => {
                // 非多态点型: 编译期绑定本类 D1 `<class>_dtor_complete`
                // (与 `lower_destroy` 同一命名约定)。
                let class_sym = self.lowerer.get_symbol(class.as_symbol())?;
                let class_name = self.lowerer.symbol_name(class_sym);
                Operand::FnAddr(dtor_variant_name(&class_name, DtorVariant::Complete), fn_ty)
            },
            HirDeleteDtor::VirtualDtor { class, slot } => {
                // 多态点型: 经该静态点型主 vtable 的**析构槽**虚派发,
                // 使 `delete (Base*)derived` 销毁完整派生对象。
                let vptr_ptr_ty = self.lowerer.vptr_ptr_type();
                let vptr_ptr = self.fresh_syn("del_vptr_ptr", vptr_ptr_ty.clone());
                self.push_inst(Inst {
                    dest: Some(vptr_ptr.clone()),
                    kind: InstKind::Cast {
                        ty: vptr_ptr_ty,
                        e: ptr_val.clone(),
                    },
                    span: dummy_span(),
                });
                // 槽号仍以布局层为真源 (析构槽在基/派生类里是同一条),
                // 查不到时回退 sema 登记值。
                let slot = self
                    .lowerer
                    .abis
                    .get(&class)
                    .and_then(|abi| abi.vcall_slot("~"))
                    .unwrap_or(slot);
                let fp = self.load_vtable_entry(&Operand::Var(vptr_ptr), slot)?;
                Operand::Var(fp)
            },
        };
        self.push_inst(Inst {
            dest: None,
            kind: InstKind::Call {
                callee,
                args: vec![ptr_val.clone()],
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        });
        Ok(())
    }

    /// 发射 `operator delete`(ptr) / `operator delete[]`(ptr) 调用。
    fn emit_delete_free(&mut self, ptr_val: Operand, is_array: bool) -> Result<(), LowerIrError> {
        use crate::cpp::runtime::CxxRuntimeSymbol;

        let op_sym = if is_array {
            CxxRuntimeSymbol::DeleteArray
        } else {
            CxxRuntimeSymbol::Delete
        };

        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let delete_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let fn_name = op_sym.name().to_string();

        // 如果是 runtime symbol，需要注册为 external
        if (fn_name.starts_with("dccpp_")
            || fn_name.starts_with("__dcc_")
            || fn_name.starts_with("__cxa_")
            || fn_name == "__dynamic_cast"
            || fn_name.starts_with("_Z"))
            && !self.lowerer.output.external_symbols.contains(&fn_name)
        {
            self.lowerer.output.external_symbols.push(fn_name.clone());
        }

        let delete_callee = Operand::FnAddr(fn_name, delete_fn_ty);

        // 把指针转成 void*
        let void_ptr_var = self.fresh_syn("del_void", void_ptr_ty.clone());
        let cast_inst = Inst {
            dest: Some(void_ptr_var.clone()),
            kind: InstKind::Cast {
                ty: void_ptr_ty,
                e: ptr_val,
            },
            span: dummy_span(),
        };
        self.push_inst(cast_inst);

        let delete_call = Inst {
            dest: None,
            kind: InstKind::Call {
                callee: delete_callee,
                args: vec![Operand::Var(void_ptr_var)],
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        };
        self.push_inst(delete_call);

        Ok(())
    }

    /// 花括号初始化列表 lowering。
    ///
    /// 创建一个临时数组局部变量，逐个存入元素，返回数组首地址。
    fn lower_init_list(
        &mut self,
        elems: &[HirExprId],
        arr_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        use std::sync::Arc;

        // 生成唯一的临时变量名
        let tmp_name = format!("__init_list_{}", self.init_list_counter);
        self.init_list_counter += 1;

        // 注册为局部变量
        self.ssa
            .local_types
            .insert(tmp_name.clone(), arr_ty.clone());
        self.ssa.original_locals.insert(tmp_name.clone());

        let var = self.fresh_var(&tmp_name, arr_ty.clone());
        self.locals.insert(tmp_name.clone(), var.clone());

        // 逐个存储元素
        let elem_ty = match arr_ty {
            Type::Array(inner, _) => inner.base.clone(),
            other => other.clone(),
        };
        for (i, elem_id) in elems.iter().enumerate() {
            let elem_val = self.lower_expr(*elem_id)?;
            let idx_op = Operand::Int(i as i128);
            let store_inst = Inst {
                dest: None,
                kind: InstKind::IndexStore {
                    arr: Operand::Var(var.clone()),
                    idx: idx_op,
                    src: elem_val,
                    ty: elem_ty.clone(),
                    is_volatile: false,
                },
                span: dummy_span(),
            };
            self.push_inst(store_inst);
        }

        // 返回数组首地址（退化为指针）
        let elem_qt = match arr_ty {
            Type::Array(inner, _) => inner.as_ref().clone(),
            _ => QualType::new(arr_ty.clone()),
        };
        let ptr_ty = Type::Ptr(Box::new(elem_qt));
        let addr_var = self.fresh_syn("init_list_ptr", ptr_ty.clone());
        let addr_inst = Inst {
            dest: Some(addr_var.clone()),
            kind: InstKind::AddrOf { base: tmp_name },
            span: dummy_span(),
        };
        self.push_inst(addr_inst);

        Ok(Operand::Var(addr_var))
    }

    /// 该类是否有**已登记的构造器定义** (`<class>_ctor_{complete,base}`)。
    ///
    /// `new` / placement new 发不发构造调用以此为准: sema 只为「需要初始化的类」
    /// 合成隐式默认构造 (`synthesize_implicit_default_ctor`), 对纯聚合体没有定义
    /// 就不发调用, 免得凭空引入 undefined symbol。
    fn has_ctor_definition(&self, class: ClassId, variant: CtorVariant) -> bool {
        let Ok(sym) = self.lowerer.get_symbol(class.as_symbol()) else {
            return false;
        };
        let ctor_name = ctor_variant_name(&self.lowerer.symbol_name(sym), variant);
        self.lowerer.hir.functions.iter().any(|f| {
            self.lowerer
                .get_symbol(f.symbol)
                .map(|s| self.lowerer.symbol_name(s) == ctor_name)
                .unwrap_or(false)
        })
    }

    /// 计算类型的大小（字节）
    fn compute_type_size(&mut self, ty: &CxxType) -> Result<u64, LowerIrError> {
        // 类类型的**对象大小**直取布局层 `ClassAbi::size` (单一真源)。
        // 不能走 `lower_type` 后的 `type_size(&ast::Type)`: `Type::Struct` 的字段
        // 列表只含数据成员 (`ClassAbi::flatten_fields`), **不含 vptr**, 于是
        // `struct B { virtual ~B(); };` 算成 size=1 —— `new B` 只分 1 字节,
        // 构造函数写 vptr 就是堆溢出 (实测 `delete (B*)d` 段错误)。
        if let CxxType::Class(id) = ty {
            if let Some(abi) = self.lowerer.abis.get(id) {
                if abi.size > 0 {
                    return Ok(abi.size);
                }
            }
        }
        let ir_ty = self.lowerer.lower_type(ty)?;
        Ok(type_size(&ir_ty))
    }

    /// 获取类型对应的 typeinfo 指针（用于异常类型匹配）
    fn get_typeinfo_for(&mut self, ty: &CxxType) -> Result<Operand, LowerIrError> {
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        match ty {
            CxxType::Class(id) => {
                let abi = self.lowerer.abis.get(id).ok_or_else(|| {
                    LowerIrError::Internal(format!(
                        "ABI for class {} not found in get_typeinfo_for",
                        id.0
                    ))
                })?;
                let sym_name = abi.rtti.typeinfo_symbol.clone();
                if !self.lowerer.output.rodata.contains(&sym_name) {
                    self.lowerer.output.rodata.push(sym_name.clone());
                }
                Ok(Operand::SymAddr(sym_name, void_ptr_ty))
            },
            _ => {
                // 非类类型：返回 null（简化处理，SJLJ 运行时按类型名匹配）
                Ok(Operand::Int(0))
            },
        }
    }

    // === statement lowering ===

    fn lower_stmt(&mut self, stmt_id: HirStmtId) -> Result<(), LowerIrError> {
        let stmt = self.get_stmt(stmt_id)?;

        match &stmt.kind {
            HirStmtKind::Expr(expr_id) => {
                let _ = self.lower_expr(*expr_id)?;
                Ok(())
            },

            HirStmtKind::Compound(stmts) => {
                for s in stmts {
                    self.lower_stmt(*s)?;
                    // 如果当前块已经有终结符（return/break/continue），
                    // 后续语句不再执行
                    let term = self.ssa.blocks[&self.current_bb].term.clone();
                    if !matches!(term, Terminator::Unreachable) {
                        break;
                    }
                }
                Ok(())
            },

            HirStmtKind::Return(None) => {
                self.set_term(Terminator::Jump(self.return_bb.clone()));
                Ok(())
            },

            HirStmtKind::Return(Some(expr_id)) => {
                // 函数返回**引用** (`T& f() { return obj; }`): 引用的 ABI 表示就
                // 是所指对象的地址, 所以必须按「左值地址」降级, 而不是取对象的值。
                // 漏掉这一步的现场: `<iostream>` 的 `std::cout()/cerr()/clog()/
                // cin()` 全部是 `inline ostream& cout(){ static ostream os; return
                // os; }` —— 返回类型是 `ostream&`, 而被返回的 `os` 表达式类型只是
                // `Class`, 于是走「聚合静态对象」读路径按全局符号取**第 0 个字**
                // (`mov __dcc_static_0(%rip),%rax`), 调用方拿到 0 当指针用。
                let val =
                    if matches!(self.func.ret_ty, CxxType::LValueRef(_) | CxxType::RValueRef(_)) {
                        let ret_ty = self.lowerer.lower_type(&self.func.ret_ty)?;
                        let addr = self.lower_object_lvalue_addr(*expr_id)?;
                        let dest = self.fresh_syn("ref_ret", ret_ty.clone());
                        self.push_inst(Inst {
                            dest: Some(dest.clone()),
                            kind: InstKind::Cast {
                                ty: ret_ty,
                                e: addr,
                            },
                            span: dummy_span(),
                        });
                        Operand::Var(dest)
                    } else {
                        self.lower_expr(*expr_id)?
                    };
                // ret_val 已移除: 直接在当前块返回, 不经过统一 return_bb.
                self.set_term(Terminator::Ret { val: Some(val) });
                Ok(())
            },

            HirStmtKind::Decl { symbol, init } => {
                let sym = self.lowerer.get_symbol(*symbol)?;
                let name = sym.name.clone();
                let sym_cxx_ty = sym.ty.clone();
                let ty = self.lowerer.lower_type(&sym.ty)?;
                let sym_is_volatile = sym.is_volatile;
                drop(sym);

                // 创建局部变量（version 0）
                let var = self.fresh_var(&name, ty.clone());
                self.ssa.local_types.insert(name.clone(), ty.clone());
                self.ssa.original_locals.insert(name.clone());
                let decl_quals = if sym_is_volatile {
                    QualifierFlags::VOLATILE
                } else {
                    QualifierFlags::NONE
                };
                self.ssa.local_quals.insert(name.clone(), decl_quals);

                if let Some(init_id) = init {
                    let init_val = self.lower_init_value(&sym_cxx_ty, *init_id)?;
                    // 赋值
                    let assign_inst = Inst {
                        dest: Some(var.clone()),
                        kind: InstKind::Cast {
                            ty: ty.clone(),
                            e: init_val,
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(assign_inst);
                }

                self.locals.insert(name, var);
                Ok(())
            },

            HirStmtKind::StaticDecl {
                symbol,
                guard_symbol,
                init,
            } => {
                use crate::cpp::runtime::CxxRuntimeSymbol;

                let var_sym = self.lowerer.get_symbol(*symbol)?;
                let var_name = self.lowerer.symbol_name(var_sym);
                let var_ty = self.lowerer.lower_type(&var_sym.ty)?;
                drop(var_sym);

                let guard_sym = self.lowerer.get_symbol(*guard_symbol)?;
                let guard_name = self.lowerer.symbol_name(guard_sym);
                drop(guard_sym);

                // guard 变量类型: i64* (指针)
                let guard_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Long)));
                // __cxa_guard_acquire 参数类型: i64*，返回类型: int
                let guard_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Int)));

                // 获取 guard 变量地址
                //
                // 必须是 `InstKind::AddrOf` 形式而不能用 `Operand::SymAddr`:
                // 前者重建为 `&__dcc_static_guard_<n>` => `lea sym(%rip)` (地址),
                // 后者重建为 `Ident{ty: Ptr}` => codegen 按标量全局取值, 于是
                // `__cxa_guard_acquire` 拿到的不是 guard 地址而是它的内容。
                let guard_addr = self.lower_sym_addr(guard_name, guard_ptr_ty.clone());

                // 调用 __cxa_guard_acquire(guard_ptr)
                let acquire_callee = Operand::FnAddr(
                    CxxRuntimeSymbol::GuardAcquire.name().to_string(),
                    guard_fn_ty.clone(),
                );
                let acquire_result = self.fresh_syn("guard_acquire", Type::Int);
                let acquire_call = Inst {
                    dest: Some(acquire_result.clone()),
                    kind: InstKind::Call {
                        callee: acquire_callee,
                        args: vec![guard_addr.clone()],
                        ret_ty: Type::Int,
                    },
                    span: dummy_span(),
                };
                self.push_inst(acquire_call);

                // 如果 acquire 返回非零，需要初始化
                // 创建 init_bb 和 end_bb
                let init_bb = self.unique_bb("static_init");
                let end_bb = self.unique_bb("static_init_end");

                // 条件分支: acquire_result != 0 → init_bb, 否则 → end_bb
                self.set_term(Terminator::Branch {
                    cond: Operand::Var(acquire_result),
                    t: init_bb.clone(),
                    f: end_bb.clone(),
                });

                // init 块: 执行初始化，然后 release
                self.current_bb = init_bb.clone();

                if let Some(init_id) = init {
                    // 计算初始化值
                    let init_val = self.lower_expr(*init_id)?;
                    // 获取静态变量地址 (同样走 AddrOf, 理由同上)
                    let var_ptr_ty = Type::Ptr(Box::new(QualType::new(var_ty.clone())));
                    let var_addr = self.lower_sym_addr(var_name.clone(), var_ptr_ty);
                    // 存储初始化值到静态变量
                    let store_inst = Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: var_addr,
                            src: init_val,
                            is_volatile: false,
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(store_inst);
                }

                // 调用 __cxa_guard_release(guard_ptr)
                let release_callee = Operand::FnAddr(
                    CxxRuntimeSymbol::GuardRelease.name().to_string(),
                    Type::Ptr(Box::new(QualType::new(Type::Void))),
                );
                let release_call = Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: release_callee,
                        args: vec![guard_addr.clone()],
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                };
                self.push_inst(release_call);

                // init 块跳转到 end_bb
                self.set_term(Terminator::Jump(end_bb.clone()));

                // end 块: 继续后续代码
                self.current_bb = end_bb.clone();

                Ok(())
            },

            HirStmtKind::If {
                cond,
                then_body,
                else_body,
            } => self.lower_if(*cond, *then_body, else_body.as_ref().copied()),

            HirStmtKind::While { cond, body } => self.lower_while(*cond, *body),

            HirStmtKind::DoWhile { body, cond } => self.lower_do_while(*body, *cond),

            HirStmtKind::For {
                init,
                cond,
                inc,
                body,
            } => self.lower_for(*init, *cond, *inc, *body),

            HirStmtKind::Switch { cond, body } => self.lower_switch(*cond, *body),

            HirStmtKind::Break => self.lower_break(),

            HirStmtKind::Continue => self.lower_continue(),

            HirStmtKind::Goto(label) => self.lower_goto(label.clone()),

            HirStmtKind::Label(name, body) => self.lower_label(name.clone(), *body),

            HirStmtKind::Null => Ok(()),

            HirStmtKind::Fallthrough => Ok(()),

            HirStmtKind::CtorInitBase { base, args } => self.lower_ctor_init_base(*base, args),

            HirStmtKind::CtorInitMember { member, args } => {
                self.lower_ctor_init_member(*member, args)
            },

            HirStmtKind::SetVTablePtr {
                class,
                place,
                complete,
            } => self.lower_set_vtable_ptr(*class, *place, *complete),

            HirStmtKind::Try { body, handlers } => self.lower_try(*body, handlers),

            _ => Err(LowerIrError::Unsupported(format!(
                "HIR statement kind: {:?}",
                stmt_kind_name(&stmt.kind)
            ))),
        }
    }

    fn lower_if(
        &mut self,
        cond_id: HirExprId,
        then_id: HirStmtId,
        else_id: Option<HirStmtId>,
    ) -> Result<(), LowerIrError> {
        // Snapshot unreachable blocks BEFORE creating structural BBs.
        // BBs whose terminators we set manually are added to the
        // "old" set so the fall-through logic skips them.
        let mut before_set: std::collections::HashSet<String> =
            self.unreachable_blocks().into_iter().collect();

        let then_bb = self.unique_bb("if_then");
        let else_bb = if else_id.is_some() {
            self.unique_bb("if_else")
        } else {
            self.unique_bb("if_end")
        };
        let end_bb = self.unique_bb("if_end");
        before_set.insert(end_bb.clone()); // left as fall-through

        let cond = self.lower_expr(cond_id)?;
        self.set_term(Terminator::Branch {
            cond,
            t: then_bb.clone(),
            f: else_bb.clone(),
        });

        // then 块
        self.current_bb = then_bb.clone();
        self.lower_stmt(then_id)?;

        // else 块（如果有）
        if let Some(else_stmt) = else_id {
            self.current_bb = else_bb.clone();
            self.lower_stmt(else_stmt)?;
        } else {
            // 无 else：else_bb 直接跳到 end
            self.ssa.blocks.get_mut(&else_bb).unwrap().term = Terminator::Jump(end_bb.clone());
        }

        // All new unreachable blocks added by then/else branches are
        // implicit fall-through exits — redirect them to end_bb.
        let after = self.unreachable_blocks();
        for name in after {
            if !before_set.contains(&name) {
                self.ssa.blocks.get_mut(&name).unwrap().term = Terminator::Jump(end_bb.clone());
            }
        }

        self.current_bb = end_bb;
        Ok(())
    }

    /// 扫描 switch body 收集 case/default 标签，为每个标签创建 BB
    fn collect_switch_cases(&mut self, stmt: &HirStmt) -> Result<SwitchTargets, LowerIrError> {
        let mut cases: Vec<(i128, String)> = Vec::new();
        let mut default: Option<String> = None;

        match &stmt.kind {
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    let s = self.get_stmt(*sid)?;
                    self.collect_cases_from_stmt(&s, &mut cases, &mut default)?;
                }
            },
            _ => {
                self.collect_cases_from_stmt(stmt, &mut cases, &mut default)?;
            },
        }

        Ok((cases, default))
    }

    fn collect_cases_from_stmt(
        &mut self,
        stmt: &HirStmt,
        cases: &mut Vec<(i128, String)>,
        default: &mut Option<String>,
    ) -> Result<(), LowerIrError> {
        match &stmt.kind {
            HirStmtKind::Case { value, body } => {
                let bb = self.unique_bb(&format!("case_{}", value));
                cases.push((*value, bb));
                let body_stmt = self.get_stmt(*body)?;
                self.collect_cases_from_stmt(&body_stmt, cases, default)?;
            },
            HirStmtKind::Default(body) => {
                let bb = self.unique_bb("case_default");
                *default = Some(bb);
                let body_stmt = self.get_stmt(*body)?;
                self.collect_cases_from_stmt(&body_stmt, cases, default)?;
            },
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    let s = self.get_stmt(*sid)?;
                    self.collect_cases_from_stmt(&s, cases, default)?;
                }
            },
            _ => {},
        }
        Ok(())
    }

    /// Lower switch body: 遇到 case/default 切换到对应 BB，其他语句正常 lower
    ///
    /// `labels` 是 `collect_switch_cases` 一次性建好的**精确**标签→BB 映射,
    /// 见 [`SwitchLabels`] 的说明 (不得再从块名反推 case 值)。
    fn lower_switch_body(
        &mut self,
        body_id: HirStmtId,
        labels: &SwitchLabels,
    ) -> Result<(), LowerIrError> {
        let body_stmt = self.get_stmt(body_id)?;
        match &body_stmt.kind {
            HirStmtKind::Compound(stmts) => {
                // 找到第一个 case/default 作为起始 BB
                // 先找到第一个标签的 BB 作为当前 BB
                let first_label_bb = self.find_first_case_bb(&body_stmt, labels)?;
                if let Some(bb) = first_label_bb {
                    self.current_bb = bb;
                }
                for sid in stmts {
                    self.lower_switch_stmt(*sid, labels)?;
                }
            },
            _ => {
                self.lower_switch_stmt(body_id, labels)?;
            },
        }
        Ok(())
    }

    fn find_first_case_bb(
        &self,
        stmt: &HirStmt,
        labels: &SwitchLabels,
    ) -> Result<Option<String>, LowerIrError> {
        match &stmt.kind {
            HirStmtKind::Case { value, .. } => Ok(labels.lookup(*value).cloned()),
            HirStmtKind::Default(_) => Ok(labels.default_bb.clone()),
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    let s = self.get_stmt(*sid)?;
                    if let Some(bb) = self.find_first_case_bb(&s, labels)? {
                        return Ok(Some(bb));
                    }
                }
                Ok(None)
            },
            _ => Ok(None),
        }
    }

    fn lower_switch_stmt(
        &mut self,
        stmt_id: HirStmtId,
        labels: &SwitchLabels,
    ) -> Result<(), LowerIrError> {
        let stmt = self.get_stmt(stmt_id)?;
        match &stmt.kind {
            HirStmtKind::Case { value, body } => {
                // 切换到 case 对应的 BB (直查 collect 阶段建好的映射;
                // 查不到只可能是 collect 未覆盖的新标签形态, 兜底新建)
                let bb = match labels.lookup(*value) {
                    Some(bb) => bb.clone(),
                    None => self.unique_bb(&format!("case_{}", value)),
                };
                // 如果当前块没有 terminator，且当前块不是目标块本身，
                // fallthrough 到这个 case（第一个 case 已经是 current_bb，
                // 不需要跳转）
                let cur = self.current_bb.clone();
                if cur != bb {
                    let cur_term = self.ssa.blocks[&cur].term.clone();
                    if matches!(cur_term, Terminator::Unreachable) {
                        self.set_term(Terminator::Jump(bb.clone()));
                    }
                }
                self.current_bb = bb;
                self.lower_switch_stmt(*body, labels)?;
            },
            HirStmtKind::Default(body) => {
                let bb = match labels.default_bb {
                    Some(ref bb) => bb.clone(),
                    None => self.unique_bb("case_default"),
                };
                let cur = self.current_bb.clone();
                if cur != bb {
                    let cur_term = self.ssa.blocks[&cur].term.clone();
                    if matches!(cur_term, Terminator::Unreachable) {
                        self.set_term(Terminator::Jump(bb.clone()));
                    }
                }
                self.current_bb = bb;
                self.lower_switch_stmt(*body, labels)?;
            },
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    self.lower_switch_stmt(*sid, labels)?;
                    let cur = self.current_bb.clone();
                    let cur_term = self.ssa.blocks[&cur].term.clone();
                    if !matches!(cur_term, Terminator::Unreachable) {
                        break;
                    }
                }
            },
            HirStmtKind::Break => {
                self.lower_break()?;
            },
            _ => {
                self.lower_stmt(stmt_id)?;
            },
        }
        Ok(())
    }

    fn lower_while(&mut self, cond_id: HirExprId, body_id: HirStmtId) -> Result<(), LowerIrError> {
        // Snapshot unreachable blocks BEFORE creating structural BBs.
        // BBs whose terminators we set manually are added to the
        // "old" set so the fall-through logic skips them.
        let mut before_set: std::collections::HashSet<String> =
            self.unreachable_blocks().into_iter().collect();

        let cond_bb = self.unique_bb("while_cond");
        let body_bb = self.unique_bb("while_body");
        let end_bb = self.unique_bb("while_end");
        before_set.insert(cond_bb.clone()); // we set its Branch
        before_set.insert(end_bb.clone()); // left as fall-through

        // 跳到条件块
        self.set_term(Terminator::Jump(cond_bb.clone()));

        // 条件块
        self.current_bb = cond_bb.clone();
        let cond = self.lower_expr(cond_id)?;
        self.set_term(Terminator::Branch {
            cond,
            t: body_bb.clone(),
            f: end_bb.clone(),
        });

        // 循环体
        self.break_stack.push(end_bb.clone());
        self.continue_stack.push(cond_bb.clone());
        self.current_bb = body_bb.clone();
        self.lower_stmt(body_id)?;
        // New unreachable blocks (including body_bb) fall through to cond
        let after_body = self.unreachable_blocks();
        for name in after_body {
            if !before_set.contains(&name) {
                self.ssa.blocks.get_mut(&name).unwrap().term = Terminator::Jump(cond_bb.clone());
            }
        }
        self.break_stack.pop();
        self.continue_stack.pop();

        self.current_bb = end_bb;
        Ok(())
    }

    fn lower_do_while(
        &mut self,
        body_id: HirStmtId,
        cond_id: HirExprId,
    ) -> Result<(), LowerIrError> {
        // Snapshot unreachable blocks BEFORE creating structural BBs.
        // BBs whose terminators we set manually are added to the
        // "old" set so the fall-through logic skips them.
        let mut before_set: std::collections::HashSet<String> =
            self.unreachable_blocks().into_iter().collect();

        let body_bb = self.unique_bb("do_body");
        let cond_bb = self.unique_bb("do_cond");
        let end_bb = self.unique_bb("do_end");
        before_set.insert(cond_bb.clone()); // we set its Branch
        before_set.insert(end_bb.clone()); // left as fall-through

        self.set_term(Terminator::Jump(body_bb.clone()));

        // body (continue 跳到 cond_bb)
        self.break_stack.push(end_bb.clone());
        self.continue_stack.push(cond_bb.clone());
        self.current_bb = body_bb.clone();
        self.lower_stmt(body_id)?;
        // New unreachable blocks (including body_bb) fall through to cond
        let after_body = self.unreachable_blocks();
        for name in after_body {
            if !before_set.contains(&name) {
                self.ssa.blocks.get_mut(&name).unwrap().term = Terminator::Jump(cond_bb.clone());
            }
        }
        self.break_stack.pop();
        self.continue_stack.pop();

        // cond: true -> body, false -> end
        self.current_bb = cond_bb.clone();
        let cond = self.lower_expr(cond_id)?;
        self.set_term(Terminator::Branch {
            cond,
            t: body_bb.clone(),
            f: end_bb.clone(),
        });

        self.current_bb = end_bb;
        Ok(())
    }

    fn lower_for(
        &mut self,
        init: Option<HirStmtId>,
        cond: Option<HirExprId>,
        inc: Option<HirExprId>,
        body: HirStmtId,
    ) -> Result<(), LowerIrError> {
        // Snapshot unreachable blocks BEFORE creating structural BBs.
        // BBs whose terminators we set manually are added to the
        // "old" set so the fall-through logic skips them.
        let mut before_set: std::collections::HashSet<String> =
            self.unreachable_blocks().into_iter().collect();

        let cond_bb = self.unique_bb("for_cond");
        let body_bb = self.unique_bb("for_body");
        let inc_bb = self.unique_bb("for_inc");
        let end_bb = self.unique_bb("for_end");
        before_set.insert(cond_bb.clone()); // we set its Branch/Jump
        before_set.insert(inc_bb.clone()); // we set its Jump to cond
        before_set.insert(end_bb.clone()); // left as fall-through

        // init
        if let Some(init_id) = init {
            self.lower_stmt(init_id)?;
        }
        self.set_term(Terminator::Jump(cond_bb.clone()));

        // cond
        self.current_bb = cond_bb.clone();
        if let Some(cond_id) = cond {
            let cond_val = self.lower_expr(cond_id)?;
            self.set_term(Terminator::Branch {
                cond: cond_val,
                t: body_bb.clone(),
                f: end_bb.clone(),
            });
        } else {
            // 无条件循环
            self.set_term(Terminator::Jump(body_bb.clone()));
        }

        // body (break -> end, continue -> inc)
        self.break_stack.push(end_bb.clone());
        self.continue_stack.push(inc_bb.clone());
        self.current_bb = body_bb.clone();
        self.lower_stmt(body)?;
        // New unreachable blocks (including body_bb) fall through to inc
        let after_body = self.unreachable_blocks();
        for name in after_body {
            if !before_set.contains(&name) {
                self.ssa.blocks.get_mut(&name).unwrap().term = Terminator::Jump(inc_bb.clone());
            }
        }
        self.break_stack.pop();
        self.continue_stack.pop();

        // inc
        self.current_bb = inc_bb.clone();
        if let Some(inc_id) = inc {
            let _ = self.lower_expr(inc_id)?;
        }
        self.set_term(Terminator::Jump(cond_bb.clone()));

        self.current_bb = end_bb;
        Ok(())
    }

    fn lower_break(&mut self) -> Result<(), LowerIrError> {
        let target =
            self.break_stack.last().cloned().ok_or_else(|| {
                LowerIrError::Internal("break without enclosing loop/switch".into())
            })?;
        self.set_term(Terminator::Jump(target));
        Ok(())
    }

    fn lower_continue(&mut self) -> Result<(), LowerIrError> {
        let target = self
            .continue_stack
            .last()
            .cloned()
            .ok_or_else(|| LowerIrError::Internal("continue without enclosing loop".into()))?;
        self.set_term(Terminator::Jump(target));
        Ok(())
    }

    fn lower_goto(&mut self, label: String) -> Result<(), LowerIrError> {
        let target = if let Some(bb) = self.label_map.get(&label) {
            bb.clone()
        } else {
            // 前向引用: 创建占位 BB
            let bb = self.unique_bb(&format!("label_{}", label));
            self.label_map.insert(label, bb.clone());
            bb
        };
        self.set_term(Terminator::Jump(target));
        Ok(())
    }

    fn lower_label(&mut self, name: String, body: HirStmtId) -> Result<(), LowerIrError> {
        let label_bb = if let Some(bb) = self.label_map.get(&name) {
            // 已有前向引用
            bb.clone()
        } else {
            let bb = self.unique_bb(&format!("label_{}", name));
            self.label_map.insert(name, bb.clone());
            bb
        };

        // 从当前块跳到 label 块（label 是一个入口点）
        self.set_term(Terminator::Jump(label_bb.clone()));
        self.current_bb = label_bb;
        self.lower_stmt(body)?;
        Ok(())
    }

    fn lower_switch(&mut self, cond_id: HirExprId, body_id: HirStmtId) -> Result<(), LowerIrError> {
        let end_bb = self.unique_bb("switch_end");
        let switch_bb = self.unique_bb("switch_dispatch");

        // 计算 switch 表达式
        let cond = self.lower_expr(cond_id)?;
        self.set_term(Terminator::Jump(switch_bb.clone()));

        // 扫描 body 收集 case/default 标签及其对应的 BB
        let body_stmt = self.get_stmt(body_id)?;
        let (cases, default_bb) = self.collect_switch_cases(&body_stmt)?;

        // case 值 → 入口块名的**精确**映射: body 下跳阶段只直查它,
        // 不再从块名反推 (见 [`SwitchLabels`] 的根因说明)。
        let labels = SwitchLabels::new(cases.clone(), default_bb.clone());

        // switch 分发块
        self.current_bb = switch_bb.clone();
        let values: Vec<i128> = cases.iter().map(|(v, _)| *v).collect();
        let targets: Vec<String> = cases.into_iter().map(|(_, bb)| bb).collect();
        let default = default_bb.unwrap_or_else(|| end_bb.clone());
        self.set_term(Terminator::SwitchTable {
            idx: cond,
            values,
            targets,
            default,
        });

        // 处理 body 语句
        // 注意: case/default 标签已经在 collect_switch_cases 中创建了 BB，
        // body 中的其他语句在这些 BB 中生成。
        // 为了正确处理 fallthrough，我们需要按顺序 lower body，
        // 遇到 case/default 标签时切换到对应 BB。
        self.break_stack.push(end_bb.clone());
        self.lower_switch_body(body_id, &labels)?;
        self.break_stack.pop();

        // switch 体末尾 (最后一个 case/default 之后没有别的标签) 若没有
        // terminator, 控制流按 C 语义**落出 switch** 到 switch_end。缺这条边
        // 时 switch_end 变成从 entry 不可达的孤块: 后端 SSA→AST 重建的前驱集
        // 初值是 ⊤, 不可达块的出边会被误判成回边, 于是凭空生成
        // `while(1) {}` —— 嵌套 switch 的运行期死循环即此根因.
        let cur = self.current_bb.clone();
        if cur != end_bb {
            let cur_term = self.ssa.blocks[&cur].term.clone();
            if matches!(cur_term, Terminator::Unreachable) {
                self.set_term(Terminator::Jump(end_bb.clone()));
            }
        }

        self.current_bb = end_bb;
        Ok(())
    }

    /// 降低 try/catch 语句（SJLJ 风格异常处理）
    ///
    /// 生成的伪代码:
    /// ```c
    /// __dcc_eh_context_t __eh_ctx;
    /// __dcc_eh_push_context(&__eh_ctx);
    /// if (__dcc_eh_setjmp(&__eh_ctx) == 0) {
    ///     // try body
    ///     __dcc_eh_pop_context();
    /// } else {
    ///     // exception thrown
    ///     void* exn = __dcc_eh_get_exception(&__eh_ctx);
    ///     // 逐个检查 catch handler
    ///     if (type matches handler 0) {
    ///         // catch body 0
    ///     } else if (type matches handler 1) {
    ///         // catch body 1
    ///     } else {
    ///         __dcc_eh_rethrow();
    ///     }
    ///     __dcc_eh_end_catch(&__eh_ctx);
    /// }
    /// ```
    fn lower_try(
        &mut self,
        body_id: HirStmtId,
        handlers: &[HirCatchHandler],
    ) -> Result<(), LowerIrError> {
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let int_ty = Type::Int;

        // 创建异常上下文变量（栈上分配）
        // 大小必须足以容纳运行时 __dcc_eh_context_t (含 jmp_buf)：
        // x86-64 SysV 上 jmp_buf ≈ 200B，整结构体约 256B + padding
        // 取 512 字节作为保守上限，16 字节对齐以满足 jmp_buf 要求
        let ctx_size: usize = 512;
        let ctx_ty = Type::Array(
            Box::new(QualType::new(Type::UChar)),
            crate::ast::ArraySize::Const(ctx_size),
        );
        let ctx_var = self.fresh_syn("__eh_ctx", ctx_ty.clone());
        self.ssa
            .local_types
            .insert("__eh_ctx".to_string(), ctx_ty.clone());
        self.ssa.original_locals.insert("__eh_ctx".to_string());
        self.ssa
            .local_quals
            .insert("__eh_ctx".to_string(), QualifierFlags::NONE);
        self.locals.insert("__eh_ctx".to_string(), ctx_var.clone());

        // 获取上下文指针（数组名退化为指针）
        let ctx_ptr_var =
            self.fresh_syn("__eh_ctx_ptr", Type::Ptr(Box::new(QualType::new(Type::UChar))));
        let ctx_addr_inst = Inst {
            dest: Some(ctx_ptr_var.clone()),
            kind: InstKind::Cast {
                ty: Type::Ptr(Box::new(QualType::new(Type::UChar))),
                e: Operand::Var(ctx_var.clone()),
            },
            span: dummy_span(),
        };
        self.push_inst(ctx_addr_inst);

        // 调用 __dcc_eh_push_context(ctx)
        let push_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let push_callee = Operand::FnAddr("__dcc_eh_push_context".to_string(), push_fn_ty.clone());
        let push_call = Inst {
            dest: None,
            kind: InstKind::Call {
                callee: push_callee,
                args: vec![Operand::Var(ctx_ptr_var.clone())],
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        };
        self.push_inst(push_call);

        // 调用 __dcc_eh_setjmp(ctx)，返回值决定是否异常发生
        let setjmp_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Int)));
        let setjmp_callee = Operand::FnAddr("__dcc_eh_setjmp".to_string(), setjmp_fn_ty.clone());
        let setjmp_result = self.fresh_syn("__eh_setjmp_res", int_ty.clone());
        let setjmp_call = Inst {
            dest: Some(setjmp_result.clone()),
            kind: InstKind::Call {
                callee: setjmp_callee,
                args: vec![Operand::Var(ctx_ptr_var.clone())],
                ret_ty: int_ty.clone(),
            },
            span: dummy_span(),
        };
        self.push_inst(setjmp_call);

        // 创建基本块
        let try_bb = self.unique_bb("try_body");
        let catch_bb = self.unique_bb("catch_dispatch");
        let end_bb = self.unique_bb("try_end");

        // 分支：setjmp 返回 0 进入 try body，非 0 进入 catch
        let cond = Operand::Var(setjmp_result.clone());
        // setjmp == 0 走 try，否则走 catch
        self.set_term(Terminator::Branch {
            cond: cond.clone(),
            t: catch_bb.clone(), // 非 0 (true) -> catch
            f: try_bb.clone(),   // 0 (false) -> try body
        });

        // === try body 块 ===
        self.current_bb = try_bb.clone();
        self.lower_stmt(body_id)?;
        // try body 正常结束：pop context，跳到 end
        let try_term = self.ssa.blocks[&try_bb].term.clone();
        if matches!(try_term, Terminator::Unreachable) {
            // 调用 __dcc_eh_pop_context
            let pop_callee =
                Operand::FnAddr("__dcc_eh_pop_context".to_string(), push_fn_ty.clone());
            let pop_call = Inst {
                dest: None,
                kind: InstKind::Call {
                    callee: pop_callee,
                    args: vec![],
                    ret_ty: Type::Void,
                },
                span: dummy_span(),
            };
            self.push_inst(pop_call);
            self.ssa.blocks.get_mut(&try_bb).unwrap().term = Terminator::Jump(end_bb.clone());
        }

        // === catch dispatch 块 ===
        self.current_bb = catch_bb.clone();

        // 获取异常对象指针
        let get_exn_fn_ty =
            Type::Ptr(Box::new(QualType::new(Type::Ptr(Box::new(QualType::new(Type::Void))))));
        let get_exn_callee =
            Operand::FnAddr("__dcc_eh_get_exception".to_string(), get_exn_fn_ty.clone());
        let exn_ptr_var = self.fresh_syn("__eh_exn_ptr", void_ptr_ty.clone());
        let get_exn_call = Inst {
            dest: Some(exn_ptr_var.clone()),
            kind: InstKind::Call {
                callee: get_exn_callee,
                args: vec![Operand::Var(ctx_ptr_var.clone())],
                ret_ty: void_ptr_ty.clone(),
            },
            span: dummy_span(),
        };
        self.push_inst(get_exn_call);

        // 逐个处理 catch handler
        // 对于每个 handler，生成: if (type_match) { handler body } else { next handler }
        let mut current_bb = catch_bb.clone();
        let mut next_handler_bbs: Vec<String> = Vec::new();

        for (i, handler) in handlers.iter().enumerate() {
            let handler_bb = self.unique_bb(&format!("catch_handler_{}", i));
            let next_bb = if i + 1 < handlers.len() {
                let bb = self.unique_bb(&format!("catch_next_{}", i));
                next_handler_bbs.push(bb.clone());
                bb
            } else {
                // 最后一个 handler 不匹配则 rethrow
                let bb = self.unique_bb("catch_rethrow");
                next_handler_bbs.push(bb.clone());
                bb
            };

            self.current_bb = current_bb.clone();

            // 检查类型是否匹配
            // 简化：对于基本类型，用 typeinfo 指针比较
            // catch (...) 总是匹配
            let is_catch_all = matches!(handler.param_ty, CxxType::Void) && handler.param.is_none();

            if is_catch_all {
                // catch (...)：直接进入 handler
                self.set_term(Terminator::Jump(handler_bb.clone()));
            } else {
                // 调用 __dcc_eh_type_match(exn, typeinfo)
                let typeinfo_op = self.get_typeinfo_for(&handler.param_ty)?;
                let type_match_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Int)));
                let type_match_callee =
                    Operand::FnAddr("__dcc_eh_type_match".to_string(), type_match_fn_ty);
                let match_result = self.fresh_syn("__eh_type_match", int_ty.clone());
                let match_call = Inst {
                    dest: Some(match_result.clone()),
                    kind: InstKind::Call {
                        callee: type_match_callee,
                        args: vec![Operand::Var(exn_ptr_var.clone()), typeinfo_op],
                        ret_ty: int_ty.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(match_call);

                // 匹配则进入 handler，否则进入下一个
                self.set_term(Terminator::Branch {
                    cond: Operand::Var(match_result),
                    t: handler_bb.clone(),
                    f: next_bb.clone(),
                });
            }

            // handler body
            self.current_bb = handler_bb.clone();

            // 如果有 catch 参数，把异常对象绑定到参数
            if let Some(param_sym_id) = handler.param {
                let param_sym = self.lowerer.get_symbol(param_sym_id)?;
                let param_name = param_sym.name.clone();
                let param_is_volatile = param_sym.is_volatile;
                drop(param_sym);
                let param_ir_ty = self.lowerer.lower_type(&handler.param_ty)?;

                // 创建参数变量
                let param_var = self.fresh_var(&param_name, param_ir_ty.clone());
                self.ssa
                    .local_types
                    .insert(param_name.clone(), param_ir_ty.clone());
                self.ssa.original_locals.insert(param_name.clone());
                let catch_quals = if param_is_volatile {
                    QualifierFlags::VOLATILE
                } else {
                    QualifierFlags::NONE
                };
                self.ssa.local_quals.insert(param_name.clone(), catch_quals);

                // 把 void* 异常指针转为参数类型并赋值
                // 对于引用类型：直接绑定指针（引用即指针）
                // 对于值类型：解引用拷贝
                let typed_ptr_ty = Type::Ptr(Box::new(QualType::new(param_ir_ty.clone())));
                let typed_ptr_var = self.fresh_syn("__catch_typed_ptr", typed_ptr_ty.clone());
                let cast_inst = Inst {
                    dest: Some(typed_ptr_var.clone()),
                    kind: InstKind::Cast {
                        ty: typed_ptr_ty,
                        e: Operand::Var(exn_ptr_var.clone()),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_inst);

                // 解引用得到值
                let load_inst = Inst {
                    dest: Some(param_var.clone()),
                    kind: InstKind::Load {
                        e: Operand::Var(typed_ptr_var),
                        ty: param_ir_ty.clone(),
                        is_volatile: false,
                    },
                    span: dummy_span(),
                };
                self.push_inst(load_inst);

                self.locals.insert(param_name, param_var);
            }

            // 执行 catch body
            self.lower_stmt(handler.body)?;

            // catch body 结束后，调用 __dcc_eh_end_catch 并跳到 end
            let handler_term = self.ssa.blocks[&handler_bb].term.clone();
            if matches!(handler_term, Terminator::Unreachable) {
                let end_catch_callee =
                    Operand::FnAddr("__dcc_eh_end_catch".to_string(), push_fn_ty.clone());
                let end_catch_call = Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: end_catch_callee,
                        args: vec![Operand::Var(ctx_ptr_var.clone())],
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                };
                self.push_inst(end_catch_call);
                self.ssa.blocks.get_mut(&handler_bb).unwrap().term =
                    Terminator::Jump(end_bb.clone());
            }

            current_bb = next_bb.clone();
        }

        // 最后一个 handler 不匹配：rethrow
        if !handlers.is_empty() {
            let rethrow_bb = next_handler_bbs.last().unwrap().clone();
            self.current_bb = rethrow_bb.clone();
            let rethrow_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
            let rethrow_callee = Operand::FnAddr("__dcc_eh_rethrow".to_string(), rethrow_fn_ty);
            let rethrow_call = Inst {
                dest: None,
                kind: InstKind::Call {
                    callee: rethrow_callee,
                    args: vec![],
                    ret_ty: Type::Void,
                },
                span: dummy_span(),
            };
            self.push_inst(rethrow_call);
            self.ssa.blocks.get_mut(&rethrow_bb).unwrap().term = Terminator::Unreachable;
        }

        self.current_bb = end_bb;
        Ok(())
    }

    // --- ctor initializer lowering ---

    /// 构造函数：基类初始化。
    fn lower_ctor_init_base(
        &mut self,
        base_class: ClassId,
        args: &[HirExprId],
    ) -> Result<(), LowerIrError> {
        // this 指针
        let this = Operand::Var(self.ssa.params[0].clone());

        // 获取 this 指向的类的 ClassId（从函数 owner 判断）
        let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
        let owner_class = match fn_sym.owner {
            Some(owner_id) => ClassId::from(owner_id),
            None => {
                return Err(LowerIrError::Internal("constructor without owning class".to_string()));
            },
        };

        // 计算基类子对象指针
        let base_ptr_ty = {
            let base_ty = CxxType::Pointer(Box::new(CxxType::Class(base_class)));
            self.lowerer.lower_type(&base_ty)?
        };

        // 计算基类子对象地址
        let offset = self
            .lowerer
            .base_subobject_offset(owner_class, base_class)?
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "base class {} not found in class {}",
                    base_class.0, owner_class.0
                ))
            })? as i64;
        let base_ptr = if offset == 0 {
            // primary base: 直接 cast
            if self.ssa.params[0].ty == base_ptr_ty {
                this.clone()
            } else {
                let dest = self.fresh_syn("base_this", base_ptr_ty.clone());
                let cast_inst = Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Cast {
                        ty: base_ptr_ty,
                        e: this.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_inst);
                Operand::Var(dest)
            }
        } else {
            // 非 primary base: 加偏移
            let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
            let byte_ptr = self.fresh_syn("ctor_byte_ptr", byte_ptr_ty.clone());
            let cast_to_byte = Inst {
                dest: Some(byte_ptr.clone()),
                kind: InstKind::Cast {
                    ty: byte_ptr_ty.clone(),
                    e: this.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(cast_to_byte);

            let adj_byte = self.fresh_syn("ctor_adj_byte", byte_ptr_ty.clone());
            let gep_inst = Inst {
                dest: Some(adj_byte.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(byte_ptr),
                    r: Operand::Int(offset as i128),
                },
                span: dummy_span(),
            };
            self.push_inst(gep_inst);

            let dest = self.fresh_syn("base_this", base_ptr_ty.clone());
            let cast_back = Inst {
                dest: Some(dest.clone()),
                kind: InstKind::Cast {
                    ty: base_ptr_ty,
                    e: Operand::Var(adj_byte),
                },
                span: dummy_span(),
            };
            self.push_inst(cast_back);
            Operand::Var(dest)
        };

        // 调用基类构造函数（base variant）
        let base_sym = self.lowerer.get_symbol(base_class.as_symbol())?;
        let ctor_name = ctor_variant_name(&self.lowerer.symbol_name(base_sym), CtorVariant::Base);
        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));

        let mut lowered_args = Vec::with_capacity(1 + args.len());
        lowered_args.push(base_ptr);
        for arg_id in args {
            lowered_args.push(self.lower_expr(*arg_id)?);
        }

        let callee_op = Operand::FnAddr(ctor_name, fn_ty);
        let inst = Inst {
            dest: None,
            kind: InstKind::Call {
                callee: callee_op,
                args: lowered_args,
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(())
    }

    /// 设置 vptr 指向指定类的 vtable。
    ///
    /// 用于构造/析构函数中更新对象的虚表指针。
    /// 多继承时每个基类子对象都有自己的 vptr，需要分别设置。
    ///
    /// `complete = false` (基子对象构造 C2 / 析构) 时跳过 `complete_only`
    /// 的 vptr: 那些是虚基子对象的 secondary vbptr, 只有在构造完整对象时
    /// 才能用最派生类的 secondary vbtable 写入。
    fn lower_set_vtable_ptr(
        &mut self,
        class: ClassId,
        place_id: HirExprId,
        complete: bool,
    ) -> Result<(), LowerIrError> {
        let place = self.lower_expr(place_id)?;
        // 未登记布局 → 该类没有对象模型 (例如仅前向声明就被用到的类),
        // 没有 vptr 槽, 写入自然为空。sema 侧现在**无条件**发射
        // SetVTablePtr (解析期无法知道类是否多态), 因此这里必须能跳过,
        // 不能当成内部错误。
        let Some(abi) = self.lowerer.abis.get(&class).cloned() else {
            return Ok(());
        };

        let vptr_ptr_ty = self.lowerer.vptr_ptr_type();

        for vptr in &abi.vptrs {
            if !complete && vptr.complete_only {
                continue;
            }
            let vtable = match abi.vtables.get(vptr.vtable_index) {
                Some(vt) => vt,
                None => continue,
            };

            // 计算 vptr 所在地址：place + vptr.offset
            let vptr_addr = if vptr.offset == 0 {
                // offset 为 0 时，直接 cast place 到 vptr_ptr_ty
                match &place {
                    Operand::Var(v) if v.ty == vptr_ptr_ty => place.clone(),
                    _ => {
                        let dest = self.fresh_syn("vptr_addr", vptr_ptr_ty.clone());
                        let cast = Inst {
                            dest: Some(dest.clone()),
                            kind: InstKind::Cast {
                                ty: vptr_ptr_ty.clone(),
                                e: place.clone(),
                            },
                            span: dummy_span(),
                        };
                        self.push_inst(cast);
                        Operand::Var(dest)
                    },
                }
            } else {
                let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
                let byte_ptr = self.fresh_syn("vptr_byte_ptr", byte_ptr_ty.clone());
                let cast_byte = Inst {
                    dest: Some(byte_ptr.clone()),
                    kind: InstKind::Cast {
                        ty: byte_ptr_ty.clone(),
                        e: place.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_byte);

                let adj_byte = self.fresh_syn("vptr_adj_byte", byte_ptr_ty.clone());
                let gep = Inst {
                    dest: Some(adj_byte.clone()),
                    kind: InstKind::Bin {
                        op: BinOp::Add,
                        l: Operand::Var(byte_ptr),
                        r: Operand::Int(vptr.offset as i128),
                    },
                    span: dummy_span(),
                };
                self.push_inst(gep);

                let dest = self.fresh_syn("vptr_addr", vptr_ptr_ty.clone());
                let cast_back = Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Cast {
                        ty: vptr_ptr_ty.clone(),
                        e: Operand::Var(adj_byte),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_back);
                Operand::Var(dest)
            };

            // vptr 指向 vtable 的 address point（第一个虚函数槽）
            // vtable 全局符号是 vtable 起点，vptr = &vtable + address_point
            //
            // 必须用 `AddrOf` 而不是 `SymAddr`: vtable 在 native.rs 里发射为
            // `void*[]` 数组全局, 而 `SymAddr(name, ty)` 在 SSA→AST 重建时生成
            // `Expr::Ident{ty}`, codegen 按标量全局**取值** (`mov sym(%rip),%rax`),
            // 拿到的是 vtable 第 0 个槽的内容 (VBaseOffset / offset-to-top),
            // 于是 vptr 变成一个几十字的小地址, 任何虚调用都段错误。
            // `AddrOf` 重建为 `&sym` → `lea sym(%rip)`, 才是真正的符号地址。
            let vtable_symbol = vtable.symbol.clone();
            let address_point = vtable.address_point;

            let vt_base_ty = self.lowerer.vtable_entry_type();
            let vt_base = self.fresh_syn("vt_base", vt_base_ty.clone());
            let base_addr = Inst {
                dest: Some(vt_base.clone()),
                kind: InstKind::AddrOf {
                    base: vtable_symbol.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(base_addr);

            let vptr_val = if address_point == 0 {
                Operand::Var(vt_base)
            } else {
                let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
                let byte_ptr = self.fresh_syn("vt_byte_ptr", byte_ptr_ty.clone());
                let cast_byte = Inst {
                    dest: Some(byte_ptr.clone()),
                    kind: InstKind::Cast {
                        ty: byte_ptr_ty.clone(),
                        e: Operand::Var(vt_base),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_byte);

                let adj_byte = self.fresh_syn("vt_adj_byte", byte_ptr_ty.clone());
                let gep = Inst {
                    dest: Some(adj_byte.clone()),
                    kind: InstKind::Bin {
                        op: BinOp::Add,
                        l: Operand::Var(byte_ptr),
                        r: Operand::Int(address_point as i128),
                    },
                    span: dummy_span(),
                };
                self.push_inst(gep);

                let dest = self.fresh_syn("vptr_val", vptr_ptr_ty.clone());
                let cast_back = Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Cast {
                        ty: vptr_ptr_ty.clone(),
                        e: Operand::Var(adj_byte),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_back);
                Operand::Var(dest)
            };

            // 存储 vptr。
            //
            // store 宽度真源 = **被写对象的类型**, 这里那是一个 vtable 指针槽
            // (`vtable_entry_type()`, 8 字节)。上面算出的 `vptr_addr` 类型是
            // `vptr_ptr_type()` —— 本后端当「不透明对象指针」用的 `*const u8`,
            // 直接拿它当 store 地址会被 SSA→AST 重建解成 1 字节 pointee,
            // vptr 只写最低一字节 (`mov %al,(%rcx)`), 对象 vptr 变垃圾指针,
            // 任何虚调用/`delete` 立刻段错误。故存前显式抬成「指向指针的指针」。
            let slot_addr_ty = Type::Ptr(Box::new(QualType::new(self.lowerer.vtable_entry_type())));
            let vptr_slot_addr = if slot_addr_ty == vptr_ptr_ty {
                vptr_addr
            } else {
                let slot_addr = self.fresh_syn("vptr_slot_addr", slot_addr_ty.clone());
                self.push_inst(Inst {
                    dest: Some(slot_addr.clone()),
                    kind: InstKind::Cast {
                        ty: slot_addr_ty,
                        e: vptr_addr,
                    },
                    span: dummy_span(),
                });
                Operand::Var(slot_addr)
            };
            let store = Inst {
                dest: None,
                kind: InstKind::Store {
                    e: vptr_slot_addr,
                    src: vptr_val,
                    is_volatile: false,
                },
                span: dummy_span(),
            };
            self.push_inst(store);
        }

        Ok(())
    }

    /// 构造函数：成员初始化。
    fn lower_ctor_init_member(
        &mut self,
        member_sym_id: SymbolId,
        args: &[HirExprId],
    ) -> Result<(), LowerIrError> {
        let member_sym = self.lowerer.get_symbol(member_sym_id)?;
        let member_name = member_sym.name.clone();
        let member_ty = member_sym.ty.clone();
        drop(member_sym); // 释放借用，避免与后续可变借用冲突

        // 简单情况：单参数标量成员 → 直接赋值到字段
        // 复杂情况：类类型成员 → 调用构造函数（placement new 风格）
        match &member_ty {
            CxxType::Class(member_class) => {
                // 类类型成员：调用构造函数
                // 计算成员地址：this + member_offset
                let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
                let owner_class = match fn_sym.owner {
                    Some(owner_id) => ClassId::from(owner_id),
                    None => {
                        return Err(LowerIrError::Internal(
                            "constructor without owning class".to_string(),
                        ));
                    },
                };

                let member_offset = self.find_member_offset(owner_class, &member_name)?;
                let this = Operand::Var(self.ssa.params[0].clone());

                let member_ptr_ty = self
                    .lowerer
                    .lower_type(&CxxType::Pointer(Box::new(member_ty.clone())))?;

                let member_ptr = self.compute_member_ptr(this, member_offset, &member_ptr_ty)?;

                // 调用成员构造函数
                let class_sym = self.lowerer.get_symbol(member_class.as_symbol())?;
                let ctor_name =
                    ctor_variant_name(&self.lowerer.symbol_name(class_sym), CtorVariant::Complete);
                let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));

                let mut lowered_args = Vec::with_capacity(1 + args.len());
                lowered_args.push(member_ptr);
                for arg_id in args {
                    lowered_args.push(self.lower_expr(*arg_id)?);
                }

                let callee_op = Operand::FnAddr(ctor_name, fn_ty);
                let inst = Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: callee_op,
                        args: lowered_args,
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
            },
            _ => {
                // 标量成员: 空参数 → 零值初始化; 单参数 → 直接 Store;
                // 多参数 → 仍报错 (构造函数参数包展开尚未支持)
                let init_val = if args.is_empty() {
                    match &member_ty {
                        CxxType::Integer { .. } => Operand::Int(0),
                        CxxType::Float { bits } => {
                            let c = match bits {
                                32 => FloatConst::F32(0.0),
                                _ => FloatConst::F64(0.0),
                            };
                            Operand::Float(c, Type::Double)
                        },
                        CxxType::Bool => Operand::Int(0),
                        CxxType::Pointer(_) => Operand::Int(0),
                        _ => {
                            return Err(LowerIrError::Unsupported(format!(
                                "empty-arg member init for type: {:?}",
                                member_ty
                            )))
                        },
                    }
                } else if args.len() == 1 {
                    self.lower_expr(args[0])?
                } else {
                    return Err(LowerIrError::Unsupported(
                        "multi-arg non-class member init".to_string(),
                    ));
                };
                let this = Operand::Var(self.ssa.params[0].clone());
                let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
                let owner_class = match fn_sym.owner {
                    Some(owner_id) => ClassId::from(owner_id),
                    None => {
                        return Err(LowerIrError::Internal(
                            "constructor without owning class".to_string(),
                        ))
                    },
                };
                let is_volatile = fn_sym.ty.is_volatile();
                drop(fn_sym);
                let obj_ty = self.lowerer.lower_type(&CxxType::Class(owner_class))?;
                let field_ty = self.lowerer.lower_type(&member_ty)?;
                let inst = Inst {
                    dest: None,
                    kind: InstKind::MemberStore {
                        obj: this,
                        obj_ty,
                        field: member_name,
                        src: init_val,
                        ty: field_ty,
                        is_volatile,
                        is_arrow: true,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
            },
        }

        Ok(())
    }

    /// 查找类中某个成员字段的偏移。
    fn find_member_offset(
        &self,
        class_id: ClassId,
        member_name: &str,
    ) -> Result<u64, LowerIrError> {
        let abi = self.lowerer.abis.get(&class_id).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", class_id.0))
        })?;

        // 先在自身字段中找
        for f in &abi.fields {
            if f.name == member_name {
                return Ok(f.offset);
            }
        }

        // 再在基类字段中找（递归）
        for base in &abi.bases {
            let base_abi = self.lowerer.abis.get(&base.base).ok_or_else(|| {
                LowerIrError::Internal(format!("ABI for base class {} not found", base.base.0))
            })?;
            for f in &base_abi.fields {
                if f.name == member_name {
                    return Ok(base.offset + f.offset);
                }
            }
        }

        Err(LowerIrError::Internal(format!(
            "member '{}' not found in class {}",
            member_name, class_id.0
        )))
    }

    /// 计算成员指针：base_ptr + offset → 转成目标类型指针。
    fn compute_member_ptr(
        &mut self,
        base_ptr: Operand,
        offset: u64,
        target_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        if offset == 0 {
            let base_ptr_ty = match &base_ptr {
                Operand::Var(v) => v.ty.clone(),
                _ => {
                    return Err(LowerIrError::Internal(
                        "compute_member_ptr: base_ptr is not a var".to_string(),
                    ));
                },
            };
            if base_ptr_ty == *target_ty {
                return Ok(base_ptr);
            }
            let dest = self.fresh_syn("member_ptr", target_ty.clone());
            let inst = Inst {
                dest: Some(dest.clone()),
                kind: InstKind::Cast {
                    ty: target_ty.clone(),
                    e: base_ptr,
                },
                span: dummy_span(),
            };
            self.push_inst(inst);
            return Ok(Operand::Var(dest));
        }

        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
        let byte_ptr = self.fresh_syn("member_byte_ptr", byte_ptr_ty.clone());
        let cast_to_byte = Inst {
            dest: Some(byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: base_ptr,
            },
            span: dummy_span(),
        };
        self.push_inst(cast_to_byte);

        let adj_byte = self.fresh_syn("member_adj_byte", byte_ptr_ty.clone());
        let gep_inst = Inst {
            dest: Some(adj_byte.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(byte_ptr),
                r: Operand::Int(offset as i128),
            },
            span: dummy_span(),
        };
        self.push_inst(gep_inst);

        let dest = self.fresh_syn("member_ptr", target_ty.clone());
        let cast_back = Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast {
                ty: target_ty.clone(),
                e: Operand::Var(adj_byte),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_back);
        Ok(Operand::Var(dest))
    }

    // === main function lowering ===

    fn lower_body(&mut self, body_id: HirBodyId) -> Result<(), LowerIrError> {
        let body = self.get_body(body_id)?.clone();
        let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
        let is_constructor = matches!(fn_sym.kind, CxxSymbolKind::Constructor);
        let is_destructor = matches!(fn_sym.kind, CxxSymbolKind::Destructor);
        drop(fn_sym); // 释放借用

        // 构造函数：先执行初始化列表（基类构造 + 成员初始化）
        if is_constructor {
            for init_id in &body.ctor_initializers {
                self.lower_stmt(*init_id)?;
                let term = self.ssa.blocks[&self.current_bb].term.clone();
                if !matches!(term, Terminator::Unreachable) {
                    break;
                }
            }
        }

        // 函数体语句
        for stmt_id in &body.stmts {
            self.lower_stmt(*stmt_id)?;
            let term = self.ssa.blocks[&self.current_bb].term.clone();
            if !matches!(term, Terminator::Unreachable) {
                break;
            }
        }

        // 析构函数：函数体之后，按逆序析构成员 → 逆序析构基类
        if is_destructor {
            self.lower_dtor_epilogue()?;
        }

        Ok(())
    }

    /// 析构函数收尾：按 ABI 的 dtor_order 逆序析构成员和基类。
    fn lower_dtor_epilogue(&mut self) -> Result<(), LowerIrError> {
        // 如果当前块已有终结符（如 return），不再追加
        let term = self.ssa.blocks[&self.current_bb].term.clone();
        if !matches!(term, Terminator::Unreachable) {
            return Ok(());
        }

        let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
        let owner_class = match fn_sym.owner {
            Some(owner_id) => ClassId::from(owner_id),
            None => {
                return Err(LowerIrError::Internal("destructor without owning class".to_string()));
            },
        };

        let abi = self.lowerer.abis.get(&owner_class).ok_or_else(|| {
            LowerIrError::Internal(format!("ABI for class {} not found", owner_class.0))
        })?;

        let this = Operand::Var(self.ssa.params[0].clone());

        // 克隆 dtor_order 以释放 abi 的借用
        let dtor_order = abi.dtor_order.clone();
        drop(abi);

        // 按 dtor_order 依次析构（成员逆序 → 基类逆序）
        for step in &dtor_order {
            match step {
                DtorStep::Member(name) => {
                    // 成员析构：只处理类类型成员，标量类型无需析构
                    let member_offset = match self.find_member_offset_safe(owner_class, name) {
                        Some(off) => off,
                        None => continue,
                    };
                    // 判断成员是否为类类型
                    let member_ty = self.member_type_by_name(owner_class, name)?;
                    if matches!(member_ty, CxxType::Class(_)) {
                        let member_class = if let CxxType::Class(cid) = member_ty {
                            cid
                        } else {
                            unreachable!()
                        };
                        let member_ptr_ty = self.lowerer.lower_type(&CxxType::Pointer(
                            Box::new(CxxType::Class(member_class)),
                        ))?;
                        let member_ptr =
                            self.compute_member_ptr(this.clone(), member_offset, &member_ptr_ty)?;

                        // 调用成员析构函数（base variant，因为成员是完整子对象）
                        let class_sym = self.lowerer.get_symbol(member_class.as_symbol())?;
                        let dtor_name = format!("{}_dtor_base", class_sym.name);
                        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                        let callee_op = Operand::FnAddr(dtor_name, fn_ty);

                        let inst = Inst {
                            dest: None,
                            kind: InstKind::Call {
                                callee: callee_op,
                                args: vec![member_ptr],
                                ret_ty: Type::Void,
                            },
                            span: dummy_span(),
                        };
                        self.push_inst(inst);
                    }
                },
                DtorStep::Base(base_class) => {
                    // 基类析构：调整 this 指针到基类子对象，调用基类 dtor_base
                    let base_layout = self
                        .lowerer
                        .find_base_layout(owner_class, *base_class)?
                        .ok_or_else(|| {
                            LowerIrError::Internal(format!(
                                "base class {} not found in class {}",
                                base_class.0, owner_class.0
                            ))
                        })?;

                    let base_ptr_ty = self
                        .lowerer
                        .lower_type(&CxxType::Pointer(Box::new(CxxType::Class(*base_class))))?;

                    let offset = base_layout.offset;
                    let base_ptr = self.compute_member_ptr(this.clone(), offset, &base_ptr_ty)?;

                    let base_sym = self.lowerer.get_symbol(base_class.as_symbol())?;
                    let dtor_name = format!("{}_dtor_base", base_sym.name);
                    let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                    let callee_op = Operand::FnAddr(dtor_name, fn_ty);

                    let inst = Inst {
                        dest: None,
                        kind: InstKind::Call {
                            callee: callee_op,
                            args: vec![base_ptr],
                            ret_ty: Type::Void,
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(inst);
                },
                DtorStep::VirtualBase(_) => {
                    return Err(LowerIrError::Unsupported("virtual base destructor".to_string()));
                },
            }
        }

        Ok(())
    }

    /// 安全地查找成员偏移（找不到返回 None，用于析构时跳过无法识别的成员）。
    fn find_member_offset_safe(&self, class_id: ClassId, member_name: &str) -> Option<u64> {
        self.find_member_offset(class_id, member_name).ok()
    }

    /// 查找成员的 CxxType（通过符号表查询）。
    fn member_type_by_name(
        &self,
        class_id: ClassId,
        member_name: &str,
    ) -> Result<CxxType, LowerIrError> {
        // 通过符号表查询成员类型（只查本类自身字段，基类成员由基类析构负责）
        let owner_sym = class_id.as_symbol();
        for sym in self.lowerer.symbols {
            if sym.owner == Some(owner_sym)
                && sym.name == member_name
                && matches!(sym.kind, CxxSymbolKind::Field | CxxSymbolKind::Variable)
            {
                return Ok(sym.ty.clone());
            }
        }
        // 兜底：找不到时默认 int（非类类型，跳过析构）
        Ok(CxxType::Integer {
            bits: 32,
            signed: true,
        })
    }
}

// === helper traits/functions ===

impl Operand {
    fn into_var(self) -> Option<SsaVar> {
        match self {
            Operand::Var(v) => Some(v),
            _ => None,
        }
    }
}

fn expr_kind_name(kind: &HirExprKind) -> &'static str {
    match kind {
        HirExprKind::Local(_) => "Local",
        HirExprKind::Global(_) => "Global",
        HirExprKind::FieldAccess { .. } => "FieldAccess",
        HirExprKind::BaseCast { .. } => "BaseCast",
        HirExprKind::VBaseAdjust { .. } => "VBaseAdjust",
        HirExprKind::Call { .. } => "Call",
        HirExprKind::Construct { .. } => "Construct",
        HirExprKind::Destroy { .. } => "Destroy",
        HirExprKind::IntLiteral(_) => "IntLiteral",
        HirExprKind::FloatLiteral(_) => "FloatLiteral",
        HirExprKind::BoolLiteral(_) => "BoolLiteral",
        HirExprKind::NullPtr => "NullPtr",
        HirExprKind::This => "This",
        HirExprKind::AddressOf(_) => "AddressOf",
        HirExprKind::Dereference(_) => "Dereference",
        HirExprKind::BinaryOp { .. } => "BinaryOp",
        HirExprKind::UnaryOp { .. } => "UnaryOp",
        HirExprKind::Index { .. } => "Index",
        HirExprKind::Assign { .. } => "Assign",
        HirExprKind::CompoundAssign { .. } => "CompoundAssign",
        HirExprKind::Conditional { .. } => "Conditional",
        HirExprKind::Cast { .. } => "Cast",
        HirExprKind::TypeInfoRef(_) => "TypeInfoRef",
        HirExprKind::DynamicCast { .. } => "DynamicCast",
        HirExprKind::Throw { .. } => "Throw",
        HirExprKind::New { .. } => "New",
        HirExprKind::Delete { .. } => "Delete",
        HirExprKind::SizeOf(_) => "SizeOf",
        HirExprKind::MethodRef { .. } => "MethodRef",
        HirExprKind::Comma { .. } => "Comma",
        HirExprKind::InitList { .. } => "InitList",
        _ => "Other",
    }
}

fn stmt_kind_name(kind: &HirStmtKind) -> &'static str {
    match kind {
        HirStmtKind::Null => "Null",
        HirStmtKind::Expr(_) => "Expr",
        HirStmtKind::Compound(_) => "Compound",
        HirStmtKind::If { .. } => "If",
        HirStmtKind::While { .. } => "While",
        HirStmtKind::DoWhile { .. } => "DoWhile",
        HirStmtKind::For { .. } => "For",
        HirStmtKind::Switch { .. } => "Switch",
        HirStmtKind::Case { .. } => "Case",
        HirStmtKind::Default(_) => "Default",
        HirStmtKind::Return(_) => "Return",
        HirStmtKind::Break => "Break",
        HirStmtKind::Continue => "Continue",
        HirStmtKind::Goto(_) => "Goto",
        HirStmtKind::Label(_, _) => "Label",
        HirStmtKind::Decl { .. } => "Decl",
        HirStmtKind::StaticDecl { .. } => "StaticDecl",
        HirStmtKind::Try { .. } => "Try",
        HirStmtKind::CtorInitMember { .. } => "CtorInitMember",
        HirStmtKind::CtorInitBase { .. } => "CtorInitBase",
        _ => "Other",
    }
}

fn dummy_span() -> Span {
    Span::point("<hir>", 1, 1)
}

// === IrLowerer: function lowering entry point ===

impl<'a> IrLowerer<'a> {
    fn lower_function(&mut self, func: &HirFunction) -> Result<Function, LowerIrError> {
        let mut fl = FuncLowerer::new(self, func)?;
        fl.lower_body(func.body)?;
        fl.finish()
    }
}

// === tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::abi_layout::AbiCalculator;
    use crate::cpp::sema::{Access, CxxSymbol, CxxSymbolKind, CxxType, Sema};

    fn make_simple_add_hir() -> (HirTranslationUnit, Vec<CxxSymbol>) {
        let mut sema = Sema::new();

        // 添加函数 add
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Integer {
                bits: 32,
                signed: true,
            }),
            params: vec![
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            ],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym =
            sema.add_symbol("add".to_string(), CxxSymbolKind::Function, fn_ty, Access::Public);

        // 添加参数 a, b
        let a_sym = sema.add_symbol(
            "a".to_string(),
            CxxSymbolKind::Variable,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        let b_sym = sema.add_symbol(
            "b".to_string(),
            CxxSymbolKind::Variable,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );

        // Save symbols before consuming sema into hir
        let symbols = sema.take_symbols();

        let mut hir = sema.into_hir();

        // 构建表达式: a + b
        // expr 0: Local(a)
        let expr_a = HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::Local(a_sym),
            ty: CxxType::Integer {
                bits: 32,
                signed: true,
            },
        };
        // expr 1: Local(b)
        let expr_b = HirExpr {
            id: HirExprId(1),
            kind: HirExprKind::Local(b_sym),
            ty: CxxType::Integer {
                bits: 32,
                signed: true,
            },
        };
        // expr 2: BinaryOp(Add, a, b)
        let expr_add = HirExpr {
            id: HirExprId(2),
            kind: HirExprKind::BinaryOp {
                op: BinaryOp::Add,
                lhs: HirExprId(0),
                rhs: HirExprId(1),
            },
            ty: CxxType::Integer {
                bits: 32,
                signed: true,
            },
        };
        hir.exprs.push(expr_a);
        hir.exprs.push(expr_b);
        hir.exprs.push(expr_add);

        // stmt 0: Return(a + b)
        let stmt_ret = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Return(Some(HirExprId(2))),
        };
        hir.stmts.push(stmt_ret);

        // body 0: [stmt 0]
        let body = HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        };
        hir.bodies.push(body);

        // function
        let func = HirFunction {
            symbol: fn_sym,
            body: HirBodyId(0),
            params: vec![a_sym, b_sym],
            ret_ty: CxxType::Integer {
                bits: 32,
                signed: true,
            },
        };
        hir.functions.push(func);

        (hir, symbols)
    }

    #[test]
    fn lower_empty_hir() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let mut lowerer = IrLowerer::new(&hir, &symbols);
        assert!(lowerer.lower().is_ok());
        let out = lowerer.finish();
        assert!(out.functions.is_empty());
    }

    #[test]
    fn lower_add_function() {
        let (hir, symbols) = make_simple_add_hir();
        let mut lowerer = IrLowerer::new(&hir, &symbols);
        assert!(lowerer.lower().is_ok());
        let out = lowerer.finish();

        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];

        // 验证函数名
        assert_eq!(func.name, "add");

        // 验证返回类型
        assert_eq!(func.ret_ty, Type::Int);

        // 验证参数
        assert_eq!(func.params.len(), 2);
        assert_eq!(func.params[0].base, "a");
        assert_eq!(func.params[0].ty, Type::Int);
        assert_eq!(func.params[1].base, "b");
        assert_eq!(func.params[1].ty, Type::Int);

        // 验证入口块存在
        assert!(func.blocks.contains_key("entry"));
        assert_eq!(func.entry, "entry");

        // 验证 return 块存在
        assert!(func
            .blocks
            .iter()
            .any(|(_, bb)| matches!(bb.term, Terminator::Ret { .. })));
    }

    #[test]
    fn lower_type_integer() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        assert_eq!(
            lowerer
                .lower_type(&CxxType::Integer {
                    bits: 32,
                    signed: true
                })
                .unwrap(),
            Type::Int
        );
        assert_eq!(
            lowerer
                .lower_type(&CxxType::Integer {
                    bits: 32,
                    signed: false
                })
                .unwrap(),
            Type::UInt
        );
        assert_eq!(
            lowerer
                .lower_type(&CxxType::Integer {
                    bits: 64,
                    signed: true
                })
                .unwrap(),
            Type::LongLong
        );
        assert_eq!(
            lowerer
                .lower_type(&CxxType::Integer {
                    bits: 8,
                    signed: true
                })
                .unwrap(),
            Type::SChar
        );
    }

    #[test]
    fn lower_type_bool_void() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        assert_eq!(lowerer.lower_type(&CxxType::Bool).unwrap(), Type::Bool);
        assert_eq!(lowerer.lower_type(&CxxType::Void).unwrap(), Type::Void);
    }

    #[test]
    fn lower_type_float() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        assert_eq!(lowerer.lower_type(&CxxType::Float { bits: 32 }).unwrap(), Type::Float);
        assert_eq!(lowerer.lower_type(&CxxType::Float { bits: 64 }).unwrap(), Type::Double);
    }

    #[test]
    fn lower_type_pointer() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        let ptr_ty = CxxType::Pointer(Box::new(CxxType::Integer {
            bits: 32,
            signed: true,
        }));
        let result = lowerer.lower_type(&ptr_ty).unwrap();
        match result {
            Type::Ptr(inner) => {
                assert_eq!(inner.base, Type::Int);
            },
            _ => panic!("expected Ptr"),
        }
    }

    #[test]
    fn lower_type_reference() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        // 左值引用 → 指针
        let lref_ty = CxxType::LValueRef(Box::new(CxxType::Integer {
            bits: 32,
            signed: true,
        }));
        let result = lowerer.lower_type(&lref_ty).unwrap();
        match result {
            Type::Ptr(inner) => {
                assert_eq!(inner.base, Type::Int);
            },
            _ => panic!("expected Ptr for LValueRef"),
        }

        // 右值引用 → 指针
        let rref_ty = CxxType::RValueRef(Box::new(CxxType::Integer {
            bits: 32,
            signed: true,
        }));
        let result = lowerer.lower_type(&rref_ty).unwrap();
        match result {
            Type::Ptr(inner) => {
                assert_eq!(inner.base, Type::Int);
            },
            _ => panic!("expected Ptr for RValueRef"),
        }
    }

    #[test]
    fn lower_type_array() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        let arr_ty = CxxType::Array(
            Box::new(CxxType::Integer {
                bits: 32,
                signed: true,
            }),
            Some(10),
        );
        let result = lowerer.lower_type(&arr_ty).unwrap();
        match result {
            Type::Array(_, crate::ast::ArraySize::Const(n)) => {
                assert_eq!(n, 10);
            },
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn lower_function_entry_and_return_blocks() {
        let (hir, symbols) = make_simple_add_hir();
        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.lower().unwrap();
        let func = &lowerer.output.functions[0];

        // ret_val 移除后, 非 void 函数的 return 语句直接在 entry 块生成 Ret,
        // 不再先 Jump 到统一 return 块.
        let entry = &func.blocks["entry"];
        match &entry.term {
            Terminator::Ret { val: Some(_) } => {},
            other => panic!("entry terminator should be Ret with value, got {:?}", other),
        }
    }

    #[test]
    fn lower_unsupported_returns_error() {
        let mut sema = Sema::new();
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym =
            sema.add_symbol("test".to_string(), CxxSymbolKind::Function, fn_ty, Access::Public);

        // Save symbols BEFORE consuming sema
        let symbols = sema.take_symbols();

        let mut hir = sema.into_hir();

        // 构造一个不支持的表达式 (MethodRef: 成员方法引用尚未降级)
        let expr = HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::MethodRef {
                object: HirExprId(0),
                method: fn_sym,
            },
            ty: CxxType::Integer {
                bits: 64,
                signed: false,
            },
        };
        hir.exprs.push(expr);

        // 语句: 求值该不支持表达式
        let stmt = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Expr(HirExprId(0)),
        };
        hir.stmts.push(stmt);

        let body = HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        };
        hir.bodies.push(body);

        let func = HirFunction {
            symbol: fn_sym,
            body: HirBodyId(0),
            params: vec![],
            ret_ty: CxxType::Void,
        };
        hir.functions.push(func);

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        let result = lowerer.lower();
        // MethodRef 目前不支持，应该返回 Unsupported
        assert!(result.is_err());
        match result.unwrap_err() {
            LowerIrError::Unsupported(msg) => {
                assert!(msg.contains("MethodRef"));
            },
            _ => panic!("expected Unsupported error"),
        }
    }

    // === Stage 4: 非虚继承 lowering 测试 ===

    /// 创建一个简单的继承层次：Base { x: int }, Derived : Base { y: int }
    /// 返回 (hir, symbols, base_class_id, derived_class_id)
    fn make_inheritance_hierarchy() -> (HirTranslationUnit, Vec<CxxSymbol>, ClassId, ClassId) {
        let mut sema = Sema::new();

        // Base 类
        let base_sym = sema.add_symbol(
            "Base".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(0)),
            Access::Public,
        );
        let base_id = ClassId::from(base_sym);

        // Base::x 字段
        let x_sym = sema.add_symbol(
            "x".to_string(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(x_sym, base_sym);

        // Derived 类
        let derived_sym = sema.add_symbol(
            "Derived".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(1)),
            Access::Public,
        );
        let derived_id = ClassId::from(derived_sym);

        // Derived::y 字段
        let y_sym = sema.add_symbol(
            "y".to_string(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(y_sym, derived_sym);

        // Save symbols before consuming sema into hir
        let symbols = sema.take_symbols();

        let hir = sema.into_hir();

        (hir, symbols, base_id, derived_id)
    }

    /// 注册 Base 和 Derived 的 ABI 描述符
    fn register_inheritance_abis(lowerer: &mut IrLowerer, base_id: ClassId, derived_id: ClassId) {
        use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, FieldLayout};

        // Base: { x: int @ 0 }, size=4, align=4
        let base_abi = ClassAbi {
            class_id: base_id,
            size: 4,
            align: 4,
            non_virtual_size: 4,
            fields: vec![FieldLayout {
                name: "x".to_string(),
                offset: 0,
                size: 4,
                align: 4,
            }],
            bases: vec![],
            virtual_bases: vec![],
            primary_base: None,
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![],
            field_init_order: vec!["x".to_string()],
            dtor_order: vec![DtorStep::Member("x".to_string())],
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(base_id, base_abi);

        // Derived : Base { y: int }
        // Base@0 (primary), y@4, size=8, align=4
        let derived_abi = ClassAbi {
            class_id: derived_id,
            size: 8,
            align: 4,
            non_virtual_size: 8,
            fields: vec![FieldLayout {
                name: "y".to_string(),
                offset: 4,
                size: 4,
                align: 4,
            }],
            bases: vec![BaseLayout {
                base: base_id,
                offset: 0,
                is_primary: true,
            }],
            virtual_bases: vec![],
            primary_base: Some(base_id),
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![base_id],
            field_init_order: vec!["y".to_string()],
            dtor_order: vec![DtorStep::Member("y".to_string()), DtorStep::Base(base_id)],
            this_adjust_map: {
                let mut m = std::collections::HashMap::new();
                m.insert(base_id, crate::cpp::abi_layout::ThisAdjust::offset(0));
                m
            },
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(derived_id, derived_abi);
    }

    #[test]
    fn lower_class_type_flattens_base_fields() {
        let (hir, symbols, base_id, derived_id) = make_inheritance_hierarchy();
        let mut lowerer = IrLowerer::new(&hir, &symbols);
        register_inheritance_abis(&mut lowerer, base_id, derived_id);

        // Derived 的类型应该展平基类字段
        let derived_ty = lowerer.lower_type(&CxxType::Class(derived_id)).unwrap();

        match derived_ty {
            Type::Struct(name, fields, align, _) => {
                assert_eq!(name, "Derived");
                assert_eq!(align, 4);
                // 展平后应该有 2 个字段：x (base) + y (derived)
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].name, "x");
                assert_eq!(fields[0].offset, 0); // base 字段 offset 0
                assert_eq!(fields[1].name, "y");
                assert_eq!(fields[1].offset, 4); // 派生字段 offset 4
            },
            _ => panic!("expected Struct type"),
        }
    }

    #[test]
    fn lower_base_cast_primary_base() {
        // BaseCast: Derived* -> Base* (primary base, offset 0)
        let (hir, symbols, base_id, derived_id) = make_inheritance_hierarchy();

        // 创建测试函数：接受 Derived*，返回 Base*
        let mut hir = hir;
        // 符号表已有 4 个符号（base_sym, x_sym, derived_sym, y_sym）
        // 新符号 ID 从 4 开始
        let param_sym = SymbolId(4);
        let fn_sym = SymbolId(5);

        // 表达式 0: Local(param)
        let expr_local = HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::Local(param_sym),
            ty: CxxType::Pointer(Box::new(CxxType::Class(derived_id))),
        };
        hir.exprs.push(expr_local);

        // 表达式 1: BaseCast derived -> base
        let expr_cast = HirExpr {
            id: HirExprId(1),
            kind: HirExprKind::BaseCast {
                object: HirExprId(0),
                from: derived_id,
                to: base_id,
            },
            ty: CxxType::Pointer(Box::new(CxxType::Class(base_id))),
        };
        hir.exprs.push(expr_cast);

        // stmt 0: return base_cast_result
        let stmt_ret = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Return(Some(HirExprId(1))),
        };
        hir.stmts.push(stmt_ret);

        // body
        let body = HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        };
        hir.bodies.push(body);

        // function
        let func = HirFunction {
            symbol: fn_sym,
            body: HirBodyId(0),
            params: vec![param_sym],
            ret_ty: CxxType::Pointer(Box::new(CxxType::Class(base_id))),
        };
        hir.functions.push(func);

        // 扩展符号表
        let mut symbols = symbols;
        symbols.push(CxxSymbol {
            id: param_sym,
            name: "ptr".to_string(),
            kind: CxxSymbolKind::Variable,
            ty: CxxType::Pointer(Box::new(CxxType::Class(derived_id))),
            owner: None,
            access: Access::Public,
            linkage_name: None,
            scope: crate::cpp::sema::ScopeId(0),
            is_volatile: false,
        });
        symbols.push(CxxSymbol {
            id: fn_sym,
            name: "to_base".to_string(),
            kind: CxxSymbolKind::Function,
            ty: CxxType::Function {
                ret: Box::new(CxxType::Pointer(Box::new(CxxType::Class(base_id)))),
                params: vec![CxxType::Pointer(Box::new(CxxType::Class(derived_id)))],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            },
            owner: None,
            access: Access::Public,
            linkage_name: None,
            scope: crate::cpp::sema::ScopeId(0),
            is_volatile: false,
        });

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        register_inheritance_abis(&mut lowerer, base_id, derived_id);
        assert!(lowerer.lower().is_ok());

        let out = lowerer.finish();
        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];

        // 验证入口块有指令（primary base offset=0，应该只有一个 cast 指令）
        let entry = &func.blocks[&func.entry];
        // 至少有 1 条指令（base_cast 或直接变量）
        // 对于 primary base，类型不同时会有 Cast 指令
        let has_cast_or_gep = entry.insts.iter().any(|inst| {
            matches!(inst.kind, InstKind::Cast { .. })
                || matches!(inst.kind, InstKind::Bin { op: BinOp::Add, .. })
        });
        assert!(has_cast_or_gep || entry.insts.is_empty());
    }

    #[test]
    fn lower_base_cast_non_primary_base() {
        // 多继承：第二个基类（非 primary）需要加偏移
        let mut sema = Sema::new();

        // A 类
        let a_sym = sema.add_symbol(
            "A".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(0)),
            Access::Public,
        );
        let a_id = ClassId::from(a_sym);

        // A::a 字段 (int)
        let a_field_sym = sema.add_symbol(
            "a".to_string(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(a_field_sym, a_sym);

        // B 类
        let b_sym = sema.add_symbol(
            "B".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(1)),
            Access::Public,
        );
        let b_id = ClassId::from(b_sym);

        // B::b 字段 (double, 8 bytes)
        let b_field_sym = sema.add_symbol(
            "b".to_string(),
            CxxSymbolKind::Field,
            CxxType::Float { bits: 64 },
            Access::Public,
        );
        sema.set_owner(b_field_sym, b_sym);

        // C 类 : A, B
        let c_sym = sema.add_symbol(
            "C".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(2)),
            Access::Public,
        );
        let c_id = ClassId::from(c_sym);

        // C::c 字段
        let c_field_sym = sema.add_symbol(
            "c".to_string(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(c_field_sym, c_sym);

        // Save symbols BEFORE consuming sema
        let symbols = sema.take_symbols();

        let hir = sema.into_hir();

        // 测试：lower C* -> B* (non-primary base, offset=8)

        let mut lowerer = IrLowerer::new(&hir, &symbols);

        use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, FieldLayout};

        // A: { a: int @ 0 }, size=4, align=4
        let a_abi = ClassAbi {
            class_id: a_id,
            size: 4,
            align: 4,
            non_virtual_size: 4,
            fields: vec![FieldLayout {
                name: "a".to_string(),
                offset: 0,
                size: 4,
                align: 4,
            }],
            bases: vec![],
            virtual_bases: vec![],
            primary_base: None,
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![],
            field_init_order: vec!["a".to_string()],
            dtor_order: vec![DtorStep::Member("a".to_string())],
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(a_id, a_abi);

        // B: { b: double @ 0 }, size=8, align=8
        let b_abi = ClassAbi {
            class_id: b_id,
            size: 8,
            align: 8,
            non_virtual_size: 8,
            fields: vec![FieldLayout {
                name: "b".to_string(),
                offset: 0,
                size: 8,
                align: 8,
            }],
            bases: vec![],
            virtual_bases: vec![],
            primary_base: None,
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![],
            field_init_order: vec!["b".to_string()],
            dtor_order: vec![DtorStep::Member("b".to_string())],
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(b_id, b_abi);

        // C : A, B { c: int }
        // A@0 (primary, 4 bytes, align 4)
        // B@8 (aligned to 8, 8 bytes)
        // c@16 (4 bytes)
        // total=24, align=8
        let c_abi = ClassAbi {
            class_id: c_id,
            size: 24,
            align: 8,
            non_virtual_size: 24,
            fields: vec![FieldLayout {
                name: "c".to_string(),
                offset: 16,
                size: 4,
                align: 4,
            }],
            bases: vec![
                BaseLayout {
                    base: a_id,
                    offset: 0,
                    is_primary: true,
                },
                BaseLayout {
                    base: b_id,
                    offset: 8,
                    is_primary: false,
                },
            ],
            virtual_bases: vec![],
            primary_base: Some(a_id),
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![a_id, b_id],
            field_init_order: vec!["c".to_string()],
            dtor_order: vec![
                DtorStep::Member("c".to_string()),
                DtorStep::Base(b_id),
                DtorStep::Base(a_id),
            ],
            this_adjust_map: {
                let mut m = std::collections::HashMap::new();
                m.insert(a_id, crate::cpp::abi_layout::ThisAdjust::offset(0));
                m.insert(b_id, crate::cpp::abi_layout::ThisAdjust::offset(8));
                m
            },
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(c_id, c_abi);

        // 测试：lower C* -> B* (non-primary base, offset=8)
        let mut hir = hir;
        // 已有 6 个符号（A, a, B, b, C, c），新符号从 6 开始
        let param_sym = SymbolId(6);
        let fn_sym = SymbolId(7);

        // expr 0: Local(param) - C*
        let expr_local = HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::Local(param_sym),
            ty: CxxType::Pointer(Box::new(CxxType::Class(c_id))),
        };
        hir.exprs.push(expr_local);

        // expr 1: BaseCast C -> B
        let expr_cast = HirExpr {
            id: HirExprId(1),
            kind: HirExprKind::BaseCast {
                object: HirExprId(0),
                from: c_id,
                to: b_id,
            },
            ty: CxxType::Pointer(Box::new(CxxType::Class(b_id))),
        };
        hir.exprs.push(expr_cast);

        // stmt 0: return
        let stmt_ret = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Return(Some(HirExprId(1))),
        };
        hir.stmts.push(stmt_ret);

        let body = HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        };
        hir.bodies.push(body);

        let func = HirFunction {
            symbol: fn_sym,
            body: HirBodyId(0),
            params: vec![param_sym],
            ret_ty: CxxType::Pointer(Box::new(CxxType::Class(b_id))),
        };
        hir.functions.push(func);

        // Add new symbols (param, function)
        let param_symbol = CxxSymbol {
            id: param_sym,
            name: "ptr".to_string(),
            kind: CxxSymbolKind::Variable,
            ty: CxxType::Pointer(Box::new(CxxType::Class(c_id))),
            owner: None,
            access: Access::Public,
            linkage_name: None,
            scope: crate::cpp::sema::ScopeId(0),
            is_volatile: false,
        };
        let fn_symbol = CxxSymbol {
            id: fn_sym,
            name: "to_b".to_string(),
            kind: CxxSymbolKind::Function,
            ty: CxxType::Function {
                ret: Box::new(CxxType::Pointer(Box::new(CxxType::Class(b_id)))),
                params: vec![CxxType::Pointer(Box::new(CxxType::Class(c_id)))],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            },
            owner: None,
            access: Access::Public,
            linkage_name: None,
            scope: crate::cpp::sema::ScopeId(0),
            is_volatile: false,
        };

        // Use take_symbols() from sema
        let mut symbols = symbols;
        symbols.push(param_symbol);
        symbols.push(fn_symbol);

        let mut lowerer2 = IrLowerer::new(&hir, &symbols);
        register_abc_abis(&mut lowerer2, a_id, b_id, c_id);
        assert!(lowerer2.lower().is_ok());

        let out = lowerer2.finish();
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // non-primary base (offset 8): 应该有 GEP 操作（cast to byte + add + cast back）
        // 至少 3 条指令
        assert!(entry.insts.len() >= 3);

        // 检查是否有 Add 指令（指针加偏移）
        let has_add = entry
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, InstKind::Bin { op: BinOp::Add, .. }));
        assert!(has_add, "non-primary base cast should have GEP add instruction");
    }

    /// 注册 A, B, C 三继承 ABI（辅助函数）
    fn register_abc_abis(lowerer: &mut IrLowerer, a_id: ClassId, b_id: ClassId, c_id: ClassId) {
        use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, FieldLayout};

        let a_abi = ClassAbi {
            class_id: a_id,
            size: 4,
            align: 4,
            non_virtual_size: 4,
            fields: vec![FieldLayout {
                name: "a".to_string(),
                offset: 0,
                size: 4,
                align: 4,
            }],
            bases: vec![],
            virtual_bases: vec![],
            primary_base: None,
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![],
            field_init_order: vec!["a".to_string()],
            dtor_order: vec![DtorStep::Member("a".to_string())],
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(a_id, a_abi);

        let b_abi = ClassAbi {
            class_id: b_id,
            size: 8,
            align: 8,
            non_virtual_size: 8,
            fields: vec![FieldLayout {
                name: "b".to_string(),
                offset: 0,
                size: 8,
                align: 8,
            }],
            bases: vec![],
            virtual_bases: vec![],
            primary_base: None,
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![],
            field_init_order: vec!["b".to_string()],
            dtor_order: vec![DtorStep::Member("b".to_string())],
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(b_id, b_abi);

        let c_abi = ClassAbi {
            class_id: c_id,
            size: 24,
            align: 8,
            non_virtual_size: 24,
            fields: vec![FieldLayout {
                name: "c".to_string(),
                offset: 16,
                size: 4,
                align: 4,
            }],
            bases: vec![
                BaseLayout {
                    base: a_id,
                    offset: 0,
                    is_primary: true,
                },
                BaseLayout {
                    base: b_id,
                    offset: 8,
                    is_primary: false,
                },
            ],
            virtual_bases: vec![],
            primary_base: Some(a_id),
            vptrs: vec![],
            vtables: vec![],
            rtti: Default::default(),
            base_ctor_order: vec![a_id, b_id],
            field_init_order: vec!["c".to_string()],
            dtor_order: vec![
                DtorStep::Member("c".to_string()),
                DtorStep::Base(b_id),
                DtorStep::Base(a_id),
            ],
            this_adjust_map: {
                let mut m = std::collections::HashMap::new();
                m.insert(a_id, crate::cpp::abi_layout::ThisAdjust::offset(0));
                m.insert(b_id, crate::cpp::abi_layout::ThisAdjust::offset(8));
                m
            },
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(c_id, c_abi);
    }

    #[test]
    fn lower_ctor_init_base_order() {
        // 测试构造函数中基类初始化顺序（按声明顺序）
        let mut sema = Sema::new();

        // A 类
        let a_sym = sema.add_symbol(
            "A".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(0)),
            Access::Public,
        );
        let a_id = ClassId::from(a_sym);

        // B 类
        let b_sym = sema.add_symbol(
            "B".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(1)),
            Access::Public,
        );
        let b_id = ClassId::from(b_sym);

        // C 类 : A, B
        let c_sym = sema.add_symbol(
            "C".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(2)),
            Access::Public,
        );
        let c_id = ClassId::from(c_sym);

        // C 的构造函数
        let ctor_sym = sema.add_symbol(
            "C".to_string(),
            CxxSymbolKind::Constructor,
            CxxType::Function {
                ret: Box::new(CxxType::Void),
                params: vec![CxxType::Pointer(Box::new(CxxType::Class(c_id)))],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            },
            Access::Public,
        );
        sema.set_owner(ctor_sym, c_sym);

        // 注意：this_sym = SymbolId(4) 必须按顺序添加符号，使索引对应
        // A(0), B(1), C(2), ctor(3), this(4)
        let this_sym = SymbolId(4);

        // Save symbols before consuming sema into hir
        let symbols = sema.take_symbols();

        let mut hir = sema.into_hir();

        // stmt 0: CtorInitBase(A)
        let stmt_init_a = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::CtorInitBase {
                base: a_id,
                args: vec![],
            },
        };
        hir.stmts.push(stmt_init_a);

        // stmt 1: CtorInitBase(B)
        let stmt_init_b = HirStmt {
            id: HirStmtId(1),
            kind: HirStmtKind::CtorInitBase {
                base: b_id,
                args: vec![],
            },
        };
        hir.stmts.push(stmt_init_b);

        // stmt 2: return (构造函数体空)
        let stmt_ret = HirStmt {
            id: HirStmtId(2),
            kind: HirStmtKind::Return(None),
        };
        hir.stmts.push(stmt_ret);

        // body: ctor_initializers = [A, B], body = [return]
        let body = HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(2)],
            ctor_initializers: vec![HirStmtId(0), HirStmtId(1)],
        };
        hir.bodies.push(body);

        // function
        let func = HirFunction {
            symbol: ctor_sym,
            body: HirBodyId(0),
            params: vec![this_sym],
            ret_ty: CxxType::Void,
        };
        hir.functions.push(func);

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        register_abc_abis(&mut lowerer, a_id, b_id, c_id);
        assert!(lowerer.lower().is_ok());

        let out = lowerer.finish();
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 入口块中应该有 2 个 Call 指令（A_ctor_base 和 B_ctor_base）
        // 在 return 跳转之前
        let call_count = entry
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call { .. }))
            .count();
        assert_eq!(call_count, 2, "ctor should call 2 base constructors");

        // 验证顺序：先 A 再 B
        let calls: Vec<&str> = entry
            .insts
            .iter()
            .filter_map(|inst| match &inst.kind {
                InstKind::Call {
                    callee: Operand::FnAddr(name, _),
                    ..
                } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("A"), "first call should be A_ctor_base");
        assert!(calls[1].contains("B"), "second call should be B_ctor_base");
    }

    #[test]
    fn lower_dtor_reverse_order() {
        // 测试析构函数逆序析构（成员逆序 -> 基类逆序）
        let mut sema = Sema::new();

        // A 类
        let a_sym = sema.add_symbol(
            "A".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(0)),
            Access::Public,
        );
        let a_id = ClassId::from(a_sym);

        // B 类
        let b_sym = sema.add_symbol(
            "B".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(1)),
            Access::Public,
        );
        let b_id = ClassId::from(b_sym);

        // C 类 : A, B { c: int }
        let c_sym = sema.add_symbol(
            "C".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(ClassId(2)),
            Access::Public,
        );
        let c_id = ClassId::from(c_sym);

        // C 的析构函数
        let dtor_sym = sema.add_symbol(
            "~C".to_string(),
            CxxSymbolKind::Destructor,
            CxxType::Function {
                ret: Box::new(CxxType::Void),
                params: vec![CxxType::Pointer(Box::new(CxxType::Class(c_id)))],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            },
            Access::Public,
        );
        sema.set_owner(dtor_sym, c_sym);

        // this 参数（索引 4，符号表第 5 个元素）
        let this_sym = SymbolId(4);

        // Save symbols before consuming sema into hir
        let symbols = sema.take_symbols();

        let mut hir = sema.into_hir();

        // stmt 0: return (析构函数体空，只有 epilogue)
        let stmt_ret = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Return(None),
        };
        hir.stmts.push(stmt_ret);

        // body: 空体 + 无 ctor_initializers
        let body = HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        };
        hir.bodies.push(body);

        // function
        let func = HirFunction {
            symbol: dtor_sym,
            body: HirBodyId(0),
            params: vec![this_sym],
            ret_ty: CxxType::Void,
        };
        hir.functions.push(func);

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        register_abc_abis(&mut lowerer, a_id, b_id, c_id);

        // 注意：析构函数体里直接 return，所以 epilogue 不会追加到 entry 块
        // 因为 return 语句已经设置了 terminator
        // 我们需要一个没有提前 return 的析构函数来测试

        // 重新创建一个无 return 的析构函数（空体，依赖隐式 return）
        let mut hir2 = hir.clone();
        // stmt 0: 空表达式语句
        let stmt_expr = HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Expr(HirExprId(0)),
        };
        // 先 push 一个空的 int literal 表达式
        let expr_zero = HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::IntLiteral(0),
            ty: CxxType::Integer {
                bits: 32,
                signed: true,
            },
        };
        hir2.exprs.push(expr_zero);
        hir2.stmts[0] = stmt_expr; // 替换 return 为表达式语句

        let mut lowerer2 = IrLowerer::new(&hir2, &symbols);
        register_abc_abis(&mut lowerer2, a_id, b_id, c_id);
        assert!(lowerer2.lower().is_ok());

        let out = lowerer2.finish();
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 析构 epilogue 应该生成：成员逆序析构 -> 基类逆序析构
        // dtor_order: [Member("c"), Base(b), Base(a)]
        // 标量成员 c 不调用析构（member_type_by_name 默认返回 int）
        // 所以应该有 2 个 Call：B_dtor_base, A_dtor_base
        let calls: Vec<&str> = entry
            .insts
            .iter()
            .filter_map(|inst| match &inst.kind {
                InstKind::Call {
                    callee: Operand::FnAddr(name, _),
                    ..
                } => Some(name.as_str()),
                _ => None,
            })
            .collect();

        // 基类逆序：B 先析构，然后 A
        assert!(calls.len() >= 2, "dtor should have at least 2 base dtor calls");
        assert!(
            calls[0].contains("B"),
            "first base dtor should be B (reverse order), got {:?}",
            calls
        );
        assert!(
            calls[1].contains("A"),
            "second base dtor should be A (reverse order), got {:?}",
            calls
        );
    }

    // === Stage 5: 虚函数调用 & vptr 测试 ===

    /// 构造一个有虚函数的简单类的 ABI（使用 ItaniumAbiCalculator）
    fn make_virtual_class_abi(
        calc: &mut crate::cpp::abi_layout::ItaniumAbiCalculator,
        class_id: ClassId,
        methods: &[(&str, bool)],
        fields: Vec<(String, CxxType)>,
        bases: Vec<ClassId>,
    ) {
        calc.register_class(class_id, bases, vec![], fields, true);
        for &(name, is_virtual) in methods {
            // owner = class_id（简化：都是自身声明的）
            calc.register_method(class_id, name.to_string(), is_virtual, class_id);
        }
    }

    #[test]
    fn vtable_generation_from_abi() {
        // 测试：注册 ABI 后，lower() 应该生成 vtable 数据
        let mut calc = crate::cpp::abi_layout::ItaniumAbiCalculator::new();
        let class_id = ClassId(0);
        make_virtual_class_abi(
            &mut calc,
            class_id,
            &[("foo", true), ("bar", true)],
            vec![(
                "x".into(),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )],
            vec![],
        );
        let abi = calc.compute_class(class_id);

        // 符号表
        let mut sema = Sema::new();
        let _class_sym = sema.add_symbol(
            "Foo".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(class_id),
            Access::Public,
        );

        let symbols = sema.take_symbols();
        let hir = HirTranslationUnit::new();

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(class_id, abi.clone());
        assert!(lowerer.lower().is_ok());
        let out = lowerer.finish();

        // vtable 应该被生成
        assert_eq!(out.vtables.len(), 1);
        let (vt_name, vt_entries) = &out.vtables[0];
        assert!(vt_name.starts_with("_ZTV"), "vtable symbol should start with _ZTV");

        // vtable 应该有 4 个条目: offset-to-top, typeinfo, foo, bar
        assert_eq!(vt_entries.len(), 4);
        match &vt_entries[0] {
            VTableEntry::Int(v) => assert_eq!(*v, 0),
            _ => panic!("first entry should be offset-to-top (int)"),
        }
        match &vt_entries[1] {
            VTableEntry::SymAddr(s) => assert!(s.contains("ZTI")),
            _ => panic!("second entry should be typeinfo (symaddr)"),
        }
        match &vt_entries[2] {
            VTableEntry::SymAddr(s) => assert!(s.ends_with("::foo")),
            _ => panic!("third entry should be foo (symaddr)"),
        }
        match &vt_entries[3] {
            VTableEntry::SymAddr(s) => assert!(s.ends_with("::bar")),
            _ => panic!("fourth entry should be bar (symaddr)"),
        }

        // rodata 中也应该包含 vtable 符号
        assert!(out.rodata.iter().any(|s| s == vt_name));

        // address point = 16 (offset-to-top + typeinfo = 2 * 8)
        assert_eq!(abi.vtables[0].address_point, 16);
    }

    #[test]
    fn virtual_call_generates_vtable_load_and_indirect_call() {
        // 测试：虚函数调用应该生成 vptr load + vtable slot load + indirect call
        let mut sema = Sema::new();

        // 类 Foo
        let class_id = ClassId(0);
        let class_sym = sema.add_symbol(
            "Foo".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(class_id),
            Access::Public,
        );

        // 虚方法 foo
        let method_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::LValueRef(Box::new(CxxType::Class(class_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let method_sym =
            sema.add_symbol("foo".to_string(), CxxSymbolKind::Method, method_ty, Access::Public);
        sema.set_symbol_owner(method_sym, class_sym);

        // 测试函数 void test(Foo* obj) { obj->foo(); }
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::Pointer(Box::new(CxxType::Class(class_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym = sema.add_symbol(
            "test_vcall".to_string(),
            CxxSymbolKind::Function,
            fn_ty,
            Access::Public,
        );

        let param_sym = sema.add_symbol(
            "obj".to_string(),
            CxxSymbolKind::Variable,
            CxxType::Pointer(Box::new(CxxType::Class(class_id))),
            Access::Public,
        );

        let symbols = sema.take_symbols();
        let mut hir = HirTranslationUnit::new();

        // 表达式 0: Local(obj)
        hir.exprs.push(HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::Local(param_sym),
            ty: CxxType::Pointer(Box::new(CxxType::Class(class_id))),
        });

        // 表达式 1: Call(Virtual(foo, slot=0, this_adjust=0), [obj])
        hir.exprs.push(HirExpr {
            id: HirExprId(1),
            kind: HirExprKind::Call {
                callee: HirCallee::Virtual {
                    method: method_sym,
                    slot: 0,
                    this_adjust: crate::cpp::hir::ThisAdjust {
                        non_virtual: 0,
                        vbase_index: None,
                    },
                },
                args: vec![HirExprId(0)],
            },
            ty: CxxType::Void,
        });

        // 语句 0: Expr(call)
        hir.stmts.push(HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Expr(HirExprId(1)),
        });

        // body
        hir.bodies.push(HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        });

        // 函数
        hir.functions.push(HirFunction {
            symbol: fn_sym,
            body: HirBodyId(0),
            params: vec![param_sym],
            ret_ty: CxxType::Void,
        });

        // 构造 ABI
        let mut calc = crate::cpp::abi_layout::ItaniumAbiCalculator::new();
        make_virtual_class_abi(&mut calc, class_id, &[("foo", true)], vec![], vec![]);
        let abi = calc.compute_class(class_id);

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(class_id, abi);
        assert!(lowerer.lower().is_ok(), "lowering should succeed");

        let out = lowerer.finish();
        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 检查是否有 Load (vptr 加载)
        let has_vptr_load = entry
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Load { .. }));
        assert!(has_vptr_load, "virtual call should have a vptr load instruction");

        // 检查是否有 Call 且 callee 是 Var（间接调用）
        let has_indirect_call = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Call { callee, .. } if matches!(callee, Operand::Var(_)))
        });
        assert!(has_indirect_call, "virtual call should use indirect call (callee is a var)");

        // vtable 数据应被生成
        assert_eq!(out.vtables.len(), 1);
    }

    #[test]
    fn vptr_initialization_in_constructor() {
        // 测试：SetVTablePtr 语句应该生成 vptr store 指令
        let mut sema = Sema::new();

        let class_id = ClassId(0);
        let class_sym = sema.add_symbol(
            "Foo".to_string(),
            CxxSymbolKind::Class,
            CxxType::Class(class_id),
            Access::Public,
        );

        // 构造函数
        let ctor_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::Pointer(Box::new(CxxType::Class(class_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let ctor_sym = sema.add_symbol(
            "Foo_ctor".to_string(),
            CxxSymbolKind::Constructor,
            ctor_ty,
            Access::Public,
        );
        sema.set_symbol_owner(ctor_sym, class_sym);

        let this_sym = sema.add_symbol(
            "this".to_string(),
            CxxSymbolKind::Variable,
            CxxType::Pointer(Box::new(CxxType::Class(class_id))),
            Access::Public,
        );

        let symbols = sema.take_symbols();
        let mut hir = HirTranslationUnit::new();

        // 表达式 0: Local(this)
        hir.exprs.push(HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::Local(this_sym),
            ty: CxxType::Pointer(Box::new(CxxType::Class(class_id))),
        });

        // 语句 0: SetVTablePtr(Foo, this)
        hir.stmts.push(HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::SetVTablePtr {
                class: class_id,
                place: HirExprId(0),
                complete: true,
            },
        });

        // body
        hir.bodies.push(HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        });

        // 函数
        hir.functions.push(HirFunction {
            symbol: ctor_sym,
            body: HirBodyId(0),
            params: vec![this_sym],
            ret_ty: CxxType::Void,
        });

        // 构造 ABI
        let mut calc = crate::cpp::abi_layout::ItaniumAbiCalculator::new();
        make_virtual_class_abi(&mut calc, class_id, &[("vfunc", true)], vec![], vec![]);
        let abi = calc.compute_class(class_id);

        for _v in abi.vptrs.iter() {}

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(class_id, abi);
        assert!(lowerer.lower().is_ok(), "lowering should succeed");

        let out = lowerer.finish();
        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 检查是否有 Store 指令（vptr 写入）
        let has_store = entry
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Store { .. }));
        assert!(has_store, "SetVTablePtr should generate a store instruction for vptr");

        // vptr 应该指向 vtable 的 address point（不是 vtable 起点）
        // 因此应该有 Add 指令（vtable_base + address_point）
        let has_addr_point_add = entry
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Bin { op: BinOp::Add, .. }));
        assert!(has_addr_point_add, "vptr should point to address point (vtable_base + offset)");
    }

    #[test]
    fn virtual_call_with_this_adjust_thunk() {
        // 测试：多继承场景下，虚函数调用带非零 this_adjust
        // class A { virtual void a_func(); int a; };
        // class B { virtual void b_func(); double b; };
        // class C : A, B { virtual void a_func(); virtual void b_func(); int c; };
        //
        // 通过 B* 调用 b_func()，this_adjust = 0（相对于 B subobject）
        // 通过 C* 调用 b_func()（secondary vtable），需要 this_adjust 找到 B 的 vptr
        let mut calc = crate::cpp::abi_layout::ItaniumAbiCalculator::new();

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
        calc.register_method(c_id, "a_func".into(), true, a_id); // override A
        calc.register_method(c_id, "b_func".into(), true, b_id); // override B

        let abi_c = calc.compute_class(c_id);
        // C 的布局: A@0 (vptr+a+pad=16), B@16 (vptr+b=16), c@32, total=40?
        // 验证 B subobject offset = 16
        let b_offset = abi_c.bases.iter().find(|b| b.base == b_id).unwrap().offset;
        assert_eq!(b_offset, 16, "B subobject should be at offset 16");

        let mut sema = Sema::new();
        let _a_sym =
            sema.add_symbol("A".into(), CxxSymbolKind::Class, CxxType::Class(a_id), Access::Public);
        let _b_sym =
            sema.add_symbol("B".into(), CxxSymbolKind::Class, CxxType::Class(b_id), Access::Public);
        let _c_sym =
            sema.add_symbol("C".into(), CxxSymbolKind::Class, CxxType::Class(c_id), Access::Public);

        // b_func 方法（属于 B）
        let b_func_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::LValueRef(Box::new(CxxType::Class(b_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let b_func_sym =
            sema.add_symbol("b_func".into(), CxxSymbolKind::Method, b_func_ty, Access::Public);
        sema.set_symbol_owner(b_func_sym, SymbolId(1)); // B

        // 测试函数: void test(C* obj) { obj->b_func(); }
        // 通过 C* 调用 B::b_func（虚函数，slot 0 相对于 B 的 secondary vtable）
        // this_adjust = 16 (从 C 到 B subobject)
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::Pointer(Box::new(CxxType::Class(c_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym =
            sema.add_symbol("test_adjust".into(), CxxSymbolKind::Function, fn_ty, Access::Public);

        let param_sym = sema.add_symbol(
            "obj".into(),
            CxxSymbolKind::Variable,
            CxxType::Pointer(Box::new(CxxType::Class(c_id))),
            Access::Public,
        );

        let symbols = sema.take_symbols();
        let mut hir = HirTranslationUnit::new();

        // 表达式 0: Local(obj)
        hir.exprs.push(HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::Local(param_sym),
            ty: CxxType::Pointer(Box::new(CxxType::Class(c_id))),
        });

        // 表达式 1: Call(Virtual(b_func, slot=0, adjust=+16), [obj])
        hir.exprs.push(HirExpr {
            id: HirExprId(1),
            kind: HirExprKind::Call {
                callee: HirCallee::Virtual {
                    method: b_func_sym,
                    slot: 0,
                    this_adjust: crate::cpp::hir::ThisAdjust {
                        non_virtual: 16, // from C to B subobject
                        vbase_index: None,
                    },
                },
                args: vec![HirExprId(0)],
            },
            ty: CxxType::Void,
        });

        // 语句 0: Expr(call)
        hir.stmts.push(HirStmt {
            id: HirStmtId(0),
            kind: HirStmtKind::Expr(HirExprId(1)),
        });

        // body
        hir.bodies.push(HirBody {
            id: HirBodyId(0),
            stmts: vec![HirStmtId(0)],
            ctor_initializers: vec![],
        });

        // 函数
        hir.functions.push(HirFunction {
            symbol: fn_sym,
            body: HirBodyId(0),
            params: vec![param_sym],
            ret_ty: CxxType::Void,
        });

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(a_id, calc.compute_class(a_id));
        lowerer.register_abi(b_id, calc.compute_class(b_id));
        lowerer.register_abi(c_id, abi_c);

        assert!(lowerer.lower().is_ok(), "lowering should succeed");

        let out = lowerer.finish();
        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 应该有 Add 指令（调整 this 指针到 B subobject）
        let has_add = entry
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Bin { op: BinOp::Add, .. }));
        assert!(has_add, "virtual call with non-zero this_adjust should have pointer add (GEP)");

        // 应该有 vptr load
        let has_load = entry
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Load { .. }));
        assert!(has_load, "should load vptr");

        // 应该有间接调用
        let has_indirect_call = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Call { callee, .. } if matches!(callee, Operand::Var(_)))
        });
        assert!(has_indirect_call, "should have indirect call");

        // vtables: 应该有 C 的 primary vtable + secondary vtable for B
        // 还有 A 和 B 自己的 vtable
        let c_vtables: Vec<_> = out
            .vtables
            .iter()
            .filter(|(name, _)| name.contains("Class2"))
            .collect();
        assert!(!c_vtables.is_empty(), "C should have at least primary vtable");
    }
}
