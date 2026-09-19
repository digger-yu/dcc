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

use crate::ast::{BinOp, QualifierFlags, QualType, Type, UnOp};
use crate::diagnostics::Span;
use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, ThisAdjust, VTableComponent};
use crate::cpp::hir::*;
use crate::cpp::sema::{Access, ClassId, CxxSymbol, CxxSymbolKind, CxxType, SymbolId};
use crate::ssa::{BasicBlock, Function, Inst, InstKind, Operand, Phi, SsaVar, Terminator};

// === LowerIrOutput ===

/// vtable 数据条目
#[derive(Debug, Clone)]
pub enum VTableEntry {
    /// i64 整数（offset-to-top 等）
    Int(i64),
    /// 全局符号地址（typeinfo、函数指针）
    SymAddr(String),
}

/// Lowering 结果
#[derive(Debug, Default)]
pub struct LowerIrOutput {
    /// 生成的 SSA 函数
    pub functions: Vec<Function>,
    /// 生成的全局变量
    pub globals: Vec<String>,
    /// 生成的 vtable / RTTI 等只读数据（符号名列表）
    pub rodata: Vec<String>,
    /// vtable 全局数据（符号名 + 条目列表）
    pub vtables: Vec<(String, Vec<VTableEntry>)>,
    /// 需要导出的外部 runtime symbol（如 dccpp_*）
    pub external_symbols: Vec<String>,
}

/// 指针大小（x86_64 / LP64）
const POINTER_SIZE: u64 = 8;

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
        for decl in &self.hir.top_level {
            if let HirTopLevelDecl::Variable(sym_id) = decl {
                let sym = self.get_symbol(*sym_id)?;
                let name = self.symbol_name(sym);
                if !self.output.globals.contains(&name) {
                    self.output.globals.push(name);
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
                        }
                        VTableComponent::Function { symbol, .. } => {
                            entries.push(VTableEntry::SymAddr(symbol.clone()));
                        }
                        VTableComponent::VCallOffset(v) => entries.push(VTableEntry::Int(*v)),
                        VTableComponent::VBaseOffset { offset, .. } => {
                            entries.push(VTableEntry::Int(*offset));
                        }
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
    fn symbol_name(&self, sym: &CxxSymbol) -> String {
        sym.linkage_name.clone().unwrap_or_else(|| sym.name.clone())
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
                _ => Err(LowerIrError::Unsupported(format!(
                    "float with {} bits", bits
                ))),
            },
            CxxType::Pointer(inner) => {
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Ptr(Box::new(QualType::new(inner_ty))))
            }
            CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                // ABI 降级：引用 → 指针
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Ptr(Box::new(QualType::new(inner_ty))))
            }
            CxxType::Array(inner, Some(n)) => {
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Array(
                    Box::new(QualType::new(inner_ty)),
                    crate::ast::ArraySize::Const(*n),
                ))
            }
            CxxType::Array(inner, None) => {
                // 未指定大小的数组 → 指针（C 语义退化）
                let inner_ty = self.lower_type(inner)?;
                Ok(Type::Ptr(Box::new(QualType::new(inner_ty))))
            }
            CxxType::Function { ret: _, params, .. } => {
                // 函数类型 → 函数指针
                for _p in params {
                    // 验证所有参数类型都可降级（确保类型有意义）
                }
                // dcc ast::Type 没有单独的函数类型，用指针表示
                // 这里降级为 void* 指针（函数指针的 ABI 表示）
                // TODO(Stage 4): 引入 Proper function type
                Ok(Type::Ptr(Box::new(QualType::new(Type::Void))))
            }
            CxxType::Class(id) => {
                let abi = self.abis.get(id).ok_or_else(|| {
                    eprintln!("ERROR: ABI not found for class {:?}. Available ABIs: {:?}", id, self.abis.keys());
                    LowerIrError::Internal(format!("ABI for class {} not found", id.0))
                })?;
                self.lower_class_type(id, abi)
            }
            CxxType::Enum(_id) => {
                // 枚举 → int
                // TODO(Stage 4): 根据枚举底层类型选择
                Ok(Type::Int)
            }
            CxxType::MemberPointer { .. } => Err(LowerIrError::Unsupported(
                "member pointer type".to_string(),
            )),
            CxxType::Dependent(s) => Err(LowerIrError::Unsupported(format!(
                "dependent type: {}", s
            ))),
            CxxType::Alias(id) => {
                let sym = self.get_symbol(*id)?;
                self.lower_type(&sym.ty)
            }
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
                "integer with {} bits (signed={})", bits, signed
            ))),
        }
    }

    fn lower_class_type(
        &self,
        id: &ClassId,
        abi: &ClassAbi,
    ) -> Result<Type, LowerIrError> {
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
    fn collect_flattened_fields(
        &self,
        class_id: &ClassId,
        abi: &ClassAbi,
        base_offset: u64,
        fields: &mut Vec<crate::ast::Field>,
    ) -> Result<(), LowerIrError> {
        // 先处理非虚基类（按声明顺序）
        for (i, base_layout) in abi.bases.iter().enumerate() {
            let base_abi = self.abis.get(&base_layout.base).ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "ABI for base class {} not found",
                    base_layout.base.0
                ))
            })?;
            self.collect_flattened_fields(
                &base_layout.base,
                base_abi,
                base_offset + base_layout.offset,
                fields,
            )?;
        }
        // 再处理自身字段
        for f in &abi.fields {
            let field_ty = self.field_type_by_name(*class_id, f.name.as_str())?;
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
        // 最后处理虚基类字段 (Stage 6)
        // 虚基类的偏移从 vtable 的 VBaseOffset 条目中获取
        for vb_layout in &abi.virtual_bases {
            let vb_abi = self.abis.get(&vb_layout.base).ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "ABI for virtual base class {} not found",
                    vb_layout.base.0
                ))
            })?;
            // 从 primary vtable 中查找 vbase offset 值
            let vbase_offset = abi
                .vtables
                .first()
                .and_then(|vt| {
                    vt.components.iter().find_map(|comp| {
                        if let VTableComponent::VBaseOffset { target, offset } = comp {
                            if *target == vb_layout.base {
                                Some(*offset as u64)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or(0);
            self.collect_flattened_fields(
                &vb_layout.base,
                vb_abi,
                base_offset + vbase_offset,
                fields,
            )?;
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

    /// 检查字段是否属于某个基类（非当前类自身字段）。
    fn field_belongs_to_base(
        &self,
        _class_id: ClassId,
        _field_name: &str,
    ) -> Option<ClassId> {
        // Stage 4 简化：字段展平后统一通过 MemberLoad 访问，
        // 不需要区分是否属于基类。
        None
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
        Type::Array(inner, crate::ast::ArraySize::Const(n)) => {
            type_size(&inner.base) * (*n as u64)
        }
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
        }
        Type::Union(_name, fields, _align, _) => {
            let mut max_size: u64 = 0;
            for f in fields.iter() {
                let fsize = type_size(&f.ty);
                if fsize > max_size {
                    max_size = fsize;
                }
            }
            max_size.max(1)
        }
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
}

impl<'a, 'b> FuncLowerer<'a, 'b> {
    fn new(
        lowerer: &'a mut IrLowerer<'b>,
        func: &'a HirFunction,
    ) -> Result<Self, LowerIrError> {
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
            CxxSymbolKind::Method
                | CxxSymbolKind::Constructor
                | CxxSymbolKind::Destructor
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
                    }
                }
            } else {
                this_class
            };
            
            let this_ty = lowerer.lower_type(&CxxType::Pointer(Box::new(
                CxxType::Class(actual_this_class),
            )))?;
            let this_var = SsaVar {
                base: "this".to_string(),
                version: 0,
                ty: this_ty.clone(),
                span: dummy_span(),
            };
            ssa.params.push(this_var.clone());
            ssa.local_types.insert("this".to_string(), this_ty);
            ssa.original_locals.insert("this".to_string());
            ssa.local_quals.insert("this".to_string(), QualifierFlags::NONE);
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

        Ok(Self {
            lowerer,
            func,
            ssa,
            current_bb,
            locals,
            return_bb,
            versions,
            bb_counter: 0,
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            label_map: HashMap::new(),
            init_list_counter: 0,
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
                Terminator::SwitchTable { idx: _, values: _, targets, default } => {
                    let mut v = targets.clone();
                    v.push(default.clone());
                    v
                }
            };

            let bb_clone = bb.clone();
            self.ssa.blocks.get_mut(&bb).unwrap().succs = succs.clone();

            for succ in succs {
                if self.ssa.blocks.contains_key(&succ) {
                    self.ssa.blocks.get_mut(&succ).unwrap().preds.push(bb_clone.clone());
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
        let version = self.versions.get(base).copied().unwrap_or(0);
        self.versions.insert(base.to_string(), version + 1);
        SsaVar {
            base: base.to_string(),
            version,
            ty,
            span: dummy_span(),
        }
    }

    // === expression lowering ===

    /// 降低表达式，返回一个 Operand（如果表达式有值）。
    fn lower_expr(&mut self, expr_id: HirExprId) -> Result<Operand, LowerIrError> {
        let expr = self.get_expr(expr_id)?;
        let expr_ty = self.lowerer.lower_type(&expr.ty)?;

        match &expr.kind {
            HirExprKind::IntLiteral(v) => Ok(Operand::Int(*v)),

            HirExprKind::FloatLiteral(s) => {
                let val: f64 = s.parse().unwrap_or(0.0);
                Ok(Operand::Float(val, Type::Double))
            }

            HirExprKind::BoolLiteral(b) => Ok(Operand::Int(if *b { 1 } else { 0 })),

            HirExprKind::NullPtr => Ok(Operand::Int(0)),

            HirExprKind::Local(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = sym.name.clone();
                let sym_is_volatile = sym.is_volatile;
                let sym_ty = sym.ty.clone();
                let linkage_name = sym.linkage_name.clone();
                drop(sym);

                // 如果有 linkage_name，说明是静态局部变量的引用，走全局变量路径
                if let Some(lname) = linkage_name {
                    let lowered_ty = self.lowerer.lower_type(&sym_ty)?;
                    let ptr_ty = Type::Ptr(Box::new(QualType::new(lowered_ty.clone())));
                    let addr = Operand::SymAddr(lname, ptr_ty);
                    // 加载值
                    let dest = self.fresh_var("static_load", lowered_ty.clone());
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
                }

                if let Some(var) = self.locals.get(&name) {
                    // volatile 标量局部变量: 从栈内存加载, 不走 SSA 版本
                    // 复合类型(数组/结构体/联合体)直接返回 SSA 变量:
                    // 它们的子访问 (MemberLoad/IndexLoad) 已带 is_volatile 标志
                    if sym_is_volatile {
                        let ty = self.lowerer.lower_type(&sym_ty)?;
                        match ty {
                            Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {
                                return Ok(Operand::Var(var.clone()));
                            }
                            _ => {}
                        }
                        let addr = self.fresh_var("vaddr", Type::Ptr(Box::new(QualType::new(ty.clone()))));
                        let addr_inst = Inst {
                            dest: Some(addr.clone()),
                            kind: InstKind::AddrOf { base: name.clone() },
                            span: dummy_span(),
                        };
                        self.push_inst(addr_inst);
                        let dest = self.fresh_var("vload", ty);
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
                    Ok(Operand::Var(var.clone()))
                } else {
                    Err(LowerIrError::Internal(format!(
                        "local variable '{}' not found", name
                    )))
                }
            }

            HirExprKind::Global(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = self.lowerer.symbol_name(sym);
                let ty = self.lowerer.lower_type(&sym.ty)?;
                // 全局变量地址
                Ok(Operand::SymAddr(name, ty))
            }

            HirExprKind::This => {
                // this 参数是函数的第一个隐式参数
                if self.ssa.params.is_empty() {
                    return Err(LowerIrError::Internal(
                        "this used in non-member function".to_string(),
                    ));
                }
                Ok(Operand::Var(self.ssa.params[0].clone()))
            }

            HirExprKind::BinaryOp { op, lhs, rhs } => {
                self.lower_binary_op(*op, *lhs, *rhs, &expr_ty)
            }

            HirExprKind::UnaryOp { op, operand } => {
                self.lower_unary_op(*op, *operand, &expr_ty)
            }

            HirExprKind::Index { array, index } => {
                self.lower_index(*array, *index, &expr_ty)
            }

            HirExprKind::Assign { lhs, rhs } => {
                let rhs_val = self.lower_expr(*rhs)?;
                self.lower_assign(*lhs, rhs_val)
            }

            HirExprKind::CompoundAssign { op, lhs, rhs } => {
                self.lower_compound_assign(*op, *lhs, *rhs)
            }

            HirExprKind::Call { callee, args } => {
                self.lower_call(callee, args, &expr_ty)
            }

            HirExprKind::FieldAccess { object, field } => {
                self.lower_field_access(*object, *field, &expr_ty)
            }

            HirExprKind::AddressOf(inner) => {
                self.lower_address_of(*inner, &expr_ty)
            }

            HirExprKind::Dereference(inner) => {
                self.lower_dereference(*inner, &expr_ty)
            }

            HirExprKind::Cast { expr, target, kind: _ } => {
                self.lower_cast(*expr, target)
            }

            HirExprKind::BaseCast { object, from, to } => {
                self.lower_base_cast(*object, *from, *to, &expr_ty)
            }

            HirExprKind::VBaseAdjust { object, from, to } => {
                self.lower_vbase_adjust(*object, *from, *to, &expr_ty)
            }

            HirExprKind::TypeInfoRef(class_id) => {
                // typeid 表达式：返回 typeinfo 符号的地址
                // type_info* 类型简化为 *const u8（不透明指针）
                let abi = self
                    .lowerer
                    .abis
                    .get(class_id)
                    .ok_or_else(|| {
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
                Ok(Operand::SymAddr(sym_name, ty))
            }

            HirExprKind::DynamicCast { object, src, dst } => {
                self.lower_dynamic_cast(*object, src, dst, &expr_ty)
            }

            HirExprKind::Construct { class, place, args, variant } => {
                self.lower_construct(*class, *place, args, *variant)
            }

            HirExprKind::Destroy { class, place, variant } => {
                self.lower_destroy(*class, *place, *variant)
            }

            HirExprKind::Throw { object, ty } => self.lower_throw(*object, ty),

            HirExprKind::New {
                ty,
                args,
                placement,
                is_array,
            } => self.lower_new(ty, args, placement.as_deref(), *is_array),

            HirExprKind::Delete { ptr, is_array } => {
                self.lower_delete(*ptr, *is_array)
            }

            HirExprKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                // 条件表达式: dest = cond ? then : else
                let cond = self.lower_expr(*cond)?;
                let then_val = self.lower_expr(*then_expr)?;
                let else_val = self.lower_expr(*else_expr)?;
                let result = self.fresh_var("__cond", expr_ty.clone());
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
            }

            HirExprKind::Comma { lhs, rhs } => {
                // 逗号表达式: 先求 lhs (丢弃结果), 再求 rhs (作为结果)
                let _ = self.lower_expr(*lhs)?;
                self.lower_expr(*rhs)
            }

            HirExprKind::InitList { elems, target_ty: _ } => {
                self.lower_init_list(elems, &expr_ty)
            }

            HirExprKind::SizeOf(cxx_ty) => {
                // TODO(Stage 4): Add proper support
                Err(LowerIrError::Unsupported(format!(
                    "SizeOf({:?}) not implemented", cxx_ty
                )))
            }

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
            _ => {}
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

        let dest = self.fresh_var("_tmp", result_ty.clone());
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
                let dest = self.fresh_var("unary", result_ty.clone());
                self.push_inst(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Un {
                        op: ast_op,
                        e: operand,
                    },
                    span: dummy_span(),
                });
                Ok(Operand::Var(dest))
            }
            PreInc | PreDec | PostInc | PostDec => {
                let operand_expr = self.get_expr(operand_id)?;
                match &operand_expr.kind {
                    HirExprKind::Local(sid) => {
                        // 局部变量: 走完整的 SSA 重绑定或 volatile 路径
                        return self.lower_incdec_local(*sid, op, result_ty);
                    }
                    HirExprKind::FieldAccess { object, field } => {
                        // 成员字段: 走 load-op-store 路径 (总是内存路径,
                        // 因为对象可能是 lvalue/const/pointer 等任意形态)
                        return self.lower_incdec_field(*object, *field, op, result_ty);
                    }
                    _ => {
                        return Err(LowerIrError::Unsupported(
                            "increment/decrement of non-local object".to_string(),
                        ))
                    }
                };
            }
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
        let ty = self.lowerer.lower_type(&sym_ty)?;

        // volatile 标量局部变量: 走内存路径 (load -> op -> store)
        if sym_is_volatile {
            match &ty {
                Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {}
                _ => {
                    let addr = self.fresh_var(
                        "vaddr",
                        Type::Ptr(Box::new(QualType::new(ty.clone()))),
                    );
                    let addr_inst = Inst {
                        dest: Some(addr.clone()),
                        kind: InstKind::AddrOf { base: name.clone() },
                        span: dummy_span(),
                    };
                    self.push_inst(addr_inst);
                    let load_dest = self.fresh_var("vload", ty.clone());
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
                }
            }
        }

        // 非 volatile: 纯 SSA 版本重绑定
        let old = match self.locals.get(&name) {
            Some(v) => v.clone(),
            None => {
                return Err(LowerIrError::Internal(format!(
                    "local variable '{}' not found",
                    name
                )))
            }
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
        let new_var = self.fresh_var("field_inc", result_ty.clone());
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

    fn lower_logical_and(
        &mut self,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        // a && b: if a { if b { true } else { false } } else { false }
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

        // then 块：计算 rhs
        self.current_bb = then_bb.clone();
        let rhs = self.lower_expr(rhs_id)?;
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // else 块：直接 false
        self.current_bb = else_bb.clone();
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // merge 块：phi 节点
        self.current_bb = merge_bb.clone();
        let result = self.fresh_var("and_result", Type::Bool);
        let phi = Phi {
            dest: result.clone(),
            incoming: vec![
                (rhs.into_var().unwrap_or_else(|| SsaVar {
                    base: "false".to_string(),
                    version: 0,
                    ty: Type::Bool,
                    span: dummy_span(),
                }), then_bb.clone()),
                (SsaVar {
                    base: "false".to_string(),
                    version: 0,
                    ty: Type::Bool,
                    span: dummy_span(),
                }, else_bb.clone()),
            ],
        };
        // 简化：直接用 select 代替
        self.ssa.blocks.get_mut(&merge_bb).unwrap().phis.push(phi);

        Ok(Operand::Var(result))
    }

    fn lower_logical_or(
        &mut self,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
    ) -> Result<Operand, LowerIrError> {
        // a || b: if a { true } else { if b { true } else { false } }
        let then_bb = self.unique_bb("or_then");
        let else_bb = self.unique_bb("or_else");
        let merge_bb = self.unique_bb("or_merge");

        let lhs = self.lower_expr(lhs_id)?;
        self.set_term(Terminator::Branch {
            cond: lhs,
            t: then_bb.clone(),
            f: else_bb.clone(),
        });

        // then 块：直接 true
        self.current_bb = then_bb.clone();
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // else 块：计算 rhs
        self.current_bb = else_bb.clone();
        let rhs = self.lower_expr(rhs_id)?;
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // merge 块
        self.current_bb = merge_bb.clone();
        let result = self.fresh_var("or_result", Type::Bool);
        let phi = Phi {
            dest: result.clone(),
            incoming: vec![
                (SsaVar {
                    base: "true".to_string(),
                    version: 0,
                    ty: Type::Bool,
                    span: dummy_span(),
                }, then_bb.clone()),
                (rhs.into_var().unwrap_or_else(|| SsaVar {
                    base: "false".to_string(),
                    version: 0,
                    ty: Type::Bool,
                    span: dummy_span(),
                }), else_bb.clone()),
            ],
        };
        self.ssa.blocks.get_mut(&merge_bb).unwrap().phis.push(phi);

        Ok(Operand::Var(result))
    }

    // --- assignment ---

    fn lower_assign(
        &mut self,
        lhs_id: HirExprId,
        rhs: Operand,
    ) -> Result<Operand, LowerIrError> {
        let lhs_expr = self.get_expr(lhs_id)?;

        match &lhs_expr.kind {
            HirExprKind::Local(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = sym.name.clone();
                let sym_is_volatile = sym.is_volatile;
                let sym_ty = sym.ty.clone();
                let ty = self.lowerer.lower_type(&sym_ty)?;
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
                        Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) => {}
                        _ => {
                            let addr = self.fresh_var("vaddr", Type::Ptr(Box::new(QualType::new(ty.clone()))));
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
                        }
                    }
                }

                self.locals.insert(name, new_var.clone());
                Ok(Operand::Var(new_var))
            }
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
            }
            HirExprKind::FieldAccess { object, field } => {
                self.lower_field_store(*object, *field, rhs)
            }
            HirExprKind::Index { array, index } => {
                // a[i] = v → IndexStore
                let arr = self.lower_expr(*array)?;
                let idx = self.lower_expr(*index)?;
                let is_volatile = self.index_is_volatile(*array);
                let inst = Inst {
                    dest: None,
                    kind: InstKind::IndexStore {
                        arr,
                        idx,
                        src: rhs,
                        is_volatile,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                Ok(Operand::Void)
            }
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
            }
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
                    CxxSymbolKind::Method
                        | CxxSymbolKind::Constructor
                        | CxxSymbolKind::Destructor
                ) && sym.owner.is_some();

                let mut lowered_args = Vec::with_capacity(args.len());

                if is_member {
                    // 成员函数调用：第一个参数是 this 指针
                    // 需要判断 this 是否需要调整（调用基类方法时）
                    let this_owner = sym.owner.unwrap();
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
                        }
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
                            self.adjust_pointer_to_base(object, obj_class, this_owner_class, &target_ty)?
                        } else {
                            object
                        }
                    } else {
                        object
                    };

                    lowered_args.push(adjusted_this);

                    // 其余参数
                    for arg_id in &args[1..] {
                        lowered_args.push(self.lower_expr(*arg_id)?);
                    }
                } else {
                    // 非成员函数
                    for arg_id in args {
                        lowered_args.push(self.lower_expr(*arg_id)?);
                    }
                }

                // 如果符号是 dccpp_* runtime symbol，需要注册为 external
                if fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_") {
                    if !self.lowerer.output.external_symbols.contains(&fn_name) {
                        self.lowerer.output.external_symbols.push(fn_name.clone());
                    }
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
                    let dest = self.fresh_var("call_result", ret_ty.clone());
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
            HirCallee::Virtual {
                method,
                slot,
                this_adjust,
            } => self.lower_virtual_call(*method, *slot, this_adjust, args, ret_ty),
            _ => Err(LowerIrError::Unsupported(
                "indirect/ctor/dtor calls".to_string(),
            )),
        }
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
            return Err(LowerIrError::Internal(
                "virtual call missing this argument".to_string(),
            ));
        }

        // 1. 获取 this 指针对象
        let object_expr = args[0];
        let object = self.lower_expr(object_expr)?;

        // 获取对象类型的 class（用于确定 vptr 类型）
        let object_expr_data = self.get_expr(object_expr)?;
        let _object_class = match &object_expr_data.ty {
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
            let byte_ptr = self.fresh_var("vcall_byte_ptr", byte_ptr_ty.clone());
            let cast_to_byte = Inst {
                dest: Some(byte_ptr.clone()),
                kind: InstKind::Cast {
                    ty: byte_ptr_ty.clone(),
                    e: object.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(cast_to_byte);

            let adj_byte = self.fresh_var("vcall_adj_byte", byte_ptr_ty.clone());
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
            let vptr_ptr = self.fresh_var("vcall_vptr_ptr", vptr_ptr_ty.clone());
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
                }
            };
            if obj_ty == vptr_ptr_ty {
                object.clone()
            } else {
                let vptr_ptr = self.fresh_var("vcall_vptr_ptr", vptr_ptr_ty.clone());
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

        // 3. 从 vptr 加载 vtable 指针（vptr 类型为 *const *const u8）
        let vtable_ptr_ty = self.lowerer.vtable_entry_type();
        let vtable_ptr = self.fresh_var("vtable_ptr", vtable_ptr_ty.clone());
        let load_vptr = Inst {
            dest: Some(vtable_ptr.clone()),
            kind: InstKind::Load {
                e: adjusted_this.clone(),
                ty: vtable_ptr_ty.clone(),
                is_volatile: false,
            },
            span: dummy_span(),
        };
        self.push_inst(load_vptr);

        // 4. 从 vtable[slot] 加载函数指针
        // 计算 vtable[slot] 的地址：vtable_ptr + slot * sizeof(pointer)
        let slot_offset = (slot as i128) * (POINTER_SIZE as i128);
        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
        let vt_byte_ptr = self.fresh_var("vt_byte_ptr", byte_ptr_ty.clone());
        let cast_vt_byte = Inst {
            dest: Some(vt_byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: Operand::Var(vtable_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_vt_byte);

        let slot_byte_ptr = self.fresh_var("vt_slot_ptr", byte_ptr_ty.clone());
        let slot_gep = Inst {
            dest: Some(slot_byte_ptr.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(vt_byte_ptr),
                r: Operand::Int(slot_offset),
            },
            span: dummy_span(),
        };
        self.push_inst(slot_gep);

        // 加载函数指针（类型为 void*，后续通过调用使用）
        let fn_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let fn_ptr_var = self.fresh_var("fn_ptr", fn_ptr_ty.clone());
        let cast_fn_ptr = Inst {
            dest: Some(fn_ptr_var.clone()),
            kind: InstKind::Cast {
                ty: fn_ptr_ty.clone(),
                e: Operand::Var(slot_byte_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_fn_ptr);

        let fp_var = self.fresh_var("vcall_fp", fn_ptr_ty.clone());
        let load_fp = Inst {
            dest: Some(fp_var.clone()),
            kind: InstKind::Load {
                e: Operand::Var(fn_ptr_var),
                ty: fn_ptr_ty.clone(),
                is_volatile: false,
            },
            span: dummy_span(),
        };
        self.push_inst(load_fp);

        // 5. 构造参数列表：第一个参数是 this（调整后）
        let method_sym = self.lowerer.get_symbol(method)?;
        let method_owner_class = method_sym
            .owner
            .map(ClassId::from);
        drop(method_sym);

        // 构造 this 参数：使用原始对象（非 vptr 指针类型），
        // 因为被调用的虚函数期望接收正确类型的 this 指针
        let this_arg = if this_adjust.non_virtual != 0 {
            // 非零偏移：adjusted_this 指向 vptr 子对象，但函数期望的 this 是
            // 该子对象的正确类型指针。我们用 adjusted_this 转成正确类型。
            // 这里简化：直接用 adjusted_this 作为 this 参数（ABI 层已经是正确偏移）
            // 但类型需要匹配：转成 method 所在类的指针类型
            if let Some(owner_class) = method_owner_class {
                let owner_ptr_ty = self.lowerer.lower_type(&CxxType::Pointer(
                    Box::new(CxxType::Class(owner_class)),
                ))?;
                let this_var = self.fresh_var("vcall_this", owner_ptr_ty.clone());
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

        // 其余参数
        for arg_id in &args[1..] {
            lowered_args.push(self.lower_expr(*arg_id)?);
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
            let dest = self.fresh_var("vcall_result", ret_ty.clone());
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
        let object = self.lower_expr(object_id)?;
        let field_sym = self.lowerer.get_symbol(field_id)?;
        let field_name = field_sym.name.clone();
        let (object, obj_ty, object_is_arrow) =
            self.deref_object_for_member(object_id, object)?;
        let obj_is_volatile = self.obj_base_is_volatile(object_id);

        let dest = self.fresh_var("field", result_ty.clone());
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
        let object = self.lower_expr(object_id)?;
        let field_sym = self.lowerer.get_symbol(field_id)?;
        let field_name = field_sym.name.clone();
        let (object, obj_ty, object_is_arrow) =
            self.deref_object_for_member(object_id, object)?;
        let obj_is_volatile = self.obj_base_is_volatile(object_id);

        let inst = Inst {
            dest: None,
            kind: InstKind::MemberStore {
                obj: object,
                obj_ty,
                field: field_name,
                src,
                is_volatile: obj_is_volatile,
                is_arrow: object_is_arrow,
            },
            span: dummy_span(),
        };
        self.push_inst(inst);

        Ok(Operand::Void)
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
            }
            _ => {
                let obj_ty = self.lowerer.lower_type(&obj_expr.ty)?;
                Ok((object, obj_ty, false))
            }
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
                let sym = self.lowerer.get_symbol(*sym_id)?;
                let name = sym.name.clone();
                // 局部变量地址 → AddrOf 指令
                let dest = self.fresh_var("addr", result_ty.clone());
                let inst = Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::AddrOf { base: name },
                    span: dummy_span(),
                };
                self.push_inst(inst);
                Ok(Operand::Var(dest))
            }
            HirExprKind::Global(sym_id) => {
                let sym = self.lowerer.get_symbol(*sym_id)?;
                Ok(Operand::SymAddr(sym.name.clone(), result_ty.clone()))
            }
            HirExprKind::Dereference(inner_inner) => {
                // &*e → e (恒等)
                self.lower_expr(*inner_inner)
            }
            _ => Err(LowerIrError::Unsupported(
                "address of complex expression".to_string(),
            )),
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
        let n = self.versions.get("__tmp_seq").copied().unwrap_or(0) + 1;
        self.versions.insert("__tmp_seq".to_string(), n);
        SsaVar {
            base: format!("_t{}_{}", n, base),
            version: 0,
            ty,
            span: dummy_span(),
        }
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
            }
            _ => false,
        }
    }

    /// 降低下标访问 `a[i]`（读）→ `IndexLoad`
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
        let dest = self.fresh_var("deref", result_ty.clone());
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

        let dest = self.fresh_var("cast", target_ty.clone());
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
            let dest = self.fresh_var("base_cast", result_ty.clone());
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
        let byte_ptr = self.fresh_var("base_byte_ptr", byte_ptr_ty.clone());
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
        let adjusted_byte_ptr = self.fresh_var("base_adj_byte", byte_ptr_ty.clone());
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
        let dest = self.fresh_var("base_cast", result_ty.clone());
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

        // 查找虚基类的 vbase_index
        let vbase_idx = from_abi
            .virtual_bases
            .iter()
            .find(|vb| vb.base == to_class)
            .map(|vb| vb.vbase_index)
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "virtual base class {} not found in class {}",
                    to_class.0, from_class.0
                ))
            })?;

        // 计算 vbase offset 在 vtable 中的相对偏移（相对于 address point）
        // vtable 布局（从低地址到高地址）:
        //   [vbase_offset_0] [vbase_offset_1] ... [offset_to_top] [typeinfo] [vfunc_0] [vfunc_1] ...
        //                                                                   ^
        //                                                            address_point (vptr 指向这里)
        // 所以 vbase_offset_i 的地址 = vptr - (vbase_count - i) * 8 - 16
        let vbase_count = from_abi.virtual_bases.len() as i64;
        let vtable_offset_bytes = -(vbase_count - vbase_idx as i64) * POINTER_SIZE as i64
            - 2 * POINTER_SIZE as i64;

        let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));

        // 1. 将对象指针转换为 byte 指针（保存一份用于后面加偏移）
        let obj_byte_ptr = self.fresh_var("obj_byte_ptr", byte_ptr_ty.clone());
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
        let vptr_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Ptr(Box::new(
            QualType::new(Type::UChar),
        )))));
        let vptr_ptr = self.fresh_var("vptr_ptr", vptr_ptr_ty.clone());
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
        let vtable_ptr = self.fresh_var("vtable_ptr", vtable_ptr_ty.clone());
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
        let vtable_byte_ptr = self.fresh_var("vtable_byte", byte_ptr_ty.clone());
        let cast_vtable_to_byte = Inst {
            dest: Some(vtable_byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: Operand::Var(vtable_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_vtable_to_byte);

        let vbase_slot_ptr = self.fresh_var("vbase_slot", byte_ptr_ty.clone());
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
        let vbase_offset_ptr = self.fresh_var("vbase_offset_ptr", vbase_offset_ptr_ty.clone());
        let cast_to_vbase_offset_ptr = Inst {
            dest: Some(vbase_offset_ptr.clone()),
            kind: InstKind::Cast {
                ty: vbase_offset_ptr_ty,
                e: Operand::Var(vbase_slot_ptr),
            },
            span: dummy_span(),
        };
        self.push_inst(cast_to_vbase_offset_ptr);

        let vbase_offset = self.fresh_var("vbase_offset", vbase_offset_ty.clone());
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
        let adjusted_byte_ptr = self.fresh_var("vbase_adj_byte", byte_ptr_ty.clone());
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
        let dest = self.fresh_var("vbase_adjust", result_ty.clone());
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
                return Err(LowerIrError::Unsupported(
                    "dynamic_cast on non-class types".into(),
                ));
            }
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
            let dest = self.fresh_var("dyn_cast_static", result_ty.clone());
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
        let obj_void_ptr = self.fresh_var("dyn_cast_obj", void_ptr_ty.clone());
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
        let src_ti_op = Operand::SymAddr(src_typeinfo, typeinfo_ptr_ty.clone());

        // 3. dst typeinfo
        let dst_ti_op = Operand::SymAddr(dst_typeinfo, typeinfo_ptr_ty.clone());

        // 4. src2dst hint (简化为 0，表示无提示)
        let hint_op = Operand::Int(0);

        // 调用 __dynamic_cast
        let fn_name = CxxRuntimeSymbol::DynamicCast.name().to_string();
        
        // 如果是 runtime symbol，需要注册为 external
        if fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_") || fn_name.starts_with("__cxa_") || fn_name == "__dynamic_cast" {
            if !self.lowerer.output.external_symbols.contains(&fn_name) {
                self.lowerer.output.external_symbols.push(fn_name.clone());
            }
        }
        
        let fn_addr = Operand::FnAddr(fn_name, dyn_cast_fn_ty);

        let result_void_ptr = self.fresh_var("dyn_cast_result", void_ptr_ty.clone());
        let call_inst = Inst {
            dest: Some(result_void_ptr.clone()),
            kind: InstKind::Call {
                callee: fn_addr,
                args: vec![
                    Operand::Var(obj_void_ptr),
                    src_ti_op,
                    dst_ti_op,
                    hint_op,
                ],
                ret_ty: void_ptr_ty.clone(),
            },
            span: dummy_span(),
        };
        self.push_inst(call_inst);

        // 将结果转换为目标类型
        let dest = self.fresh_var("dyn_cast_final", result_ty.clone());
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
        let ctor_name = match variant {
            CtorVariant::Complete => format!("{}_ctor_complete", class_name),
            CtorVariant::Base => format!("{}_ctor_base", class_name),
            CtorVariant::Allocating => format!("{}_ctor_allocating", class_name),
        };

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
                let callee_op = Operand::FnAddr(
                    "__dcc_eh_rethrow".to_string(),
                    rethrow_fn_ty,
                );
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
            }
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
                let exn_ptr_var = self.fresh_var("exn_ptr", void_ptr_ty.clone());
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
                let typed_ptr_var = self.fresh_var(
                    "exn_typed",
                    Type::Ptr(Box::new(QualType::new(obj_ir_ty.clone()))),
                );
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
                if fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_") {
                    if !self.lowerer.output.external_symbols.contains(&fn_name) {
                        self.lowerer.output.external_symbols.push(fn_name.clone());
                    }
                }
                
                let throw_callee = Operand::FnAddr(fn_name,
                    throw_fn_ty,
                );
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
            }
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
        let ret_ty = self.lowerer.lower_type(&CxxType::Pointer(Box::new(ty.clone())))?;

        if let Some(place_args) = placement {
            // placement new: 不分配内存，直接在 ptr 上构造
            // 返回值就是 placement 地址（cast 到目标类型指针）
            let ptr_val = self.lower_expr(place_args[0])?;

            // 如果是类类型且有构造参数，调用构造函数
            if let CxxType::Class(class_id) = ty {
                if !args.is_empty() {
                    // 构造函数需要 this 指针，placement ptr 就是 this
                    self.lower_construct(*class_id, place_args[0], args, CtorVariant::Complete)?;
                }
            }

            // 把 void* / 其他指针类型 cast 到目标类型指针
            let typed_ptr_var = self.fresh_var("new_result", ret_ty.clone());
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
                return Err(LowerIrError::Internal(
                    "array new missing size argument".to_string(),
                ));
            }
            let elem_size = self.compute_type_size(ty)?;
            let count_val = self.lower_expr(args[0])?;
            let elem_size_op = Operand::Int(elem_size as i128);
            let size_var = self.fresh_var("alloc_size", Type::ULong);
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
        if fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_") || fn_name.starts_with("__cxa_") || fn_name == "__dynamic_cast" || fn_name.starts_with("_Z") {
            if !self.lowerer.output.external_symbols.contains(&fn_name) {
                self.lowerer.output.external_symbols.push(fn_name.clone());
            }
        }
        
        let new_callee = Operand::FnAddr(fn_name, new_fn_ty.clone());
        let raw_ptr_var = self.fresh_var("raw_new", void_ptr_ty.clone());
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

        // 如果是类类型且有构造参数，调用构造函数（scalar new  only）
        if !is_array {
            if let CxxType::Class(class_id) = ty {
                if !args.is_empty() {
                    // 构造函数需要 this 指针
                    // 先把 raw_ptr 转成目标类型指针
                    let typed_ptr_var = self.fresh_var(
                        "typed_new",
                        Type::Ptr(Box::new(QualType::new(self.lowerer.lower_type(ty)?))),
                    );
                    let cast_inst = Inst {
                        dest: Some(typed_ptr_var.clone()),
                        kind: InstKind::Cast {
                            ty: Type::Ptr(Box::new(QualType::new(
                                self.lowerer.lower_type(ty)?,
                            ))),
                            e: Operand::Var(raw_ptr_var.clone()),
                        },
                        span: dummy_span(),
                    };
                    self.push_inst(cast_inst);

                    // 调用构造函数
                    // 注意：这里需要用 HirExprId 来表示指针，但我们只有 Operand
                    // 简化：直接生成构造函数调用，this 指针就是 typed_ptr_var
                    let class_sym = self.lowerer.get_symbol(class_id.as_symbol())?;
                    let ctor_name = format!("{}_ctor_complete", class_sym.name);
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
        let result_var = self.fresh_var("new_result", ret_ty.clone());
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
    fn lower_delete(&mut self, ptr: HirExprId, is_array: bool) -> Result<Operand, LowerIrError> {
        use crate::cpp::runtime::CxxRuntimeSymbol;

        let ptr_val = self.lower_expr(ptr)?;

        let op_sym = if is_array {
            CxxRuntimeSymbol::DeleteArray
        } else {
            CxxRuntimeSymbol::Delete
        };

        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let delete_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let fn_name = op_sym.name().to_string();
        
        // 如果是 runtime symbol，需要注册为 external
        if fn_name.starts_with("dccpp_") || fn_name.starts_with("__dcc_") || fn_name.starts_with("__cxa_") || fn_name == "__dynamic_cast" || fn_name.starts_with("_Z") {
            if !self.lowerer.output.external_symbols.contains(&fn_name) {
                self.lowerer.output.external_symbols.push(fn_name.clone());
            }
        }
        
        let delete_callee = Operand::FnAddr(fn_name, delete_fn_ty);

        // 把指针转成 void*
        let void_ptr_var = self.fresh_var("del_void", void_ptr_ty.clone());
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

        Ok(Operand::Void)
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
        self.ssa.local_types.insert(tmp_name.clone(), arr_ty.clone());
        self.ssa.original_locals.insert(tmp_name.clone());

        let var = self.fresh_var(&tmp_name, arr_ty.clone());
        self.locals.insert(tmp_name.clone(), var.clone());

        // 逐个存储元素
        for (i, elem_id) in elems.iter().enumerate() {
            let elem_val = self.lower_expr(*elem_id)?;
            let idx_op = Operand::Int(i as i128);
            let store_inst = Inst {
                dest: None,
                kind: InstKind::IndexStore {
                    arr: Operand::Var(var.clone()),
                    idx: idx_op,
                    src: elem_val,
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
        let addr_var = self.fresh_var("init_list_ptr", ptr_ty.clone());
        let addr_inst = Inst {
            dest: Some(addr_var.clone()),
            kind: InstKind::AddrOf { base: tmp_name },
            span: dummy_span(),
        };
        self.push_inst(addr_inst);

        Ok(Operand::Var(addr_var))
    }

    /// 计算类型的大小（字节）
    fn compute_type_size(&mut self, ty: &CxxType) -> Result<u64, LowerIrError> {
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
            }
            _ => {
                // 非类类型：返回 null（简化处理，SJLJ 运行时按类型名匹配）
                Ok(Operand::Int(0))
            }
        }
    }

    // === statement lowering ===

    fn lower_stmt(&mut self, stmt_id: HirStmtId) -> Result<(), LowerIrError> {
        let stmt = self.get_stmt(stmt_id)?;

        match &stmt.kind {
            HirStmtKind::Expr(expr_id) => {
                let _ = self.lower_expr(*expr_id)?;
                Ok(())
            }

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
            }

            HirStmtKind::Return(None) => {
                self.set_term(Terminator::Jump(self.return_bb.clone()));
                Ok(())
            }

            HirStmtKind::Return(Some(expr_id)) => {
                let val = self.lower_expr(*expr_id)?;
                // ret_val 已移除: 直接在当前块返回, 不经过统一 return_bb.
                self.set_term(Terminator::Ret { val: Some(val) });
                Ok(())
            }

            HirStmtKind::Decl { symbol, init } => {
                let sym = self.lowerer.get_symbol(*symbol)?;
                let name = sym.name.clone();
                let ty = self.lowerer.lower_type(&sym.ty)?;
                let sym_is_volatile = sym.is_volatile;
                drop(sym);

                // 创建局部变量（version 0）
                let var = self.fresh_var(&name, ty.clone());
                self.ssa
                    .local_types
                    .insert(name.clone(), ty.clone());
                self.ssa.original_locals.insert(name.clone());
                let decl_quals = if sym_is_volatile {
                    QualifierFlags::VOLATILE
                } else {
                    QualifierFlags::NONE
                };
                self.ssa.local_quals.insert(name.clone(), decl_quals);

                if let Some(init_id) = init {
                    let init_val = self.lower_expr(*init_id)?;
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
            }

            HirStmtKind::StaticDecl {
                symbol,
                guard_symbol,
                init,
            } => {
                use crate::cpp::runtime::CxxRuntimeSymbol;

                let var_sym = self.lowerer.get_symbol(*symbol)?;
                let var_name = self.lowerer.symbol_name(&var_sym);
                let var_ty = self.lowerer.lower_type(&var_sym.ty)?;
                drop(var_sym);

                let guard_sym = self.lowerer.get_symbol(*guard_symbol)?;
                let guard_name = self.lowerer.symbol_name(&guard_sym);
                drop(guard_sym);

                // guard 变量类型: i64* (指针)
                let guard_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Long)));
                // __cxa_guard_acquire 参数类型: i64*，返回类型: int
                let guard_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Int)));

                // 获取 guard 变量地址
                let guard_addr = Operand::SymAddr(guard_name, guard_ptr_ty.clone());

                // 调用 __cxa_guard_acquire(guard_ptr)
                let acquire_callee = Operand::FnAddr(
                    CxxRuntimeSymbol::GuardAcquire.name().to_string(),
                    guard_fn_ty.clone(),
                );
                let acquire_result = self.fresh_var("guard_acquire", Type::Int);
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
                    // 获取静态变量地址
                    let var_ptr_ty = Type::Ptr(Box::new(QualType::new(var_ty.clone())));
                    let var_addr = Operand::SymAddr(var_name.clone(), var_ptr_ty.clone());
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
            }

            HirStmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                self.lower_if(*cond, *then_body, else_body.as_ref().copied())
            }

            HirStmtKind::While { cond, body } => {
                self.lower_while(*cond, *body)
            }

            HirStmtKind::DoWhile { body, cond } => {
                self.lower_do_while(*body, *cond)
            }

            HirStmtKind::For { init, cond, inc, body } => {
                self.lower_for(*init, *cond, *inc, *body)
            }

            HirStmtKind::Switch { cond, body } => {
                self.lower_switch(*cond, *body)
            }

            HirStmtKind::Break => self.lower_break(),

            HirStmtKind::Continue => self.lower_continue(),

            HirStmtKind::Goto(label) => self.lower_goto(label.clone()),

            HirStmtKind::Label(name, body) => self.lower_label(name.clone(), *body),

            HirStmtKind::Null => Ok(()),

            HirStmtKind::Fallthrough => Ok(()),

            HirStmtKind::CtorInitBase { base, args } => {
                self.lower_ctor_init_base(*base, args)
            }

            HirStmtKind::CtorInitMember { member, args } => {
                self.lower_ctor_init_member(*member, args)
            }

            HirStmtKind::SetVTablePtr { class, place } => {
                self.lower_set_vtable_ptr(*class, *place)
            }

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
            self.ssa
                .blocks
                .get_mut(&else_bb)
                .unwrap()
                .term = Terminator::Jump(end_bb.clone());
        }

        // All new unreachable blocks added by then/else branches are
        // implicit fall-through exits — redirect them to end_bb.
        let after = self.unreachable_blocks();
        for name in after {
            if !before_set.contains(&name) {
                self.ssa
                    .blocks
                    .get_mut(&name)
                    .unwrap()
                    .term = Terminator::Jump(end_bb.clone());
            }
        }

        self.current_bb = end_bb;
        Ok(())
    }

    /// 扫描 switch body 收集 case/default 标签，为每个标签创建 BB
    fn collect_switch_cases(
        &mut self,
        stmt: &HirStmt,
    ) -> Result<(Vec<(i128, String)>, Option<String>), LowerIrError> {
        let mut cases: Vec<(i128, String)> = Vec::new();
        let mut default: Option<String> = None;

        match &stmt.kind {
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    let s = self.get_stmt(*sid)?;
                    self.collect_cases_from_stmt(&s, &mut cases, &mut default)?;
                }
            }
            _ => {
                self.collect_cases_from_stmt(stmt, &mut cases, &mut default)?;
            }
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
            }
            HirStmtKind::Default(body) => {
                let bb = self.unique_bb("case_default");
                *default = Some(bb);
                let body_stmt = self.get_stmt(*body)?;
                self.collect_cases_from_stmt(&body_stmt, cases, default)?;
            }
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    let s = self.get_stmt(*sid)?;
                    self.collect_cases_from_stmt(&s, cases, default)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Lower switch body: 遇到 case/default 切换到对应 BB，其他语句正常 lower
    fn lower_switch_body(&mut self, body_id: HirStmtId) -> Result<(), LowerIrError> {
        let body_stmt = self.get_stmt(body_id)?;
        match &body_stmt.kind {
            HirStmtKind::Compound(stmts) => {
                // 找到第一个 case/default 作为起始 BB
                // 先找到第一个标签的 BB 作为当前 BB
                let first_label_bb = self.find_first_case_bb(&body_stmt)?;
                if let Some(bb) = first_label_bb {
                    self.current_bb = bb;
                }
                for sid in stmts {
                    self.lower_switch_stmt(*sid)?;
                }
            }
            _ => {
                self.lower_switch_stmt(body_id)?;
            }
        }
        Ok(())
    }

    fn find_first_case_bb(&mut self, stmt: &HirStmt) -> Result<Option<String>, LowerIrError> {
        match &stmt.kind {
            HirStmtKind::Case { value, .. } => {
                let name = format!("case_{}", value);
                // 从已创建的 BB 中找
                for (bb_name, _) in &self.ssa.blocks {
                    if bb_name.starts_with(&name) {
                        return Ok(Some(bb_name.clone()));
                    }
                }
                Ok(None)
            }
            HirStmtKind::Default(_) => {
                for (bb_name, _) in &self.ssa.blocks {
                    if bb_name.starts_with("case_default") {
                        return Ok(Some(bb_name.clone()));
                    }
                }
                Ok(None)
            }
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    let s = self.get_stmt(*sid)?;
                    if let Some(bb) = self.find_first_case_bb(&s)? {
                        return Ok(Some(bb));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn lower_switch_stmt(&mut self, stmt_id: HirStmtId) -> Result<(), LowerIrError> {
        let stmt = self.get_stmt(stmt_id)?;
        match &stmt.kind {
            HirStmtKind::Case { value, body } => {
                // 切换到 case 对应的 BB
                let name = format!("case_{}", value);
                let bb = self
                    .ssa
                    .blocks
                    .keys()
                    .find(|k| k.starts_with(&name))
                    .cloned()
                    .unwrap_or_else(|| self.unique_bb(&name));
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
                self.lower_switch_stmt(*body)?;
            }
            HirStmtKind::Default(body) => {
                let bb = self
                    .ssa
                    .blocks
                    .keys()
                    .find(|k| k.starts_with("case_default"))
                    .cloned()
                    .unwrap_or_else(|| self.unique_bb("case_default"));
                let cur = self.current_bb.clone();
                if cur != bb {
                    let cur_term = self.ssa.blocks[&cur].term.clone();
                    if matches!(cur_term, Terminator::Unreachable) {
                        self.set_term(Terminator::Jump(bb.clone()));
                    }
                }
                self.current_bb = bb;
                self.lower_switch_stmt(*body)?;
            }
            HirStmtKind::Compound(stmts) => {
                for sid in stmts {
                    self.lower_switch_stmt(*sid)?;
                    let cur = self.current_bb.clone();
                    let cur_term = self.ssa.blocks[&cur].term.clone();
                    if !matches!(cur_term, Terminator::Unreachable) {
                        break;
                    }
                }
            }
            HirStmtKind::Break => {
                self.lower_break()?;
            }
            _ => {
                self.lower_stmt(stmt_id)?;
            }
        }
        Ok(())
    }

    fn lower_while(
        &mut self,
        cond_id: HirExprId,
        body_id: HirStmtId,
    ) -> Result<(), LowerIrError> {
        // Snapshot unreachable blocks BEFORE creating structural BBs.
        // BBs whose terminators we set manually are added to the
        // "old" set so the fall-through logic skips them.
        let mut before_set: std::collections::HashSet<String> =
            self.unreachable_blocks().into_iter().collect();

        let cond_bb = self.unique_bb("while_cond");
        let body_bb = self.unique_bb("while_body");
        let end_bb = self.unique_bb("while_end");
        before_set.insert(cond_bb.clone()); // we set its Branch
        before_set.insert(end_bb.clone());  // left as fall-through

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
                self.ssa
                    .blocks
                    .get_mut(&name)
                    .unwrap()
                    .term = Terminator::Jump(cond_bb.clone());
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
        before_set.insert(end_bb.clone());  // left as fall-through

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
                self.ssa
                    .blocks
                    .get_mut(&name)
                    .unwrap()
                    .term = Terminator::Jump(cond_bb.clone());
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
        before_set.insert(inc_bb.clone());  // we set its Jump to cond
        before_set.insert(end_bb.clone());  // left as fall-through

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
                self.ssa
                    .blocks
                    .get_mut(&name)
                    .unwrap()
                    .term = Terminator::Jump(inc_bb.clone());
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
        let target = self
            .break_stack
            .last()
            .cloned()
            .ok_or_else(|| LowerIrError::Internal("break without enclosing loop/switch".into()))?;
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

    fn lower_switch(
        &mut self,
        cond_id: HirExprId,
        body_id: HirStmtId,
    ) -> Result<(), LowerIrError> {
        let end_bb = self.unique_bb("switch_end");
        let switch_bb = self.unique_bb("switch_dispatch");

        // 计算 switch 表达式
        let cond = self.lower_expr(cond_id)?;
        self.set_term(Terminator::Jump(switch_bb.clone()));

        // 扫描 body 收集 case/default 标签及其对应的 BB
        let body_stmt = self.get_stmt(body_id)?;
        let (cases, default_bb) = self.collect_switch_cases(&body_stmt)?;

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
        self.lower_switch_body(body_id)?;
        self.break_stack.pop();

        // 如果最后一个 case/default 块没有 terminator，跳到 end
        // (lower_switch_body 已经处理了)

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
        let ctx_var = self.fresh_var("__eh_ctx", ctx_ty.clone());
        self.ssa
            .local_types
            .insert("__eh_ctx".to_string(), ctx_ty.clone());
        self.ssa.original_locals.insert("__eh_ctx".to_string());
        self.ssa.local_quals.insert("__eh_ctx".to_string(), QualifierFlags::NONE);
        self.locals.insert("__eh_ctx".to_string(), ctx_var.clone());

        // 获取上下文指针（数组名退化为指针）
        let ctx_ptr_var = self.fresh_var(
            "__eh_ctx_ptr",
            Type::Ptr(Box::new(QualType::new(Type::UChar))),
        );
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
        let push_callee = Operand::FnAddr(
            "__dcc_eh_push_context".to_string(),
            push_fn_ty.clone(),
        );
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
        let setjmp_callee = Operand::FnAddr(
            "__dcc_eh_setjmp".to_string(),
            setjmp_fn_ty.clone(),
        );
        let setjmp_result = self.fresh_var("__eh_setjmp_res", int_ty.clone());
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
            let pop_callee = Operand::FnAddr(
                "__dcc_eh_pop_context".to_string(),
                push_fn_ty.clone(),
            );
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
            self.ssa
                .blocks
                .get_mut(&try_bb)
                .unwrap()
                .term = Terminator::Jump(end_bb.clone());
        }

        // === catch dispatch 块 ===
        self.current_bb = catch_bb.clone();

        // 获取异常对象指针
        let get_exn_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Ptr(Box::new(QualType::new(Type::Void))))));
        let get_exn_callee = Operand::FnAddr(
            "__dcc_eh_get_exception".to_string(),
            get_exn_fn_ty.clone(),
        );
        let exn_ptr_var = self.fresh_var("__eh_exn_ptr", void_ptr_ty.clone());
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
                let type_match_callee = Operand::FnAddr(
                    "__dcc_eh_type_match".to_string(),
                    type_match_fn_ty,
                );
                let match_result = self.fresh_var("__eh_type_match", int_ty.clone());
                let match_call = Inst {
                    dest: Some(match_result.clone()),
                    kind: InstKind::Call {
                        callee: type_match_callee,
                        args: vec![
                            Operand::Var(exn_ptr_var.clone()),
                            typeinfo_op,
                        ],
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
                let typed_ptr_var = self.fresh_var("__catch_typed_ptr", typed_ptr_ty.clone());
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
                let end_catch_callee = Operand::FnAddr(
                    "__dcc_eh_end_catch".to_string(),
                    push_fn_ty.clone(),
                );
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
                self.ssa
                    .blocks
                    .get_mut(&handler_bb)
                    .unwrap()
                    .term = Terminator::Jump(end_bb.clone());
            }

            current_bb = next_bb.clone();
        }

        // 最后一个 handler 不匹配：rethrow
        if !handlers.is_empty() {
            let rethrow_bb = next_handler_bbs.last().unwrap().clone();
            self.current_bb = rethrow_bb.clone();
            let rethrow_fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
            let rethrow_callee = Operand::FnAddr(
                "__dcc_eh_rethrow".to_string(),
                rethrow_fn_ty,
            );
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
            self.ssa
                .blocks
                .get_mut(&rethrow_bb)
                .unwrap()
                .term = Terminator::Unreachable;
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
                return Err(LowerIrError::Internal(
                    "constructor without owning class".to_string(),
                ));
            }
        };

        // 计算基类子对象指针
        let base_ptr_ty = {
            let base_ty = CxxType::Pointer(Box::new(CxxType::Class(base_class)));
            self.lowerer.lower_type(&base_ty)?
        };

        // 计算基类子对象地址
        let base_layout = self
            .lowerer
            .find_base_layout(owner_class, base_class)?
            .ok_or_else(|| {
                LowerIrError::Internal(format!(
                    "base class {} not found in class {}",
                    base_class.0, owner_class.0
                ))
            })?;

        let offset = base_layout.offset as i64;
        let base_ptr = if offset == 0 {
            // primary base: 直接 cast
            if self.ssa.params[0].ty == base_ptr_ty {
                this.clone()
            } else {
                let dest = self.fresh_var("base_this", base_ptr_ty.clone());
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
            let byte_ptr = self.fresh_var("ctor_byte_ptr", byte_ptr_ty.clone());
            let cast_to_byte = Inst {
                dest: Some(byte_ptr.clone()),
                kind: InstKind::Cast {
                    ty: byte_ptr_ty.clone(),
                    e: this.clone(),
                },
                span: dummy_span(),
            };
            self.push_inst(cast_to_byte);

            let adj_byte = self.fresh_var("ctor_adj_byte", byte_ptr_ty.clone());
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

            let dest = self.fresh_var("base_this", base_ptr_ty.clone());
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
        let ctor_name = format!("{}_ctor_base", base_sym.name);
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
    fn lower_set_vtable_ptr(
        &mut self,
        class: ClassId,
        place_id: HirExprId,
    ) -> Result<(), LowerIrError> {
        let place = self.lower_expr(place_id)?;
        let abi = self
            .lowerer
            .abis
            .get(&class)
            .cloned()
            .ok_or_else(|| {
                LowerIrError::Internal(format!("ABI for class {} not found", class.0))
            })?;

        let vptr_ptr_ty = self.lowerer.vptr_ptr_type();

        for vptr in &abi.vptrs {
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
                        let dest = self.fresh_var("vptr_addr", vptr_ptr_ty.clone());
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
                    }
                }
            } else {
                let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
                let byte_ptr = self.fresh_var("vptr_byte_ptr", byte_ptr_ty.clone());
                let cast_byte = Inst {
                    dest: Some(byte_ptr.clone()),
                    kind: InstKind::Cast {
                        ty: byte_ptr_ty.clone(),
                        e: place.clone(),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_byte);

                let adj_byte = self.fresh_var("vptr_adj_byte", byte_ptr_ty.clone());
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

                let dest = self.fresh_var("vptr_addr", vptr_ptr_ty.clone());
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
            let vtable_symbol = vtable.symbol.clone();
            let address_point = vtable.address_point;

            let vt_base_ty = self.lowerer.vtable_entry_type();
            let vt_base = self.fresh_var("vt_base", vt_base_ty.clone());
            let base_addr = Inst {
                dest: Some(vt_base.clone()),
                kind: InstKind::Cast {
                    ty: vt_base_ty.clone(),
                    e: Operand::SymAddr(vtable_symbol, vt_base_ty.clone()),
                },
                span: dummy_span(),
            };
            self.push_inst(base_addr);

            let vptr_val = if address_point == 0 {
                Operand::Var(vt_base)
            } else {
                let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
                let byte_ptr = self.fresh_var("vt_byte_ptr", byte_ptr_ty.clone());
                let cast_byte = Inst {
                    dest: Some(byte_ptr.clone()),
                    kind: InstKind::Cast {
                        ty: byte_ptr_ty.clone(),
                        e: Operand::Var(vt_base),
                    },
                    span: dummy_span(),
                };
                self.push_inst(cast_byte);

                let adj_byte = self.fresh_var("vt_adj_byte", byte_ptr_ty.clone());
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

                let dest = self.fresh_var("vptr_val", vptr_ptr_ty.clone());
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

            // 存储 vptr
            let store = Inst {
                dest: None,
                kind: InstKind::Store {
                    e: vptr_addr,
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
                    }
                };

                let member_offset = self.find_member_offset(owner_class, &member_name)?;
                let this = Operand::Var(self.ssa.params[0].clone());

                let member_ptr_ty =
                    self.lowerer
                        .lower_type(&CxxType::Pointer(Box::new(member_ty.clone())))?;

                let member_ptr = self.compute_member_ptr(
                    this,
                    member_offset,
                    &member_ptr_ty,
                )?;

                // 调用成员构造函数
                let class_sym = self.lowerer.get_symbol(member_class.as_symbol())?;
                let ctor_name = format!("{}_ctor_complete", class_sym.name);
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
            }
            _ => {
                // 标量成员: 空参数 → 零值初始化; 单参数 → 直接 Store;
                // 多参数 → 仍报错 (构造函数参数包展开尚未支持)
                let init_val = if args.is_empty() {
                    match &member_ty {
                        CxxType::Integer { .. } => Operand::Int(0),
                        CxxType::Float { bits } => {
                            let v = match bits { 32 => 0.0f32 as f64, _ => 0.0f64 };
                            Operand::Float(v, Type::Double)
                        }
                        CxxType::Bool => Operand::Int(0),
                        CxxType::Pointer(_) => Operand::Int(0),
                        _ => return Err(LowerIrError::Unsupported(format!(
                            "empty-arg member init for type: {:?}", member_ty
                        ))),
                    }
                } else if args.len() == 1 {
                    self.lower_expr(args[0])?
                } else {
                    return Err(LowerIrError::Unsupported(
                        "multi-arg non-class member init".to_string(),
                    ))
                };
                let this = Operand::Var(self.ssa.params[0].clone());
                let fn_sym = self.lowerer.get_symbol(self.func.symbol)?;
                let owner_class = match fn_sym.owner {
                    Some(owner_id) => ClassId::from(owner_id),
                    None => return Err(LowerIrError::Internal(
                        "constructor without owning class".to_string()
                    )),
                };
                let is_volatile = fn_sym.ty.is_volatile();
                drop(fn_sym);
                let obj_ty = self.lowerer.lower_type(&CxxType::Class(owner_class))?;
                let inst = Inst {
                    dest: None,
                    kind: InstKind::MemberStore {
                        obj: this,
                        obj_ty,
                        field: member_name,
                        src: init_val,
                        is_volatile,
                        is_arrow: true,
                    },
                    span: dummy_span(),
                };
                self.push_inst(inst);
            }
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
                LowerIrError::Internal(format!(
                    "ABI for base class {} not found",
                    base.base.0
                ))
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
                }
            };
            if base_ptr_ty == *target_ty {
                return Ok(base_ptr);
            }
            let dest = self.fresh_var("member_ptr", target_ty.clone());
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
        let byte_ptr = self.fresh_var("member_byte_ptr", byte_ptr_ty.clone());
        let cast_to_byte = Inst {
            dest: Some(byte_ptr.clone()),
            kind: InstKind::Cast {
                ty: byte_ptr_ty.clone(),
                e: base_ptr,
            },
            span: dummy_span(),
        };
        self.push_inst(cast_to_byte);

        let adj_byte = self.fresh_var("member_adj_byte", byte_ptr_ty.clone());
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

        let dest = self.fresh_var("member_ptr", target_ty.clone());
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
                return Err(LowerIrError::Internal(
                    "destructor without owning class".to_string(),
                ));
            }
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
                        let member_ptr_ty = self.lowerer.lower_type(
                            &CxxType::Pointer(Box::new(CxxType::Class(member_class))),
                        )?;
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
                }
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

                    let base_ptr_ty = self.lowerer.lower_type(&CxxType::Pointer(
                        Box::new(CxxType::Class(*base_class)),
                    ))?;

                    let offset = base_layout.offset;
                    let base_ptr =
                        self.compute_member_ptr(this.clone(), offset, &base_ptr_ty)?;

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
                }
                DtorStep::VirtualBase(_) => {
                    return Err(LowerIrError::Unsupported(
                        "virtual base destructor".to_string(),
                    ));
                }
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
        Ok(CxxType::Integer { bits: 32, signed: true })
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
    use crate::cpp::sema::{CxxSymbol, CxxSymbolKind, Sema, Access, CxxType};

    fn make_simple_add_hir() -> (HirTranslationUnit, Vec<CxxSymbol>) {
        let mut sema = Sema::new();

        // 添加函数 add
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Integer { bits: 32, signed: true }),
            params: vec![
                CxxType::Integer { bits: 32, signed: true },
                CxxType::Integer { bits: 32, signed: true },
            ],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym = sema.add_symbol("add".to_string(), CxxSymbolKind::Function, fn_ty, Access::Public);

        // 添加参数 a, b
        let a_sym = sema.add_symbol(
            "a".to_string(),
            CxxSymbolKind::Variable,
            CxxType::Integer { bits: 32, signed: true },
            Access::Public,
        );
        let b_sym = sema.add_symbol(
            "b".to_string(),
            CxxSymbolKind::Variable,
            CxxType::Integer { bits: 32, signed: true },
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
            ty: CxxType::Integer { bits: 32, signed: true },
        };
        // expr 1: Local(b)
        let expr_b = HirExpr {
            id: HirExprId(1),
            kind: HirExprKind::Local(b_sym),
            ty: CxxType::Integer { bits: 32, signed: true },
        };
        // expr 2: BinaryOp(Add, a, b)
        let expr_add = HirExpr {
            id: HirExprId(2),
            kind: HirExprKind::BinaryOp {
                op: BinaryOp::Add,
                lhs: HirExprId(0),
                rhs: HirExprId(1),
            },
            ty: CxxType::Integer { bits: 32, signed: true },
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
            ret_ty: CxxType::Integer { bits: 32, signed: true },
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
        assert!(func.blocks.iter().any(|(_, bb)| matches!(bb.term, Terminator::Ret { .. })));
    }

    #[test]
    fn lower_type_integer() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        assert_eq!(
            lowerer.lower_type(&CxxType::Integer { bits: 32, signed: true }).unwrap(),
            Type::Int
        );
        assert_eq!(
            lowerer.lower_type(&CxxType::Integer { bits: 32, signed: false }).unwrap(),
            Type::UInt
        );
        assert_eq!(
            lowerer.lower_type(&CxxType::Integer { bits: 64, signed: true }).unwrap(),
            Type::LongLong
        );
        assert_eq!(
            lowerer.lower_type(&CxxType::Integer { bits: 8, signed: true }).unwrap(),
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

        assert_eq!(
            lowerer.lower_type(&CxxType::Float { bits: 32 }).unwrap(),
            Type::Float
        );
        assert_eq!(
            lowerer.lower_type(&CxxType::Float { bits: 64 }).unwrap(),
            Type::Double
        );
    }

    #[test]
    fn lower_type_pointer() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        let ptr_ty = CxxType::Pointer(Box::new(CxxType::Integer { bits: 32, signed: true }));
        let result = lowerer.lower_type(&ptr_ty).unwrap();
        match result {
            Type::Ptr(inner) => {
                assert_eq!(inner.base, Type::Int);
            }
            _ => panic!("expected Ptr"),
        }
    }

    #[test]
    fn lower_type_reference() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        // 左值引用 → 指针
        let lref_ty = CxxType::LValueRef(Box::new(CxxType::Integer { bits: 32, signed: true }));
        let result = lowerer.lower_type(&lref_ty).unwrap();
        match result {
            Type::Ptr(inner) => {
                assert_eq!(inner.base, Type::Int);
            }
            _ => panic!("expected Ptr for LValueRef"),
        }

        // 右值引用 → 指针
        let rref_ty = CxxType::RValueRef(Box::new(CxxType::Integer { bits: 32, signed: true }));
        let result = lowerer.lower_type(&rref_ty).unwrap();
        match result {
            Type::Ptr(inner) => {
                assert_eq!(inner.base, Type::Int);
            }
            _ => panic!("expected Ptr for RValueRef"),
        }
    }

    #[test]
    fn lower_type_array() {
        let symbols: Vec<CxxSymbol> = Vec::new();
        let hir = HirTranslationUnit::new();
        let lowerer = IrLowerer::new(&hir, &symbols);

        let arr_ty = CxxType::Array(
            Box::new(CxxType::Integer { bits: 32, signed: true }),
            Some(10),
        );
        let result = lowerer.lower_type(&arr_ty).unwrap();
        match result {
            Type::Array(_, crate::ast::ArraySize::Const(n)) => {
                assert_eq!(n, 10);
            }
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
            Terminator::Ret { val: Some(_) } => {}
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
        let fn_sym = sema.add_symbol("test".to_string(), CxxSymbolKind::Function, fn_ty, Access::Public);

        // Save symbols BEFORE consuming sema
        let symbols = sema.take_symbols();

        let mut hir = sema.into_hir();

        // 构造一个不支持的表达式 (SizeOf)
        let expr = HirExpr {
            id: HirExprId(0),
            kind: HirExprKind::SizeOf(CxxType::Void),
            ty: CxxType::Integer { bits: 64, signed: false },
        };
        hir.exprs.push(expr);

        // 语句: return sizeof(void)
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
        // SizeOf 目前不支持，应该返回 Unsupported
        assert!(result.is_err());
        match result.unwrap_err() {
            LowerIrError::Unsupported(msg) => {
                assert!(msg.contains("SizeOf"));
            }
            _ => panic!("expected Unsupported error"),
        }
    }

    // === Stage 4: 非虚继承 lowering 测试 ===

    /// 创建一个简单的继承层次：Base { x: int }, Derived : Base { y: int }
    /// 返回 (hir, symbols, base_class_id, derived_class_id)
    fn make_inheritance_hierarchy(
    ) -> (HirTranslationUnit, Vec<CxxSymbol>, ClassId, ClassId) {
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
            CxxType::Integer { bits: 32, signed: true },
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
            CxxType::Integer { bits: 32, signed: true },
            Access::Public,
        );
        sema.set_owner(y_sym, derived_sym);

        // Save symbols before consuming sema into hir
        let symbols = sema.take_symbols();

        let hir = sema.into_hir();

        (hir, symbols, base_id, derived_id)
    }

    /// 注册 Base 和 Derived 的 ABI 描述符
    fn register_inheritance_abis(
        lowerer: &mut IrLowerer,
        base_id: ClassId,
        derived_id: ClassId,
    ) {
        use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, FieldLayout};

        // Base: { x: int @ 0 }, size=4, align=4
        let base_abi = ClassAbi {
            class_id: base_id,
            size: 4,
            align: 4,
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
        };
        lowerer.register_abi(base_id, base_abi);

        // Derived : Base { y: int }
        // Base@0 (primary), y@4, size=8, align=4
        let derived_abi = ClassAbi {
            class_id: derived_id,
            size: 8,
            align: 4,
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
            dtor_order: vec![
                DtorStep::Member("y".to_string()),
                DtorStep::Base(base_id),
            ],
            this_adjust_map: {
                let mut m = std::collections::HashMap::new();
                m.insert(base_id, crate::cpp::abi_layout::ThisAdjust::offset(0));
                m
            },
        };
        lowerer.register_abi(derived_id, derived_abi);
    }

    #[test]
    fn lower_class_type_flattens_base_fields() {
        let (hir, symbols, base_id, derived_id) = make_inheritance_hierarchy();
        let mut lowerer = IrLowerer::new(&hir, &symbols);
        register_inheritance_abis(&mut lowerer, base_id, derived_id);

        // Derived 的类型应该展平基类字段
        let derived_ty = lowerer
            .lower_type(&CxxType::Class(derived_id))
            .unwrap();

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
            }
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
            CxxType::Integer { bits: 32, signed: true },
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
            CxxType::Integer { bits: 32, signed: true },
            Access::Public,
        );
        sema.set_owner(c_field_sym, c_sym);

        // Save symbols BEFORE consuming sema
        let symbols = sema.take_symbols();

        let mut hir = sema.into_hir();

        // 测试：lower C* -> B* (non-primary base, offset=8)

        let mut lowerer = IrLowerer::new(&hir, &symbols);

        use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, FieldLayout};

        // A: { a: int @ 0 }, size=4, align=4
        let a_abi = ClassAbi {
            class_id: a_id,
            size: 4,
            align: 4,
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
        };
        lowerer.register_abi(a_id, a_abi);

        // B: { b: double @ 0 }, size=8, align=8
        let b_abi = ClassAbi {
            class_id: b_id,
            size: 8,
            align: 8,
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
        let has_add = entry.insts.iter().any(|inst| {
            matches!(inst.kind, InstKind::Bin { op: BinOp::Add, .. })
        });
        assert!(has_add, "non-primary base cast should have GEP add instruction");
    }

    /// 注册 A, B, C 三继承 ABI（辅助函数）
    fn register_abc_abis(
        lowerer: &mut IrLowerer,
        a_id: ClassId,
        b_id: ClassId,
        c_id: ClassId,
    ) {
        use crate::cpp::abi_layout::{BaseLayout, ClassAbi, DtorStep, FieldLayout};

        let a_abi = ClassAbi {
            class_id: a_id,
            size: 4,
            align: 4,
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
        };
        lowerer.register_abi(a_id, a_abi);

        let b_abi = ClassAbi {
            class_id: b_id,
            size: 8,
            align: 8,
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
        };
        lowerer.register_abi(b_id, b_abi);

        let c_abi = ClassAbi {
            class_id: c_id,
            size: 24,
            align: 8,
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
                InstKind::Call { callee, .. } => match callee {
                    Operand::FnAddr(name, _) => Some(name.as_str()),
                    _ => None,
                },
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
            ty: CxxType::Integer { bits: 32, signed: true },
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
                InstKind::Call { callee, .. } => match callee {
                    Operand::FnAddr(name, _) => Some(name.as_str()),
                    _ => None,
                },
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
            vec![("x".into(), CxxType::Integer { bits: 32, signed: true })],
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
        let method_sym = sema.add_symbol("foo".to_string(), CxxSymbolKind::Method, method_ty, Access::Public);
        sema.set_symbol_owner(method_sym, class_sym);

        // 测试函数 void test(Foo* obj) { obj->foo(); }
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::Pointer(Box::new(CxxType::Class(class_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym = sema.add_symbol("test_vcall".to_string(), CxxSymbolKind::Function, fn_ty, Access::Public);

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
        make_virtual_class_abi(
            &mut calc,
            class_id,
            &[("foo", true)],
            vec![],
            vec![],
        );
        let abi = calc.compute_class(class_id);

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(class_id, abi);
        assert!(lowerer.lower().is_ok(), "lowering should succeed");

        let out = lowerer.finish();
        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 检查是否有 Load (vptr 加载)
        let has_vptr_load = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Load { .. })
        });
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
        make_virtual_class_abi(
            &mut calc,
            class_id,
            &[("vfunc", true)],
            vec![],
            vec![],
        );
        let abi = calc.compute_class(class_id);
        
        for (i, v) in abi.vptrs.iter().enumerate() {
        }

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(class_id, abi);
        assert!(lowerer.lower().is_ok(), "lowering should succeed");

        let out = lowerer.finish();
        assert_eq!(out.functions.len(), 1);
        let func = &out.functions[0];
        let entry = &func.blocks[&func.entry];

        // 检查是否有 Store 指令（vptr 写入）
        let has_store = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Store { .. })
        });
        assert!(has_store, "SetVTablePtr should generate a store instruction for vptr");

        // vptr 应该指向 vtable 的 address point（不是 vtable 起点）
        // 因此应该有 Add 指令（vtable_base + address_point）
        let has_addr_point_add = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Bin { op: BinOp::Add, .. })
        });
        assert!(
            has_addr_point_add,
            "vptr should point to address point (vtable_base + offset)"
        );
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
        calc.register_class(a_id, vec![], vec![], vec![
            ("a".into(), CxxType::Integer { bits: 32, signed: true }),
        ], true);
        calc.register_method(a_id, "a_func".into(), true, a_id);

        // B
        calc.register_class(b_id, vec![], vec![], vec![
            ("b".into(), CxxType::Float { bits: 64 }),
        ], true);
        calc.register_method(b_id, "b_func".into(), true, b_id);

        // C : A, B
        calc.register_class(c_id, vec![a_id, b_id], vec![], vec![
            ("c".into(), CxxType::Integer { bits: 32, signed: true }),
        ], true);
        calc.register_method(c_id, "a_func".into(), true, a_id); // override A
        calc.register_method(c_id, "b_func".into(), true, b_id); // override B

        let abi_c = calc.compute_class(c_id);
        // C 的布局: A@0 (vptr+a+pad=16), B@16 (vptr+b=16), c@32, total=40?
        // 验证 B subobject offset = 16
        let b_offset = abi_c.bases.iter().find(|b| b.base == b_id).unwrap().offset;
        assert_eq!(b_offset, 16, "B subobject should be at offset 16");

        let mut sema = Sema::new();
        let _a_sym = sema.add_symbol("A".into(), CxxSymbolKind::Class, CxxType::Class(a_id), Access::Public);
        let _b_sym = sema.add_symbol("B".into(), CxxSymbolKind::Class, CxxType::Class(b_id), Access::Public);
        let _c_sym = sema.add_symbol("C".into(), CxxSymbolKind::Class, CxxType::Class(c_id), Access::Public);

        // b_func 方法（属于 B）
        let b_func_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::LValueRef(Box::new(CxxType::Class(b_id)))],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let b_func_sym = sema.add_symbol("b_func".into(), CxxSymbolKind::Method, b_func_ty, Access::Public);
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
        let fn_sym = sema.add_symbol("test_adjust".into(), CxxSymbolKind::Function, fn_ty, Access::Public);

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
        let has_add = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Bin { op: BinOp::Add, .. })
        });
        assert!(has_add, "virtual call with non-zero this_adjust should have pointer add (GEP)");

        // 应该有 vptr load
        let has_load = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Load { .. })
        });
        assert!(has_load, "should load vptr");

        // 应该有间接调用
        let has_indirect_call = entry.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Call { callee, .. } if matches!(callee, Operand::Var(_)))
        });
        assert!(has_indirect_call, "should have indirect call");

        // vtables: 应该有 C 的 primary vtable + secondary vtable for B
        // 还有 A 和 B 自己的 vtable
        let c_vtables: Vec<_> = out.vtables.iter()
            .filter(|(name, _)| name.contains("Class2"))
            .collect();
        assert!(c_vtables.len() >= 1, "C should have at least primary vtable");
    }
}
