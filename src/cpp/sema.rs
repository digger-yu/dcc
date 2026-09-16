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

use crate::cpp::hir::{HirExprId, HirId, HirTranslationUnit};

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
    Integer { bits: u16, signed: bool },
    /// 浮点型: bits 位数 (32 = float, 64 = double)
    Float { bits: u16 },
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
            CxxType::Void
                | CxxType::Bool
                | CxxType::Integer { .. }
                | CxxType::Float { .. }
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
    /// 输出的 HIR
    hir: HirTranslationUnit,
    /// 下一个 HIR id
    next_hir_id: usize,
}

impl Sema {
    /// 创建新的语义分析器，包含一个全局作用域。
    pub fn new() -> Self {
        let mut scopes = Vec::new();
        let global = Scope::new(ScopeId(0), ScopeKind::Global, None);
        scopes.push(global);
        let global_scope = ScopeId(0);
        Self {
            symbols: Vec::new(),
            scopes,
            current_scope: global_scope,
            global_scope,
            hir: HirTranslationUnit::new(),
            next_hir_id: 0,
        }
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
        let sym = CxxSymbol {
            id,
            name: name.clone(),
            kind,
            ty,
            owner: None,
            access,
            linkage_name: None,
            scope,
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

    // --- class 操作 ---

    /// 注册一个类，返回 ClassId 和类作用域
    pub fn register_class(&mut self, name: String, access: Access) -> (ClassId, ScopeId) {
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
        (ClassId(class_sym.0), class_scope)
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

    fn alloc_hir_id(&mut self) -> HirId {
        let id = HirId(self.next_hir_id);
        self.next_hir_id += 1;
        id
    }

    fn alloc_expr_id(&mut self) -> HirExprId {
        HirExprId(self.alloc_hir_id().0)
    }

    /// 获取构建好的 HIR
    pub fn hir(&self) -> &HirTranslationUnit {
        &self.hir
    }

    /// 消费 Sema，返回 HIR
    pub fn into_hir(self) -> HirTranslationUnit {
        self.hir
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
            CxxType::Integer { bits: 32, signed: true },
            Access::Public,
        );
        sema.push_scope(ScopeKind::Block);
        sema.add_symbol(
            "y".into(),
            CxxSymbolKind::Variable,
            CxxType::Integer { bits: 32, signed: true },
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
        let (class_id, class_scope) = sema.register_class("Foo".into(), Access::Public);
        // 在类作用域添加成员
        let prev = sema.current_scope();
        sema.push_scope(ScopeKind::Class); // 模拟进入类体
        let _field = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Field,
            CxxType::Integer { bits: 32, signed: true },
            Access::Private,
        );
        sema.pop_scope();
        let _ = (class_id, class_scope, prev);
    }

    #[test]
    fn cxx_type_remove_reference() {
        let t = CxxType::LValueRef(Box::new(CxxType::Integer { bits: 32, signed: true }));
        assert!(matches!(
            t.remove_reference(),
            CxxType::Integer { bits: 32, signed: true }
        ));
        let t2 = CxxType::Integer { bits: 32, signed: true };
        assert_eq!(t2.remove_reference(), &t2);
    }
}
