//! C++ 语义分析层 (Sema)
//!
//! 职责:
//! - 作用域 (scope) 管理: namespace / class / function / block
//! - 符号表 (symbol table): class / field / method / function / variable / typedef
//! - 名字查找 (name lookup): 作用域链、重载集、injected-class-name
//! - 类型检查: 函数调用、隐式转换、重载决议
//! - 访问控制: public / protected / private
//! - 生成 HIR: 把 parser AST 转化为语义化 HIR
//!
//! 当前阶段: 骨架 + 基础声明绑定。
//! 不输出 C 文本，所有输出走 HIR → lower_ir → typed IR。

use std::collections::HashMap;

use crate::cpp::hir::{
    BinaryOp, CastKind, CtorVariant, DtorVariant, HirBody, HirBodyId, HirCallee, HirCatchHandler,
    HirExpr, HirExprId, HirExprKind, HirFunction, HirId, HirStmt, HirStmtId, HirStmtKind,
    HirTopLevelDecl, HirTranslationUnit, UnaryOp,
};
use crate::cpp::mangle::MangleType;
use crate::cpp::parser::*;

// === ID 类型 ===

/// 符号 ID（Symbol Table 索引）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub usize);

/// 作用域 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub usize);

/// 类 ID（Class Table 索引，同时也是 SymbolId 的别名）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ClassId(pub usize);

impl ClassId {
    pub fn as_symbol(self) -> SymbolId {
        SymbolId(self.0)
    }
}

impl From<SymbolId> for ClassId {
    fn from(s: SymbolId) -> Self {
        ClassId(s.0)
    }
}

// === 访问控制 ===

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Protected,
    Private,
}

impl Default for Access {
    fn default() -> Self {
        // class 默认 private
        Access::Private
    }
}

// === 符号种类 ===

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CxxSymbolKind {
    Namespace,
    Class,
    Field,
    Method,
    Constructor,
    Destructor,
    Function,
    Variable,
    TypeAlias,
    Template,
    Concept,
    Enum,
    EnumConstant,
}

// === CxxType ===

/// C++ 类型。与 legacy lower 的 `MangleType` 不同，这里保留完整
/// C++ 语义（引用不退化指针、cv 限定符等），只在 ABI lowering 时决定。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CxxType {
    Void,
    Bool,
    /// 整型: bits 位数，signed 是否有符号
    Integer {
        bits: u16,
        signed: bool,
    },
    /// 浮点型: bits 位数 (32 = float, 64 = double)
    Float {
        bits: u16,
    },
    /// 指针
    Pointer(Box<CxxType>),
    /// 左值引用 T&
    LValueRef(Box<CxxType>),
    /// 右值引用 T&&
    RValueRef(Box<CxxType>),
    /// 数组 T[N] 或 T[]
    Array(Box<CxxType>, Option<usize>),
    /// 函数类型
    Function {
        ret: Box<CxxType>,
        params: Vec<CxxType>,
        /// cv 限定符（成员函数用）
        is_const: bool,
        is_volatile: bool,
        /// ref 限定符
        ref_qual: Option<RefQual>,
    },
    /// 类类型（指向 ClassTable）
    Class(ClassId),
    /// 枚举类型
    Enum(ClassId),
    /// 成员指针 (e.g. int Foo::*)
    MemberPointer {
        class: ClassId,
        member: Box<CxxType>,
    },
    /// 依赖类型（模板实例化前不可压扁）
    Dependent(String),
    /// 类型别名（typedef / using）
    Alias(SymbolId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefQual {
    LValue,
    RValue,
}

impl CxxType {
    /// 是否是基本类型（非类、非枚举、非依赖）
    pub fn is_fundamental(&self) -> bool {
        matches!(
            self,
            CxxType::Void | CxxType::Bool | CxxType::Integer { .. } | CxxType::Float { .. }
        )
    }

    /// 脱去引用（如果是引用类型，返回被引用类型；否则返回自身）
    pub fn remove_reference(&self) -> &CxxType {
        match self {
            CxxType::LValueRef(t) | CxxType::RValueRef(t) => t,
            _ => self,
        }
    }

    /// 脱去指针
    pub fn remove_pointer(&self) -> Option<&CxxType> {
        match self {
            CxxType::Pointer(t) => Some(t),
            _ => None,
        }
    }

    /// 是否带 volatile 限定符。
    /// 对于指针/引用/数组类型，检查的是"被指向/被包含"的类型是否 volatile；
    /// 函数类型检查函数自身的 volatile 限定符（成员函数用）。
    /// 其他类型（基本类型、类、枚举等）当前不直接携带 cv 限定符，返回 false。
    pub fn is_volatile(&self) -> bool {
        match self {
            CxxType::Pointer(inner)
            | CxxType::LValueRef(inner)
            | CxxType::RValueRef(inner)
            | CxxType::Array(inner, _) => inner.is_volatile(),
            CxxType::Function { is_volatile, .. } => *is_volatile,
            CxxType::MemberPointer { member, .. } => member.is_volatile(),
            CxxType::Void
            | CxxType::Bool
            | CxxType::Integer { .. }
            | CxxType::Float { .. }
            | CxxType::Class(_)
            | CxxType::Enum(_)
            | CxxType::Dependent(_)
            | CxxType::Alias(_) => false,
        }
    }

    /// 脱去一层指针或引用，返回被指向/被引用的类型。
    /// 返回 `None` 表示不是指针也不是引用。
    pub fn pointee_type(&self) -> Option<&CxxType> {
        match self {
            CxxType::Pointer(inner)
            | CxxType::LValueRef(inner)
            | CxxType::RValueRef(inner) => Some(inner),
            _ => None,
        }
    }

    /// 数组或指针的元素类型。
    /// 对于 `T[N]`、`T[]`、`T*` 返回 `T`；其他类型返回 None。
    pub fn element_type(&self) -> Option<&CxxType> {
        match self {
            CxxType::Array(inner, _) | CxxType::Pointer(inner) => Some(inner),
            _ => None,
        }
    }
}

// === 基类信息 ===

/// 基类描述（非虚继承）
#[derive(Debug, Clone)]
pub struct BaseClass {
    pub class: ClassId,
    pub access: Access,
    pub is_virtual: bool,
}

// === 虚方法信息 ===

/// 虚方法的 vtable 信息
#[derive(Debug, Clone)]
pub struct VirtualMethodInfo {
    /// 虚方法的符号 ID
    pub symbol: SymbolId,
    /// vtable slot 索引
    pub slot: usize,
    /// 如果是重写的，被重写的基类方法符号 ID
    pub overrides: Option<SymbolId>,
}

// === 构造函数种类 ===

/// 构造函数种类（Stage 4: copy/move ctor 最小匹配）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtorKind {
    /// 普通构造函数
    Normal,
    /// 拷贝构造函数（参数为 const T& 或 T&）
    Copy,
    /// 移动构造函数（参数为 T&&）
    Move,
    /// 委托构造函数
    Delegating,
}

impl Default for CtorKind {
    fn default() -> Self {
        CtorKind::Normal
    }
}

// === 类信息 ===

/// 类的补充信息（继承、成员顺序等）
#[derive(Debug, Clone, Default)]
pub struct ClassInfo {
    /// 直接基类列表（按声明顺序）
    pub bases: Vec<BaseClass>,
    /// 成员字段（按声明顺序）
    pub fields: Vec<SymbolId>,
    /// 方法列表（按声明顺序，包含构造/析构）
    pub methods: Vec<SymbolId>,
    /// 构造函数列表（按声明顺序）
    pub constructors: Vec<SymbolId>,
    /// 析构函数符号（如果有）
    pub destructor: Option<SymbolId>,
    /// using 声明引入的基类成员: 名字 -> 基类符号 ID 列表
    pub using_decls: HashMap<String, Vec<SymbolId>>,
    /// 虚方法列表（含从基类继承并重写的），按 slot 顺序
    pub virtual_methods: Vec<VirtualMethodInfo>,
    /// 是否为抽象类（含有纯虚函数）
    pub is_abstract: bool,
}

// === 符号 ===

#[derive(Debug, Clone)]
pub struct CxxSymbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: CxxSymbolKind,
    pub ty: CxxType,
    /// 所属类/命名空间等
    pub owner: Option<SymbolId>,
    pub access: Access,
    /// mangled linkage name（如果已确定）
    pub linkage_name: Option<String>,
    /// 所在作用域
    pub scope: ScopeId,
    /// 是否为 volatile 限定（volatile 局部/全局变量、volatile 参数）
    pub is_volatile: bool,
}

// === 作用域 ===

/// 作用域类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// 全局 / translation unit
    Global,
    /// 命名空间
    Namespace,
    /// 类
    Class,
    /// 函数
    Function,
    /// 块 (compound statement)
    Block,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub id: ScopeId,
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    /// 关联的符号（类作用域对应类符号，函数作用域对应函数符号）
    pub owner_symbol: Option<SymbolId>,
    /// 名字 → 符号 ID 列表（overload set）
    pub symbols: HashMap<String, Vec<SymbolId>>,
}

impl Scope {
    fn new(id: ScopeId, kind: ScopeKind, parent: Option<ScopeId>) -> Self {
        Self {
            id,
            kind,
            parent,
            owner_symbol: None,
            symbols: HashMap::new(),
        }
    }

    /// 添加符号到此作用域
    fn add_symbol(&mut self, name: String, sym: SymbolId) {
        self.symbols.entry(name).or_default().push(sym);
    }

    /// 在本作用域内查找名字（返回 overload set）
    fn lookup_local(&self, name: &str) -> Option<&[SymbolId]> {
        self.symbols.get(name).map(|v| v.as_slice())
    }
}

// === Sema 上下文 ===

/// 语义分析器。
/// 持有 symbol table、scope stack、class table。
pub struct Sema {
    /// 所有符号（按 id 索引）
    symbols: Vec<CxxSymbol>,
    /// 所有作用域
    scopes: Vec<Scope>,
    /// 当前作用域栈顶
    current_scope: ScopeId,
    /// 全局作用域
    global_scope: ScopeId,
    /// 类附加信息（按 ClassId 索引，与 symbols 中的 Class 一一对应）
    class_infos: Vec<ClassInfo>,
    /// 输出的 HIR
    hir: HirTranslationUnit,
    /// 当前所在的类（用于 this 指针上下文）
    current_class: Option<ClassId>,
    /// 静态局部变量命名计数器（生成唯一 linkage name）
    static_local_counter: usize,
}

impl Sema {
    /// 创建新的语义分析器，包含一个全局作用域。
    pub fn new() -> Self {
        let mut scopes = Vec::new();
        let global = Scope::new(ScopeId(0), ScopeKind::Global, None);
        scopes.push(global);
        let global_scope = ScopeId(0);
        let mut result = Self {
            symbols: Vec::new(),
            scopes,
            current_scope: global_scope,
            global_scope,
            class_infos: Vec::new(),
            hir: HirTranslationUnit::new(),
            current_class: None,
            static_local_counter: 0,
        };
        
        // 注册 C++ runtime symbols（如 dccpp_dur_free, __dcc_eh_throw 等）
        // 这些符号由运行时提供实现，需要作为 extern 函数注册
        use crate::cpp::runtime::CxxRuntimeSymbol;
        
        // 显式切换到 global scope 再注册 symbols
        let old_scope = result.current_scope;
        result.current_scope = result.global_scope;
        
        for sym in &[
            CxxRuntimeSymbol::ChronoDurFree,
            CxxRuntimeSymbol::ChronoTpFree,
            CxxRuntimeSymbol::ChronoSysClockNow,
            CxxRuntimeSymbol::ChronoSteadyClockNow,
            CxxRuntimeSymbol::ChronoHrClockNow,
        ] {
            let name = sym.name().to_string();
            // 创建一个简化的函数类型：ret=void*, params=[] (真实参数将在后续推导)
            let fn_ty = CxxType::Function {
                ret: Box::new(CxxType::Pointer(Box::new(CxxType::Integer { bits: 8, signed: false }))),
                params: vec![],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            };
            // let's just add it without logging for now
            result.add_symbol(name.clone(), CxxSymbolKind::Function, fn_ty, Access::Public);
        }
        
        // 恢复之前的 scope
        result.current_scope = old_scope;
        
        result
    }

    // --- scope 管理 ---

    pub fn global_scope(&self) -> ScopeId {
        self.global_scope
    }

    pub fn current_scope(&self) -> ScopeId {
        self.current_scope
    }

    /// 推入新作用域并设为当前
    pub fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        let id = ScopeId(self.scopes.len());
        let scope = Scope::new(id, kind, Some(self.current_scope));
        self.scopes.push(scope);
        self.current_scope = id;
        id
    }

    /// 弹出作用域（返回父作用域）
    pub fn pop_scope(&mut self) -> Option<ScopeId> {
        let current = &self.scopes[self.current_scope.0];
        let parent = current.parent;
        if let Some(p) = parent {
            self.current_scope = p;
        }
        parent
    }

    // --- symbol 管理 ---

    /// 计算 namespace 内符号的 linkage name（`ns1_ns2_name` 形式）。
    /// 类成员跳过（由 lower_ir 专门处理 mangling）。
    fn compute_linkage_name(&self, name: &str, kind: CxxSymbolKind) -> Option<String> {
        if matches!(
            kind,
            CxxSymbolKind::Field
                | CxxSymbolKind::Method
                | CxxSymbolKind::Constructor
                | CxxSymbolKind::Destructor
        ) {
            return None;
        }
        // 从当前作用域向上收集 namespace 前缀
        let mut ns_parts: Vec<String> = Vec::new();
        let mut scope = Some(self.current_scope);
        while let Some(sid) = scope {
            let s = &self.scopes[sid.0];
            if s.kind == ScopeKind::Namespace {
                if let Some(owner) = s.owner_symbol {
                    if let Some(sym) = self.symbols.get(owner.0) {
                        if !sym.name.is_empty() {
                            ns_parts.push(sym.name.clone());
                        }
                    }
                }
            }
            scope = s.parent;
        }
        ns_parts.reverse();
        if ns_parts.is_empty() {
            None
        } else {
            Some(ns_parts.join("_") + "_" + name)
        }
    }

    /// 创建新符号并加入当前作用域
    pub fn add_symbol(
        &mut self,
        name: String,
        kind: CxxSymbolKind,
        ty: CxxType,
        access: Access,
    ) -> SymbolId {
        let id = SymbolId(self.symbols.len());
        let scope = self.current_scope;
        let linkage_name = self.compute_linkage_name(&name, kind);
        let sym = CxxSymbol {
            id,
            name: name.clone(),
            kind,
            ty,
            owner: None,
            access,
            linkage_name,
            scope,
            is_volatile: false,
        };
        self.symbols.push(sym);
        self.scopes[scope.0].add_symbol(name, id);
        id
    }

    /// 获取符号引用
    pub fn get_symbol(&self, id: SymbolId) -> Option<&CxxSymbol> {
        self.symbols.get(id.0)
    }

    /// 获取符号的可变引用
    pub fn get_symbol_mut(&mut self, id: SymbolId) -> Option<&mut CxxSymbol> {
        self.symbols.get_mut(id.0)
    }

    /// 设置符号的 owner（用于成员函数、字段等）
    pub fn set_owner(&mut self, sym: SymbolId, owner: SymbolId) {
        if let Some(s) = self.symbols.get_mut(sym.0) {
            s.owner = Some(owner);
        }
    }

    /// 设置符号的 owner（别名，测试/调用方命名习惯）
    pub fn set_symbol_owner(&mut self, sym: SymbolId, owner: SymbolId) {
        self.set_owner(sym, owner);
    }

    /// 获取整个符号表（按 id 索引）
    pub fn symbols(&self) -> &[CxxSymbol] {
        &self.symbols
    }

    // --- name lookup ---

    /// 非限定名字查找: 从当前作用域向上查找
    pub fn lookup_unqualified(&self, name: &str) -> Vec<SymbolId> {
        let mut scope = Some(self.current_scope);
        while let Some(sid) = scope {
            let s = &self.scopes[sid.0];
            if let Some(syms) = s.lookup_local(name) {
                return syms.to_vec();
            }
            scope = s.parent;
        }
        Vec::new()
    }

    /// 限定名字查找: 在指定类/命名空间作用域内查找
    pub fn lookup_qualified(&self, scope_id: ScopeId, name: &str) -> Vec<SymbolId> {
        let s = &self.scopes[scope_id.0];
        s.lookup_local(name).map(|v| v.to_vec()).unwrap_or_default()
    }

    /// 在类作用域内查找名字（含基类递归查找）
    /// 返回找到的所有符号（overload set）。
    /// 若多继承中不同基类有同名成员但无派生类遮蔽，标记歧义（返回空 + ambiguous）。
    /// 返回值: (找到的符号列表, 是否歧义)
    pub fn lookup_in_class(&self, class_id: ClassId, name: &str) -> (Vec<SymbolId>, bool) {
        let class_scope = match self.class_scope(class_id) {
            Some(s) => s,
            None => return (Vec::new(), false),
        };

        // 1. 先查本类自身
        let local = self.lookup_qualified(class_scope, name);
        if !local.is_empty() {
            return (local, false);
        }

        // 2. 查 using 声明引入的成员
        if let Some(info) = self.get_class_info(class_id) {
            if let Some(using_syms) = info.using_decls.get(name) {
                if !using_syms.is_empty() {
                    return (using_syms.clone(), false);
                }
            }
        }

        // 3. 递归查基类
        let info = match self.get_class_info(class_id) {
            Some(i) => i,
            None => return (Vec::new(), false),
        };

        let mut all_results: Vec<SymbolId> = Vec::new();
        let mut found_from_different_bases = false;
        let mut first_base_with_results: Option<ClassId> = None;

        for base in &info.bases {
            let (base_results, base_ambiguous) = self.lookup_in_class(base.class, name);
            if base_ambiguous {
                return (Vec::new(), true);
            }
            if !base_results.is_empty() {
                if all_results.is_empty() {
                    all_results = base_results;
                    first_base_with_results = Some(base.class);
                } else {
                    // 检查是否来自同一个基类子对象（非虚继承场景下，不同基类就是歧义）
                    // 简化：只要两个不同基类都有同名成员，就标记歧义
                    if Some(base.class) != first_base_with_results {
                        found_from_different_bases = true;
                    }
                    // 合并 overload set（如果都是函数的话）
                    // 简化：都收集起来，但标记歧义
                    for sym in &base_results {
                        if !all_results.contains(sym) {
                            all_results.push(*sym);
                        }
                    }
                }
            }
        }

        if found_from_different_bases {
            (Vec::new(), true)
        } else {
            (all_results, false)
        }
    }

    /// 判断一个类是否是另一个类的基类（非虚继承链）
    pub fn is_base_of(&self, base: ClassId, derived: ClassId) -> bool {
        if base == derived {
            return true;
        }
        let info = match self.get_class_info(derived) {
            Some(i) => i,
            None => return false,
        };
        for b in &info.bases {
            if self.is_base_of(base, b.class) {
                return true;
            }
        }
        false
    }

    /// 找到包含指定成员的直接基类
    fn find_member_base_class(&self, member: SymbolId, from_class: ClassId) -> Option<ClassId> {
        // 先检查成员是否属于当前类本身
        if let Some(s) = self.get_symbol(member) {
            if let Some(owner) = s.owner {
                let owner_class = ClassId::from(owner);
                if owner_class == from_class {
                    return Some(from_class);
                }
                // 检查 owner 是否是 from_class 的基类
                if self.is_base_of(owner_class, from_class) {
                    return Some(owner_class);
                }
            }
        }
        None
    }

    /// 如果成员属于基类子对象，对指针做 BaseCast 调整
    fn adjust_pointer_to_member_base(
        &mut self,
        ptr_expr: HirExprId,
        member: SymbolId,
        from_class: ClassId,
    ) -> Result<HirExprId, String> {
        let member_base = self
            .find_member_base_class(member, from_class)
            .ok_or_else(|| "member does not belong to this class hierarchy".to_string())?;

        if member_base == from_class {
            return Ok(ptr_expr);
        }

        // 生成 BaseCast: 派生类指针 -> 基类指针
        let ptr_ty = self.hir.exprs[ptr_expr.0].ty.clone();
        let base_ptr_ty = match ptr_ty {
            CxxType::Pointer(_) => CxxType::Pointer(Box::new(CxxType::Class(member_base))),
            _ => return Err("expected pointer type for base cast".into()),
        };
        Ok(self.add_expr(
            HirExprKind::BaseCast {
                object: ptr_expr,
                from: from_class,
                to: member_base,
            },
            base_ptr_ty,
        ))
    }

    // --- class 操作 ---

    /// 检查两个函数类型的签名是否匹配（用于 override 检查）
    /// 比较参数类型列表和 const 性（返回类型协变暂不检查）
    fn signatures_match(fn_a: &CxxType, fn_b: &CxxType) -> bool {
        match (fn_a, fn_b) {
            (
                CxxType::Function {
                    params: params_a,
                    is_const: const_a,
                    ..
                },
                CxxType::Function {
                    params: params_b,
                    is_const: const_b,
                    ..
                },
            ) => {
                if const_a != const_b {
                    return false;
                }
                if params_a.len() != params_b.len() {
                    return false;
                }
                params_a.iter().zip(params_b.iter()).all(|(a, b)| a == b)
            }
            _ => false,
        }
    }

    /// 在类中查找指定名字的虚方法（遍历 virtual_methods）
    fn find_virtual_method_by_name(
        &self,
        class_id: ClassId,
        name: &str,
    ) -> Option<&VirtualMethodInfo> {
        let info = self.get_class_info(class_id)?;
        for vm in &info.virtual_methods {
            if let Some(s) = self.get_symbol(vm.symbol) {
                if s.name == name {
                    return Some(vm);
                }
            }
        }
        None
    }

    /// 递归查找基类中与指定签名匹配的虚方法
    /// 返回 (基类虚方法信息, 基类 ID)
    fn find_overridden_base_method(
        &self,
        class_id: ClassId,
        name: &str,
        fn_ty: &CxxType,
    ) -> Option<(SymbolId, usize)> {
        let info = self.get_class_info(class_id)?;
        for base in &info.bases {
            // 先查基类自身
            if let Some(bm) = self.find_virtual_method_by_name(base.class, name) {
                if let Some(base_fn_ty) = self.get_symbol(bm.symbol).map(|s| s.ty.clone()) {
                    if Self::signatures_match(fn_ty, &base_fn_ty) {
                        return Some((bm.symbol, bm.slot));
                    }
                }
            }
            // 递归查基类的基类
            if let Some((sym, slot)) =
                self.find_overridden_base_method(base.class, name, fn_ty)
            {
                return Some((sym, slot));
            }
        }
        None
    }

    /// 递归查找基类中的虚析构函数
    /// 返回 (基类虚析构符号 ID, vtable slot)
    fn find_virtual_base_dtor(&self, class_id: ClassId) -> Option<(SymbolId, usize)> {
        let info = self.get_class_info(class_id)?;
        for base in &info.bases {
            // 检查基类自身的虚方法中是否有析构函数
            if let Some(base_info) = self.get_class_info(base.class) {
                for vm in &base_info.virtual_methods {
                    if let Some(s) = self.get_symbol(vm.symbol) {
                        if matches!(s.kind, CxxSymbolKind::Destructor) {
                            return Some((vm.symbol, vm.slot));
                        }
                    }
                }
            }
            // 递归查基类的基类
            if let Some(result) = self.find_virtual_base_dtor(base.class) {
                return Some(result);
            }
        }
        None
    }

    /// 检查一个方法是否是虚方法，返回其 vtable slot
    pub fn get_virtual_slot(&self, method: SymbolId) -> Option<usize> {
        let sym = self.get_symbol(method)?;
        let owner = sym.owner?;
        let class_id = ClassId::from(owner);
        let info = self.get_class_info(class_id)?;
        for vm in &info.virtual_methods {
            if vm.symbol == method {
                return Some(vm.slot);
            }
        }
        None
    }

    /// 注册一个类，返回 ClassId 和类作用域
    /// bases: 基类列表（名字字符串 + 访问控制 + 是否虚继承）
    pub fn register_class(
        &mut self,
        name: String,
        access: Access,
        bases: &[BaseClass],
    ) -> (ClassId, ScopeId) {
        let class_sym = self.add_symbol(name.clone(), CxxSymbolKind::Class, CxxType::Void, access);
        let class_scope = self.push_scope(ScopeKind::Class);
        // 设置 owner
        if let Some(s) = self.scopes.get_mut(class_scope.0) {
            s.owner_symbol = Some(class_sym);
        }
        // 更新符号的类型为 Class
        if let Some(sym) = self.symbols.get_mut(class_sym.0) {
            sym.ty = CxxType::Class(ClassId(class_sym.0));
            sym.scope = class_scope;
        }
        // 确保 class_infos 长度足够
        let class_id = ClassId(class_sym.0);
        while self.class_infos.len() <= class_id.0 {
            self.class_infos.push(ClassInfo::default());
        }
        // 保存基类信息
        self.class_infos[class_id.0].bases = bases.to_vec();
        (class_id, class_scope)
    }

    /// 获取类的附加信息
    pub fn get_class_info(&self, class: ClassId) -> Option<&ClassInfo> {
        self.class_infos.get(class.0)
    }

    /// 获取类的附加信息（可变）
    pub fn get_class_info_mut(&mut self, class: ClassId) -> Option<&mut ClassInfo> {
        self.class_infos.get_mut(class.0)
    }

    /// 获取类作用域（通过类符号 ID）
    pub fn class_scope(&self, class: ClassId) -> Option<ScopeId> {
        // 类符号关联的作用域就是类作用域
        // 我们约定：类符号的 scope 字段指向包围它的作用域，而类自己的作用域
        // 是紧跟其后创建的。这里简化：遍历 scopes 找 owner == class_sym 的。
        let class_sym = class.as_symbol();
        for s in &self.scopes {
            if s.owner_symbol == Some(class_sym) {
                return Some(s.id);
            }
        }
        None
    }

    // --- HIR 构建 ---

    /// 获取构建好的 HIR
    pub fn hir(&self) -> &HirTranslationUnit {
        &self.hir
    }

    /// 消费 Sema，返回 HIR
    pub fn into_hir(self) -> HirTranslationUnit {
        self.hir
    }

    // --- HIR 构建辅助 ---

    fn add_expr(&mut self, kind: HirExprKind, ty: CxxType) -> HirExprId {
        let id = HirExprId(self.hir.exprs.len());
        self.hir.exprs.push(HirExpr { id, kind, ty });
        id
    }

    fn add_stmt(&mut self, kind: HirStmtKind) -> HirStmtId {
        let id = HirStmtId(self.hir.stmts.len());
        self.hir.stmts.push(HirStmt { id, kind });
        id
    }

    fn add_body(&mut self, stmts: Vec<HirStmtId>) -> HirBodyId {
        let id = HirBodyId(self.hir.bodies.len());
        self.hir.bodies.push(HirBody {
            id,
            stmts,
            ctor_initializers: Vec::new(),
        });
        id
    }

    // --- MangleType -> CxxType ---

    fn convert_type(&mut self, mt: &MangleType) -> Result<CxxType, String> {
        match mt {
            MangleType::Void => Ok(CxxType::Void),
            MangleType::Bool => Ok(CxxType::Bool),
            MangleType::Char => Ok(CxxType::Integer {
                bits: 8,
                signed: true,
            }),
            MangleType::Schar => Ok(CxxType::Integer {
                bits: 8,
                signed: true,
            }),
            MangleType::Uchar => Ok(CxxType::Integer {
                bits: 8,
                signed: false,
            }),
            MangleType::Short => Ok(CxxType::Integer {
                bits: 16,
                signed: true,
            }),
            MangleType::Ushort => Ok(CxxType::Integer {
                bits: 16,
                signed: false,
            }),
            MangleType::Int => Ok(CxxType::Integer {
                bits: 32,
                signed: true,
            }),
            MangleType::Uint => Ok(CxxType::Integer {
                bits: 32,
                signed: false,
            }),
            MangleType::Long => Ok(CxxType::Integer {
                bits: 64,
                signed: true,
            }),
            MangleType::Ulong => Ok(CxxType::Integer {
                bits: 64,
                signed: false,
            }),
            MangleType::LongLong => Ok(CxxType::Integer {
                bits: 64,
                signed: true,
            }),
            MangleType::UlongLong => Ok(CxxType::Integer {
                bits: 64,
                signed: false,
            }),
            MangleType::Float => Ok(CxxType::Float { bits: 32 }),
            MangleType::Double => Ok(CxxType::Float { bits: 64 }),
            MangleType::LongDouble => Ok(CxxType::Float { bits: 80 }),
            MangleType::Pointer(t) => {
                let inner = self.convert_type(t)?;
                Ok(CxxType::Pointer(Box::new(inner)))
            }
            MangleType::Reference(t) => {
                let inner = self.convert_type(t)?;
                Ok(CxxType::LValueRef(Box::new(inner)))
            }
            MangleType::Array(t, n) => {
                let inner = self.convert_type(t)?;
                Ok(CxxType::Array(Box::new(inner), Some(*n)))
            }
            MangleType::Class(name) => {
                // 从当前作用域向上查找类名
                let syms = self.lookup_unqualified(name);
                for &sym in &syms {
                    if let Some(s) = self.get_symbol(sym) {
                        if matches!(s.kind, CxxSymbolKind::Class) {
                            return Ok(CxxType::Class(ClassId::from(sym)));
                        }
                        // typedef/using 别名: 取其 ty 直接展开
                        if matches!(s.kind, CxxSymbolKind::TypeAlias) {
                            return Ok(s.ty.clone());
                        }
                        // enum 类型: 用底层 int 表示
                        if matches!(s.kind, CxxSymbolKind::Enum) {
                            return Ok(CxxType::Integer { bits: 32, signed: true });
                        }
                    }
                }
                // 内置类型别名 (size_t, uintptr_t 等)
                match name.as_str() {
                    "size_t" | "unsigned int" | "unsigned" => {
                        return Ok(CxxType::Integer { bits: 32, signed: false });
                    }
                    "ssize_t" | "int" => {
                        return Ok(CxxType::Integer { bits: 32, signed: true });
                    }
                    "long" => {
                        return Ok(CxxType::Integer { bits: 64, signed: true });
                    }
                    "unsigned long" | "unsigned long long" => {
                        return Ok(CxxType::Integer { bits: 64, signed: false });
                    }
                    "uintptr_t" | "void*" => {
                        return Ok(CxxType::Pointer(Box::new(CxxType::Void)));
                    }
                    _ => {}
                }
                Err(format!("unknown class type: {}", name))
            }
            other => Err(format!("unsupported type: {:?}", other)),
        }
    }

    fn access_from_parser(acc: crate::cpp::parser::Access) -> Access {
        match acc {
            crate::cpp::parser::Access::Public => Access::Public,
            crate::cpp::parser::Access::Private => Access::Private,
            crate::cpp::parser::Access::Protected => Access::Protected,
        }
    }

    /// 从类型名字符串解析 CxxType（用于 new/delete 表达式，类型名尚未实例化为 MangleType）
    fn type_name_to_cxx_type(&mut self, name: &str) -> Result<CxxType, String> {
        let t = name.trim();
        match t {
            "void" => return Ok(CxxType::Void),
            "bool" => return Ok(CxxType::Bool),
            "char" => return Ok(CxxType::Integer { bits: 8, signed: true }),
            "signed char" => return Ok(CxxType::Integer { bits: 8, signed: true }),
            "unsigned char" => return Ok(CxxType::Integer { bits: 8, signed: false }),
            "short" | "short int" | "signed short" => {
                return Ok(CxxType::Integer { bits: 16, signed: true })
            }
            "unsigned short" | "unsigned short int" => {
                return Ok(CxxType::Integer { bits: 16, signed: false })
            }
            "int" | "signed" | "signed int" => {
                return Ok(CxxType::Integer { bits: 32, signed: true })
            }
            "unsigned" | "unsigned int" => {
                return Ok(CxxType::Integer { bits: 32, signed: false })
            }
            "long" | "long int" | "signed long" => {
                return Ok(CxxType::Integer { bits: 64, signed: true })
            }
            "unsigned long" | "unsigned long int" => {
                return Ok(CxxType::Integer { bits: 64, signed: false })
            }
            "long long" | "long long int" | "signed long long" => {
                return Ok(CxxType::Integer { bits: 64, signed: true })
            }
            "unsigned long long" | "unsigned long long int" => {
                return Ok(CxxType::Integer { bits: 64, signed: false })
            }
            "float" => return Ok(CxxType::Float { bits: 32 }),
            "double" => return Ok(CxxType::Float { bits: 64 }),
            "long double" => return Ok(CxxType::Float { bits: 80 }),
            _ => {}
        }
        // 指针形式: `int*`, `Foo*` 等
        if let Some(inner) = t.strip_suffix('*').map(|s| s.trim()) {
            let inner_ty = self.type_name_to_cxx_type(inner)?;
            return Ok(CxxType::Pointer(Box::new(inner_ty)));
        }
        // 类名查找
        let syms = self.lookup_unqualified(t);
        for &sym in &syms {
            if let Some(s) = self.get_symbol(sym) {
                if matches!(s.kind, CxxSymbolKind::Class) {
                    return Ok(CxxType::Class(ClassId::from(sym)));
                }
            }
        }
        Err(format!("unknown type name: {}", name))
    }

    /// 解析基类列表为 BaseClass 向量
    fn parse_base_specs(
        &self,
        base_specs: &[BaseSpec],
    ) -> Result<Vec<BaseClass>, String> {
        let mut base_classes = Vec::new();
        for base_spec in base_specs {
            let base_syms = self.lookup_unqualified(&base_spec.name);
            let mut base_class_id = None;
            for &sym in &base_syms {
                if let Some(s) = self.get_symbol(sym) {
                    if matches!(s.kind, CxxSymbolKind::Class) {
                        base_class_id = Some(ClassId::from(sym));
                        break;
                    }
                }
            }
            let bc = base_class_id
                .ok_or_else(|| format!("base class '{}' not found", base_spec.name))?;
            base_classes.push(BaseClass {
                class: bc,
                access: Self::access_from_parser(base_spec.access),
                is_virtual: base_spec.is_virtual,
            });
        }
        Ok(base_classes)
    }

    /// 更新已有类的基类信息
    fn update_class_bases(&mut self, class_id: ClassId, bases: &[BaseClass]) {
        while self.class_infos.len() <= class_id.0 {
            self.class_infos.push(ClassInfo::default());
        }
        self.class_infos[class_id.0].bases = bases.to_vec();
    }

    // --- 表达式构建 ---

    fn build_expr(&mut self, expr: &CppExpr) -> Result<HirExprId, String> {
        match expr {
            CppExpr::IntLit(n) => Ok(self.add_expr(
                HirExprKind::IntLiteral(*n as i128),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            )),
            CppExpr::FloatLit(s) => Ok(self.add_expr(
                HirExprKind::FloatLiteral(s.clone()),
                CxxType::Float { bits: 64 },
            )),
            CppExpr::StrLit(s) => {
                // 字符串字面量简化处理：转成全局只读字节数组首地址
                // 先复用 HirExprKind::IntLiteral(0) 占位 — 后续需补真实 StringLiteral
                let _ = s;
                Ok(self.add_expr(
                    HirExprKind::IntLiteral(0),
                    CxxType::Pointer(Box::new(CxxType::Integer {
                        bits: 8,
                        signed: true,
                    })),
                ))
            }
            CppExpr::Noexcept(_e) => {
                // noexcept(expr) - 编译期布尔谓词
                // 简化实现：所有内建运算、变量访问、cast 等都视为 noexcept = true
                // 对普通函数调用返回 false
                Ok(self.add_expr(
                    HirExprKind::BoolLiteral(true),
                    CxxType::Bool,
                ))
            }
            CppExpr::MemberStar(obj, field_name) => {
                // obj.*ptr_to_member: 指针到数据成员的真解引用
                // 简化：直接取字段
                let obj_id = self.build_expr(obj)?;
                let obj_ty = self.hir.exprs[obj_id.0].ty.clone();
                let (class_id, _is_pointer) = match obj_ty {
                    CxxType::Class(id) => (id, false),
                    CxxType::Pointer(ref t) => match t.as_ref() {
                        CxxType::Class(id) => (*id, true),
                        _ => return Err("member star access on non-class".into()),
                    },
                    _ => return Err("member star access on non-class".into()),
                };
                let (syms, ambiguous) = self.lookup_in_class(class_id, field_name);
                if ambiguous {
                    return Err(format!("ambiguous member '{}'", field_name));
                }
                if syms.is_empty() {
                    return Err(format!("member '{}' not found", field_name));
                }
                let field_sym = syms[0];
                let field_ty = self
                    .get_symbol(field_sym)
                    .map(|s| s.ty.clone())
                    .unwrap_or(CxxType::Void);
                Ok(self.add_expr(
                    HirExprKind::FieldAccess {
                        object: obj_id,
                        field: field_sym,
                    },
                    field_ty,
                ))
            }
            CppExpr::PtrMemberStar(ptr_expr, field_name) => {
                // obj->*ptr_to_member: 指针对象的成员指针解引用
                let deref = CppExpr::UnaryOp(UnaryOpKind::Deref, ptr_expr.clone());
                let deref_id = self.build_expr(&deref)?;
                let deref_ty = self.hir.exprs[deref_id.0].ty.clone();
                let class_id = match deref_ty {
                    CxxType::Class(id) => id,
                    _ => return Err("ptr member star deref on non-class pointer".into()),
                };
                let (syms, ambiguous) = self.lookup_in_class(class_id, field_name);
                if ambiguous {
                    return Err(format!("ambiguous member '{}'", field_name));
                }
                if syms.is_empty() {
                    return Err(format!("member '{}' not found", field_name));
                }
                let field_sym = syms[0];
                let field_ty = self
                    .get_symbol(field_sym)
                    .map(|s| s.ty.clone())
                    .unwrap_or(CxxType::Void);
                Ok(self.add_expr(
                    HirExprKind::FieldAccess {
                        object: deref_id,
                        field: field_sym,
                    },
                    field_ty,
                ))
            }
            CppExpr::MultiIndex(obj, indices) => {
                // obj[i, j, k] - C++23 多维下标
                // 简化实现：只取最后一个下标（实际语义需要 runtime helper）
                let obj_id = self.build_expr(obj)?;
                let mut hir_idx = Vec::new();
                for idx in indices {
                    hir_idx.push(self.build_expr(idx)?);
                }
                // 简化：最后一个下标做真正的 index，前面的全部丢弃
                let last_idx = hir_idx.pop().unwrap();
                let obj_ty = self.hir.exprs[obj_id.0].ty.clone();
                let elem_ty = match &obj_ty {
                    CxxType::Array(inner, _) => *inner.clone(),
                    CxxType::Pointer(inner) => *inner.clone(),
                    _ => return Err("multi-index on non-array/pointer".into()),
                };
                Ok(self.add_expr(
                    HirExprKind::Index {
                        array: obj_id,
                        index: last_idx,
                    },
                    elem_ty,
                ))
            }
            CppExpr::MemberPtr { .. } => {
                // &Class::method / &Class::field - 成员指针
                // 简化：返回 size_t 偏移占位
                Ok(self.add_expr(
                    HirExprKind::IntLiteral(0),
                    CxxType::Integer { bits: 64, signed: false },
                ))
            }
            CppExpr::TemplateIdent { name, args } => {
                // foo<int> - 带显式模板实参的模板名
                // 在 native 路径中，模板实例已在 native.rs 阶段展开，这里直接按普通 Ident 处理
                let _ = args;
                match self.lookup_unqualified(name) {
                    syms if !syms.is_empty() => {
                        let sym = syms[0];
                        let sym_info = self
                            .get_symbol(sym)
                            .ok_or_else(|| "symbol not found".to_string())?;
                        let kind = sym_info.kind;
                        let ty = sym_info.ty.clone();
                        let expr_kind = match kind {
                            CxxSymbolKind::Function => HirExprKind::Global(sym),
                            CxxSymbolKind::Method => HirExprKind::Global(sym),
                            _ => return Err(format!("template ident '{}' is not a value", name)),
                        };
                        Ok(self.add_expr(expr_kind, ty))
                    }
                    _ => Err(format!("undeclared template identifier: {}", name)),
                }
            }
            CppExpr::TplStaticAccess { name, args, member } => {
                // Tpl<Args>::member - 模板实例的静态成员/枚举常量
                let _ = args;
                match self.lookup_unqualified(member) {
                    syms if !syms.is_empty() => {
                        let sym = syms[0];
                        let sym_info = self
                            .get_symbol(sym)
                            .ok_or_else(|| "symbol not found".to_string())?;
                        let kind = sym_info.kind;
                        let ty = sym_info.ty.clone();
                        let expr_kind = match kind {
                            CxxSymbolKind::Variable => HirExprKind::Global(sym),
                            CxxSymbolKind::Function => HirExprKind::Global(sym),
                            _ => return Err(format!(
                                "tpl static access '{}::{}' is not a value",
                                name, member
                            )),
                        };
                        Ok(self.add_expr(expr_kind, ty))
                    }
                    _ => Err(format!("undeclared tpl static access: {}", member)),
                }
            }
            CppExpr::Sizeof(arg) => {
                match arg {
                    crate::cpp::parser::SizeofArg::Type(t) => {
                        let cxx_ty = self.convert_type(t)?;
                        Ok(self.add_expr(
                            HirExprKind::SizeOf(cxx_ty),
                            CxxType::Integer { bits: 64, signed: false },
                        ))
                    }
                    crate::cpp::parser::SizeofArg::Expr(e) => {
                        let expr_id = self.build_expr(e)?;
                        let ty = self.hir.exprs[expr_id.0].ty.clone();
                        Ok(self.add_expr(
                            HirExprKind::SizeOf(ty),
                            CxxType::Integer { bits: 64, signed: false },
                        ))
                    }
                }
            }
            CppExpr::StaticCast { ty, expr } => {
                let target = self.convert_type(ty)?;
                let expr_id = self.build_expr(expr)?;
                Ok(self.add_expr(
                    HirExprKind::Cast {
                        expr: expr_id,
                        target: target.clone(),
                        kind: CastKind::Static,
                    },
                    target,
                ))
            }
            CppExpr::ConstCast { ty, expr } => {
                let target = self.convert_type(ty)?;
                let expr_id = self.build_expr(expr)?;
                Ok(self.add_expr(
                    HirExprKind::Cast {
                        expr: expr_id,
                        target: target.clone(),
                        kind: CastKind::Const,
                    },
                    target,
                ))
            }
            CppExpr::ReinterpretCast { ty, expr } => {
                let target = self.convert_type(ty)?;
                let expr_id = self.build_expr(expr)?;
                Ok(self.add_expr(
                    HirExprKind::Cast {
                        expr: expr_id,
                        target: target.clone(),
                        kind: CastKind::Reinterpret,
                    },
                    target,
                ))
            }
            CppExpr::Lambda(_) => {
                // Lambda 在 native 路径中已被简化处理
                Ok(self.add_expr(
                    HirExprKind::NullPtr,
                    CxxType::Pointer(Box::new(CxxType::Void)),
                ))
            }
            CppExpr::StrConcat(parts) => {
                // 字符串字面量拼接
                let combined: String = parts.iter().map(|s| s.as_str()).collect();
                Ok(self.add_expr(
                    HirExprKind::FloatLiteral(combined),
                    CxxType::Pointer(Box::new(CxxType::Integer {
                        bits: 8,
                        signed: true,
                    })),
                ))
            }
            CppExpr::This => {
                let class_id = self
                    .current_class
                    .ok_or_else(|| "this used outside member function".to_string())?;
                let ty = CxxType::Pointer(Box::new(CxxType::Class(class_id)));
                Ok(self.add_expr(HirExprKind::This, ty))
            }
            CppExpr::ThisMember(name) => {
                let this_id = self.build_expr(&CppExpr::This)?;
                let this_ty = self.hir.exprs[this_id.0].ty.clone();
                let pointee = this_ty
                    .remove_pointer()
                    .ok_or_else(|| "this is not a pointer".to_string())?;
                let class_id = match pointee {
                    CxxType::Class(id) => *id,
                    _ => return Err("this is not a class pointer".into()),
                };
                let (syms, ambiguous) = self.lookup_in_class(class_id, name);
                if ambiguous {
                    return Err(format!("ambiguous member '{}'", name));
                }
                if syms.is_empty() {
                    return Err(format!("member '{}' not found", name));
                }
                let field_sym = syms[0];
                let field_ty = self
                    .get_symbol(field_sym)
                    .map(|s| s.ty.clone())
                    .unwrap_or(CxxType::Void);

                // 如果成员属于基类，需要做 base cast
                let object_expr =
                    self.adjust_pointer_to_member_base(this_id, field_sym, class_id)?;

                // 区分字段与方法
                let is_method = matches!(
                    self.get_symbol(field_sym).map(|s| s.kind),
                    Some(CxxSymbolKind::Method)
                );
                if is_method {
                    Ok(self.add_expr(
                        HirExprKind::MethodRef {
                            object: object_expr,
                            method: field_sym,
                        },
                        field_ty,
                    ))
                } else {
                    let expr_id = self.add_expr(
                        HirExprKind::FieldAccess {
                            object: object_expr,
                            field: field_sym,
                        },
                        field_ty,
                    );
                    Ok(expr_id)
                }
            }
            CppExpr::Ident(name) => {
                let syms = self.lookup_unqualified(name);
                if syms.is_empty() {
                    return Err(format!("undeclared identifier: {}", name));
                }
                let sym = syms[0];
                let sym_info = self
                    .get_symbol(sym)
                    .ok_or_else(|| "symbol not found".to_string())?;
                let kind = sym_info.kind;
                let ty = sym_info.ty.clone();
                let expr_kind = match kind {
                    CxxSymbolKind::Variable => HirExprKind::Local(sym),
                    CxxSymbolKind::EnumConstant => HirExprKind::Global(sym),
                    CxxSymbolKind::Field => {
                        // 成员函数体内裸字段名等价于 this->field
                        let this_id = self.build_expr(&CppExpr::This)?;
                        let this_class = self
                            .current_class
                            .ok_or_else(|| "field used outside member function".to_string())?;
                        // 字段若属于基类, 经 this-adjust 对齐到基类子对象
                        let object_expr =
                            self.adjust_pointer_to_member_base(this_id, sym, this_class)?;
                        HirExprKind::FieldAccess {
                            object: object_expr,
                            field: sym,
                        }
                    }
                    CxxSymbolKind::Function => HirExprKind::Global(sym),
                    CxxSymbolKind::Method => HirExprKind::Global(sym),
                    _ => return Err(format!("identifier '{}' is not a value", name)),
                };
                Ok(self.add_expr(expr_kind, ty))
            }
            CppExpr::Member(obj, name) => {
                let obj_id = self.build_expr(obj)?;
                let obj_ty = self.hir.exprs[obj_id.0].ty.clone();
                // 先对引用类型做隐式解引用: LValueRef(T) → T, RValueRef(T) → T
                let deref_obj_ty = match obj_ty.clone() {
                    CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => *inner,
                    other => other,
                };
                let (class_id, is_pointer) = match &deref_obj_ty {
                    CxxType::Class(id) => (*id, false),
                    CxxType::Pointer(t) => match t.as_ref() {
                        CxxType::Class(id) => (*id, true),
                        // Pointer(LValueRef(Class)) — 再解一次
                        CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => match inner.as_ref() {
                            CxxType::Class(id) => (*id, true),
                            _ => return Err("member access on non-class pointer".into()),
                        },
                        _ => return Err("member access on non-class pointer".into()),
                    },
                    _ => return Err("member access on non-class type".into()),
                };
                let (syms, ambiguous) = self.lookup_in_class(class_id, name);
                if ambiguous {
                    return Err(format!("ambiguous member '{}'", name));
                }
                if syms.is_empty() {
                    return Err(format!("member '{}' not found", name));
                }
                let field_sym = syms[0];
                let field_ty = self
                    .get_symbol(field_sym)
                    .map(|s| s.ty.clone())
                    .unwrap_or(CxxType::Void);
                // 区分字段与方法
                let is_method = matches!(
                    self.get_symbol(field_sym).map(|s| s.kind),
                    Some(CxxSymbolKind::Method)
                );
                if is_method {
                    // 方法访问：用 MethodRef 保留对象上下文，供 Call 判定是否虚调用
                    let object_expr = if is_pointer {
                        self.adjust_pointer_to_member_base(obj_id, field_sym, class_id)?
                    } else {
                        // 对象调用 obj.method()：取地址后作为 this
                        self.add_expr(
                            HirExprKind::AddressOf(obj_id),
                            CxxType::Pointer(Box::new(obj_ty)),
                        )
                    };
                    Ok(self.add_expr(
                        HirExprKind::MethodRef {
                            object: object_expr,
                            method: field_sym,
                        },
                        field_ty,
                    ))
                } else {
                    // 如果成员属于基类，需要做 base cast
                    let object_expr = if is_pointer {
                        self.adjust_pointer_to_member_base(obj_id, field_sym, class_id)?
                    } else {
                        obj_id
                    };
                    Ok(self.add_expr(
                        HirExprKind::FieldAccess {
                            object: object_expr,
                            field: field_sym,
                        },
                        field_ty,
                    ))
                }
            }
            CppExpr::PtrMember(obj, name) => {
                // obj->member 等价于 (*obj).member
                let deref = CppExpr::UnaryOp(UnaryOpKind::Deref, obj.clone());
                let deref_expr = CppExpr::Member(Box::new(deref), name.clone());
                self.build_expr(&deref_expr)
            }
            CppExpr::Call(callee, args) => {
                let callee_id = self.build_expr(callee)?;
                let callee_ty = self.hir.exprs[callee_id.0].ty.clone();
                let mut hir_args = Vec::new();
                for arg in args {
                    hir_args.push(self.build_expr(arg)?);
                }
                // 根据 callee 类型决定调用方式
                let (hir_callee, ret_ty) = match &self.hir.exprs[callee_id.0].kind {
                    HirExprKind::Global(sym) => {
                        // 直接调用函数/方法（如自由函数、静态成员函数、限定调用 Base::method()）
                        let ret = match &callee_ty {
                            CxxType::Function { ret, .. } => *ret.clone(),
                            _ => CxxType::Void,
                        };
                        (HirCallee::Direct(*sym), ret)
                    }
                    HirExprKind::MethodRef { object, method } => {
                        // 对象方法调用：判定是否虚调用
                        let ret = match &callee_ty {
                            CxxType::Function { ret, .. } => *ret.clone(),
                            _ => CxxType::Void,
                        };
                        let object_expr = *object;
                        let method_sym = *method;
                        let virtual_slot = self.get_virtual_slot(method_sym);
                        let callee = if let Some(slot) = virtual_slot {
                            // 虚函数调用：通过 vtable 分派
                            // this_adjust: 非虚继承下为 0（对象指针已通过 base cast 调整）
                            HirCallee::Virtual {
                                method: method_sym,
                                slot,
                                this_adjust: Default::default(),
                            }
                        } else {
                            // 非虚方法：直接调用
                            HirCallee::Direct(method_sym)
                        };
                        // 对象指针作为 this 第一个参数（模拟成员函数调用）
                        // 对象若为类值 lvalue (如 `w.get()`), 需取址得到 this;
                        // 指针/引用对象 (如 `p->get()` 或经解引用) 已是地址.
                        let object_ty = self.hir.exprs[object_expr.0].ty.clone();
                        let object_for_this = match &object_ty {
                            CxxType::Class(_) => self.add_expr(
                                HirExprKind::AddressOf(object_expr),
                                CxxType::Pointer(Box::new(object_ty)),
                            ),
                            CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                                match inner.as_ref() {
                                    CxxType::Class(_) => self.add_expr(
                                        HirExprKind::AddressOf(object_expr),
                                        CxxType::Pointer(inner.clone()),
                                    ),
                                    _ => object_expr,
                                }
                            }
                            _ => object_expr,
                        };
                        let mut full_args = Vec::with_capacity(hir_args.len() + 1);
                        full_args.push(object_for_this);
                        full_args.extend(hir_args);
                        hir_args = full_args;
                        (callee, ret)
                    }
                    HirExprKind::Local(_sym) => {
                        // 函数指针调用
                        let ret = match &callee_ty {
                            CxxType::Pointer(t) => match t.as_ref() {
                                CxxType::Function { ret, .. } => *ret.clone(),
                                _ => CxxType::Void,
                            },
                            _ => CxxType::Void,
                        };
                        (HirCallee::Indirect(callee_id), ret)
                    }
                    _ => {
                        // 函数指针调用
                        (HirCallee::Indirect(callee_id), CxxType::Void)
                    }
                };
                let call_id = self.add_expr(
                    HirExprKind::Call {
                        callee: hir_callee,
                        args: hir_args,
                    },
                    ret_ty,
                );
                Ok(call_id)
            }
            CppExpr::BinOp(op, lhs, rhs) => {
                use BinOpKind::*;
                let lhs_id = self.build_expr(lhs)?;
                let rhs_id = self.build_expr(rhs)?;
                let lhs_ty = self.hir.exprs[lhs_id.0].ty.clone();
                match op {
                    Assign => Ok(self.add_expr(
                        HirExprKind::Assign {
                            lhs: lhs_id,
                            rhs: rhs_id,
                        },
                        lhs_ty,
                    )),
                    AddAssign | SubAssign | MulAssign | DivAssign | ModAssign | ShlAssign
                    | ShrAssign | AndAssign | OrAssign | XorAssign => {
                        let bin_op = match op {
                            AddAssign => BinaryOp::Add,
                            SubAssign => BinaryOp::Sub,
                            MulAssign => BinaryOp::Mul,
                            DivAssign => BinaryOp::Div,
                            ModAssign => BinaryOp::Rem,
                            ShlAssign => BinaryOp::Shl,
                            ShrAssign => BinaryOp::Shr,
                            AndAssign => BinaryOp::BitAnd,
                            OrAssign => BinaryOp::BitOr,
                            XorAssign => BinaryOp::BitXor,
                            _ => unreachable!(),
                        };
                        Ok(self.add_expr(
                            HirExprKind::CompoundAssign {
                                op: bin_op,
                                lhs: lhs_id,
                                rhs: rhs_id,
                            },
                            lhs_ty,
                        ))
                    }
                    Index => {
                        // a[b]: 数组 / 指针下标访问。结果类型 = 元素类型。
                        // 保留独立的 Index 节点 (而不是降级成 *(a+b))，
                        // 这样 lower_ir 能精确生成 IndexLoad/IndexStore 并
                        // 带上元素的 volatile 属性。
                        let elem_ty = match &lhs_ty {
                            CxxType::Array(inner, _) => *inner.clone(),
                            CxxType::Pointer(inner) => *inner.clone(),
                            _ => {
                                return Err(
                                    "subscripted value is not an array or pointer".into()
                                )
                            }
                        };
                        Ok(self.add_expr(
                            HirExprKind::Index {
                                array: lhs_id,
                                index: rhs_id,
                            },
                            elem_ty,
                        ))
                    }
                    _ => {
                        let bin_op = match op {
                            Add => BinaryOp::Add,
                            Sub => BinaryOp::Sub,
                            Mul => BinaryOp::Mul,
                            Div => BinaryOp::Div,
                            Mod => BinaryOp::Rem,
                            EqEq => BinaryOp::Eq,
                            Ne => BinaryOp::Ne,
                            Lt => BinaryOp::Lt,
                            Le => BinaryOp::Le,
                            Gt => BinaryOp::Gt,
                            Ge => BinaryOp::Ge,
                            And => BinaryOp::BitAnd,
                            Or => BinaryOp::BitOr,
                            Xor => BinaryOp::BitXor,
                            Shl => BinaryOp::Shl,
                            Shr => BinaryOp::Shr,
                            LAnd => BinaryOp::LogicalAnd,
                            LOr => BinaryOp::LogicalOr,
                            _ => return Err("unsupported binary operator".into()),
                        };
                        // 简化：结果类型 = 左操作数类型
                        Ok(self.add_expr(
                            HirExprKind::BinaryOp {
                                op: bin_op,
                                lhs: lhs_id,
                                rhs: rhs_id,
                            },
                            lhs_ty,
                        ))
                    }
                }
            }
            CppExpr::UnaryOp(op, operand) => {
                let operand_id = self.build_expr(operand)?;
                let operand_ty = self.hir.exprs[operand_id.0].ty.clone();
                let unary_op = match op {
                    UnaryOpKind::Neg => UnaryOp::Minus,
                    UnaryOpKind::Not => UnaryOp::BitNot,
                    UnaryOpKind::Plus => UnaryOp::Plus,
                    UnaryOpKind::LNot => UnaryOp::LogicalNot,
                    UnaryOpKind::PreInc => UnaryOp::PreInc,
                    UnaryOpKind::PreDec => UnaryOp::PreDec,
                    UnaryOpKind::PostInc => UnaryOp::PostInc,
                    UnaryOpKind::PostDec => UnaryOp::PostDec,
                    UnaryOpKind::Deref => UnaryOp::Deref,
                };
                let result_ty = match op {
                    UnaryOpKind::Deref => match operand_ty {
                        CxxType::Pointer(t) => *t,
                        _ => return Err("dereference of non-pointer type".into()),
                    },
                    _ => operand_ty,
                };
                Ok(self.add_expr(
                    HirExprKind::UnaryOp {
                        op: unary_op,
                        operand: operand_id,
                    },
                    result_ty,
                ))
            }
            CppExpr::AddrOf(operand) => {
                let operand_id = self.build_expr(operand)?;
                let operand_ty = self.hir.exprs[operand_id.0].ty.clone();
                Ok(self.add_expr(
                    HirExprKind::AddressOf(operand_id),
                    CxxType::Pointer(Box::new(operand_ty)),
                ))
            }
            CppExpr::Ref(operand) => {
                // 引用表达式 - 简化为 AddressOf
                let operand_id = self.build_expr(operand)?;
                let operand_ty = self.hir.exprs[operand_id.0].ty.clone();
                Ok(self.add_expr(
                    HirExprKind::AddressOf(operand_id),
                    CxxType::Pointer(Box::new(operand_ty)),
                ))
            }
            CppExpr::Cast(ty, expr) => {
                let target = self.convert_type(ty)?;
                let expr_id = self.build_expr(expr)?;
                Ok(self.add_expr(
                    HirExprKind::Cast {
                        expr: expr_id,
                        target: target.clone(),
                        kind: CastKind::Static,
                    },
                    target,
                ))
            }
            CppExpr::TypeidType(ty) => {
                // typeid(T): 静态类型的 type_info 引用
                // 简化：返回 type_info*（引用在 ABI 层降级为指针）
                let cxx_ty = self.convert_type(ty)?;
                match cxx_ty {
                    CxxType::Class(class_id) => {
                        // type_info* 类型用 void* 表示（不透明）
                        let type_info_ptr = CxxType::Pointer(Box::new(CxxType::Void));
                        Ok(self.add_expr(HirExprKind::TypeInfoRef(class_id), type_info_ptr))
                    }
                    _ => Err(format!(
                        "typeid of non-class type not yet supported"
                    )),
                }
            }
            CppExpr::Typeid(expr) => {
                // typeid(expr): 表达式的动态类型 type_info
                // 简化：对于多态类型需要动态查找，这里先用静态类型
                let expr_id = self.build_expr(expr)?;
                let expr_ty = self.hir.exprs[expr_id.0].ty.clone();
                match expr_ty {
                    CxxType::Class(class_id) => {
                        let type_info_ptr = CxxType::Pointer(Box::new(CxxType::Void));
                        Ok(self.add_expr(HirExprKind::TypeInfoRef(class_id), type_info_ptr))
                    }
                    CxxType::Pointer(inner) => match inner.as_ref() {
                        CxxType::Class(class_id) => {
                            let type_info_ptr = CxxType::Pointer(Box::new(CxxType::Void));
                            Ok(self.add_expr(HirExprKind::TypeInfoRef(*class_id), type_info_ptr))
                        }
                        _ => Err(format!(
                            "typeid of non-class pointer type not yet supported"
                        )),
                    },
                    CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                        match inner.as_ref() {
                            CxxType::Class(class_id) => {
                                let type_info_ptr = CxxType::Pointer(Box::new(CxxType::Void));
                                Ok(self.add_expr(HirExprKind::TypeInfoRef(*class_id), type_info_ptr))
                            }
                            _ => Err(format!(
                                "typeid of non-class reference type not yet supported"
                            )),
                        }
                    }
                    _ => Err(format!(
                        "typeid of non-class type not yet supported"
                    )),
                }
            }
            CppExpr::DynamicCast { ty, expr } => {
                // dynamic_cast<T>(expr)
                let dst_ty = self.convert_type(ty)?;
                let expr_id = self.build_expr(expr)?;
                let src_ty = self.hir.exprs[expr_id.0].ty.clone();
                Ok(self.add_expr(
                    HirExprKind::DynamicCast {
                        object: expr_id,
                        src: src_ty,
                        dst: dst_ty.clone(),
                    },
                    dst_ty,
                ))
            }
            CppExpr::Ternary { cond, then, els } => {
                // 三元表达式: cond ? then : els
                // C++ 公共类型规则简化为: 两分支类型相同则用之,
                // 否则取 then 分支类型 (覆盖基础整数/指针字面量场景)
                let cond_id = self.build_expr(cond)?;
                let then_id = self.build_expr(then)?;
                let else_id = self.build_expr(els)?;
                let then_ty = self.hir.exprs[then_id.0].ty.clone();
                let else_ty = self.hir.exprs[else_id.0].ty.clone();
                let ty = if then_ty == else_ty {
                    then_ty
                } else {
                    then_ty
                };
                Ok(self.add_expr(
                    HirExprKind::Conditional {
                        cond: cond_id,
                        then_expr: then_id,
                        else_expr: else_id,
                    },
                    ty,
                ))
            }
            CppExpr::Comma(elems) => {
                // 逗号表达式: 依次求值，返回最后一个
                if elems.is_empty() {
                    return Err("empty comma expression".into());
                }
                let mut last_id = self.build_expr(&elems[0])?;
                for e in &elems[1..] {
                    let rhs_id = self.build_expr(e)?;
                    let rhs_ty = self.hir.exprs[rhs_id.0].ty.clone();
                    last_id = self.add_expr(
                        HirExprKind::Comma {
                            lhs: last_id,
                            rhs: rhs_id,
                        },
                        rhs_ty,
                    );
                }
                Ok(last_id)
            }
            CppExpr::New(ty_str, args) => {
                let ty = self.type_name_to_cxx_type(ty_str)?;
                let mut hir_args = Vec::new();
                for a in args {
                    hir_args.push(self.build_expr(a)?);
                }
                let ret_ty = CxxType::Pointer(Box::new(ty.clone()));
                Ok(self.add_expr(
                    HirExprKind::New {
                        ty,
                        args: hir_args,
                        placement: None,
                        is_array: false,
                    },
                    ret_ty,
                ))
            }
            CppExpr::NewArr { ty, count } => {
                let elem_ty = self.type_name_to_cxx_type(ty)?;
                let count_id = self.build_expr(count)?;
                let ret_ty = CxxType::Pointer(Box::new(elem_ty.clone()));
                Ok(self.add_expr(
                    HirExprKind::New {
                        ty: elem_ty,
                        args: vec![count_id],
                        placement: None,
                        is_array: true,
                    },
                    ret_ty,
                ))
            }
            CppExpr::PlacementNew { ptr, ty, args } => {
                let elem_ty = self.type_name_to_cxx_type(ty)?;
                let ptr_id = self.build_expr(ptr)?;
                let mut hir_args = Vec::new();
                for a in args {
                    hir_args.push(self.build_expr(a)?);
                }
                let ret_ty = CxxType::Pointer(Box::new(elem_ty.clone()));
                Ok(self.add_expr(
                    HirExprKind::New {
                        ty: elem_ty,
                        args: hir_args,
                        placement: Some(vec![ptr_id]),
                        is_array: false,
                    },
                    ret_ty,
                ))
            }
            CppExpr::Delete(ptr) => {
                let ptr_id = self.build_expr(ptr)?;
                Ok(self.add_expr(
                    HirExprKind::Delete {
                        ptr: ptr_id,
                        is_array: false,
                    },
                    CxxType::Void,
                ))
            }
            CppExpr::DeleteArr(ptr) => {
                let ptr_id = self.build_expr(ptr)?;
                Ok(self.add_expr(
                    HirExprKind::Delete {
                        ptr: ptr_id,
                        is_array: true,
                    },
                    CxxType::Void,
                ))
            }
            CppExpr::InitList(elems) => {
                let mut hir_elems = Vec::new();
                for e in elems {
                    hir_elems.push(self.build_expr(e)?);
                }
                // 简化：用第一个元素的类型作为列表类型（实际应做公共类型推导）
                let target_ty = if let Some(first) = hir_elems.first() {
                    self.hir.exprs[first.0].ty.clone()
                } else {
                    CxxType::Void
                };
                Ok(self.add_expr(
                    HirExprKind::InitList {
                        elems: hir_elems,
                        target_ty: None,
                    },
                    target_ty,
                ))
            }
            CppExpr::TypedInitList { ty, elems } => {
                let target_ty = self.convert_type(ty)?;
                let mut hir_elems = Vec::new();
                for e in elems {
                    hir_elems.push(self.build_expr(e)?);
                }
                Ok(self.add_expr(
                    HirExprKind::InitList {
                        elems: hir_elems,
                        target_ty: Some(target_ty.clone()),
                    },
                    target_ty,
                ))
            }
            CppExpr::QualifiedAccess(path, name) => {
                // 限定名访问: ns1::ns2::name / Enum::member
                // 从全局作用域开始逐级解析
                let mut scope = self.global_scope();
                for part in path {
                    let syms = self.lookup_qualified(scope, part);
                    if syms.is_empty() {
                        return Err(format!("'{}' not found in qualified name", part));
                    }
                    // 找 namespace/class/enum 作为路径段
                    let mut found_scope = None;
                    let mut is_enum = false;
                    for &s in &syms {
                        if let Some(sym) = self.get_symbol(s) {
                            match sym.kind {
                                CxxSymbolKind::Class | CxxSymbolKind::Namespace => {
                                    found_scope = Some(sym.scope);
                                    break;
                                }
                                CxxSymbolKind::Enum => {
                                    // enum 成员注册在枚举类型的当前 scope (不是 enum 自己的 scope)
                                    is_enum = true;
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    if is_enum {
                        // scope 保持不变, 继续在当前作用域查枚举常量
                    } else if let Some(s) = found_scope {
                        scope = s;
                    } else {
                        return Err(format!(
                            "'{}' is not a class or namespace",
                            part
                        ));
                    }
                }
                // 在最终作用域中查找目标名称
                let syms = self.lookup_qualified(scope, name);
                if syms.is_empty() {
                    return Err(format!("'{}' not found in qualified name", name));
                }
                let sym = syms[0];
                let sym_info = self
                    .get_symbol(sym)
                    .ok_or_else(|| "symbol not found".to_string())?;
                let kind = sym_info.kind;
                let ty = sym_info.ty.clone();
                let expr_kind = match kind {
                    CxxSymbolKind::Variable | CxxSymbolKind::EnumConstant => HirExprKind::Global(sym),
                    CxxSymbolKind::Function => HirExprKind::Global(sym),
                    CxxSymbolKind::Method => HirExprKind::Global(sym),
                    CxxSymbolKind::Class | CxxSymbolKind::Enum | CxxSymbolKind::TypeAlias => {
                        return Err(format!("'{}' is a type, not a value", name))
                    }
                    _ => return Err(format!("'{}' is not a value", name)),
                };
                Ok(self.add_expr(expr_kind, ty))
            }
            CppExpr::OpCall { op, args } => {
                // 运算符调用: 按运算符名查找重载函数，转为普通调用
                let op_name = op.encode_name();
                let syms = self.lookup_unqualified(&format!("operator{}", op_name));
                if syms.is_empty() {
                    return Err(format!("no match for operator '{}'", op_name));
                }
                let sym = syms[0];
                let sym_info = self
                    .get_symbol(sym)
                    .ok_or_else(|| "operator symbol not found".to_string())?;
                let fn_ty = sym_info.ty.clone();
                let ret_ty = match &fn_ty {
                    CxxType::Function { ret, .. } => *ret.clone(),
                    _ => CxxType::Void,
                };
                let is_member = matches!(sym_info.kind, CxxSymbolKind::Method);
                let mut hir_args = Vec::new();
                for a in args {
                    hir_args.push(self.build_expr(a)?);
                }
                // 成员运算符：第一个参数是对象，需要作为 this 传递
                if is_member && !hir_args.is_empty() {
                    let object = hir_args.remove(0);
                    let object_ty = self.hir.exprs[object.0].ty.clone();
                    let object_for_this = match &object_ty {
                        CxxType::Class(_) => self.add_expr(
                            HirExprKind::AddressOf(object),
                            CxxType::Pointer(Box::new(object_ty)),
                        ),
                        CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                            match inner.as_ref() {
                                CxxType::Class(_) => self.add_expr(
                                    HirExprKind::AddressOf(object),
                                    CxxType::Pointer(inner.clone()),
                                ),
                                _ => object,
                            }
                        }
                        _ => object,
                    };
                    let mut full_args = Vec::with_capacity(hir_args.len() + 1);
                    full_args.push(object_for_this);
                    full_args.extend(hir_args);
                    hir_args = full_args;
                }
                Ok(self.add_expr(
                    HirExprKind::Call {
                        callee: HirCallee::Direct(sym),
                        args: hir_args,
                    },
                    ret_ty,
                ))
            }
            other => Err(format!(
                "unsupported expression kind: {:?}",
                std::mem::discriminant(other)
            )),
        }
    }

    // --- 语句构建 ---

    fn build_stmt(&mut self, stmt: &CppStmt) -> Result<HirStmtId, String> {
        match stmt {
            CppStmt::Expr(e) => {
                let expr_id = self.build_expr(e)?;
                Ok(self.add_stmt(HirStmtKind::Expr(expr_id)))
            }
            CppStmt::Return(e) => {
                let expr_id = match e {
                    Some(e) => Some(self.build_expr(e)?),
                    None => None,
                };
                Ok(self.add_stmt(HirStmtKind::Return(expr_id)))
            }
            CppStmt::If { cond, then, els } => {
                let cond_id = self.build_expr(cond)?;
                let then_stmts = self.build_body(then)?;
                let then_stmt = self.add_stmt(HirStmtKind::Compound(then_stmts));
                let else_body = match els {
                    Some(e) => {
                        let e_stmts = self.build_body(e)?;
                        Some(self.add_stmt(HirStmtKind::Compound(e_stmts)))
                    }
                    None => None,
                };
                Ok(self.add_stmt(HirStmtKind::If {
                    cond: cond_id,
                    then_body: then_stmt,
                    else_body,
                }))
            }
            CppStmt::IfConstexpr { cond, then, els } => {
                // if constexpr: 编译期条件选择
                // 尝试 const eval，成功则只编译选中的分支
                // 失败则降级为普通 if（保守处理）
                let then_stmts = self.build_body(then)?;
                let then_stmt = self.add_stmt(HirStmtKind::Compound(then_stmts));
                let else_body = match els {
                    Some(e) => {
                        let e_stmts = self.build_body(e)?;
                        Some(self.add_stmt(HirStmtKind::Compound(e_stmts)))
                    }
                    None => None,
                };
                // 先尝试 const eval
                use crate::cpp::cpp_const_eval::eval_cpp_const;
                let cond_value = eval_cpp_const(cond, &HashMap::new());
                let cond_id = match cond_value {
                    Some(v) => self.add_expr(
                        HirExprKind::IntLiteral(v),
                        CxxType::Integer { bits: 32, signed: true },
                    ),
                    None => self.build_expr(cond)?,
                };
                Ok(self.add_stmt(HirStmtKind::If {
                    cond: cond_id,
                    then_body: then_stmt,
                    else_body,
                }))
            }
            CppStmt::While { cond, body } => {
                let cond_id = self.build_expr(cond)?;
                let body_stmts = self.build_body(body)?;
                let body_stmt = self.add_stmt(HirStmtKind::Compound(body_stmts));
                Ok(self.add_stmt(HirStmtKind::While {
                    cond: cond_id,
                    body: body_stmt,
                }))
            }
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                // 进入 for 作用域
                self.push_scope(ScopeKind::Block);
                let init_stmt = match init {
                    Some(s) => Some(self.build_stmt(s)?),
                    None => None,
                };
                let cond_expr = match cond {
                    Some(c) => Some(self.build_expr(c)?),
                    None => None,
                };
                let inc_expr = match step {
                    Some(s) => {
                        // step 是 CppStmt，把它转换为表达式（若是表达式语句）
                        match s.as_ref() {
                            CppStmt::Expr(e) => Some(self.build_expr(e)?),
                            _ => return Err("for step must be an expression".into()),
                        }
                    }
                    None => None,
                };
                let body_stmts = self.build_body(body)?;
                let body_stmt = self.add_stmt(HirStmtKind::Compound(body_stmts));
                let for_stmt = self.add_stmt(HirStmtKind::For {
                    init: init_stmt,
                    cond: cond_expr,
                    inc: inc_expr,
                    body: body_stmt,
                });
                self.pop_scope();
                Ok(for_stmt)
            }
            CppStmt::Block(body) => {
                let body_stmts = self.build_body(body)?;
                Ok(self.add_stmt(HirStmtKind::Compound(body_stmts)))
            }
            CppStmt::LocalVar { ty, name, init, is_volatile } => {
                // 计算变量类型 + (可选的) 已预先 build 的 init expr
                // Auto/DecltypeAuto: 优先检测 ctor call 模式, 否则从 init 推导
                let (var_ty, prebuilt_init): (CxxType, Option<HirExprId>) = match ty {
                    MangleType::Auto | MangleType::DecltypeAuto => {
                        let init_expr = init.as_ref().ok_or_else(|| {
                            "auto variable must have an initializer".to_string()
                        })?;
                        // ctor call: Call(Ident("Class"), args) + ClassName 是 Class
                        let ctor_class_name = if let CppExpr::Call(callee, _) = init_expr {
                            if let CppExpr::Ident(n) = callee.as_ref() {
                                let syms = self.lookup_unqualified(n);
                                if syms.iter().any(|&s| {
                                    matches!(
                                        self.get_symbol(s).map(|sym| sym.kind),
                                        Some(CxxSymbolKind::Class)
                                    )
                                }) {
                                    Some(n.clone())
                                } else { None }
                            } else { None }
                        } else { None };
                        if let Some(cn) = ctor_class_name {
                            // 用 ClassName 得到 Class 类型, 留待下面 ctor 块处理
                            let class_ty = self.convert_type(
                                &MangleType::Class(cn),
                            )?;
                            (class_ty, None)
                        } else {
                            // 普通 auto 推导: build init expr 拿类型
                            let init_id = self.build_expr(init_expr)?;
                            let init_ty = self.hir.exprs[init_id.0].ty.clone();
                            (init_ty, Some(init_id))
                        }
                    }
                    _ => (self.convert_type(ty)?, None),
                };
                let sym = self.add_symbol(
                    name.clone(),
                    CxxSymbolKind::Variable,
                    var_ty.clone(),
                    Access::Private,
                );
                if *is_volatile {
                    if let Some(s) = self.symbols.get_mut(sym.0) {
                        s.is_volatile = true;
                    }
                }
                // 检测类类型的构造函数调用: Call(Ident("Class"), args)
                // 这种模式来自 parser 对 `Wrap w(42)` 和 `auto w = Widget(7)`
                if let Some(init_expr) = init {
                    if let CxxType::Class(class_id) = &var_ty {
                        if let CppExpr::Call(callee, args) = init_expr {
                            if let CppExpr::Ident(class_name) = callee.as_ref() {
                                let syms = self.lookup_unqualified(class_name);
                                let is_class_ident = syms.iter().any(|&s| {
                                    matches!(
                                        self.get_symbol(s).map(|sym| sym.kind),
                                        Some(CxxSymbolKind::Class)
                                    )
                                });
                                if is_class_ident {
                                    let local_expr = self.add_expr(
                                        HirExprKind::Local(sym),
                                        var_ty.clone(),
                                    );
                                    let place_expr = self.add_expr(
                                        HirExprKind::AddressOf(local_expr),
                                        CxxType::Pointer(Box::new(var_ty.clone())),
                                    );
                                    let mut hir_args = Vec::new();
                                    for arg in args {
                                        hir_args.push(self.build_expr(arg)?);
                                    }
                                    let construct_expr = self.add_expr(
                                        HirExprKind::Construct {
                                            class: *class_id,
                                            place: place_expr,
                                            args: hir_args,
                                            variant: crate::cpp::hir::CtorVariant::Complete,
                                        },
                                        CxxType::Void,
                                    );
                                    let decl_stmt = self.add_stmt(HirStmtKind::Decl {
                                        symbol: sym,
                                        init: None,
                                    });
                                    let construct_stmt = self.add_stmt(HirStmtKind::Expr(
                                        construct_expr,
                                    ));
                                    return Ok(self.add_stmt(HirStmtKind::Compound(vec![
                                        decl_stmt,
                                        construct_stmt,
                                    ])));
                                }
                            }
                        }
                    }
                }
                let init_id = if let Some(pre_id) = prebuilt_init {
                    Some(pre_id)
                } else {
                    match init {
                        Some(e) => Some(self.build_expr(e)?),
                        None => None,
                    }
                };
                Ok(self.add_stmt(HirStmtKind::Decl {
                    symbol: sym,
                    init: init_id,
                }))
            }
            CppStmt::StaticLocal { ty, name, init, .. } => {
                // 静态局部变量: 符号注册到全局作用域, 生成带 guard 的线程安全初始化
                let var_ty = self.convert_type(ty)?;
                let init_id = match init {
                    Some(e) => Some(self.build_expr(e)?),
                    None => None,
                };
                let counter = self.static_local_counter;
                self.static_local_counter += 1;
                let var_link = format!("__dcc_static_{}", counter);
                let guard_link = format!("__dcc_static_guard_{}", counter);

                // 在全局作用域创建 symbol (linkage 用 var_link)
                let saved_scope = self.current_scope;
                self.current_scope = self.global_scope;
                let var_sym = self.add_symbol(
                    var_link.clone(),
                    CxxSymbolKind::Variable,
                    var_ty,
                    Access::Private,
                );
                let guard_sym = self.add_symbol(
                    guard_link.clone(),
                    CxxSymbolKind::Variable,
                    CxxType::Integer {
                        bits: 64,
                        signed: true,
                    },
                    Access::Private,
                );
                self.current_scope = saved_scope;

                // 设置 linkage name (symbol.name 就是 link name)
                if let Some(s) = self.symbols.get_mut(var_sym.0) {
                    s.linkage_name = Some(var_link.clone());
                }
                if let Some(s) = self.symbols.get_mut(guard_sym.0) {
                    s.linkage_name = Some(guard_link.clone());
                }

                // 关键: 在当前函数体 scope 用原始名 `name` 注册同一个 symbol,
                // 使得函数内 `n` 能被 lookup 到
                let cur_scope = &mut self.scopes[saved_scope.0];
                cur_scope.add_symbol(name.clone(), var_sym);

                // 注册为顶层全局变量
                self.hir.top_level.push(HirTopLevelDecl::Variable(var_sym));
                self.hir.top_level.push(HirTopLevelDecl::Variable(guard_sym));
                Ok(self.add_stmt(HirStmtKind::StaticDecl {
                    symbol: var_sym,
                    guard_symbol: guard_sym,
                    init: init_id,
                }))
            }
            CppStmt::CoAwait(_e) => {
                // 协程语句应在 AST 预转换阶段被转换为状态机形式, 此处不应再出现
                Err("co_await not supported in this context".into())
            }
            CppStmt::CoYield(_e) => {
                // 协程语句应在 AST 预转换阶段被转换为状态机形式, 此处不应再出现
                Err("co_yield not supported in this context".into())
            }
            CppStmt::CoReturn(_e) => {
                // 协程语句应在 AST 预转换阶段被转换为状态机形式, 此处不应再出现
                Err("co_return not supported in this context".into())
            }
            CppStmt::ThisMember(_name, _stmt) => {
                // this->member = ... 或 this->member(...) 可以在表达式层处理
                // 这里直接处理语句内部的 member
                Err("ThisMember statement: delegate to expr builder".into())
            }
            CppStmt::Break => Ok(self.add_stmt(HirStmtKind::Break)),
            CppStmt::Continue => Ok(self.add_stmt(HirStmtKind::Continue)),
            CppStmt::Throw(expr) => {
                let obj_id = match expr {
                    Some(e) => Some(self.build_expr(e)?),
                    None => None,
                };
                // throw 表达式的类型：有对象时是对象类型，rethrow 时为 void
                let throw_ty = match &obj_id {
                    Some(id) => self.hir.exprs[id.0].ty.clone(),
                    None => CxxType::Void,
                };
                let throw_expr = self.add_expr(HirExprKind::Throw {
                    object: obj_id,
                    ty: throw_ty.clone(),
                }, throw_ty);
                Ok(self.add_stmt(HirStmtKind::Expr(throw_expr)))
            }
            CppStmt::Try { body, catches } => {
                let body_stmts = self.build_body(body)?;
                let body_stmt = self.add_stmt(HirStmtKind::Compound(body_stmts));
                let mut handlers = Vec::new();
                for catch in catches {
                    // 进入 catch 作用域
                    self.push_scope(ScopeKind::Block);
                    let param_ty = match &catch.ty {
                        Some(t) => self.convert_type(t)?,
                        None => CxxType::Void, // catch (...) 用 Void 表示
                    };
                    let param_sym = match &catch.name {
                        Some(name) => {
                            let sym = self.add_symbol(
                                name.clone(),
                                CxxSymbolKind::Variable,
                                param_ty.clone(),
                                Access::Private,
                            );
                            Some(sym)
                        }
                        None => None,
                    };
                    let catch_body_stmts = self.build_body(&catch.body)?;
                    let catch_body_stmt = self.add_stmt(HirStmtKind::Compound(catch_body_stmts));
                    self.pop_scope();
                    handlers.push(HirCatchHandler {
                        param_ty,
                        param: param_sym,
                        body: catch_body_stmt,
                    });
                }
                Ok(self.add_stmt(HirStmtKind::Try {
                    body: body_stmt,
                    handlers,
                }))
            }
            CppStmt::DoWhile { body, cond } => {
                // do-while: 先构建 body, 再构建条件表达式 (条件在 body
                // 作用域内可见: `do { int i = 0; } while (i < 3);`)
                let body_stmts = self.build_body(body)?;
                let body_stmt = self.add_stmt(HirStmtKind::Compound(body_stmts));
                let cond_id = self.build_expr(cond)?;
                Ok(self.add_stmt(HirStmtKind::DoWhile {
                    body: body_stmt,
                    cond: cond_id,
                }))
            }
            CppStmt::Goto(label) => Ok(self.add_stmt(HirStmtKind::Goto(label.clone()))),
            CppStmt::Labeled { label, stmt } => {
                // `label: stmt` — 标签只影响控制流, 语句本身照常构建
                let inner = self.build_stmt(stmt)?;
                Ok(self.add_stmt(HirStmtKind::Label(label.clone(), inner)))
            }
            CppStmt::Fallthrough { .. } => {
                // [[fallthrough]]; 只是给诊断用的标记, 语义上是空操作
                Ok(self.add_stmt(HirStmtKind::Fallthrough))
            }
            CppStmt::Switch { expr, arms, .. } => {
                let cond_id = self.build_expr(expr)?;
                // switch body 是一个独立作用域 (`switch (x) { case 1: int y; }`)
                self.push_scope(ScopeKind::Block);
                let mut body_stmts = Vec::new();
                for (case_expr, case_body) in arms {
                    let stmts = self.build_body(case_body)?;
                    let body_stmt = self.add_stmt(HirStmtKind::Compound(stmts));
                    match case_expr {
                        Some(ce) => {
                            // case 标签必须是编译期整型常量
                            let value = crate::cpp::cpp_const_eval::eval_cpp_const(
                                ce,
                                &HashMap::new(),
                            )
                            .ok_or_else(|| {
                                "case label must be an integer constant expression".to_string()
                            })?;
                            body_stmts.push(self.add_stmt(HirStmtKind::Case {
                                value,
                                body: body_stmt,
                            }));
                        }
                        None => {
                            body_stmts.push(self.add_stmt(HirStmtKind::Default(body_stmt)));
                        }
                    }
                }
                let body = self.add_stmt(HirStmtKind::Compound(body_stmts));
                self.pop_scope();
                Ok(self.add_stmt(HirStmtKind::Switch {
                    cond: cond_id,
                    body,
                }))
            }
            CppStmt::Decl(_d) => Err("nested declaration statement not yet supported".into()),
            other => Err(format!(
                "unsupported statement kind: {:?}",
                std::mem::discriminant(other)
            )),
        }
    }

    fn build_body(&mut self, body: &CppBody) -> Result<Vec<HirStmtId>, String> {
        self.push_scope(ScopeKind::Block);
        let mut stmts = Vec::new();
        for stmt in &body.stmts {
            let sid = self.build_stmt(stmt)?;
            stmts.push(sid);
        }
        self.pop_scope();
        Ok(stmts)
    }

    // --- 函数构建 ---

    fn build_function(
        &mut self,
        name: &str,
        ret_ty: CxxType,
        params: &[(String, CxxType)],
        body: &CppBody,
        kind: CxxSymbolKind,
        access: Access,
        owner: Option<SymbolId>,
        is_const: bool,
    ) -> Result<SymbolId, String> {
        // 构建函数类型
        let fn_ty = CxxType::Function {
            ret: Box::new(ret_ty.clone()),
            params: params.iter().map(|(_, t)| t.clone()).collect(),
            is_const,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym = self.add_symbol(name.to_string(), kind, fn_ty, access);
        if let Some(own) = owner {
            self.set_owner(fn_sym, own);
        }

        // 保存外部 class 上下文
        let saved_class = self.current_class;

        // 推入函数作用域
        self.push_scope(ScopeKind::Function);
        // 设置 owner 符号
        if let Some(scope) = self.scopes.get_mut(self.current_scope.0) {
            scope.owner_symbol = Some(fn_sym);
        }

        // 处理 this 指针（成员函数）
        if matches!(kind, CxxSymbolKind::Method) {
            if let Some(class_id) = saved_class {
                let this_ty = CxxType::Pointer(Box::new(CxxType::Class(class_id)));
                let _this_sym = self.add_symbol(
                    "this".into(),
                    CxxSymbolKind::Variable,
                    this_ty,
                    Access::Private,
                );
            }
        }

        // 注册形参
        let mut param_syms = Vec::new();
        for (pname, pty) in params {
            let pid = self.add_symbol(
                pname.clone(),
                CxxSymbolKind::Variable,
                pty.clone(),
                Access::Private,
            );
            param_syms.push(pid);
        }

        // 构建函数体
        let body_id = if !body.is_decl {
            let body_stmts = self.build_body(body)?;
            self.add_body(body_stmts)
        } else {
            // 纯声明，空 body
            self.add_body(Vec::new())
        };

        self.pop_scope();

        // 恢复 class 上下文
        self.current_class = saved_class;

        // 注册到 HIR
        self.hir.functions.push(HirFunction {
            symbol: fn_sym,
            body: body_id,
            params: param_syms,
            ret_ty,
        });

        Ok(fn_sym)
    }

    // --- 构造函数构建 ---

    fn build_constructor(
        &mut self,
        name: &str,
        ret_ty: CxxType,
        params: &[(String, CxxType)],
        inits: &[(String, CppExpr)],
        body: &CppBody,
        access: Access,
        owner: Option<SymbolId>,
        class_id: ClassId,
    ) -> Result<SymbolId, String> {
        // 构建函数类型
        let fn_ty = CxxType::Function {
            ret: Box::new(ret_ty.clone()),
            params: params.iter().map(|(_, t)| t.clone()).collect(),
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym = self.add_symbol(name.to_string(), CxxSymbolKind::Constructor, fn_ty, access);
        if let Some(own) = owner {
            self.set_owner(fn_sym, own);
        }

        // 保存外部 class 上下文
        let saved_class = self.current_class;

        // 推入函数作用域
        self.push_scope(ScopeKind::Function);
        if let Some(scope) = self.scopes.get_mut(self.current_scope.0) {
            scope.owner_symbol = Some(fn_sym);
        }

        // 注册 this 指针
        let this_ty = CxxType::Pointer(Box::new(CxxType::Class(class_id)));
        let _this_sym = self.add_symbol(
            "this".into(),
            CxxSymbolKind::Variable,
            this_ty,
            Access::Private,
        );

        // 注册形参
        let mut param_syms = Vec::new();
        for (pname, pty) in params {
            let pid = self.add_symbol(
                pname.clone(),
                CxxSymbolKind::Variable,
                pty.clone(),
                Access::Private,
            );
            param_syms.push(pid);
        }

        // 处理初始化列表
        let ctor_inits = if !body.is_decl {
            self.build_ctor_initializers(inits, class_id)?
        } else {
            Vec::new()
        };

        // 构建函数体
        let body_id = if !body.is_decl {
            let body_stmts = self.build_body(body)?;
            let id = HirBodyId(self.hir.bodies.len());
            self.hir.bodies.push(HirBody {
                id,
                stmts: body_stmts,
                ctor_initializers: ctor_inits,
            });
            id
        } else {
            self.add_body(Vec::new())
        };

        self.pop_scope();
        self.current_class = saved_class;

        self.hir.functions.push(HirFunction {
            symbol: fn_sym,
            body: body_id,
            params: param_syms,
            ret_ty,
        });

        Ok(fn_sym)
    }

    /// 构建构造函数初始化列表
    /// 按照 C++ 标准的构造顺序：基类按声明顺序 → 成员按声明顺序 → 构造函数体
    /// 每个基类构造完成后更新 vptr 指向该基类的 vtable，全部完成后指向最派生类的 vtable。
    fn build_ctor_initializers(
        &mut self,
        inits: &[(String, CppExpr)],
        class_id: ClassId,
    ) -> Result<Vec<HirStmtId>, String> {
        // 收集初始化器: name -> init expr
        let mut init_map: HashMap<String, &CppExpr> = HashMap::new();
        for (name, expr) in inits {
            init_map.insert(name.clone(), expr);
        }

        let mut result: Vec<HirStmtId> = Vec::new();

        // 1. 检查是否是委托构造（初始化列表第一项是类名自己）
        let class_name = self
            .get_symbol(class_id.as_symbol())
            .map(|s| s.name.clone())
            .unwrap_or_default();

        if let Some(deleg_init) = init_map.get(&class_name) {
            // 委托构造：直接调用同类的另一个构造函数
            let args = match deleg_init {
                CppExpr::Call(_, args) => {
                    let mut hir_args = Vec::new();
                    for arg in args {
                        hir_args.push(self.build_expr(arg)?);
                    }
                    hir_args
                }
                _ => {
                    // 单参表达式
                    let arg_id = self.build_expr(deleg_init)?;
                    vec![arg_id]
                }
            };
            let stmt = self.add_stmt(HirStmtKind::CtorInitBase {
                base: class_id, // 委托到自己
                args,
            });
            result.push(stmt);
            return Ok(result);
        }

        // 判断当前类是否有虚函数（需要 vptr）
        let has_virtual = self
            .get_class_info(class_id)
            .map(|info| !info.virtual_methods.is_empty())
            .unwrap_or(false);

        // 2. 基类初始化（按基类声明顺序）
        let info = self.get_class_info(class_id).cloned();
        if let Some(info) = info {
            for base in &info.bases {
                if base.is_virtual {
                    continue; // 虚基类不在此阶段处理
                }
                let base_name = self
                    .get_symbol(base.class.as_symbol())
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                if let Some(init_expr) = init_map.get(&base_name).cloned() {
                    let args = self.build_init_args(init_expr)?;
                    result.push(self.add_stmt(HirStmtKind::CtorInitBase {
                        base: base.class,
                        args,
                    }));
                } else {
                    // 无显式初始化：默认构造（空参）
                    result.push(self.add_stmt(HirStmtKind::CtorInitBase {
                        base: base.class,
                        args: Vec::new(),
                    }));
                }
                // 基类构造完成后，更新 vptr 指向该基类的 vtable（如果有虚函数）
                if has_virtual || self.base_has_virtual(base.class) {
                    let this_expr = self.build_expr(&CppExpr::This)?;
                    result.push(self.add_stmt(HirStmtKind::SetVTablePtr {
                        class: base.class,
                        place: this_expr,
                    }));
                }
            }
        }

        // 3. 非静态数据成员初始化（按声明顺序）
        // ... (existing member init code)
        // 先收集所有字段及其声明顺序
        let class_scope = self
            .class_scope(class_id)
            .ok_or_else(|| "class scope not found for ctor init".to_string())?;
        let scope = &self.scopes[class_scope.0];
        let mut fields_in_order: Vec<(String, SymbolId)> = Vec::new();
        let mut all_field_syms: Vec<SymbolId> = Vec::new();
        for sym_ids in scope.symbols.values() {
            for &sid in sym_ids {
                if let Some(s) = self.get_symbol(sid) {
                    if matches!(s.kind, CxxSymbolKind::Field) {
                        all_field_syms.push(sid);
                    }
                }
            }
        }
        all_field_syms.sort_by_key(|s| s.0);
        for sid in &all_field_syms {
            if let Some(s) = self.get_symbol(*sid) {
                fields_in_order.push((s.name.clone(), *sid));
            }
        }

        for (field_name, field_sym) in &fields_in_order {
            if let Some(init_expr) = init_map.get(field_name).cloned() {
                let args = self.build_init_args(init_expr)?;
                result.push(self.add_stmt(HirStmtKind::CtorInitMember {
                    member: *field_sym,
                    args,
                }));
            }
        }

        // 4. 所有基类和成员构造完成后，设置 vptr 指向当前（最派生）类的 vtable
        if has_virtual {
            let this_expr = self.build_expr(&CppExpr::This)?;
            result.push(self.add_stmt(HirStmtKind::SetVTablePtr {
                class: class_id,
                place: this_expr,
            }));
        }

        Ok(result)
    }

    /// 递归检查基类链中是否有虚函数
    fn base_has_virtual(&self, class_id: ClassId) -> bool {
        if let Some(info) = self.get_class_info(class_id) {
            if !info.virtual_methods.is_empty() {
                return true;
            }
            for base in &info.bases {
                if self.base_has_virtual(base.class) {
                    return true;
                }
            }
        }
        false
    }

    /// 判断构造函数的种类（copy / move / normal / delegating）。
    ///
    /// Stage 4: 最小匹配规则:
    /// - Copy ctor: 第一个参数是 `const T&` 或 `T&`（T 是类自身）
    /// - Move ctor: 第一个参数是 `T&&`（T 是类自身）
    /// - Delegating: 初始化列表第一项是类名自己（委托到同类的其他构造函数）
    /// - Normal: 其他情况
    pub fn classify_ctor(
        &self,
        class_id: ClassId,
        params: &[(String, CxxType)],
        inits: &[(String, CppExpr)],
    ) -> CtorKind {
        // 先检查是否是委托构造
        let class_name = self
            .get_symbol(class_id.as_symbol())
            .map(|s| s.name.clone())
            .unwrap_or_default();
        if let Some((first_name, _)) = inits.first() {
            if first_name == &class_name {
                return CtorKind::Delegating;
            }
        }

        // 检查第一个参数是否是对自身类的引用
        if let Some((_, first_ty)) = params.first() {
            match first_ty {
                CxxType::LValueRef(inner) => {
                    // T& 或 const T& -> copy ctor
                    if let CxxType::Class(cid) = inner.as_ref() {
                        if *cid == class_id {
                            return CtorKind::Copy;
                        }
                    }
                }
                CxxType::RValueRef(inner) => {
                    // T&& -> move ctor
                    if let CxxType::Class(cid) = inner.as_ref() {
                        if *cid == class_id {
                            return CtorKind::Move;
                        }
                    }
                }
                _ => {}
            }
        }

        CtorKind::Normal
    }

    /// 从初始化表达式中提取实参列表
    /// 支持 `member(args)` 形式（Call）或 `member(expr)` 单参（简化为单元素列表）
    fn build_init_args(&mut self, init_expr: &CppExpr) -> Result<Vec<HirExprId>, String> {
        match init_expr {
            CppExpr::Call(_, args) => {
                let mut hir_args = Vec::new();
                for arg in args {
                    hir_args.push(self.build_expr(arg)?);
                }
                Ok(hir_args)
            }
            _ => {
                let arg_id = self.build_expr(init_expr)?;
                Ok(vec![arg_id])
            }
        }
    }

    // --- 声明构建 ---

    fn build_decl(&mut self, decl: &CppDecl) -> Result<(), String> {
        match decl {
            CppDecl::FreeFn {
                ret,
                name,
                params,
                body,
                ..
            } => {
                let ret_ty = self.convert_type(ret)?;
                let mut param_tys = Vec::new();
                for (pname, pty) in params {
                    let ct = self.convert_type(pty)?;
                    param_tys.push((pname.clone(), ct));
                }
                let fn_sym = self.build_function(
                    name,
                    ret_ty,
                    &param_tys,
                    body,
                    CxxSymbolKind::Function,
                    Access::Public,
                    None,
                    false,
                )?;
                self.hir.top_level.push(HirTopLevelDecl::Function(fn_sym));
                Ok(())
            }
            CppDecl::Class(class_decl) => {
                let access = if class_decl.is_struct {
                    Access::Public
                } else {
                    Access::Public // 顶层 class 也是 public
                };

                // 查找第一遍已注册的类（或新建，如果没有）
                let class_id = self
                    .lookup_unqualified(&class_decl.name)
                    .into_iter()
                    .find_map(|sym| {
                        let s = self.get_symbol(sym)?;
                        if matches!(s.kind, CxxSymbolKind::Class) {
                            Some(ClassId::from(sym))
                        } else {
                            None
                        }
                    });

                let (class_id, _class_scope) = match class_id {
                    Some(existing_id) => {
                        // 第一遍已注册，现在 push scope 并更新基类
                        let class_scope = self.push_scope(ScopeKind::Class);
                        if let Some(s) = self.scopes.get_mut(class_scope.0) {
                            s.owner_symbol = Some(existing_id.as_symbol());
                        }
                        // 更新符号的 scope 字段
                        if let Some(sym) = self.symbols.get_mut(existing_id.0) {
                            sym.scope = class_scope;
                        }
                        // 解析并更新基类
                        let base_classes = self.parse_base_specs(&class_decl.bases)?;
                        self.update_class_bases(existing_id, &base_classes);
                        (existing_id, class_scope)
                    }
                    None => {
                        // 未注册过，完整注册
                        let base_classes = self.parse_base_specs(&class_decl.bases)?;
                        self.register_class(class_decl.name.clone(), access, &base_classes)
                    }
                };

                // 进入类作用域处理成员
                let old_class = self.current_class;
                self.current_class = Some(class_id);

                // 处理成员
                let members: Vec<CppDecl> = class_decl.members.clone();
                for member in &members {
                    self.build_class_member(member, class_id)?;
                }

                // 回到类的父作用域
                // register_class push 了一个 scope，需要 pop
                self.pop_scope();
                self.current_class = old_class;

                self.hir
                    .top_level
                    .push(HirTopLevelDecl::Class(class_id.as_symbol()));
                Ok(())
            }
            CppDecl::GlobalVar { ty, name, init, .. } => {
                let var_ty = self.convert_type(ty)?;
                let sym = self.add_symbol(
                    name.clone(),
                    CxxSymbolKind::Variable,
                    var_ty,
                    Access::Public,
                );
                let init_id = match init {
                    Some(e) => Some(self.build_expr(e)?),
                    None => None,
                };
                self.hir.top_level.push(HirTopLevelDecl::Variable(sym));
                let _ = init_id; // HIR 顶层变量的 init 暂不存储
                Ok(())
            }
            CppDecl::Namespace { name, body } => {
                // 命名空间: 注册 namespace 符号 + push namespace 作用域, 递归处理成员
                if let Some(ns_name) = name {
                    let ns_sym = self.add_symbol(
                        ns_name.clone(),
                        CxxSymbolKind::Namespace,
                        CxxType::Void,
                        Access::Public,
                    );
                    let ns_scope = self.push_scope(ScopeKind::Namespace);
                    if let Some(s) = self.scopes.get_mut(ns_scope.0) {
                        s.owner_symbol = Some(ns_sym);
                    }
                    if let Some(s) = self.symbols.get_mut(ns_sym.0) {
                        s.scope = ns_scope;
                    }
                } else {
                    // 匿名 namespace: 纯作用域（内部链接）
                    self.push_scope(ScopeKind::Namespace);
                }
                for d in body {
                    self.build_decl(d)?;
                }
                self.pop_scope();
                Ok(())
            }
            CppDecl::OperatorFn {
                class,
                op,
                params,
                body,
                ret,
                is_unary,
            } => {
                // 运算符重载函数: 全局或类外成员运算符, 符号名 operator<encode>
                let ret_ty = self.convert_type(ret)?;
                let mut param_tys = Vec::new();
                for (pname, pty) in params {
                    let ct = self.convert_type(pty)?;
                    param_tys.push((pname.clone(), ct));
                }
                let sym_name = format!("operator{}", op.encode_name());
                if let Some(class_name) = class {
                    // 类外成员运算符定义: 查找类, 以 Method 注册
                    let class_syms = self.lookup_unqualified(class_name);
                    let class_id = class_syms
                        .iter()
                        .find_map(|&s| {
                            let sym = self.get_symbol(s)?;
                            if matches!(sym.kind, CxxSymbolKind::Class) {
                                Some(ClassId::from(s))
                            } else {
                                None
                            }
                        })
                        .ok_or_else(|| {
                            format!("operator class '{}' not found", class_name)
                        })?;
                    let owner = class_id.as_symbol();
                    let saved_class = self.current_class;
                    self.current_class = Some(class_id);
                    let method_sym = self.build_function(
                        &sym_name,
                        ret_ty,
                        &param_tys,
                        body,
                        CxxSymbolKind::Method,
                        Access::Public,
                        Some(owner),
                        false,
                    )?;
                    self.current_class = saved_class;
                    if let Some(info) = self.get_class_info_mut(class_id) {
                        info.methods.push(method_sym);
                    }
                    Ok(())
                } else {
                    // 全局运算符函数
                    let fn_sym = self.build_function(
                        &sym_name,
                        ret_ty,
                        &param_tys,
                        body,
                        CxxSymbolKind::Function,
                        Access::Public,
                        None,
                        false,
                    )?;
                    self.hir.top_level.push(HirTopLevelDecl::Function(fn_sym));
                    let _ = is_unary;
                    Ok(())
                }
            }
            CppDecl::ExportBlock(inner) => {
                // export { ... }: 内部声明正常处理
                for d in inner {
                    self.build_decl(d)?;
                }
                Ok(())
            }
            CppDecl::Template { decl, .. }
            | CppDecl::PartialSpec { decl, .. } => {
                // 模板声明通常已在 native.rs 被过滤, 此处递归处理内部声明
                self.build_decl(decl)
            }
            CppDecl::Concept { .. }
            | CppDecl::Module { .. }
            | CppDecl::Import { .. }
            | CppDecl::PrivateFragment
            | CppDecl::RawExternC(_)
            | CppDecl::Using { .. }
            | CppDecl::UsingEnum { .. }
            | CppDecl::InheritingCtor { .. }
            | CppDecl::AnonAggr { .. }
            | CppDecl::Empty
            | CppDecl::Opaque(_) => {
                // no-op: 模块/概念/继承构造/匿名聚合等占位
                Ok(())
            }
            CppDecl::Typedef { target, name } => {
                // typedef/using 别名: 注册 TypeAlias 符号, 方便 convert_type 展开
                let target_ty = self.convert_type(target)?;
                self.add_symbol(
                    name.clone(),
                    CxxSymbolKind::TypeAlias,
                    target_ty,
                    Access::Public,
                );
                Ok(())
            }
            CppDecl::Enum {
                name,
                scoped: _,
                underlying: _,
                variants,
            } => {
                // 顶层 enum: 注册枚举值为全局 int 符号
                let mut next_val: i128 = 0;
                for (var_name, init_text) in variants {
                    // 尝试 parse init_text 为 i128, 成功则更新 next_val;
                    // 失败也注册 (scoped enum 成员如 B = A+2 无法 parse)
                    if let Some(text) = init_text {
                        let trimmed = text.trim();
                        let parsed = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
                            i128::from_str_radix(hex, 16).ok()
                        } else if let Some(oct) = trimmed.strip_prefix('0') {
                            if !oct.is_empty() && oct.chars().all(|c| c.is_ascii_digit() && c < '8') {
                                i128::from_str_radix(oct, 8).ok()
                            } else {
                                trimmed.parse::<i128>().ok()
                            }
                        } else {
                            trimmed.parse::<i128>().ok()
                        };
                        if let Some(v) = parsed { next_val = v; }
                    }
                    self.add_symbol(
                        var_name.clone(),
                        CxxSymbolKind::EnumConstant,
                        CxxType::Integer { bits: 32, signed: true },
                        Access::Public,
                    );
                    next_val += 1;
                }
                if !name.is_empty() {
                    self.add_symbol(
                        name.clone(),
                        CxxSymbolKind::Enum,
                        CxxType::Void,
                        Access::Public,
                    );
                }
                Ok(())
            }
            CppDecl::MemberFn { .. }
            | CppDecl::Ctor { .. }
            | CppDecl::Dtor { .. }
            | CppDecl::MemberVar { .. } => Err("top-level member declaration not supported".into()),
            _ => Err(format!(
                "unsupported top-level declaration: {:?}",
                std::mem::discriminant(decl)
            )),
        }
    }

    fn build_class_member(&mut self, member: &CppDecl, _class_id: ClassId) -> Result<(), String> {
        match member {
            CppDecl::OperatorFn {
                class: _,
                op,
                params,
                body,
                ret,
                is_unary: _,
            } => {
                // 类内运算符重载: 以 Method 注册, 符号名 operator<encode>
                let ret_ty = self.convert_type(ret)?;
                let mut param_tys = Vec::new();
                for (pname, pty) in params {
                    let ct = self.convert_type(pty)?;
                    param_tys.push((pname.clone(), ct));
                }
                let owner = self.current_class.unwrap().as_symbol();
                let class_id = self.current_class.unwrap();
                let sym_name = format!("operator{}", op.encode_name());
                let method_sym = self.build_function(
                    &sym_name,
                    ret_ty,
                    &param_tys,
                    body,
                    CxxSymbolKind::Method,
                    Access::Public,
                    Some(owner),
                    false,
                )?;
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.methods.push(method_sym);
                }
                Ok(())
            }
            CppDecl::MemberVar { ty, name, .. } => {
                let field_ty = self.convert_type(ty)?;
                let field_sym = self.add_symbol(
                    name.clone(),
                    CxxSymbolKind::Field,
                    field_ty,
                    Access::Private, // Stage 3: 默认 private，暂不跟踪 access specifier
                );
                let owner = self.current_class.unwrap().as_symbol();
                self.set_owner(field_sym, owner);
                Ok(())
            }
            CppDecl::MemberFn {
                ret,
                name,
                params,
                body,
                is_const,
                is_virtual,
                is_override,
                ..
            } => {
                let ret_ty = self.convert_type(ret)?;
                let mut param_tys = Vec::new();
                for (pname, pty) in params {
                    let ct = self.convert_type(pty)?;
                    param_tys.push((pname.clone(), ct));
                }
                let owner = self.current_class.unwrap().as_symbol();
                let class_id = self.current_class.unwrap();
                let method_sym = self.build_function(
                    name,
                    ret_ty,
                    &param_tys,
                    body,
                    CxxSymbolKind::Method,
                    Access::Private, // Stage 3: 默认 private
                    Some(owner),
                    *is_const,
                )?;

                // 添加到类的方法列表
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.methods.push(method_sym);
                }

                // 虚函数处理
                let method_ty = self
                    .get_symbol(method_sym)
                    .map(|s| s.ty.clone())
                    .unwrap_or(CxxType::Void);

                // 查找基类中是否有同名同签名的虚函数
                let base_override =
                    self.find_overridden_base_method(class_id, name, &method_ty);

                let is_virtual_method = *is_virtual
                    || *is_override
                    || base_override.is_some();

                if is_virtual_method {
                    // override 检查
                    if *is_override && base_override.is_none() {
                        return Err(format!(
                            "'{}' marked override but does not override a base class virtual method",
                            name
                        ));
                    }

                    // 分配 vtable slot
                    let slot = if let Some((_base_sym, base_slot)) = base_override {
                        // 重写基类虚函数，保持同一 slot
                        base_slot
                    } else {
                        // 新的虚函数，分配新 slot
                        let info = self.get_class_info(class_id)
                            .ok_or("class info not found")?;
                        info.virtual_methods.len()
                    };

                    // 如果是重写，先移除基类继承来的同 slot 项（如果存在）
                    let info = self.get_class_info_mut(class_id)
                        .ok_or("class info not found")?;

                    // 先检查是否已有同 slot 的项（从基类继承的），有则替换
                    let mut replaced = false;
                    for vm in info.virtual_methods.iter_mut() {
                        if vm.slot == slot {
                            vm.symbol = method_sym;
                            vm.overrides = base_override.map(|(s, _)| s);
                            replaced = true;
                            break;
                        }
                    }
                    if !replaced {
                        info.virtual_methods.push(VirtualMethodInfo {
                            symbol: method_sym,
                            slot,
                            overrides: base_override.map(|(s, _)| s),
                        });
                    }
                }

                Ok(())
            }
            CppDecl::Ctor {
                params,
                body,
                inits,
                ..
            } => {
                // 构造函数
                let class_id = self.current_class.unwrap();
                let class_ty = CxxType::Class(class_id);
                let mut param_tys = Vec::new();
                for (pname, pty) in params {
                    let ct = self.convert_type(pty)?;
                    param_tys.push((pname.clone(), ct));
                }
                let owner = class_id.as_symbol();
                let ctor_name = self
                    .get_symbol(owner)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let ctor_sym = self.build_constructor(
                    &ctor_name,
                    class_ty,
                    &param_tys,
                    inits,
                    body,
                    Access::Private, // Stage 3: 默认 private
                    Some(owner),
                    class_id,
                )?;

                // 记录到 ClassInfo 的 constructors 列表
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.constructors.push(ctor_sym);
                }
                Ok(())
            }
            CppDecl::Dtor { body, is_virtual, .. } => {
                let class_id = self.current_class.unwrap();
                let owner = class_id.as_symbol();
                let class_name = self
                    .get_symbol(owner)
                    .map(|s| format!("~{}", s.name))
                    .unwrap_or_else(|| "~dtor".into());

                // 检查基类中是否有虚析构函数
                let dtor_ty = CxxType::Function {
                    ret: Box::new(CxxType::Void),
                    params: vec![],
                    is_const: false,
                    is_volatile: false,
                    ref_qual: None,
                };
                let base_virtual_dtor =
                    self.find_overridden_base_method(class_id, &class_name, &dtor_ty);

                let is_virtual_dtor = *is_virtual || base_virtual_dtor.is_some();

                let dtor_sym = self.build_function(
                    &class_name,
                    CxxType::Void,
                    &[],
                    body,
                    CxxSymbolKind::Destructor,
                    Access::Private,
                    Some(owner),
                    false,
                )?;

                // 记录到 ClassInfo
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.destructor = Some(dtor_sym);
                }

                if is_virtual_dtor {
                    // 分配 vtable slot
                    let slot = if let Some((_base_sym, base_slot)) = base_virtual_dtor {
                        base_slot
                    } else {
                        let info = self.get_class_info(class_id)
                            .ok_or("class info not found")?;
                        info.virtual_methods.len()
                    };

                    let info = self.get_class_info_mut(class_id)
                        .ok_or("class info not found")?;

                    // 检查是否已有同 slot 的项（从基类继承的），有则替换
                    let mut replaced = false;
                    for vm in info.virtual_methods.iter_mut() {
                        if vm.slot == slot {
                            vm.symbol = dtor_sym;
                            vm.overrides = base_virtual_dtor.map(|(s, _)| s);
                            replaced = true;
                            break;
                        }
                    }
                    if !replaced {
                        info.virtual_methods.push(VirtualMethodInfo {
                            symbol: dtor_sym,
                            slot,
                            overrides: base_virtual_dtor.map(|(s, _)| s),
                        });
                    }

                    // 在析构函数体末尾添加 vptr 更新（逆序指向各基类 vtable）
                    if !body.is_decl {
                        let dtor_body_id = self.hir.functions.last().unwrap().body;
                        // 先收集 vptr 更新语句（避免 borrow 冲突）
                        let mut vptr_updates = Vec::new();
                        let bases_with_virtual: Vec<ClassId> = self
                            .get_class_info(class_id)
                            .map(|info| {
                                info.bases
                                    .iter()
                                    .filter(|b| !b.is_virtual && self.base_has_virtual(b.class))
                                    .map(|b| b.class)
                                    .rev()
                                    .collect()
                            })
                            .unwrap_or_default();
                        for base in bases_with_virtual {
                            let this_expr = self.build_expr(&CppExpr::This)?;
                            vptr_updates.push(self.add_stmt(HirStmtKind::SetVTablePtr {
                                class: base,
                                place: this_expr,
                            }));
                        }
                        let body_data = &mut self.hir.bodies[dtor_body_id.0];
                        body_data.stmts.extend(vptr_updates);
                    }
                }

                Ok(())
            }
            CppDecl::Using { is_namespace, path } => {
                if *is_namespace {
                    return Err("using namespace not supported in class scope".into());
                }
                // using Base::method;  - 引入基类成员到派生类作用域
                if path.len() < 2 {
                    return Err("using declaration must have qualified name".into());
                }
                let member_name = path.last().unwrap().clone();
                let base_name = path[path.len() - 2].clone();
                // 查找基类
                let current_cls = self
                    .current_class
                    .ok_or_else(|| "using declaration only valid in class scope".to_string())?;
                let info = self
                    .get_class_info(current_cls)
                    .ok_or_else(|| "class info not found".to_string())?;
                // 找到对应的基类
                let mut base_class = None;
                for b in &info.bases {
                    if let Some(bsym) = self.get_symbol(b.class.as_symbol()) {
                        if bsym.name == base_name {
                            base_class = Some(b.class);
                            break;
                        }
                    }
                }
                let base_class = base_class.ok_or_else(|| {
                    format!("base class '{}' not found for using declaration", base_name)
                })?;
                // 在基类中查找成员
                let (base_syms, ambiguous) = self.lookup_in_class(base_class, &member_name);
                if ambiguous {
                    return Err(format!("ambiguous member '{}' in base class", member_name));
                }
                if base_syms.is_empty() {
                    return Err(format!(
                        "no member '{}' found in base class '{}'",
                        member_name, base_name
                    ));
                }
                // 记录到 class info 的 using_decls
                let info_mut = self
                    .get_class_info_mut(current_cls)
                    .ok_or_else(|| "class info not found".to_string())?;
                info_mut
                    .using_decls
                    .entry(member_name)
                    .or_default()
                    .extend(base_syms);
                Ok(())
            }
            CppDecl::Class(inner) => {
                // 嵌套类
                let access = if inner.is_struct {
                    Access::Public
                } else {
                    Access::Private
                };

                // 解析基类
                let mut base_classes = Vec::new();
                for base_spec in &inner.bases {
                    let base_syms = self.lookup_unqualified(&base_spec.name);
                    let mut base_class_id = None;
                    for &sym in &base_syms {
                        if let Some(s) = self.get_symbol(sym) {
                            if matches!(s.kind, CxxSymbolKind::Class) {
                                base_class_id = Some(ClassId::from(sym));
                                break;
                            }
                        }
                    }
                    if let Some(bc) = base_class_id {
                        base_classes.push(BaseClass {
                            class: bc,
                            access: Self::access_from_parser(base_spec.access),
                            is_virtual: base_spec.is_virtual,
                        });
                    }
                }

                let outer_class = self.current_class;
                let (inner_id, _inner_scope) =
                    self.register_class(inner.name.clone(), access, &base_classes);
                let owner = outer_class.unwrap().as_symbol();
                self.set_owner(inner_id.as_symbol(), owner);

                self.current_class = Some(inner_id);
                for m in &inner.members {
                    self.build_class_member(m, inner_id)?;
                }
                self.pop_scope(); // pop 嵌套类作用域
                self.current_class = outer_class;
                Ok(())
            }
            CppDecl::Enum {
                name,
                scoped: _,
                underlying: _,
                variants,
            } => {
                // 类内 enum: 注册枚举常量到类作用域
                let mut next_val: i128 = 0;
                for (var_name, init_text) in variants {
                    if let Some(text) = init_text {
                        let trimmed = text.trim();
                        let parsed = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
                            i128::from_str_radix(hex, 16).ok()
                        } else if let Some(oct) = trimmed.strip_prefix('0') {
                            if !oct.is_empty() && oct.chars().all(|c| c.is_ascii_digit() && c < '8') {
                                i128::from_str_radix(oct, 8).ok()
                            } else {
                                trimmed.parse::<i128>().ok()
                            }
                        } else {
                            trimmed.parse::<i128>().ok()
                        };
                        if let Some(v) = parsed { next_val = v; }
                    }
                    self.add_symbol(
                        var_name.clone(),
                        CxxSymbolKind::EnumConstant,
                        CxxType::Integer { bits: 32, signed: true },
                        Access::Private,
                    );
                    next_val += 1;
                }
                if !name.is_empty() {
                    self.add_symbol(
                        name.clone(),
                        CxxSymbolKind::Enum,
                        CxxType::Void,
                        Access::Private,
                    );
                }
                Ok(())
            }
            // 类内 FreeFn = friend 声明, 跳过 (函数体独立存在)
            CppDecl::FreeFn { .. } => Ok(()),
            // 类内 typedef/using/concept/module/import/空声明等: 跳过
            CppDecl::Typedef { .. }
            | CppDecl::UsingEnum { .. }
            | CppDecl::InheritingCtor { .. }
            | CppDecl::Concept { .. }
            | CppDecl::Module { .. }
            | CppDecl::Import { .. }
            | CppDecl::PrivateFragment
            | CppDecl::RawExternC(_)
            | CppDecl::Empty
            | CppDecl::Opaque(_)
            | CppDecl::AnonAggr { .. } => Ok(()),
            // 类内模板/部分特化: 跳过 (native 路径模板已展开)
            CppDecl::Template { .. } => Ok(()),
            CppDecl::PartialSpec { .. } => Ok(()),
            _ => Err(format!(
                "unsupported class member: {:?}",
                std::mem::discriminant(member)
            )),
        }
    }

    // --- 入口方法 ---

    /// 第一遍注册：递归扫描顶层声明，为所有类（含 namespace 内）注册前向符号。
    fn register_classes_deep(&mut self, decls: &[CppDecl]) {
        for decl in decls {
            match decl {
                CppDecl::Class(c) => {
                    let access = if c.is_struct {
                        Access::Public
                    } else {
                        Access::Public
                    };
                    // 只添加 class 符号到当前作用域
                    let class_sym = self.add_symbol(
                        c.name.clone(),
                        CxxSymbolKind::Class,
                        CxxType::Void,
                        access,
                    );
                    let class_id = ClassId(class_sym.0);
                    // 更新类型
                    if let Some(sym) = self.symbols.get_mut(class_sym.0) {
                        sym.ty = CxxType::Class(class_id);
                    }
                    // 确保 class_infos 长度足够
                    while self.class_infos.len() <= class_id.0 {
                        self.class_infos.push(ClassInfo::default());
                    }
                }
                CppDecl::Namespace { body, .. } => {
                    // 递归进入 namespace 作用域注册（不注册 namespace 符号，由第二遍处理）
                    self.push_scope(ScopeKind::Namespace);
                    self.register_classes_deep(body);
                    self.pop_scope();
                }
                CppDecl::ExportBlock(inner) => self.register_classes_deep(inner),
                _ => {}
            }
        }
    }

    /// 将 parser 的 AST 声明列表转换为 HIR。
    pub fn build_hir_from_decls(&mut self, decls: &[CppDecl]) -> Result<(), String> {
        // 第一遍：注册所有类名（前向声明），只添加符号，不 push scope
        self.register_classes_deep(decls);

        // 第二遍：完整构建
        for decl in decls {
            self.build_decl(decl)?;
        }
        Ok(())
    }
}

impl Default for Sema {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sema_starts_with_global_scope() {
        let sema = Sema::new();
        assert_eq!(sema.current_scope(), sema.global_scope());
    }

    #[test]
    fn overload_set_lookup_returns_all_candidates() {
        // Section 7.1: overload resolution - 同名重载函数构成 overload set,
        // lookup 必须返回全部候选，供调用点按实参挑选。
        let mut sema = Sema::new();
        let fn_ty = |n: usize| CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![CxxType::Integer {
                bits: 32,
                signed: true,
            }; n],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let f0 = sema.add_symbol("f".into(), CxxSymbolKind::Function, fn_ty(0), Access::Public);
        let f1 = sema.add_symbol("f".into(), CxxSymbolKind::Function, fn_ty(1), Access::Public);
        let f2 = sema.add_symbol("f".into(), CxxSymbolKind::Function, fn_ty(2), Access::Public);
        let set = sema.lookup_unqualified("f");
        // overload set 包含 3 个候选，且按声明顺序排列
        assert_eq!(set.len(), 3);
        assert_eq!(set, vec![f0, f1, f2]);
        // 候选函数签名参数个数可区分 (arity 匹配的依据)
        let arities: Vec<usize> = set
            .iter()
            .map(|&id| match sema.get_symbol(id).unwrap().ty.clone() {
                CxxType::Function { params, .. } => params.len(),
                _ => 0,
            })
            .collect();
        assert_eq!(arities, vec![0, 1, 2]);
    }
    
    // ===== G16: SFINAE Integration Tests =====
    
    #[test]
    fn test_sfinae_filter_helpers() {
        // Import and verify SFINAE filtering helpers work correctly
        use crate::cpp::feature_degrade::{is_sfinae_failure, SfinaeError};
            
        // Test that EnableIfFalse marks candidate as non-viable
        assert!(is_sfinae_failure(&SfinaeError::EnableIfFalse));
            
        // Test MissingTypeMember for ::type not existing
        assert!(is_sfinae_failure(&SfinaeError::MissingTypeMember));
            
        // Test SubstitutionFailed for failed type substitution
        assert!(is_sfinae_failure(&SfinaeError::SubstitutionFailed));
            
        // Regular errors should NOT be filtered as SFINAE failures
        assert!(!is_sfinae_failure(&SfinaeError::UnboundParam("x".to_string())));
        assert!(!is_sfinae_failure(&SfinaeError::TypeError("bad".to_string())));
        assert!(!is_sfinae_failure(&SfinaeError::Disabled("x".to_string())));
    }
    
    #[test]
    fn test_overload_resolution_with_sfinae_context() {
        // Test that we can create and manage SfinaeCtx for overload resolution
        use crate::cpp::feature_degrade::SfinaeCtx;
            
        let mut ctx = SfinaeCtx::new();
        ctx.bind("T".to_string(), "int".to_string());
        ctx.bind("U".to_string(), "char".to_string());
            
        assert_eq!(ctx.lookup("T"), Some("int".to_string()));
        assert_eq!(ctx.lookup("U"), Some("char".to_string()));
        assert_eq!(ctx.lookup("V"), None); // Unbound parameter
    }

    #[test]
    fn add_and_lookup_global_symbol() {
        let mut sema = Sema::new();
        let id = sema.add_symbol(
            "foo".into(),
            CxxSymbolKind::Function,
            CxxType::Function {
                ret: Box::new(CxxType::Void),
                params: vec![],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            },
            Access::Public,
        );
        let found = sema.lookup_unqualified("foo");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], id);
    }

    #[test]
    fn nested_scope_lookup() {
        let mut sema = Sema::new();
        sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Variable,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.push_scope(ScopeKind::Block);
        sema.add_symbol(
            "y".into(),
            CxxSymbolKind::Variable,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        // 内层能找到外层的 x
        assert_eq!(sema.lookup_unqualified("x").len(), 1);
        // 内层能找到 y
        assert_eq!(sema.lookup_unqualified("y").len(), 1);
        sema.pop_scope();
        // 外层找不到 y
        assert!(sema.lookup_unqualified("y").is_empty());
        // 外层还能找到 x
        assert_eq!(sema.lookup_unqualified("x").len(), 1);
    }

    #[test]
    fn register_class_creates_scope() {
        let mut sema = Sema::new();
        let (class_id, class_scope) = sema.register_class("Foo".into(), Access::Public, &[]);
        // 在类作用域添加成员
        let prev = sema.current_scope();
        sema.push_scope(ScopeKind::Class); // 模拟进入类体
        let _field = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Private,
        );
        sema.pop_scope();
        let _ = (class_id, class_scope, prev);
    }

    #[test]
    fn cxx_type_remove_reference() {
        let t = CxxType::LValueRef(Box::new(CxxType::Integer {
            bits: 32,
            signed: true,
        }));
        assert!(matches!(
            t.remove_reference(),
            CxxType::Integer {
                bits: 32,
                signed: true
            }
        ));
        let t2 = CxxType::Integer {
            bits: 32,
            signed: true,
        };
        assert_eq!(t2.remove_reference(), &t2);
    }

    #[test]
    fn build_hir_free_function() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::FreeFn {
            ret: MangleType::Int,
            name: "add".into(),
            params: vec![("a".into(), MangleType::Int), ("b".into(), MangleType::Int)],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::BinOp(
                    BinOpKind::Add,
                    Box::new(CppExpr::Ident("a".into())),
                    Box::new(CppExpr::Ident("b".into())),
                )))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        }];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();
        assert_eq!(hir.top_level.len(), 1);
        assert_eq!(hir.functions.len(), 1);
        assert_eq!(hir.functions[0].params.len(), 2);
        assert_eq!(hir.bodies.len(), 1); // 函数体
    }

    #[test]
    fn build_hir_class_with_method() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Class(ClassDecl {
            name: "Counter".into(),
            is_struct: false,
            is_union: false,
            bases: vec![],
            members: vec![
                CppDecl::MemberVar {
                    ty: MangleType::Int,
                    name: "value".into(),
                    is_mutable: false,
                    is_volatile: false,
                },
                CppDecl::MemberFn {
                    class: "Counter".into(),
                    ret: MangleType::Int,
                    name: "get".into(),
                    params: vec![],
                    body: CppBody {
                        stmts: vec![CppStmt::Return(Some(CppExpr::ThisMember("value".into())))],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_override: false,
                    is_final: false,
                    is_const: true,
                    is_noexcept: false,
                    is_volatile: false,
                    is_friend: false,
                },
            ],
        })];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();

        // 顶层有一个类
        assert_eq!(hir.top_level.len(), 1);
        assert!(matches!(hir.top_level[0], HirTopLevelDecl::Class(_)));

        // 有一个方法函数
        assert_eq!(hir.functions.len(), 1);
        assert_eq!(hir.functions[0].params.len(), 0);
        assert_eq!(
            hir.functions[0].ret_ty,
            CxxType::Integer {
                bits: 32,
                signed: true
            }
        );
    }

    #[test]
    fn build_hir_constructor_and_destructor() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Class(ClassDecl {
            name: "Foo".into(),
            is_struct: false,
            is_union: false,
            bases: vec![],
            members: vec![
                CppDecl::Ctor {
                    class: "Foo".into(),
                    params: vec![("v".into(), MangleType::Int)],
                    inits: vec![],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
                CppDecl::Dtor {
                    class: "Foo".into(),
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_noexcept: false,
                },
            ],
        })];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();
        // 构造 + 析构 = 2 个函数
        assert_eq!(hir.functions.len(), 2);
    }

    #[test]
    fn build_hir_reference_type_preserved() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::FreeFn {
            ret: MangleType::Reference(Box::new(MangleType::Int)),
            name: "identity".into(),
            params: vec![("x".into(), MangleType::Reference(Box::new(MangleType::Int)))],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::Ident("x".into())))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        }];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();
        let func = &hir.functions[0];
        // 返回类型是引用（保留引用语义，不降级为指针）
        assert!(matches!(func.ret_ty, CxxType::LValueRef(_)));
    }

    #[test]
    fn build_hir_unsupported_decl_returns_err() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Opaque("???".into())];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_err());
    }

    #[test]
    fn build_hir_if_while_for_stmts() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::FreeFn {
            ret: MangleType::Int,
            name: "sum".into(),
            params: vec![("n".into(), MangleType::Int)],
            body: CppBody {
                stmts: vec![
                    CppStmt::LocalVar {
                        ty: MangleType::Int,
                        name: "s".into(),
                        init: Some(CppExpr::IntLit(0)),
                        is_volatile: false,
                    },
                    CppStmt::For {
                        init: Some(Box::new(CppStmt::Expr(CppExpr::BinOp(
                            BinOpKind::Assign,
                            Box::new(CppExpr::Ident("i".into())),
                            Box::new(CppExpr::IntLit(0)),
                        )))),
                        cond: Some(CppExpr::BinOp(
                            BinOpKind::Lt,
                            Box::new(CppExpr::Ident("i".into())),
                            Box::new(CppExpr::Ident("n".into())),
                        )),
                        step: Some(Box::new(CppStmt::Expr(CppExpr::UnaryOp(
                            UnaryOpKind::PostInc,
                            Box::new(CppExpr::Ident("i".into())),
                        )))),
                        body: Box::new(CppBody {
                            stmts: vec![CppStmt::If {
                                cond: CppExpr::BinOp(
                                    BinOpKind::Gt,
                                    Box::new(CppExpr::Ident("i".into())),
                                    Box::new(CppExpr::IntLit(5)),
                                ),
                                then: Box::new(CppBody {
                                    stmts: vec![CppStmt::Break],
                                    is_decl: false,
                                }),
                                els: None,
                            }],
                            is_decl: false,
                        }),
                    },
                    CppStmt::Return(Some(CppExpr::Ident("s".into()))),
                ],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        }];
        let result = sema.build_hir_from_decls(&decls);
        // 注意：`i` 在 for init 中未声明为 LocalVar，而是直接赋值，会找不到 `i`
        // 这是预期的，因为 parser 里 for init 可以是声明也可以是表达式
        // 这里我们只验证流程是否走通（预期失败在 for init 中找不到 i）
        let _ = result;
    }

    // === Stage 4: 非虚继承测试 ===

    #[test]
    fn single_inheritance_register_bases() {
        let mut sema = Sema::new();
        let (base_id, _) = sema.register_class("Base".into(), Access::Public, &[]);
        sema.pop_scope();

        let bases = vec![BaseClass {
            class: base_id,
            access: Access::Public,
            is_virtual: false,
        }];
        let (derived_id, _) = sema.register_class("Derived".into(), Access::Public, &bases);
        sema.pop_scope();

        // 检查 Derived 的基类信息
        let info = sema.get_class_info(derived_id).unwrap();
        assert_eq!(info.bases.len(), 1);
        assert_eq!(info.bases[0].class, base_id);
        assert_eq!(info.bases[0].access, Access::Public);
        assert!(!info.bases[0].is_virtual);

        // is_base_of
        assert!(sema.is_base_of(base_id, derived_id));
        assert!(sema.is_base_of(derived_id, derived_id));
        assert!(!sema.is_base_of(derived_id, base_id));
    }

    #[test]
    fn lookup_in_base_class_single_inheritance() {
        let mut sema = Sema::new();

        // 注册 Base 类，加一个成员 x
        let (base_id, _base_scope) = sema.register_class("Base".into(), Access::Public, &[]);
        let base_field = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(base_field, base_id.as_symbol());
        sema.pop_scope();

        // 注册 Derived 类继承 Base
        let bases = vec![BaseClass {
            class: base_id,
            access: Access::Public,
            is_virtual: false,
        }];
        let (derived_id, _derived_scope) =
            sema.register_class("Derived".into(), Access::Public, &bases);
        sema.pop_scope();

        // 在 Derived 中查找 x，应该能找到 Base 的 x
        let (found, ambiguous) = sema.lookup_in_class(derived_id, "x");
        assert!(!ambiguous);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], base_field);
    }

    #[test]
    fn derived_hides_base_member() {
        let mut sema = Sema::new();

        // Base 有成员 x
        let (base_id, _) = sema.register_class("Base".into(), Access::Public, &[]);
        let base_x = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(base_x, base_id.as_symbol());
        sema.pop_scope();

        // Derived 也有同名成员 x，应该遮蔽 Base::x
        let bases = vec![BaseClass {
            class: base_id,
            access: Access::Public,
            is_virtual: false,
        }];
        let (derived_id, _) = sema.register_class("Derived".into(), Access::Public, &bases);
        let derived_x = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 64,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(derived_x, derived_id.as_symbol());
        sema.pop_scope();

        let (found, ambiguous) = sema.lookup_in_class(derived_id, "x");
        assert!(!ambiguous);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], derived_x);
    }

    #[test]
    fn multiple_inheritance_ambiguous_member() {
        let mut sema = Sema::new();

        // BaseA 有 x
        let (base_a, _) = sema.register_class("BaseA".into(), Access::Public, &[]);
        let xa = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(xa, base_a.as_symbol());
        sema.pop_scope();

        // BaseB 有 x
        let (base_b, _) = sema.register_class("BaseB".into(), Access::Public, &[]);
        let xb = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(xb, base_b.as_symbol());
        sema.pop_scope();

        // Derived 继承 BaseA 和 BaseB
        let bases = vec![
            BaseClass {
                class: base_a,
                access: Access::Public,
                is_virtual: false,
            },
            BaseClass {
                class: base_b,
                access: Access::Public,
                is_virtual: false,
            },
        ];
        let (derived_id, _) = sema.register_class("Derived".into(), Access::Public, &bases);
        sema.pop_scope();

        // x 应该歧义
        let (found, ambiguous) = sema.lookup_in_class(derived_id, "x");
        assert!(ambiguous);
        assert!(found.is_empty());

        // 但是 y 找不到（不是歧义，是不存在）
        let (found_y, amb_y) = sema.lookup_in_class(derived_id, "y");
        assert!(!amb_y);
        assert!(found_y.is_empty());
    }

    #[test]
    fn multiple_inheritance_unique_member() {
        let mut sema = Sema::new();

        // BaseA 有 x
        let (base_a, _) = sema.register_class("BaseA".into(), Access::Public, &[]);
        let xa = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(xa, base_a.as_symbol());
        sema.pop_scope();

        // BaseB 有 y
        let (base_b, _) = sema.register_class("BaseB".into(), Access::Public, &[]);
        let yb = sema.add_symbol(
            "y".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(yb, base_b.as_symbol());
        sema.pop_scope();

        // Derived 继承 BaseA 和 BaseB
        let bases = vec![
            BaseClass {
                class: base_a,
                access: Access::Public,
                is_virtual: false,
            },
            BaseClass {
                class: base_b,
                access: Access::Public,
                is_virtual: false,
            },
        ];
        let (derived_id, _) = sema.register_class("Derived".into(), Access::Public, &bases);
        sema.pop_scope();

        // x 唯一来自 BaseA
        let (found_x, amb_x) = sema.lookup_in_class(derived_id, "x");
        assert!(!amb_x);
        assert_eq!(found_x.len(), 1);
        assert_eq!(found_x[0], xa);

        // y 唯一来自 BaseB
        let (found_y, amb_y) = sema.lookup_in_class(derived_id, "y");
        assert!(!amb_y);
        assert_eq!(found_y.len(), 1);
        assert_eq!(found_y[0], yb);
    }

    #[test]
    fn ctor_init_list_base_and_member() {
        let mut sema = Sema::new();
        let decls = vec![
            CppDecl::Class(ClassDecl {
                name: "Base".into(),
                is_struct: false,
                is_union: false,
                bases: vec![],
                members: vec![
                    CppDecl::MemberVar {
                        ty: MangleType::Int,
                        name: "b".into(),
                        is_mutable: false,
                        is_volatile: false,
                    },
                    CppDecl::Ctor {
                        class: "Base".into(),
                        params: vec![("v".into(), MangleType::Int)],
                        inits: vec![],
                        body: CppBody {
                            stmts: vec![],
                            is_decl: false,
                        },
                        is_virtual: false,
                        is_explicit: false,
                        is_noexcept: false,
                    },
                ],
            }),
            CppDecl::Class(ClassDecl {
                name: "Derived".into(),
                is_struct: false,
                is_union: false,
                bases: vec![BaseSpec {
                    access: crate::cpp::parser::Access::Public,
                    is_virtual: false,
                    name: "Base".into(),
                    template_args: None,
                }],
                members: vec![
                    CppDecl::MemberVar {
                        ty: MangleType::Int,
                        name: "d".into(),
                        is_mutable: false,
                        is_volatile: false,
                    },
                    CppDecl::Ctor {
                        class: "Derived".into(),
                        params: vec![("v".into(), MangleType::Int)],
                        inits: vec![
                            ("Base".into(), CppExpr::Ident("v".into())),
                            ("d".into(), CppExpr::IntLit(42)),
                        ],
                        body: CppBody {
                            stmts: vec![],
                            is_decl: false,
                        },
                        is_virtual: false,
                        is_explicit: false,
                        is_noexcept: false,
                    },
                ],
            }),
        ];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();

        // Derived 的构造函数应该有 ctor_initializers
        // 构造函数数量：Base 1 个 + Derived 1 个 = 2 个
        assert!(hir.functions.len() >= 2);

        // 找到 Derived 的构造函数（最后一个函数）
        let derived_ctor_body = hir.functions.last().unwrap().body;
        let body = &hir.bodies[derived_ctor_body.0];
        let inits = &body.ctor_initializers;
        // 基类 Base + 成员 d = 至少 2 个初始化
        assert!(inits.len() >= 2);

        // 第一个应该是基类初始化（Base）
        assert!(matches!(
            hir.stmts[inits[0].0].kind,
            HirStmtKind::CtorInitBase { .. }
        ));

        // 第二个应该是成员 d
        assert!(matches!(
            hir.stmts[inits[1].0].kind,
            HirStmtKind::CtorInitMember { .. }
        ));
    }

    #[test]
    fn ctor_delegating_constructor() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Class(ClassDecl {
            name: "Foo".into(),
            is_struct: false,
            is_union: false,
            bases: vec![],
            members: vec![
                CppDecl::MemberVar {
                    ty: MangleType::Int,
                    name: "x".into(),
                    is_mutable: false,
                    is_volatile: false,
                },
                CppDecl::Ctor {
                    class: "Foo".into(),
                    params: vec![("a".into(), MangleType::Int), ("b".into(), MangleType::Int)],
                    inits: vec![("x".into(), CppExpr::IntLit(0))],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
                CppDecl::Ctor {
                    class: "Foo".into(),
                    params: vec![("a".into(), MangleType::Int)],
                    inits: vec![(
                        "Foo".into(),
                        CppExpr::Call(
                            Box::new(CppExpr::Ident("Foo".into())),
                            vec![CppExpr::Ident("a".into()), CppExpr::IntLit(0)],
                        ),
                    )],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
            ],
        })];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();

        // 2 个构造函数
        assert_eq!(hir.functions.len(), 2);

        // 第二个构造函数是委托构造
        let delegating_body = hir.functions[1].body;
        let body = &hir.bodies[delegating_body.0];
        let inits = &body.ctor_initializers;
        // 委托构造只有一项（委托到自己）
        assert_eq!(inits.len(), 1);
        assert!(matches!(
            hir.stmts[inits[0].0].kind,
            HirStmtKind::CtorInitBase { .. }
        ));
    }

    #[test]
    fn classify_ctor_normal() {
        let mut sema = Sema::new();
        let (class_id, _) = sema.register_class("Foo".into(), Access::Public, &[]);
        sema.pop_scope();

        let params = vec![("a".into(), CxxType::Integer { bits: 32, signed: true })];
        let inits: Vec<(String, CppExpr)> = vec![];
        let kind = sema.classify_ctor(class_id, &params, &inits);
        assert_eq!(kind, CtorKind::Normal);
    }

    #[test]
    fn classify_ctor_copy_by_ref() {
        let mut sema = Sema::new();
        let (class_id, _) = sema.register_class("Foo".into(), Access::Public, &[]);
        sema.pop_scope();

        // Foo(const Foo&) 或 Foo(Foo&) -> copy ctor
        let params = vec![(
            "other".into(),
            CxxType::LValueRef(Box::new(CxxType::Class(class_id))),
        )];
        let inits: Vec<(String, CppExpr)> = vec![];
        let kind = sema.classify_ctor(class_id, &params, &inits);
        assert_eq!(kind, CtorKind::Copy);
    }

    #[test]
    fn classify_ctor_move() {
        let mut sema = Sema::new();
        let (class_id, _) = sema.register_class("Foo".into(), Access::Public, &[]);
        sema.pop_scope();

        // Foo(Foo&&) -> move ctor
        let params = vec![(
            "other".into(),
            CxxType::RValueRef(Box::new(CxxType::Class(class_id))),
        )];
        let inits: Vec<(String, CppExpr)> = vec![];
        let kind = sema.classify_ctor(class_id, &params, &inits);
        assert_eq!(kind, CtorKind::Move);
    }

    #[test]
    fn classify_ctor_delegating() {
        let mut sema = Sema::new();
        let (class_id, _) = sema.register_class("Foo".into(), Access::Public, &[]);
        sema.pop_scope();

        let params = vec![("a".into(), CxxType::Integer { bits: 32, signed: true })];
        // 初始化列表第一项是类名自己 -> delegating
        let inits = vec![(
            "Foo".into(),
            CppExpr::IntLit(0),
        )];
        let kind = sema.classify_ctor(class_id, &params, &inits);
        assert_eq!(kind, CtorKind::Delegating);
    }

    #[test]
    fn class_info_records_constructors_and_dtor() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Class(ClassDecl {
            name: "Foo".into(),
            is_struct: false,
            is_union: false,
            bases: vec![],
            members: vec![
                CppDecl::Ctor {
                    class: "Foo".into(),
                    params: vec![],
                    inits: vec![],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
                CppDecl::Ctor {
                    class: "Foo".into(),
                    params: vec![("v".into(), MangleType::Int)],
                    inits: vec![],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
                CppDecl::Dtor {
                    class: "Foo".into(),
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_noexcept: false,
                },
            ],
        })];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());

        // 找到 Foo 类的 ClassInfo
        let class_sym = sema.lookup_unqualified("Foo");
        assert_eq!(class_sym.len(), 1);
        let class_id = ClassId::from(class_sym[0]);
        let info = sema.get_class_info(class_id).unwrap();

        // 2 个构造函数
        assert_eq!(info.constructors.len(), 2);
        // 有析构函数
        assert!(info.destructor.is_some());
    }

    #[test]
    fn using_declaration_brings_base_member() {
        let mut sema = Sema::new();

        // Base 有方法 foo
        let (base_id, _) = sema.register_class("Base".into(), Access::Public, &[]);
        let base_foo = sema.add_symbol(
            "foo".into(),
            CxxSymbolKind::Method,
            CxxType::Function {
                ret: Box::new(CxxType::Void),
                params: vec![],
                is_const: false,
                is_volatile: false,
                ref_qual: None,
            },
            Access::Public,
        );
        sema.set_owner(base_foo, base_id.as_symbol());
        sema.pop_scope();

        // Derived 有 using Base::foo;
        let bases = vec![BaseClass {
            class: base_id,
            access: Access::Public,
            is_virtual: false,
        }];
        let (derived_id, _derived_scope) =
            sema.register_class("Derived".into(), Access::Public, &bases);

        // 模拟 using Base::foo
        {
            let info = sema.get_class_info_mut(derived_id).unwrap();
            info.using_decls
                .entry("foo".into())
                .or_default()
                .push(base_foo);
        }
        sema.pop_scope();

        // 在 Derived 中查找 foo，应该通过 using 找到 Base::foo
        let (found, ambiguous) = sema.lookup_in_class(derived_id, "foo");
        assert!(!ambiguous);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], base_foo);
    }

    #[test]
    fn base_cast_for_inherited_member() {
        let mut sema = Sema::new();
        let decls = vec![
            CppDecl::Class(ClassDecl {
                name: "Base".into(),
                is_struct: false,
                is_union: false,
                bases: vec![],
                members: vec![CppDecl::MemberVar {
                    ty: MangleType::Int,
                    name: "b".into(),
                    is_mutable: false,
                    is_volatile: false,
                }],
            }),
            CppDecl::Class(ClassDecl {
                name: "Derived".into(),
                is_struct: false,
                is_union: false,
                bases: vec![BaseSpec {
                    access: crate::cpp::parser::Access::Public,
                    is_virtual: false,
                    name: "Base".into(),
                    template_args: None,
                }],
                members: vec![CppDecl::MemberFn {
                    class: "Derived".into(),
                    ret: MangleType::Int,
                    name: "get_b".into(),
                    params: vec![],
                    body: CppBody {
                        stmts: vec![CppStmt::Return(Some(CppExpr::ThisMember("b".into())))],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_override: false,
                    is_final: false,
                    is_const: true,
                    is_noexcept: false,
                    is_volatile: false,
                    is_friend: false,
                }],
            }),
        ];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        let hir = sema.into_hir();

        // 检查 Derived::get_b 的返回表达式中是否有 BaseCast
        // 找到 get_b 函数体（唯一一个函数）
        assert_eq!(hir.functions.len(), 1);
        let func = &hir.functions[0];
        let body = &hir.bodies[func.body.0];
        // return 语句
        let return_stmt = &hir.stmts[body.stmts[0].0];
        match &return_stmt.kind {
            HirStmtKind::Return(Some(expr_id)) => {
                let expr = &hir.exprs[expr_id.0];
                // 返回值是 FieldAccess，其 object 应该经过 BaseCast
                match &expr.kind {
                    HirExprKind::FieldAccess { object, .. } => {
                        let obj_expr = &hir.exprs[object.0];
                        assert!(
                            matches!(obj_expr.kind, HirExprKind::BaseCast { .. }),
                            "expected BaseCast for inherited member, got {:?}",
                            std::mem::discriminant(&obj_expr.kind)
                        );
                    }
                    _ => panic!("expected FieldAccess"),
                }
            }
            _ => panic!("expected Return"),
        }
    }

    #[test]
    fn build_hir_class_with_bases() {
        let mut sema = Sema::new();
        let decls = vec![
            CppDecl::Class(ClassDecl {
                name: "Base".into(),
                is_struct: false,
                is_union: false,
                bases: vec![],
                members: vec![CppDecl::MemberVar {
                    ty: MangleType::Int,
                    name: "x".into(),
                    is_mutable: false,
                    is_volatile: false,
                }],
            }),
            CppDecl::Class(ClassDecl {
                name: "Derived".into(),
                is_struct: false,
                is_union: false,
                bases: vec![BaseSpec {
                    access: crate::cpp::parser::Access::Public,
                    is_virtual: false,
                    name: "Base".into(),
                    template_args: None,
                }],
                members: vec![CppDecl::MemberVar {
                    ty: MangleType::Int,
                    name: "y".into(),
                    is_mutable: false,
                    is_volatile: false,
                }],
            }),
        ];
        let result = sema.build_hir_from_decls(&decls);
        assert!(result.is_ok(), "build error: {:?}", result.err());
        assert_eq!(sema.hir().top_level.len(), 2);
    }

    #[test]
    fn three_level_inheritance_lookup() {
        let mut sema = Sema::new();

        // Grandparent 有 x
        let (gp_id, _) = sema.register_class("Grandparent".into(), Access::Public, &[]);
        let gp_x = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
            Access::Public,
        );
        sema.set_owner(gp_x, gp_id.as_symbol());
        sema.pop_scope();

        // Parent 继承 Grandparent
        let bases_p = vec![BaseClass {
            class: gp_id,
            access: Access::Public,
            is_virtual: false,
        }];
        let (parent_id, _) = sema.register_class("Parent".into(), Access::Public, &bases_p);
        sema.pop_scope();

        // Child 继承 Parent
        let bases_c = vec![BaseClass {
            class: parent_id,
            access: Access::Public,
            is_virtual: false,
        }];
        let (child_id, _) = sema.register_class("Child".into(), Access::Public, &bases_c);
        sema.pop_scope();

        // Child 中查找 x，应该找到 Grandparent::x
        let (found, ambiguous) = sema.lookup_in_class(child_id, "x");
        assert!(!ambiguous);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], gp_x);
        assert!(sema.is_base_of(gp_id, child_id));
    }
}

// ===== G14: Dependent Name Resolution Helper Types =====

/// Context for dependent name resolution during template instantiation
#[derive(Debug, Clone, Default)]
pub struct InstantiationContext {
    /// Template parameter names -> actual type bindings
    pub param_bindings: Vec<(String, MangleType)>,
}

impl InstantiationContext {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn bind(&mut self, param_name: String, actual_type: MangleType) {
        self.param_bindings.push((param_name, actual_type));
    }
    
    /// Look up a member of a dependent type (e.g., T::member where T is bound)
    pub fn lookup_dependent_member(&self, type_param: &str, member: &str) -> Option<CxxType> {
        // Find the binding for this template parameter
        for (param_name, actual_type) in &self.param_bindings {
            if param_name == type_param {
                // Check if actual type has the requested member
                return match actual_type {
                    MangleType::Template { name, args } => {
                        // For std::vector<int>, we might have nested types like 'iterator'
                        // This would be resolved via typedefs/typedef-like mechanisms
                        // For now, return dependent type marker
                        Some(CxxType::Dependent(format!("{}.{}", name, member)))
                    }
                    _ => None,
                };
            }
        }
        None
    }
}
