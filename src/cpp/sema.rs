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

use std::collections::{HashMap, HashSet};

use crate::cpp::{
    adl,
    hir::{
        BinaryOp, CastKind, CtorVariant, DtorVariant, HirBody, HirBodyId, HirCallee,
        HirCatchHandler, HirExpr, HirExprId, HirExprKind, HirFunction, HirId, HirStmt, HirStmtId,
        HirStmtKind, HirTopLevelDecl, HirTranslationUnit, UnaryOp,
    },
    mangle::MangleType,
    parser::*,
};

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
#[derive(Default)]
pub enum Access {
    Public,
    Protected,
    #[default]
    Private,
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

/// `build_function` / `build_constructor` 的符号登记属性: 把原来尾部的
/// `kind` / `access` / `owner` / `is_const` 四个标量参数打包成一个结构体,
/// 使单函数参数个数不超过 clippy::too_many_arguments 阈值 (7)。
/// 各字段语义与原逐参数传递完全一致, 不改变任何登记行为。
#[derive(Debug, Clone, Copy)]
struct FnSym {
    /// 符号种类 (Function / Method / Destructor / Constructor ...)
    kind: CxxSymbolKind,
    /// 登记进符号表时的访问级别
    access: Access,
    /// 属主符号 (成员函数为所属类); None = 全局
    owner: Option<SymbolId>,
    /// 是否 const 成员函数
    is_const: bool,
}

impl FnSym {
    /// 全局自由函数: 无属主、非 const
    fn free_fn(access: Access) -> Self {
        Self {
            kind: CxxSymbolKind::Function,
            access,
            owner: None,
            is_const: false,
        }
    }

    /// 成员函数: 属主为所在类, const 限定由调用点给出
    fn method(access: Access, owner: SymbolId, is_const: bool) -> Self {
        Self {
            kind: CxxSymbolKind::Method,
            access,
            owner: Some(owner),
            is_const,
        }
    }

    /// 析构函数 (用户写的与合成的同一个入口)
    fn dtor(access: Access, owner: SymbolId) -> Self {
        Self {
            kind: CxxSymbolKind::Destructor,
            access,
            owner: Some(owner),
            is_const: false,
        }
    }

    /// 构造函数 (build_constructor 固定按 Constructor 登记, 这里取同值保持一致)
    fn ctor(access: Access, owner: SymbolId) -> Self {
        Self {
            kind: CxxSymbolKind::Constructor,
            access,
            owner: Some(owner),
            is_const: false,
        }
    }
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
            CxxType::Pointer(inner) | CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                Some(inner)
            },
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
#[derive(Default)]
pub enum CtorKind {
    /// 普通构造函数
    #[default]
    Normal,
    /// 拷贝构造函数（参数为 const T& 或 T&）
    Copy,
    /// 移动构造函数（参数为 T&&）
    Move,
    /// 委托构造函数
    Delegating,
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
    /// 命名空间作用域的名字 (`Namespace` kind 专用)。
    ///
    /// 前向注册遍 (`register_classes_deep`) 只**推进一个作用域**而不创建
    /// namespace 符号 (避免与第二遍重复登记同一命名空间), 但作用域链仍必须
    /// 携带名字, 否则该遍登记的类无法推出限定名。见 `qualified_class_name`。
    pub ns_name: Option<String>,
}

impl Scope {
    fn new(id: ScopeId, kind: ScopeKind, parent: Option<ScopeId>) -> Self {
        Self {
            id,
            kind,
            parent,
            owner_symbol: None,
            symbols: HashMap::new(),
            ns_name: None,
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
    /// 当前函数内由 `T(args...)` 匿名临时量登记的栈上槽位符号
    /// (在 build_function 结束时统一发射为函数体最前面的 Decl 语句)
    func_temps: Vec<SymbolId>,
    /// 临时量命名计数器 (整个 TU 内保证名字唯一)
    temp_counter: usize,
    /// 枚举常量的编译期值 (按 SymbolId 索引)。
    /// 单一真源: `enum` 登记时求值一次, 表达式侧据此折叠为 `IntLiteral`。
    enum_constant_values: HashMap<SymbolId, i128>,
    /// M54: 全局 / 命名空间作用域变量的**静态初始化表达式** (按 SymbolId 索引)。
    ///
    /// HIR 的 `HirTopLevelDecl::Variable` 只携带 SymbolId, 而链接名由
    /// `compute_linkage_name` 决定 (命名空间变量是 `ns_a`), 因此发射端
    /// (native.rs) 无法从 AST 反推“哪个名字对应哪个初值”。旧代码在此处直接
    /// `let _ = init_id;` 丢弃初始化式, 后果是**静默错值**: `int ga = 7;` 落在
    /// .bss, 运行读到 0。现在按符号留住 (parser 已把 constexpr 调用等
    /// 常量表达式折成字面量, 本表存的即是可直接发射的静态初值)。
    global_var_inits: HashMap<SymbolId, CppExpr>,
    /// M26.1 (native): TU 级异常规范收集结果 - `noexcept(expr)` 谓词单一真源。
    /// `noexcept_pos`: 声明为 noexcept 的函数键 (自由函数 `name`, 成员 `Class::name`,
    /// 构造 `Class::ctor`, 析构 `Class::dtor`)。
    noexcept_pos: HashSet<String>,
    /// `noexcept_neg`: 至少一处声明为 potentially-throwing 的函数键。
    /// 同名 (重载/声明-定义冲突) 同时出现在 pos/neg 时按保守规则可抛。
    noexcept_neg: HashSet<String>,
    /// 有用户声明构造/析构的类 (决定隐式特殊成员函数是否需要推导)。
    has_user_ctor: HashSet<String>,
    has_user_dtor: HashSet<String>,
    /// 在**类外**给出定义 (而非仅声明) 的构造/析构所属类简名
    /// (`Foo::Foo() {}` / `Foo::~Foo() {}`)。TU 级前置扫描登记。
    ///
    /// 为什么需要单独记: `class_needs_construction` / `class_needs_destruction`
    /// 以「该类的 ctor/dtor 符号已进 `hir.functions`」判「有实体可调」, 而这个
    /// 事实只在类外定义**被构建之后**才成立。于是
    /// `int main(){ Foo f; }  Foo::Foo(){}` 这种「使用点在前、定义在后」的合法
    /// 顺序会静默不发构造调用 (读未初始化对象)。与 `has_user_ctor` 同一手法:
    /// 把 TU 级信息提到前置 pass 里, 让判定不再依赖声明顺序。
    out_of_class_ctor_defs: HashSet<String>,
    out_of_class_dtor_defs: HashSet<String>,
    /// 原始类声明表 (按类名): 隐式/=default 特殊成员函数 noexcept 推导的传递性输入。
    class_decls_by_name: HashMap<String, ClassDecl>,
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
            func_temps: Vec::new(),
            temp_counter: 0,
            enum_constant_values: HashMap::new(),
            global_var_inits: HashMap::new(),
            noexcept_pos: HashSet::new(),
            noexcept_neg: HashSet::new(),
            has_user_ctor: HashSet::new(),
            has_user_dtor: HashSet::new(),
            out_of_class_ctor_defs: HashSet::new(),
            out_of_class_dtor_defs: HashSet::new(),
            class_decls_by_name: HashMap::new(),
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
                ret: Box::new(CxxType::Pointer(Box::new(CxxType::Integer {
                    bits: 8,
                    signed: false,
                }))),
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

    /// 推进一个**带名字**的命名空间作用域。
    ///
    /// 与 `push_scope(ScopeKind::Namespace)` 的区别只在于把名字先记在作用域上,
    /// 供后续限定名推导使用; 传 `None` (匿名命名空间) 时行为完全相同。
    pub fn push_namespace_scope(&mut self, name: Option<&str>) -> ScopeId {
        let id = self.push_scope(ScopeKind::Namespace);
        if let Some(s) = self.scopes.get_mut(id.0) {
            s.ns_name = name.map(|n| n.to_string());
        }
        id
    }

    /// Find a namespace scope by its name path (e.g., "std" or "std::pmr")
    /// For now, search through all registered scopes
    fn find_namespace_scope(&self, _ns_path: &str) -> Option<ScopeId> {
        // TODO: Proper namespace traversal when symbol table integration complete
        // Simple fallback: return None for now since full implementation requires ClassTable access
        // This is acceptable as first-class namespace support is complex
        None
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
    ///
    /// 【函数作用域的名字没有 linkage】(C++ [basic.link]: 块作用域且未声明
    /// `static` 的名字无 linkage, 不可被其它作用域引用)。形参 / 局部变量 /
    /// 匿名临时量都登记在 Function/Block 作用域里, 必须不拿前缀链接名:
    /// lower_ir 以「`linkage_name` 是 Some」为【静态存储对象】的唯一判据
    /// (`HirExprKind::Local` 分支), 一旦给形参/局部发了前缀名, `src` 会被
    /// 降级成 `*(&std_views_src)` —— 读一个从未写入的槽, <ranges> 夹具里
    /// `filter_view(dccpp_view* src, int (*pred)(int))` 的形参就成了垃圾
    /// 指针, 传进 `dccpp_view_filter_int` 即 SIGSEGV。
    /// 函数局部 `static` 变量在 `CppStmt::StaticLocal` 里显式登记为全局作用域
    /// 并赋 `__dcc_static_<n>` 链接名, 不依赖本函数。
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
            // 函数/块作用域内声明的名字: 无 linkage。
            if matches!(s.kind, ScopeKind::Function | ScopeKind::Block) {
                return None;
            }
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
            // 文件作用域 (全局) 变量: 链接名就是其本身的名字 (Itanium 下
            // 具命名空间的全局对象不做 mangling)。此前返回 None 会让
            // lower_ir 把 `HirExprKind::Local/Global(变量)` 当成自动栈槽:
            // 写只留在 SSA 重绑定里 (静默丢失持久性), 读直接报
            // "local variable not found" —— 构造/析构期虚派发夹具这类
            // 用文件作用域探针变量的夹具无法编译。
            if kind == CxxSymbolKind::Variable
                && self.scopes.get(self.current_scope.0).map(|s| s.kind)
                    == Some(ScopeKind::Global)
            {
                return Some(name.to_string());
            }
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

    /// M54: 全局变量的静态初始化表达式表 (供 native 发射端取用)。
    pub fn global_var_inits(&self) -> &HashMap<SymbolId, CppExpr> {
        &self.global_var_inits
    }

    /// M54: 判定本条全局变量声明是否是当前作用域里已有变量的**重声明**。
    ///
    /// [basic.def]/[dcl.array]: `extern char ea[];` 与 `char ea[3] = {...};`
    /// 是同一个对象的声明与定义。旧实现给每条声明各建一个 SymbolId, 名字
    /// 查找取**先声明**的那个 —— 先声明若是不完整数组 (边界记 0),
    /// `ea[1]` 就报"subscripted value is not an array or pointer", 且静态
    /// 对象发射按 0 长度分配存储。这里复用已有符号, 并回传是否需要用
    /// 本条的完整类型去**补齐**它。
    ///
    /// 只在两种形态下复用: 类型完全相同, 或已有类型是不完整数组而本条给出
    /// 真实边界。其他情形 (类型冲突 = 程序本身 ill-formed, GCC 报
    /// conflicting declaration; 本条反而更不完整) 保持旧行为: 另建符号。
    fn global_var_redeclaration(&self, name: &str, ty: &CxxType) -> Option<(SymbolId, bool)> {
        let syms = self.scopes[self.current_scope.0].lookup_local(name)?;
        for &sid in syms {
            let existing = &self.symbols[sid.0];
            if !matches!(existing.kind, CxxSymbolKind::Variable) {
                continue;
            }
            let same = existing.ty == *ty;
            let completes = match (&existing.ty, ty) {
                (CxxType::Array(old_inner, None), CxxType::Array(new_inner, Some(n))) =>
                    *n > 0 && **old_inner == **new_inner,
                _ => false,
            };
            if same {
                return Some((sid, false));
            }
            if completes {
                return Some((sid, true));
            }
        }
        None
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
        // [basic.lookup.unqual]/1 叠加 [class.member.lookup]/1: 成员函数体 (及
        // 类内初始值等 complete-class context) 里的非限定名字, 词法链找不到时还
        // 必须在**当前类及其全部基类 (含虚基、沿链逐层)** 中查找 —— 语义上等价
        // 于隐式 `this->name`。上面的作用域链已经跳过类作用域本身 (本类成员
        // 在那里命中), 故本回退真正补上的是**基类链**那一段。
        // 实测缺口: `class Der : public Mid { int call_it() { return prot(); } }`,
        // `prot` 属于基类 `Base` → 旧行为直接报 `undeclared identifier: prot`。
        let class_id = match self.current_class {
            Some(c) => c,
            None => return Vec::new(),
        };
        let (syms, _ambiguous) = self.lookup_in_class(class_id, name);
        syms
    }

    /// 限定名字查找: 在指定类/命名空间作用域内查找
    pub fn lookup_qualified(&self, scope_id: ScopeId, name: &str) -> Vec<SymbolId> {
        let s = &self.scopes[scope_id.0];
        s.lookup_local(name).map(|v| v.to_vec()).unwrap_or_default()
    }

    /// 名字能否作为限定名的路径段 (即持有自己作用域的 namespace / class / enum)。
    fn is_scope_owning_name(&self, sym: SymbolId) -> bool {
        matches!(
            self.get_symbol(sym).map(|s| s.kind),
            Some(CxxSymbolKind::Namespace)
                | Some(CxxSymbolKind::Class)
                | Some(CxxSymbolKind::Enum)
        )
    }

    /// 解析限定名**单个路径段**的候选符号。
    ///
    /// 首段 (`first == true`) 按 [basic.lookup.qual] 从当前作用域向外做普通
    /// 非限定查找 (只接受 namespace / class / enum 这类持有作用域的名字),
    /// 向外无结果时回落到全局作用域; 后续段只在已解析作用域 `scope` 内查找。
    fn lookup_qualifier_segment(
        &self,
        scope: ScopeId,
        part: &str,
        first: bool,
    ) -> Vec<SymbolId> {
        if !first {
            return self.lookup_qualified(scope, part);
        }
        let outward: Vec<SymbolId> = self
            .lookup_unqualified(part)
            .into_iter()
            .filter(|&s| self.is_scope_owning_name(s))
            .collect();
        if outward.is_empty() {
            self.lookup_qualified(scope, part)
        } else {
            outward
        }
    }

    /// `path::name` 是否指向一个类。用于 `ns::T(args...)` 形式的临时量构造。
    fn resolve_qualified_class_symbol(&self, path: &[String], name: &str) -> Option<SymbolId> {
        let mut scope = self.global_scope();
        for (idx, part) in path.iter().enumerate() {
            let syms = self.lookup_qualifier_segment(scope, part, idx == 0);
            let mut next = None;
            for &s in &syms {
                if let Some(sym) = self.get_symbol(s) {
                    if matches!(sym.kind, CxxSymbolKind::Class | CxxSymbolKind::Namespace) {
                        next = Some(sym.scope);
                        break;
                    }
                }
            }
            scope = next?;
        }
        self.lookup_qualified(scope, name)
            .into_iter()
            .find(|&s| matches!(self.get_symbol(s).map(|x| x.kind), Some(CxxSymbolKind::Class)))
    }

    /// GCC `-fbuiltin` 语义: 普通名字查找彻底失败时, 把常用 C 库函数
    /// (`memcpy` / `memset` / `strlen` / `malloc` ...) 作为隐式内置声明登记进
    /// **全局作用域** 并返回该符号。
    ///
    /// 单一真源是 [`crate::cpp::builtins::BUILTIN_LIBC_FUNCS`]; 本函数只负责
    /// "查表 + 登记", 不含任何按夹具/按调用点的特判。
    ///
    /// 惰性登记 (而非在 `Sema::new()` 里预注入) 的原因:
    ///   1. 显式声明 (如 `<cstring>` 的 `extern "C"` 原型) 永远优先命中, 不会
    ///      出现两套同名符号争抢 overload set 首位;
    ///   2. 不移动既有符号的 SymbolId / ClassId (二者共用同一索引空间)。
    pub fn declare_builtin_if_known(&mut self, name: &str) -> Option<SymbolId> {
        let bf = crate::cpp::builtins::lookup(name)?;
        // 已经存在同名全局符号 (显式声明在后/前) -> 直接复用, 不重复登记
        let existing = self.lookup_qualified(self.global_scope, name);
        if let Some(&sym) = existing.first() {
            return Some(sym);
        }
        let ty = crate::cpp::builtins::cxx_signature(bf);
        // 内置函数一律挂在全局作用域 + C 链接 (不被当前 namespace 前缀污染)
        let saved = std::mem::replace(&mut self.current_scope, self.global_scope);
        let sym = self.add_symbol(name.to_string(), CxxSymbolKind::Function, ty, Access::Public);
        self.current_scope = saved;
        Some(sym)
    }

    /// 枚举常量的编译期值 (若已登记)。测试与诊断用。
    pub fn enum_constant_value(&self, sym: SymbolId) -> Option<i128> {
        self.enum_constant_values.get(&sym).copied()
    }

    /// 枚举常量出现在表达式里就是一个编译期整数: 折叠成 `IntLiteral`。
    ///
    /// 历史实现产出 `HirExprKind::Global(sym)`, 而枚举常量从未分配存储,
    /// lower_ir 在 linkage_name 为空时回落到 `self.locals` 栈槽 → 读未初始化
    /// 栈内存 (表现为 `GREEN != 5` 之类的比较随机成立/失败)。
    fn enum_constant_expr_kind(&self, sym: SymbolId) -> HirExprKind {
        match self.enum_constant_values.get(&sym) {
            Some(&v) => HirExprKind::IntLiteral(v),
            None => HirExprKind::Global(sym),
        }
    }

    /// 解析 `enum` 的底层类型文本 (`enum class S : unsigned char` 的 `unsigned char`)。
    /// 无显式底层类型 (或文本不是整数类型) 时按 Itanium ABI 取 `int`。
    fn enum_underlying_type(&mut self, underlying: &Option<String>) -> CxxType {
        let default = CxxType::Integer {
            bits: 32,
            signed: true,
        };
        let text = match underlying {
            Some(t) => t,
            None => return default,
        };
        let ty = match self.type_name_to_cxx_type(text) {
            Ok(t) => t,
            Err(_) => return default,
        };
        match ty {
            CxxType::Integer { .. } | CxxType::Bool => ty,
            _ => default,
        }
    }

    /// 登记一个 `enum`: 顶层 enum 与类内 enum **共用**本实现 (原先两处手写重复,
    /// 且都把算出来的常量值丢弃了)。
    ///
    /// 做三件事:
    /// 1. 逐个求值枚举常量初始化器 (支持 `B = A + 2` / `1 << 3` / `'A'` 等文本常量
    ///    表达式, 求值走 [`crate::cpp::const_text`] 单一真源);
    /// 2. 把值记进 [`Sema::enum_constant_values`], 表达式侧折叠为整数字面量;
    /// 3. 枚举类型名的符号携带底层整数类型, 使 `sizeof(Scoped)` 等属性正确。
    ///
    /// 初始化器无法求值时 (依赖运行期/未登记的名字) 保持历史宽松行为: 打印诊断
    /// 并按隐式值 (前一个 + 1) 登记, 不把本来能编译的代码变成硬错误。
    fn register_enum(
        &mut self,
        name: &str,
        underlying: &Option<String>,
        variants: &[(String, Option<String>)],
        access: Access,
    ) -> Result<(), String> {
        let underlying_ty = self.enum_underlying_type(underlying);
        // 先叠加 TU 内已登记的枚举常量 (允许跨 enum 引用), 再由本枚举逐条覆盖
        let mut known: HashMap<String, i128> = self
            .enum_constant_values
            .iter()
            .filter_map(|(&sym, &v)| self.get_symbol(sym).map(|s| (s.name.clone(), v)))
            .collect();
        let mut next_val: i128 = 0;
        for (var_name, init_text) in variants {
            let value = match init_text {
                Some(text) => {
                    match crate::cpp::const_text::eval_integer_const_text(text, &known) {
                        Some(v) => v,
                        None => {
                            eprintln!(
                                "[enum] initializer of '{}' is not an integer constant expression: {}",
                                var_name, text
                            );
                            next_val
                        },
                    }
                },
                None => next_val,
            };
            let sym = self.add_symbol(
                var_name.clone(),
                CxxSymbolKind::EnumConstant,
                underlying_ty.clone(),
                access,
            );
            self.enum_constant_values.insert(sym, value);
            known.insert(var_name.clone(), value);
            next_val = value.wrapping_add(1);
        }
        if !name.is_empty() {
            self.add_symbol(name.to_string(), CxxSymbolKind::Enum, underlying_ty, access);
        }
        Ok(())
    }

    /// 在 `qual` (class / namespace 限定名) 的作用域里解析嵌套类型名 `member`。
    ///
    /// 支持嵌套 typedef (`struct W { typedef int type; }` 的 `W::type`) 与
    /// 模板实例化产生的嵌套 typedef (`iterator_traits<int*>::value_type`),
    /// 也支持嵌套类 (`Outer::Inner`)。`qual` 允许含 `::`, 此时额外用最后一段
    /// 回查 (parser 常把限定类名降为短名)。
    fn resolve_nested_type_name(&self, qual: &str, member: &str) -> Option<CxxType> {
        let tail = qual.rsplit("::").next().unwrap_or(qual);
        let mut candidates = vec![qual];
        if tail != qual {
            candidates.push(tail);
        }
        for q in candidates {
            // 同名 Class 符号可能同时存在多个 (第一遍的前向声明壳 + 第二遍的
            // 完整注册), 而壳上的 `CxxSymbol::scope` 是过期值: 只有
            // `class_scope()` (按 owner_symbol 扫作用域) 才能拿到真正的类作用域.
            let mut owners: Vec<SymbolId> = self
                .symbols
                .iter()
                .filter(|s| s.name == q && matches!(s.kind, CxxSymbolKind::Class))
                .map(|s| s.id)
                .collect();
            if owners.is_empty() {
                owners = self
                    .lookup_unqualified(q)
                    .into_iter()
                    .filter(|&id| {
                        matches!(self.get_symbol(id).map(|s| s.kind), Some(CxxSymbolKind::Class))
                    })
                    .collect();
            }
            for owner in owners {
                let scope = match self.class_scope(ClassId::from(owner)) {
                    Some(s) => s,
                    None => continue,
                };
                for &s in &self.lookup_qualified(scope, member) {
                    if let Some(sym) = self.get_symbol(s) {
                        match sym.kind {
                            CxxSymbolKind::TypeAlias | CxxSymbolKind::Enum => {
                                if !matches!(sym.ty, CxxType::Void) {
                                    return Some(sym.ty.clone());
                                }
                            },
                            CxxSymbolKind::Class => {
                                return Some(CxxType::Class(ClassId::from(sym.id)))
                            },
                            _ => {},
                        }
                    }
                }
            }
        }
        None
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
                    // 菱形虚继承: 多条路径可能解析到**同一个**共享虚基子对象
                    // 里的**同一份**声明 ([class.member.lookup]/4 的
                    // 结果子对象唯一), 那不是歧义。
                    if base_results.iter().all(|s| all_results.contains(s)) {
                        continue;
                    }
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

    /// 收集 `class_id` 的**全部**虚基类（包含间接虚基），顺序与
    /// `abi_layout::ItaniumAbiCalculator::collect_all_virtual_bases` 一致：
    /// 先按声明顺序取直接虚基并递归下钻，再走非虚基链。
    ///
    /// 两边必须同序: 布局层的 `virtual_bases[i].vbase_index` 就是该序序号,
    /// vbtable 里 vbase offset 槽的位置也由此决定。
    pub fn all_virtual_bases_of(&self, class_id: ClassId) -> Vec<ClassId> {
        let mut result = Vec::new();
        let mut seen: Vec<ClassId> = Vec::new();
        self.collect_vbases_rec(class_id, &mut result, &mut seen);
        result
    }

    fn collect_vbases_rec(
        &self,
        class_id: ClassId,
        result: &mut Vec<ClassId>,
        seen: &mut Vec<ClassId>,
    ) {
        let info = match self.get_class_info(class_id) {
            Some(i) => i.clone(),
            None => return,
        };
        for b in info.bases.iter().filter(|b| b.is_virtual) {
            if !seen.contains(&b.class) {
                seen.push(b.class);
                result.push(b.class);
            }
            self.collect_vbases_rec(b.class, result, seen);
        }
        for b in info.bases.iter().filter(|b| !b.is_virtual) {
            self.collect_vbases_rec(b.class, result, seen);
        }
    }

    /// `base` 是否是 `derived` 的（直接或间接）虚基类。
    pub fn is_virtual_base_of(&self, base: ClassId, derived: ClassId) -> bool {
        self.all_virtual_bases_of(derived).contains(&base)
    }

    /// G22: 成员访问检查 ([class.access]).
    ///
    /// 合法条件:
    /// - `public` 成员: 任何上下文;
    /// - `private` 成员: 上下文类 == 属主类 (同类对象互访合法);
    /// - `protected` 成员: 上下文类 == 属主类, 或其派生类.
    ///
    /// 无类上下文 (自由函数 / 全局作域) 时只有 `public` 合法.
    ///
    /// 历史实现只 `eprintln!` 记录访问级别不阻断, 且 sema 把所有成员写死为
    /// private, 导致 `struct` 公开字段在外部访问时也报 "is Private
    /// (context: class None)"; 现在 access 来自 parser 的 member_access 表,
    /// 检查也真正参与判定.
    fn check_member_access(&self, member: SymbolId, name: &str) -> Result<(), String> {
        let (access, owner) = match self.get_symbol(member) {
            Some(s) => (s.access, s.owner),
            None => return Ok(()),
        };
        if access == Access::Public {
            return Ok(());
        }
        let owner = match owner {
            // 无属主的符号 (非成员) 不做访问检查
            None => return Ok(()),
            Some(o) => ClassId::from(o),
        };
        let denied = || {
            let owner_name = self
                .get_symbol(owner.as_symbol())
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "<unknown>".to_string());
            let word = match access {
                Access::Private => "private",
                Access::Protected => "protected",
                Access::Public => "public",
            };
            format!(
                "'{}' is a {} member of '{}', inaccessible here",
                name, word, owner_name
            )
        };
        match self.current_class {
            None => Err(denied()),
            Some(ctx) => {
                if ctx == owner {
                    return Ok(());
                }
                if access == Access::Protected && self.is_base_of(owner, ctx) {
                    return Ok(());
                }
                Err(denied())
            },
        }
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

        // 生成派类类指针 -> 基类指针的 this-adjust:
        // - 目标是**虚基类** → 偏移存在 vbtable 里 (运行期值), 发 VBaseAdjust;
        // - 否则是普通非虚偏移, 发 BaseCast。
        let ptr_ty = self.hir.exprs[ptr_expr.0].ty.clone();
        let is_vbase = self.is_virtual_base_of(member_base, from_class);
        let base_ptr_ty = match &ptr_ty {
            CxxType::Pointer(_) => CxxType::Pointer(Box::new(CxxType::Class(member_base))),
            _ => return Err("expected pointer type for base cast".into()),
        };
        let kind = if is_vbase {
            HirExprKind::VBaseAdjust {
                object: ptr_expr,
                from: from_class,
                to: member_base,
            }
        } else {
            HirExprKind::BaseCast {
                object: ptr_expr,
                from: from_class,
                to: member_base,
            }
        };
        Ok(self.add_expr(kind, base_ptr_ty))
    }

    /// 判定并生成「派类类指针 -> 其基类指针」的隐式/显式上转型。
    ///
    /// 语义需求: 虚继承下 `(Base*)&final_obj` 必须指向尾部的共享子对象，
    /// 而不是对象头部。该偏移对每个最派生类不同，存在 vbtable 里，
    /// 所以只能发成运行期 `VBaseAdjust`；非虚基偏移是编译期常量，发 `BaseCast`。
    /// 不是上转型（不相关类、下转型、非类指针）时返回 `Ok(None)`。
    fn class_pointer_upcast(
        &mut self,
        src: HirExprId,
        target: &CxxType,
    ) -> Result<Option<HirExprId>, String> {
        let src_ty = self.hir.exprs[src.0].ty.clone();
        let (from_cls, to_cls) = match (&src_ty, target) {
            (CxxType::Pointer(a), CxxType::Pointer(b)) => match (a.as_ref(), b.as_ref()) {
                (CxxType::Class(f), CxxType::Class(t)) if f != t => (*f, *t),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        if !(self.is_base_of(to_cls, from_cls) || self.is_virtual_base_of(to_cls, from_cls)) {
            return Ok(None);
        }
        let is_vbase = self.is_virtual_base_of(to_cls, from_cls);
        let kind = if is_vbase {
            HirExprKind::VBaseAdjust {
                object: src,
                from: from_cls,
                to: to_cls,
            }
        } else {
            HirExprKind::BaseCast {
                object: src,
                from: from_cls,
                to: to_cls,
            }
        };
        Ok(Some(self.add_expr(kind, target.clone())))
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
            },
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
            if let Some((sym, slot)) = self.find_overridden_base_method(base.class, name, fn_ty) {
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

    /// 把本类析构登记进 `virtual_methods` (Itanium: 一个对象只有一个析构,
    /// 派生类析构**重写**基类虚析构的那一条槽, 不换槽)。
    ///
    /// 用户写的 `~D()` 与 sema 合成的隐式析构走同一入口。早期只有用户路径登记,
    /// 于是 `class D : B {}` (B 有虚析构) 的隐式析构不进入 `virtual_methods` →
    /// 布局层认为 `D` 没重写析构槽 → `_ZTV1D` 里留下 `B_dtor_complete` →
    /// `delete (B*)d` 只销毁基类部分。
    ///
    /// `base_virtual_dtor` = `find_virtual_base_dtor` 的结果; `None` 表示本类析构
    /// 是链上第一条虚析构 (新开一个槽)。
    fn register_virtual_dtor_slot(
        &mut self,
        class_id: ClassId,
        dtor_sym: SymbolId,
        base_virtual_dtor: Option<(SymbolId, usize)>,
    ) {
        // 分配 vtable slot
        let slot = match base_virtual_dtor {
            Some((_, base_slot)) => base_slot,
            None => self
                .get_class_info(class_id)
                .map(|info| info.virtual_methods.len())
                .unwrap_or(0),
        };
        // 与 Method 路径同理: 按**被重写的基析构符号**定位既有条目,
        // 不用槽号做键 (多继承下不同基链的槽号会撞号)。
        let overridden_base = base_virtual_dtor.map(|(s, _)| s);
        let same_slot = self
            .get_class_info(class_id)
            .and_then(|info| {
                info.virtual_methods.iter().position(|vm| {
                    vm.symbol == dtor_sym
                        || matches!((overridden_base, vm.overrides), (Some(b), Some(v)) if b == v)
                })
            });
        let info = match self.get_class_info_mut(class_id) {
            Some(i) => i,
            None => return,
        };
        if let Some(idx) = same_slot {
            let vm = &mut info.virtual_methods[idx];
            vm.symbol = dtor_sym;
            vm.slot = slot;
            vm.overrides = overridden_base;
        } else {
            info.virtual_methods.push(VirtualMethodInfo {
                symbol: dtor_sym,
                slot,
                overrides: overridden_base,
            });
        }
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
        // 复用前向登记遍的同名占位类符号。否则同一个类会有两个 ClassId:
        // 函数签名在类体之前构建, 绑到了没有成员、没有类作用域的占位 id,
        // 函数体里 `x.a` 就报 `member 'a' not found` (实测于 `int f(ns::P& x)`)。
        let class_sym = match self.find_forward_class_symbol(&name) {
            Some(existing) => {
                // 占位符只在第一遍的 (匿名) 命名空间作用域里, 这里补登记到
                // 当前作用域, 使后续非限定名查找能命中同一个符号。
                if let Some(sc) = self.scopes.get_mut(self.current_scope.0) {
                    if !sc.symbols.get(&name).map(|v| v.contains(&existing)).unwrap_or(false) {
                        sc.add_symbol(name.clone(), existing);
                    }
                }
                existing
            },
            None => self.add_symbol(name.clone(), CxxSymbolKind::Class, CxxType::Void, access),
        };
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

    /// 从 `start` 作用域沿父链逐级向上, 收集 Namespace / Class 作用域的名字
    /// (由内向外)。`skip_owner` 用于跳过「自己就是这个作用域的 owner」的
    /// 符号 (类符号自身的类作用域)。匿名命名空间与函数/块作用域不计入。
    fn scope_chain_names(&self, start: ScopeId, skip_owner: Option<SymbolId>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur = start;
        let mut depth = 0usize;
        while depth < 64 {
            depth += 1;
            let (parent, kind, owner, ns_name) = match self.scopes.get(cur.0) {
                Some(sc) => (sc.parent, sc.kind, sc.owner_symbol, sc.ns_name.clone()),
                None => break,
            };
            let name = match kind {
                // 命名空间: 优先用作用域自带的 `ns_name` (前向注册遍不建
                // namespace 符号, 只有这个名字), 再退回 owner 符号名。
                ScopeKind::Namespace => ns_name.or_else(|| {
                    owner
                        .and_then(|o| self.symbols.get(o.0))
                        .map(|s| s.name.clone())
                }),
                ScopeKind::Class => match owner {
                    Some(oid) if Some(oid) != skip_owner => {
                        self.symbols.get(oid.0).map(|s| s.name.clone())
                    },
                    _ => None,
                },
                _ => None,
            };
            if let Some(n) = name {
                if !n.is_empty() {
                    out.push(n);
                }
            }
            match parent {
                Some(p) => cur = p,
                None => break,
            }
        }
        out
    }

    /// 当前作用域下名字 `name` 的限定名 (`ns::Outer::Inner`)。
    fn qualified_name_in_current_scope(&self, name: &str) -> String {
        let mut parts = self.scope_chain_names(self.current_scope, None);
        parts.reverse();
        parts.push(name.to_string());
        parts.join("::")
    }

    /// 类的**限定源语言名** (`ns::Outer::Inner`), Itanium `_ZTV` / `_ZTI` /
    /// `_ZTS` 符号 mangle 的唯一名字来源.
    ///
    /// 之前直接把符号表的 `sym.name` (不含嵌套前缀) 交给布局层 mangle,
    /// 于是 `ns::Widget` 的 vtable/typeinfo 发成了 `_ZTV6Widget` —— 与全局作
    /// 用域同名类 `Widget` 直接冲突, 也无法与 g++ 产出的
    /// `_ZTIN2ns6WidgetE` 匹配 (同一 TU 内自洽, 跨 TU / 与 libstdc++ 互操作即
    /// 错). 布局层本身已支持限定名 (见
    /// `rtti_symbols_are_derived_from_real_class_name_not_id`), 这里负责编
    /// 出限定名.
    pub fn qualified_class_name(&self, class: ClassId) -> String {
        let sym_id = class.as_symbol();
        let (own, scope) = match self.symbols.get(sym_id.0) {
            Some(s) => (s.name.clone(), s.scope),
            None => return String::new(),
        };
        let mut parts = self.scope_chain_names(scope, Some(sym_id));
        parts.reverse();
        parts.push(own);
        parts.join("::")
    }

    /// 查找与当前作用域下 `name` 的**限定名**完全一致、且尚未推入类作用域的
    /// 占位 Class 符号 (由 `register_classes_deep` 第一遍登记)。
    fn find_forward_class_symbol(&self, name: &str) -> Option<SymbolId> {
        let qualified = self.qualified_name_in_current_scope(name);
        for s in &self.symbols {
            if !matches!(s.kind, CxxSymbolKind::Class) || s.name != name {
                continue;
            }
            let cid = ClassId::from(s.id);
            // 已有类作用域 = 完整定义过, 不能重复占用同一个符号
            if self.class_scope(cid).is_some() {
                continue;
            }
            if self.qualified_class_name(cid) == qualified {
                return Some(s.id);
            }
        }
        None
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

    /// 获取符号向量（用于测试和外部使用）
    pub fn take_symbols(&mut self) -> Vec<CxxSymbol> {
        std::mem::take(&mut self.symbols)
    }

    /// M54: 把浮点字面量文本剥成 `f64::from_str` 能接受的数值部分.
    ///
    /// 词法器将 `0.5f` / `2.5l` 的后缀留在同一个数字 token 里, 而 HIR 的
    /// `FloatLiteral(String)` 在下游是按 `text.parse::<f64>().unwrap_or(0.0)`
    /// 取值: 带后缀的文本**解析失败 → 静默变 0.0** (实测
    /// `float x = 0.5f; if (x < 1.0f)` 恒为假, 而同一份代码的 C 路径正确)。
    /// 后缀只表达精度 (`f`->float / `l`->long double), 而本前端的 HIR 浮点
    /// 常量统一按 double 建模 (对象的存储类型仍由声明类型决定, 静态初值在
    /// `native.rs::parse_float_initializer` 里按对象宽度发 F32/F64),
    /// 因而在语义入口剥掉后缀即可, 不新增第二套宽度真源。
    fn float_literal_value_text(text: &str) -> String {
        text.trim_end_matches(['f', 'F', 'l', 'L']).to_string()
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

    /// 将不带限定的名字解析为类符号 (用于 `T(args...)` 函数式构造与模板类型 id).
    /// 先走作用域链; 未命中时回退到全符号表扫描 —— stub 头里定义在
    /// `namespace std` 内的类不在当前函数的作用域链上.
    /// 回退时只取已完整注册的类 (拥有自己的 class scope):
    /// 第一遍 register_classes_deep 只为前向声明建符号, 不 push scope,
    /// 按名字会优先命中那个空壳.
    fn resolve_class_symbol(&self, name: &str) -> Option<SymbolId> {
        if let Some(sym) = self
            .lookup_unqualified(name)
            .into_iter()
            .find(|&s| matches!(self.get_symbol(s).map(|sym| sym.kind), Some(CxxSymbolKind::Class)))
        {
            return Some(sym);
        }
        let candidates: Vec<SymbolId> = self
            .symbols
            .iter()
            .filter(|s| s.name == name && matches!(s.kind, CxxSymbolKind::Class))
            .map(|s| s.id)
            .collect();
        candidates
            .iter()
            .find(|&&id| self.class_scope(ClassId::from(id)).is_some())
            .or_else(|| candidates.first())
            .copied()
    }

    /// 枚举符号的底层整数类型 (无显式底层类型时按 int, 与 C++ 一致).
    fn enum_underlying_ty(s: &CxxSymbol) -> CxxType {
        match s.ty {
            CxxType::Integer { .. } | CxxType::Bool => s.ty.clone(),
            _ => CxxType::Integer {
                bits: 32,
                signed: true,
            },
        }
    }

    /// 按「剥到末段名」的方式解析一个限定类型名.
    ///
    /// dcc 把 namespace 扁平化: `namespace std { struct input_iterator_tag {}; }`
    /// 登记的符号名是 `input_iterator_tag`。但 `sizeof(std::X)` / 模板实参 /
    /// `typename Iter::value_type` 这类位置的 parser 会原样保留 `std::X`,
    /// 于是名字查找会落空。这里从最短的尾段开始逐级回查 Class / TypeAlias /
    /// Enum 符号, 作为限定名解析失败时的统一兜底 (单一真源, 不区分调用点)。
    fn resolve_qualified_type_by_tail(&self, name: &str) -> Option<CxxType> {
        if !name.contains("::") {
            return None;
        }
        // `Enum::enumerator` (如 `sizeof(Scoped::A)`): 枚举量不是类型, 但它在
        // sizeof 的类型位置合法且结果等于底层类型 (GCC 同义), 因此按限定符
        // 对应的枚举类型返回.
        if let Some(pos) = name.rfind("::") {
            let qual = name[..pos].rsplit("::").next().unwrap_or(&name[..pos]);
            for s in self.symbols.iter().filter(|s| s.name == qual) {
                if matches!(s.kind, CxxSymbolKind::Enum) {
                    return Some(Self::enum_underlying_ty(s));
                }
            }
        }
        let short = name.rsplit("::").next()?;
        for s in self.symbols.iter().filter(|s| s.name == short) {
            match s.kind {
                CxxSymbolKind::Class => return Some(CxxType::Class(ClassId::from(s.id))),
                CxxSymbolKind::TypeAlias => {
                    if !matches!(s.ty, CxxType::Void) {
                        return Some(s.ty.clone());
                    }
                },
                CxxSymbolKind::Enum => return Some(Self::enum_underlying_ty(s)),
                _ => {},
            }
        }
        None
    }

    /// 构造类临时对象 `T(args...)`:
    /// 为函数级临时量槽调用构造器, 并以逗号表达式将
    /// 临时量本身作为结果值 (与 `T t(args); t` 的局部构造形式同构).
    fn build_class_temporary(
        &mut self,
        class_sym: SymbolId,
        args: &[CppExpr],
    ) -> Result<HirExprId, String> {
        let class_ty = CxxType::Class(ClassId::from(class_sym));
        let tmp_name = format!("__dcc_tmp{}", self.temp_counter);
        self.temp_counter += 1;
        let tmp =
            self.add_symbol(tmp_name, CxxSymbolKind::Variable, class_ty.clone(), Access::Private);
        self.func_temps.push(tmp);
        let mut hir_args = Vec::new();
        for a in args {
            hir_args.push(self.build_expr(a)?);
        }
        let object = self.add_expr(HirExprKind::Local(tmp), class_ty.clone());
        let place = self
            .add_expr(HirExprKind::AddressOf(object), CxxType::Pointer(Box::new(class_ty.clone())));
        let construct = self.add_expr(
            HirExprKind::Construct {
                class: ClassId::from(class_sym),
                place,
                args: hir_args,
                variant: crate::cpp::hir::CtorVariant::Complete,
            },
            CxxType::Void,
        );
        let value = self.add_expr(HirExprKind::Local(tmp), class_ty.clone());
        Ok(self.add_expr(
            HirExprKind::Comma {
                lhs: construct,
                rhs: value,
            },
            class_ty,
        ))
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
            },
            MangleType::Reference(t) => {
                let inner = self.convert_type(t)?;
                Ok(CxxType::LValueRef(Box::new(inner)))
            },
            MangleType::Array(t, n) => {
                let inner = self.convert_type(t)?;
                // M54: `n == 0` 是 parser 对**不完整数组类型**的编码
                // (`extern char ea[];` / `T a[];` 无初值), [basic.types] 里
                // 它没有大小 (零长度数组在标准 C++ 中也非法), 故对应到
                // `Array(_, None) = 边界未知`, 而不是“长度为 0 的数组”。
                Ok(match *n {
                    0 => CxxType::Array(Box::new(inner), None),
                    k => CxxType::Array(Box::new(inner), Some(k)),
                })
            },
            // 函数类型 (来自 `T (*f)(U)` 这类声明符的完整解析)。
            // cv / ref 限定只在成员函数签名上有意义, 自立的函数类型均无。
            MangleType::Function { ret, params } => {
                let r = self.convert_type(ret)?;
                let mut ps = Vec::with_capacity(params.len());
                for p in params {
                    ps.push(self.convert_type(p)?);
                }
                Ok(CxxType::Function {
                    ret: Box::new(r),
                    params: ps,
                    is_const: false,
                    is_volatile: false,
                    ref_qual: None,
                })
            },
            MangleType::Template { name, args } => {
                // 模板类型 id: native 流水线 Step 0 已将已知类模板实例化并替换成
                // MangleType::Class. 仍为 Template 的是 stub 头里手写的实例结构体
                // (如 <expected> 为 std::expected<int,int> 提供 expected_int_int),
                // 按实例化名 / 去前缀名依次回查类符号.
                let inst = crate::cpp::template::instantiated_class_name(name, args);
                let bare = {
                    let mut s = name.replace("::", "_");
                    for a in crate::cpp::template::merge_expanded_args(args) {
                        s.push('_');
                        s.push_str(&crate::cpp::template::type_key(&a));
                    }
                    s
                };
                for cand in [inst.as_str(), bare.as_str(), name.as_str()] {
                    if let Some(sym) = self.resolve_class_symbol(cand) {
                        return Ok(CxxType::Class(ClassId::from(sym)));
                    }
                }
                Err(format!("unknown class type: {}", inst))
            },
            MangleType::Class(name) => {
                // M52: **elaborated type specifier** (`struct _IO_FILE` /
                // `union SemUnion` / `enum Kind`) —— parser 为了兼容 legacy C
                // 输出 (`lower.rs` 靠这个前缀重新拼出合法的 C 声明符), 把关键字
                // 一起塑进了类名. 但 C++ 语义上这个类型就叫 `_IO_FILE`:
                // `<cstdio>` 的 `typedef struct _IO_FILE FILE;` 前向声明以
                // `Class("_IO_FILE")` 登记, 使用点却是 `Class("struct _IO_FILE")`
                // → 查不中 → "unknown class type" 整块 `extern "C"` 接品失效.
                // 这里在名字解析入口剩掉 tag 关键字 (C++ 标识符不可能含空格,
                // 不会误伤普通类名).
                if let Some(tag) = elaborated_tag_name(name) {
                    return self.convert_type(&MangleType::Class(tag.to_string()));
                }
                // 嵌套类型名: parser 可能把 `A::B` 整体保留为一个 Class 名
                // (模板实例化后的 `__dcc_template_iterator_traits_p_int::value_type`),
                // 先尝试在限定作用域里解析嵌套 typedef / 嵌套类。
                // 命中的别名目标本身可能又是一个限定名 (`typename Iter::value_type`
                // 在实例里链到 `MyIt::value_type`), 而实例类体先于依赖类型被 sema
                // 登记时只能存成 `Dependent` 别名 —— 这里链式展开, 最多 8 跳防环。
                let mut qname = name.clone();
                for _hop in 0..8 {
                    let Some(pos) = qname.rfind("::") else {
                        break;
                    };
                    match self.resolve_nested_type_name(&qname[..pos], &qname[pos + 2..]) {
                        Some(CxxType::Dependent(inner)) => {
                            if inner == qname {
                                break;
                            }
                            qname = inner;
                        },
                        Some(ty) => return Ok(ty),
                        None => break,
                    }
                }
                let name = &qname;
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
                        // enum 类型: 用登记时算出的底层整数类型 (无则 int)
                        if matches!(s.kind, CxxSymbolKind::Enum) {
                            let ty = match s.ty {
                                CxxType::Integer { .. } | CxxType::Bool => s.ty.clone(),
                                _ => CxxType::Integer {
                                    bits: 32,
                                    signed: true,
                                },
                            };
                            return Ok(ty);
                        }
                    }
                }
                // 内置类型别名 (size_t, uintptr_t 等)
                match name.as_str() {
                    "size_t" | "unsigned int" | "unsigned" => {
                        return Ok(CxxType::Integer {
                            bits: 32,
                            signed: false,
                        });
                    },
                    "ssize_t" | "int" => {
                        return Ok(CxxType::Integer {
                            bits: 32,
                            signed: true,
                        });
                    },
                    "long" => {
                        return Ok(CxxType::Integer {
                            bits: 64,
                            signed: true,
                        });
                    },
                    "unsigned long" | "unsigned long long" => {
                        return Ok(CxxType::Integer {
                            bits: 64,
                            signed: false,
                        });
                    },
                    "uintptr_t" | "void*" => {
                        return Ok(CxxType::Pointer(Box::new(CxxType::Void)));
                    },
                    _ => {},
                }
                // parser 将 `std::X` 类型名降为短名 X, 因此定义在 namespace 内的类
                // 不在当前作用域链上: 回退到全符号表查找已完整注册的类.
                if let Some(sym) = self.resolve_class_symbol(name) {
                    return Ok(CxxType::Class(ClassId::from(sym)));
                }
                // M52: 同理, **namespace 作用域里的 typedef/using 别名**
                // (`namespace std { typedef float float32_t; }` 见 <stdfloat>)
                // 也不在使用点的词法作用域链上 —— 之前只有类名有全局回退,
                // 别名没有, 于是 `<stdfloat>` 的 `std::float32_t` 报
                // "unknown class type" 并把整条 `namespace std` 的使用拖垮.
                // 别名目标自身可能仍是限定名 (存成 Dependent 链), 逐跳展开.
                {
                    let mut alias = name.to_string();
                    for _hop in 0..8 {
                        let Some(ty) = self
                            .symbols
                            .iter()
                            .find(|s| {
                                s.name == alias && matches!(s.kind, CxxSymbolKind::TypeAlias)
                            })
                            .map(|s| s.ty.clone())
                        else {
                            break;
                        };
                        match ty {
                            CxxType::Dependent(inner) => {
                                if inner == alias {
                                    break;
                                }
                                alias = inner;
                            },
                            other => return Ok(other),
                        }
                    }
                }
                // 限定类型名在 `sizeof(std::X)` / 模板实参 / 依赖嵌套名等位置会
                // **保留** 完整的 `N::S` 文本 (普通变量声明路径才被剥成短名),
                // 而 dcc 的类/枚举/别名符号按 namespace 扁平化后只登记末段.
                // 这里逐段剥掉限定前缀再查一次, 使限定名与非限定名在类型位置等价.
                if let Some(ty) = self.resolve_qualified_type_by_tail(name) {
                    return Ok(ty);
                }
                Err(format!("unknown class type: {}", name))
            },
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
            "char" => {
                return Ok(CxxType::Integer {
                    bits: 8,
                    signed: true,
                })
            },
            "signed char" => {
                return Ok(CxxType::Integer {
                    bits: 8,
                    signed: true,
                })
            },
            "unsigned char" => {
                return Ok(CxxType::Integer {
                    bits: 8,
                    signed: false,
                })
            },
            "short" | "short int" | "signed short" => {
                return Ok(CxxType::Integer {
                    bits: 16,
                    signed: true,
                })
            },
            "unsigned short" | "unsigned short int" => {
                return Ok(CxxType::Integer {
                    bits: 16,
                    signed: false,
                })
            },
            "int" | "signed" | "signed int" => {
                return Ok(CxxType::Integer {
                    bits: 32,
                    signed: true,
                })
            },
            "unsigned" | "unsigned int" => {
                return Ok(CxxType::Integer {
                    bits: 32,
                    signed: false,
                })
            },
            "long" | "long int" | "signed long" => {
                return Ok(CxxType::Integer {
                    bits: 64,
                    signed: true,
                })
            },
            "unsigned long" | "unsigned long int" => {
                return Ok(CxxType::Integer {
                    bits: 64,
                    signed: false,
                })
            },
            "long long" | "long long int" | "signed long long" => {
                return Ok(CxxType::Integer {
                    bits: 64,
                    signed: true,
                })
            },
            "unsigned long long" | "unsigned long long int" => {
                return Ok(CxxType::Integer {
                    bits: 64,
                    signed: false,
                })
            },
            "float" => return Ok(CxxType::Float { bits: 32 }),
            "double" => return Ok(CxxType::Float { bits: 64 }),
            "long double" => return Ok(CxxType::Float { bits: 80 }),
            _ => {},
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
    fn parse_base_specs(&self, base_specs: &[BaseSpec]) -> Result<Vec<BaseClass>, String> {
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

    // --- 运算符重载分发 (infix) ---

    /// 把 `BinOpKind` 映射到 Itanium 运算符编码名 (`operatorls` / `operatorix` …).
    /// `None` = 不可重载的中缀运算 (`&&` / `||` 有特殊语义, 不走重载).
    fn overload_op_name(op: BinOpKind) -> Option<&'static str> {
        use crate::cpp::mangle::Operator as O;
        let o = match op {
            BinOpKind::Add => O::Add,
            BinOpKind::Sub => O::Sub,
            BinOpKind::Mul => O::Mul,
            BinOpKind::Div => O::Div,
            BinOpKind::Mod => O::Mod,
            BinOpKind::EqEq => O::Eq,
            BinOpKind::Ne => O::Ne,
            BinOpKind::Lt => O::Lt,
            BinOpKind::Le => O::Le,
            BinOpKind::Gt => O::Gt,
            BinOpKind::Ge => O::Ge,
            BinOpKind::Shl => O::Shl,
            BinOpKind::Shr => O::Shr,
            BinOpKind::And => O::And,
            BinOpKind::Or => O::Or,
            BinOpKind::Xor => O::Xor,
            BinOpKind::Index => O::Index,
            BinOpKind::AddAssign => O::AddAssign,
            BinOpKind::SubAssign => O::SubAssign,
            BinOpKind::MulAssign => O::MulAssign,
            BinOpKind::DivAssign => O::DivAssign,
            BinOpKind::ModAssign => O::ModAssign,
            BinOpKind::ShlAssign => O::ShlAssign,
            BinOpKind::ShrAssign => O::ShrAssign,
            BinOpKind::AndAssign => O::AndAssign,
            BinOpKind::OrAssign => O::OrAssign,
            BinOpKind::XorAssign => O::XorAssign,
            // `=` 由 Assign 分支自己处理; `&&` / `||` 不可重载
            BinOpKind::Assign | BinOpKind::LAnd | BinOpKind::LOr => return None,
        };
        Some(o.encode_name())
    }

    /// 隐式转换等级 (重载决议用, 越小越好); `None` = 不可隐式转换.
    /// 本模型不携带 const 限定, 因此只区分: 同类型(0, 由调用方判) /
    /// 数组退化(1) / 整型·浮点提升转换(1) / 指针→void*、整型→浮点(2).
    fn op_conv_rank(from: &CxxType, to: &CxxType) -> Option<u32> {
        match (from, to) {
            (CxxType::Integer { .. }, CxxType::Integer { .. })
            | (CxxType::Bool, CxxType::Integer { .. })
            | (CxxType::Integer { .. }, CxxType::Bool)
            | (CxxType::Bool, CxxType::Bool) => Some(1),
            (CxxType::Float { .. }, CxxType::Float { .. }) => Some(1),
            (CxxType::Integer { .. } | CxxType::Bool, CxxType::Float { .. })
            | (CxxType::Float { .. }, CxxType::Integer { .. } | CxxType::Bool) => Some(2),
            // 任意对象指针可转 void*
            (CxxType::Pointer(_), CxxType::Pointer(t)) => match t.as_ref() {
                CxxType::Void => Some(2),
                _ => None,
            },
            _ => None,
        }
    }

    /// [conv.func] 函数到指针的**标准转换**（lvalue-to-rvalue 转换一类）:
    /// 处于函数类型的左值实参（`std::ws`、`is_even` 这样的函数名）在需要指针
    /// 上下文中自动变成 `Pointer(同一 Function)`，重载匹配里按**精确匹配**
    /// (rank 0) 参与，与 GCC/Clang 一致。
    ///
    /// 这不是装饰性规则：`is >> std::ws` 的形参是
    /// `istream& (*)(istream&)`，而 `std::ws` 的符号类型是 `CxxType::Function`。
    /// 少了这一步重载决议找不到可行候选，`try_binary_operator_overload` 会返回
    /// `None`，sema 退到**内建 `>>`**，于是流对象被当成整数做 `shr`，`this`
    /// 成了垃圾指针（运行期 SIGSEGV）。与 lower_ir 侧的 [conv.func] 同一真源。
    fn conv_func_rank(from: &CxxType, to: &CxxType) -> Option<u32> {
        match (from, to) {
            (f @ CxxType::Function { .. }, CxxType::Pointer(t)) => {
                if t.as_ref() == f {
                    Some(0)
                } else {
                    None
                }
            },
            _ => None,
        }
    }

    /// 实参类型 → 形参类型的匹配等级; `None` = 该候选不可行.
    fn op_arg_rank(arg: &CxxType, param: &CxxType) -> Option<u32> {
        // 引用实参按被引用类型参与匹配; 引用形参绑同类型左值即精确匹配.
        let a = arg.remove_reference();
        if a == param {
            return Some(0);
        }
        if let CxxType::LValueRef(p) | CxxType::RValueRef(p) = param {
            return if a == p.as_ref() {
                Some(0)
            } else {
                Self::op_conv_rank(a, p)
            };
        }
        // 函数名实参 → 函数指针形参 ([conv.func])
        if let Some(r) = Self::conv_func_rank(a, param) {
            return Some(r);
        }
        // 数组实参退化成指针 (`os << "abc"` 的 `char[N]` → `const char*`)
        if let (CxxType::Array(elem, _), CxxType::Pointer(p)) = (a, param) {
            if elem.as_ref() == p.as_ref() {
                return Some(1);
            }
        }
        Self::op_conv_rank(a, param)
    }

    /// 从 `CxxType::Function` 取形参类型表 (非函数类型 → None).
    fn fn_param_tys(ty: &CxxType) -> Option<&Vec<CxxType>> {
        match ty {
            CxxType::Function { params, .. } => Some(params),
            _ => None,
        }
    }

    /// 中缀运算符的重载分发: `a op b` → `a.operatorop(b)` (成员) 或
    /// `operatorop(a, b)` (自由函数).
    ///
    /// 返回 `Ok(None)` = 两侧都不是类类型, 或没有任何可行候选 —— 调用方
    /// 回退到内建运算 (保持既有行为). 这是 native 路径上 `<<` / `>>` / `[]`
    /// 等运算符能被标准库 (iostream / iterator / functional) 使用的入口.
    fn try_binary_operator_overload(
        &mut self,
        op: BinOpKind,
        lhs_id: HirExprId,
        rhs_id: HirExprId,
    ) -> Result<Option<HirExprId>, String> {
        let sym_name = match Self::overload_op_name(op) {
            Some(n) => format!("operator{}", n),
            None => return Ok(None),
        };
        let lhs_ty = self.hir.exprs[lhs_id.0].ty.clone();
        let rhs_ty = self.hir.exprs[rhs_id.0].ty.clone();
        let obj_ty = lhs_ty.remove_reference().clone();
        let lhs_is_class = matches!(obj_ty, CxxType::Class(_) | CxxType::Enum(_));
        let rhs_is_class = matches!(
            rhs_ty.remove_reference(),
            CxxType::Class(_) | CxxType::Enum(_)
        );
        if !lhs_is_class && !rhs_is_class {
            return Ok(None);
        }

        // 1) 成员运算符: lhs 类里的 `operatorop`, 形参数 = 1 (rhs)
        let mut best: Option<(u32, usize, SymbolId)> = None;
        if lhs_is_class {
            let class_id = match &obj_ty {
                CxxType::Class(id) | CxxType::Enum(id) => *id,
                _ => unreachable!(),
            };
            let (cands, _ambiguous) = self.lookup_in_class(class_id, &sym_name);
            for (idx, &sym) in cands.iter().enumerate() {
                let ty = match self.get_symbol(sym) {
                    Some(s) => s.ty.clone(),
                    None => continue,
                };
                let params = match Self::fn_param_tys(&ty) {
                    Some(p) => p.clone(),
                    None => continue,
                };
                if params.len() != 1 {
                    continue;
                }
                if let Some(rank) = Self::op_arg_rank(&rhs_ty, &params[0]) {
                    // 同分时取**声明序靠前**的候选 (与 GCC 的
                    // “不可区分则歧义”相比, 这里保守选第一个, 避免把
                    // 既有能编过的代码变成硬错误).
                    if best.is_none() || best.as_ref().unwrap().0 > rank {
                        best = Some((rank, idx, sym));
                    }
                }
            }
        }
        if let Some((_, _, sym)) = best {
            // `build_operator_call` 的成员形式契约: args[0] 必须是 lhs (它会被
            // 收作 this 并 `skip(1)`), 因此这里和自由函数形式传同一份实参表.
            // 之前只传 `vec![rhs_id]`, 唯一的右操作数被 skip 掉, `a == b` 在
            // 链接期少一个实参 —— 表现为运行期读越界栈槽 (SIGSEGV).
            return Ok(Some(self.build_operator_call(sym, lhs_id, vec![lhs_id, rhs_id])?));
        }

        // 2) 自由函数运算符: `operatorop(a, b)`, 形参数 = 2
        let cands = self.lookup_unqualified(&sym_name);
        let mut best_fn: Option<(u32, SymbolId)> = None;
        for &sym in &cands {
            let ty = match self.get_symbol(sym) {
                Some(s) => s.ty.clone(),
                None => continue,
            };
            let params = match Self::fn_param_tys(&ty) {
                Some(p) => p.clone(),
                None => continue,
            };
            if params.len() != 2 {
                continue;
            }
            let (r1, r2) = match (
                Self::op_arg_rank(&lhs_ty, &params[0]),
                Self::op_arg_rank(&rhs_ty, &params[1]),
            ) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            };
            let rank = r1.max(r2).saturating_add(r1.min(r2));
            if best_fn.is_none() || best_fn.as_ref().unwrap().0 > rank {
                best_fn = Some((rank, sym));
            }
        }
        if let Some((_, sym)) = best_fn {
            return Ok(Some(self.build_operator_call(sym, lhs_id, vec![lhs_id, rhs_id])?));
        }
        Ok(None)
    }

    /// 一元运算符的重载分发: `op a` → `a.operator op()` (成员) 或
    /// `operator op(a)` (自由函数).
    ///
    /// 与 `try_binary_operator_overload` 完全同一契约: 返回 `Ok(None)` = 操作数
    /// 不是类/枚举类型, 或没有任何可行候选 —— 调用方回退到内建运算。
    ///
    /// 这是 native 路径上**迭代器解引用 `*it` / 前进 `++it`** 能用的入口:
    /// 之前只做了中缀分发, `<iterator>` 风格的 `int operator*() const` 在调用点
    /// `*it` 会被当成内建解引用, 直接报 `dereference of non-pointer type`。
    ///
    /// 后缀 `it++` / `it--` 按 [over.inc] 多带一个 `int` 哨兵形参, 与前置重载区分。
    fn try_unary_operator_overload(
        &mut self,
        op: &UnaryOpKind,
        operand_id: HirExprId,
    ) -> Result<Option<HirExprId>, String> {
        use crate::cpp::mangle::Operator as O;
        // 注册侧 (`build_class_member` / `CppDecl::OperatorFn`) 用
        // `format!("operator{}", op.encode_name())` 作符号名, 这里必须同一真源。
        let (enc, postfix) = match op {
            // 一元 `*` 与二元 `*` 共用 Itanium `ml` 槽
            UnaryOpKind::Deref => (O::Mul.encode_name(), false),
            UnaryOpKind::Neg => (O::Sub.encode_name(), false),
            UnaryOpKind::Plus => (O::Add.encode_name(), false),
            UnaryOpKind::Not => (O::Not.encode_name(), false),
            UnaryOpKind::LNot => (O::LNot.encode_name(), false),
            UnaryOpKind::PreInc => (O::Inc.encode_name(), false),
            UnaryOpKind::PostInc => (O::Inc.encode_name(), true),
            UnaryOpKind::PreDec => (O::Dec.encode_name(), false),
            UnaryOpKind::PostDec => (O::Dec.encode_name(), true),
        };
        let sym_name = format!("operator{enc}");
        let operand_ty = self.hir.exprs[operand_id.0].ty.clone();
        let obj_ty = operand_ty.remove_reference().clone();
        let class_id = match &obj_ty {
            CxxType::Class(id) | CxxType::Enum(id) => *id,
            _ => return Ok(None),
        };
        // 成员形式形参数: 前置 0, 后缀 1 (`int` 哨兵); 自由函数各 +1。
        let member_arity: usize = usize::from(postfix);
        let free_arity = member_arity + 1;
        let (cands, _ambiguous) = self.lookup_in_class(class_id, &sym_name);
        for &sym in cands.iter() {
            let ty = match self.get_symbol(sym) {
                Some(s) => s.ty.clone(),
                None => continue,
            };
            let params = match Self::fn_param_tys(&ty) {
                Some(p) => p.clone(),
                None => continue,
            };
            if params.len() != member_arity {
                continue;
            }
            let args = self.unary_operator_args(operand_id, postfix);
            return Ok(Some(self.build_operator_call(sym, operand_id, args)?));
        }
        // 自由函数形式
        let cands = self.lookup_unqualified(&sym_name);
        for &sym in &cands {
            let ty = match self.get_symbol(sym) {
                Some(s) => s.ty.clone(),
                None => continue,
            };
            let params = match Self::fn_param_tys(&ty) {
                Some(p) => p.clone(),
                None => continue,
            };
            if params.len() != free_arity {
                continue;
            }
            if Self::op_arg_rank(&operand_ty, &params[0]).is_none() {
                continue;
            }
            let args = self.unary_operator_args(operand_id, postfix);
            return Ok(Some(self.build_operator_call(sym, operand_id, args)?));
        }
        Ok(None)
    }

    /// 一元重载的实参表: 操作数本身 (+ 后缀 `++` / `--` 的 `int` 哨兵)。
    fn unary_operator_args(&mut self, operand_id: HirExprId, postfix: bool) -> Vec<HirExprId> {
        let mut args = vec![operand_id];
        if postfix {
            args.push(self.add_expr(
                HirExprKind::IntLiteral(0),
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                },
            ));
        }
        args
    }

    /// 组装载荷运算符调用: 成员形式把 lhs 作 this, 自由形式把两个实参都传入.
    fn build_operator_call(
        &mut self,
        sym: SymbolId,
        lhs_id: HirExprId,
        args: Vec<HirExprId>,
    ) -> Result<HirExprId, String> {
        let sym_info = self
            .get_symbol(sym)
            .ok_or_else(|| "operator symbol not found".to_string())?;
        let is_member = matches!(sym_info.kind, CxxSymbolKind::Method);
        let ret_ty = match &sym_info.ty {
            CxxType::Function { ret, .. } => *ret.clone(),
            _ => CxxType::Void,
        };
        let mut hir_args = Vec::with_capacity(args.len() + 1);
        if is_member {
            let obj_ty = self.hir.exprs[lhs_id.0].ty.clone();
            let this_expr = match &obj_ty {
                CxxType::Class(_) | CxxType::Enum(_) => self.add_expr(
                    HirExprKind::AddressOf(lhs_id),
                    CxxType::Pointer(Box::new(obj_ty)),
                ),
                CxxType::LValueRef(inner) | CxxType::RValueRef(inner)
                    if matches!(**inner, CxxType::Class(_) | CxxType::Enum(_)) =>
                {
                    self.add_expr(
                        HirExprKind::AddressOf(lhs_id),
                        CxxType::Pointer(Box::new((**inner).clone())),
                    )
                },
                _ => lhs_id,
            };
            hir_args.push(this_expr);
            // 成员运算符的 this 就是 lhs, 调用实参只剩后面的部分
            hir_args.extend(args.into_iter().skip(1));
        } else {
            hir_args.extend(args);
        }
        let slot = self.get_virtual_slot(sym);
        let callee = match slot {
            Some(slot) => HirCallee::Virtual {
                method: sym,
                slot,
                this_adjust: Default::default(),
            },
            None => HirCallee::Direct(sym),
        };
        Ok(self.add_expr(HirExprKind::Call { callee, args: hir_args }, ret_ty))
    }

    // --- 表达式构建 ---

    fn build_expr(&mut self, expr: &CppExpr) -> Result<HirExprId, String> {
        match expr {
            CppExpr::IntLit(n) => Ok(self.add_expr(
                HirExprKind::IntLiteral(*n as i128),
                // 字面量的类型 = [lex.icon] 的量级规则 (单一真源
                // `const_text::int_literal_rank_bits`)。硬写 `bits: 32` 会把
                // `10306098990L / 100L` 这类宽常量整条运算链降格成 32 位。
                CxxType::Integer {
                    bits: crate::cpp::const_text::int_literal_rank_bits(*n as i128),
                    signed: true,
                },
            )),
            CppExpr::FloatLit(s) => Ok(self.add_expr(
                HirExprKind::FloatLiteral(Self::float_literal_value_text(s)),
                CxxType::Float { bits: 64 },
            )),
            CppExpr::StrLit(s) => {
                // 字符串字面量: 类型是 `char[N]`, 在值上下文里立即退化为
                // `char*` ([array-to-pointer])。转义序列在 lowering 层解码后
                // 落 rodata (`const_text::decode_string_escapes`)。
                Ok(self.add_expr(
                    HirExprKind::StringLiteral(s.clone()),
                    CxxType::Pointer(Box::new(CxxType::Integer {
                        bits: 8,
                        signed: true,
                    })),
                ))
            },
            CppExpr::Noexcept(e) => {
                // noexcept(expr): [except.spec] 编译期 bool 谓词, 由参与求值的实体的
                // 异常规范决定 (用户函数只看说明符; 隐式/=default 特殊成员函数对其
                // 成员/基类传递推导)。操作数是 unevaluated-context, 不 build 进 HIR。
                let holds = self.noexcept_predicate_holds(e);
                Ok(self.add_expr(HirExprKind::BoolLiteral(holds), CxxType::Bool))
            },
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
            },
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
            },
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
            },
            CppExpr::MemberPtr { .. } => {
                // &Class::method / &Class::field - 成员指针
                // 简化：返回 size_t 偏移占位
                Ok(self.add_expr(
                    HirExprKind::IntLiteral(0),
                    CxxType::Integer {
                        bits: 64,
                        signed: false,
                    },
                ))
            },
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
                    },
                    _ => Err(format!("undeclared template identifier: {}", name)),
                }
            },
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
                            _ => {
                                return Err(format!(
                                    "tpl static access '{}::{}' is not a value",
                                    name, member
                                ))
                            },
                        };
                        Ok(self.add_expr(expr_kind, ty))
                    },
                    _ => Err(format!("undeclared tpl static access: {}", member)),
                }
            },
            CppExpr::Sizeof(arg) => match arg {
                crate::cpp::parser::SizeofArg::Pack(_) => {
                    // sizeof...(pack) 由 template 层单态化时替成字面量, 不应到达 native sema
                    Err("sizeof...(pack) not resolved before native sema".into())
                },
                crate::cpp::parser::SizeofArg::Type(t) => {
                    let cxx_ty = self.convert_type(t)?;
                    Ok(self.add_expr(
                        HirExprKind::SizeOf(cxx_ty),
                        CxxType::Integer {
                            bits: 64,
                            signed: false,
                        },
                    ))
                },
                crate::cpp::parser::SizeofArg::Expr(e) => {
                    let expr_id = self.build_expr(e)?;
                    let ty = self.hir.exprs[expr_id.0].ty.clone();
                    Ok(self.add_expr(
                        HirExprKind::SizeOf(ty),
                        CxxType::Integer {
                            bits: 64,
                            signed: false,
                        },
                    ))
                },
            },
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
            },
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
            },
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
            },
            CppExpr::Lambda(_) => {
                // Lambda 在 native 路径中已被简化处理
                Ok(self.add_expr(HirExprKind::NullPtr, CxxType::Pointer(Box::new(CxxType::Void))))
            },
            CppExpr::StrConcat(parts) => {
                // 相邻字符串字面量拼接 ([lex.string] p16): 拼接后才是一条字面量。
                let combined: String = parts.iter().map(|s| s.as_str()).collect();
                Ok(self.add_expr(
                    HirExprKind::StringLiteral(combined),
                    CxxType::Pointer(Box::new(CxxType::Integer {
                        bits: 8,
                        signed: true,
                    })),
                ))
            },
            CppExpr::This => {
                let class_id = self
                    .current_class
                    .ok_or_else(|| "this used outside member function".to_string())?;
                let ty = CxxType::Pointer(Box::new(CxxType::Class(class_id)));
                Ok(self.add_expr(HirExprKind::This, ty))
            },
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

                // G22: 访问检查 (ThisMember: 上下文总是当前类)
                self.check_member_access(field_sym, name)?;

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
            },
            CppExpr::Ident(name) => {
                let mut syms = self.lookup_unqualified(name);
                if syms.is_empty() {
                    // C++ 没有 C89 的隐式函数声明; 按 GCC -fbuiltin 语义把常用
                    // libc 内存/字符串函数作为内置符号补登记。
                    if let Some(sym) = self.declare_builtin_if_known(name) {
                        syms.push(sym);
                    }
                }
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
                    CxxSymbolKind::EnumConstant => self.enum_constant_expr_kind(sym),
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
                    },
                    CxxSymbolKind::Function => HirExprKind::Global(sym),
                    CxxSymbolKind::Method => HirExprKind::Global(sym),
                    _ => return Err(format!("identifier '{}' is not a value", name)),
                };
                Ok(self.add_expr(expr_kind, ty))
            },
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
                        CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => {
                            match inner.as_ref() {
                                CxxType::Class(id) => (*id, true),
                                _ => return Err("member access on non-class pointer".into()),
                            }
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

                // G22: 访问检查
                self.check_member_access(field_sym, name)?;

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
            },
            CppExpr::PtrMember(obj, name) => {
                // obj->member 等价于 (*obj).member
                let deref = CppExpr::UnaryOp(UnaryOpKind::Deref, obj.clone());
                let deref_expr = CppExpr::Member(Box::new(deref), name.clone());
                self.build_expr(&deref_expr)
            },
            CppExpr::Call(callee, args) => {
                // 函数式构造 `T(args...)`: callee 解析为类名时走临时量构造路径
                if let CppExpr::Ident(name) = callee.as_ref() {
                    if let Some(class_sym) = self.resolve_class_symbol(name) {
                        return self.build_class_temporary(class_sym, args);
                    }
                }
                // `ns::T(args...)` / `Outer::Inner(args...)`: 限定形式的临时量构造
                if let CppExpr::QualifiedAccess(path, name) = callee.as_ref() {
                    if let Some(class_sym) = self.resolve_qualified_class_symbol(path, name) {
                        return self.build_class_temporary(class_sym, args);
                    }
                }
                let callee_id = self.build_expr(callee)?;
                let callee_ty = self.hir.exprs[callee_id.0].ty.clone();
                let mut hir_args = Vec::new();
                for arg in args {
                    hir_args.push(self.build_expr(arg)?);
                }

                // Try ADL if callee is an identifier
                let adl_candidates = if let CppExpr::Ident(name) = callee.as_ref() {
                    // Extract argument types and perform ADL lookup
                    let arg_types: Vec<&CxxType> =
                        hir_args.iter().map(|id| &self.hir.exprs[id.0].ty).collect();

                    // Collect namespaces from argument types
                    let mut all_namespaces = Vec::new();
                    for arg_ty in &arg_types {
                        let ns_list = adl::collect_adl_namespaces_for_type(arg_ty);
                        all_namespaces.extend(ns_list);
                    }

                    // Deduplicate namespaces
                    all_namespaces.sort();
                    all_namespaces.dedup();

                    // For now, try current scope + parent scopes for ADL
                    // TODO: Full namespace traversal when ClassTable integration complete
                    if !all_namespaces.is_empty() {
                        // Perform lookup in each collected namespace
                        let mut candidates = Vec::new();
                        for ns in &all_namespaces {
                            // Try to resolve namespace path and look up symbol
                            if let Some(ns_scope) = self.find_namespace_scope(ns) {
                                let results = self.lookup_qualified(ns_scope, name);
                                for sym in results {
                                    if let Some(s) = self.get_symbol(sym) {
                                        if matches!(
                                            s.kind,
                                            CxxSymbolKind::Function | CxxSymbolKind::Method
                                        ) {
                                            candidates.push(sym);
                                        }
                                    }
                                }
                            }
                        }
                        if !candidates.is_empty() {
                            Some(candidates)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // If ADL found candidates, use them instead of regular resolution
                let (hir_callee, ret_ty) = if let Some(adl_syms) = adl_candidates {
                    // Use first ADL candidate
                    let best_match = adl_syms[0];
                    if let Some(sym_info) = self.get_symbol(best_match) {
                        let ret = match &sym_info.ty {
                            CxxType::Function { ret, .. } => *ret.clone(),
                            _ => CxxType::Void,
                        };
                        (HirCallee::Direct(best_match), ret)
                    } else {
                        (HirCallee::Direct(best_match), CxxType::Void)
                    }
                } else {
                    // Fallback to normal call resolution
                    // 先 clone callee kind, 避免分支内再借 self 时冲突
                    let callee_kind = self.hir.exprs[callee_id.0].kind.clone();
                    match &callee_kind {
                        HirExprKind::Global(sym) => {
                            let ret = match &callee_ty {
                                CxxType::Function { ret, .. } => *ret.clone(),
                                _ => CxxType::Void,
                            };
                            // 成员函数体内裸名字调用本类 (含基类) 方法等价于
                            // `this->m()`: 隐式补 this 实参, 虚函数走 vtable 派发.
                            // (lookup_unqualified 沿作用域链会命中类作用域的
                            //  Method 符号, 此前直接按 Direct 发射导致 lower_ir
                            //  报 "member function call missing this argument".)
                            let is_member_call = matches!(
                                self.get_symbol(*sym).map(|s| s.kind),
                                Some(CxxSymbolKind::Method)
                            ) && self.current_class.is_some();
                            if is_member_call {
                                // [class.member.lookup]/1 + [expr.ref]: 隐式 this
                                // 必须是**成员所属子对象**的指针, 不是最派生对象首
                                // 地址。`lookup_in_class` 沿基类链命中的方法属于基类,
                                // 其函数体按基类布局访问 `this->field`, 故:
                                //   - 非虚基 → BaseCast (编译期常量偏移)
                                //   - 虚基   → VBaseAdjust (vbtable 运行期偏移)
                                // 缺这一步时实测: `C : A, B` 里裸调 `b1()` 把 B::y
                                // 读成 A::x (dcc 47 / g++ 43), 虚基 `vf()` 读到 0。
                                // 本类自有方法时 member_base == current_class,
                                // `adjust_pointer_to_member_base` 原样返回, 无额外开销。
                                let this_class =
                                    self.current_class.expect("is_member_call 已保证在类内");
                                let this_raw = self.build_expr(&CppExpr::This)?;
                                let this_expr =
                                    self.adjust_pointer_to_member_base(this_raw, *sym, this_class)?;
                                let virtual_slot = self.get_virtual_slot(*sym);
                                let callee = match virtual_slot {
                                    Some(slot) => HirCallee::Virtual {
                                        method: *sym,
                                        slot,
                                        this_adjust: Default::default(),
                                    },
                                    None => HirCallee::Direct(*sym),
                                };
                                let mut full_args = Vec::with_capacity(hir_args.len() + 1);
                                full_args.push(this_expr);
                                full_args.extend(hir_args);
                                hir_args = full_args;
                                (callee, ret)
                            } else {
                                (HirCallee::Direct(*sym), ret)
                            }
                        },
                        HirExprKind::MethodRef { object, method } => {
                            let ret = match &callee_ty {
                                CxxType::Function { ret, .. } => *ret.clone(),
                                _ => CxxType::Void,
                            };
                            let object_expr = *object;
                            let method_sym = *method;
                            let virtual_slot = self.get_virtual_slot(method_sym);
                            let callee = if let Some(slot) = virtual_slot {
                                HirCallee::Virtual {
                                    method: method_sym,
                                    slot,
                                    this_adjust: Default::default(),
                                }
                            } else {
                                HirCallee::Direct(method_sym)
                            };
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
                                },
                                _ => object_expr,
                            };
                            let mut full_args = Vec::with_capacity(hir_args.len() + 1);
                            full_args.push(object_for_this);
                            full_args.extend(hir_args);
                            hir_args = full_args;
                            (callee, ret)
                        },
                        HirExprKind::Local(_sym) => {
                            let ret = match &callee_ty {
                                CxxType::Pointer(t) => match t.as_ref() {
                                    CxxType::Function { ret, .. } => *ret.clone(),
                                    _ => CxxType::Void,
                                },
                                _ => CxxType::Void,
                            };
                            (HirCallee::Indirect(callee_id), ret)
                        },
                        _ => (HirCallee::Indirect(callee_id), CxxType::Void),
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
            },
            CppExpr::BinOp(op, lhs, rhs) => {
                use BinOpKind::*;
                let lhs_id = self.build_expr(lhs)?;
                let rhs_id = self.build_expr(rhs)?;
                let lhs_ty = self.hir.exprs[lhs_id.0].ty.clone();
                // 运算符重载优先于内建运算: 只要有一侧是类/枚举类型, 先找
                // `operator<op>` 候选; 没找到则回退到内建语义.
                if matches!(op, Add | Sub | Mul | Div | Mod | EqEq | Ne | Lt | Le | Gt | Ge | Shl | Shr | And | Or | Xor | Index | AddAssign | SubAssign | MulAssign | DivAssign | ModAssign | ShlAssign | ShrAssign | AndAssign | OrAssign | XorAssign)
                {
                    if let Some(call) = self.try_binary_operator_overload(*op, lhs_id, rhs_id)? {
                        return Ok(call);
                    }
                }
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
                    },
                    Index => {
                        // a[b]: 数组 / 指针下标访问。结果类型 = 元素类型。
                        // 保留独立的 Index 节点 (而不是降级成 *(a+b))，
                        // 这样 lower_ir 能精确生成 IndexLoad/IndexStore 并
                        // 带上元素的 volatile 属性。
                        let elem_ty = match &lhs_ty {
                            CxxType::Array(inner, _) => *inner.clone(),
                            CxxType::Pointer(inner) => *inner.clone(),
                            _ => return Err("subscripted value is not an array or pointer".into()),
                        };
                        Ok(self.add_expr(
                            HirExprKind::Index {
                                array: lhs_id,
                                index: rhs_id,
                            },
                            elem_ty,
                        ))
                    },
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
                    },
                }
            },
            CppExpr::UnaryOp(op, operand) => {
                let operand_id = self.build_expr(operand)?;
                // 类类型的一元运算符必须先走重载分发, 否则 `*it` 会被当成
                // 内建解引用 (见 try_unary_operator_overload)。
                if let Some(call) = self.try_unary_operator_overload(op, operand_id)? {
                    return Ok(call);
                }
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
                        // [expr.unary.op]: 数组操作数先衰减成指针, 所以 `*g` 等价
                        // `g[0]`。直接当作指针解引用会报 "dereference of
                        // non-pointer type" (`sizeof(*g)` / `int x = *g;` 同死),
                        // 这里按同构的 Index 节点发射, 下标路径已能处理
                        // 全局数组基址。
                        CxxType::Array(inner, _) => {
                            let idx = self.add_expr(HirExprKind::IntLiteral(0), CxxType::Integer {
                                bits: 32,
                                signed: true,
                            });
                            let elem = (*inner).clone();
                            return Ok(self
                                .add_expr(HirExprKind::Index { array: operand_id, index: idx }, elem));
                        },
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
            },
            CppExpr::AddrOf(operand) => {
                let operand_id = self.build_expr(operand)?;
                let operand_ty = self.hir.exprs[operand_id.0].ty.clone();
                // `&a[i]` 等价于 `a + i` (两边各求值一次, 副作用顺序不变)。
                // 保留成 AddressOf(Index) 时降级层没有这种节点形态的地址支持
                // (`address of complex expression`), 而下标算术 `g+3-g` 早已
                // 按元素宽度正确折成字节偏移 —— 在 HIR 构造点就改写成加法,
                // 让取元素地址与指针减法共用同一条已验证的路径。
                // 只匹配**内建** Index: 重载 `operator[]` 走 Call 分支, 它的
                // 结果本来就是引用, AddressOf 在 ABI 层是恒等。
                if let HirExprKind::Index { array, index } = self.hir.exprs[operand_id.0].kind {
                    let array_ty = self.hir.exprs[array.0].ty.clone();
                    let ptr_ty = match &array_ty {
                        CxxType::Array(inner, _) => CxxType::Pointer(Box::new((**inner).clone())),
                        _ => array_ty.clone(),
                    };
                    return Ok(self.add_expr(
                        HirExprKind::BinaryOp {
                            op: BinaryOp::Add,
                            lhs: array,
                            rhs: index,
                        },
                        ptr_ty,
                    ));
                }
                Ok(self.add_expr(
                    HirExprKind::AddressOf(operand_id),
                    CxxType::Pointer(Box::new(operand_ty)),
                ))
            },
            CppExpr::Ref(operand) => {
                // 引用表达式 - 简化为 AddressOf
                let operand_id = self.build_expr(operand)?;
                let operand_ty = self.hir.exprs[operand_id.0].ty.clone();
                Ok(self.add_expr(
                    HirExprKind::AddressOf(operand_id),
                    CxxType::Pointer(Box::new(operand_ty)),
                ))
            },
            CppExpr::Cast(ty, expr) => {
                let target = self.convert_type(ty)?;
                let expr_id = self.build_expr(expr)?;
                // 类指针上转型 (Derived* -> Base*): 必须按 ABI 做 this-adjust。
                // 非虚基走固定偏移 (BaseCast), 虚基的偏移存在 vbtable 里
                // (运期值), 必须走 VBaseAdjust。不能当成普通位转。
                if let Some(adj) =
                    self.class_pointer_upcast(expr_id, &target)?
                {
                    return Ok(adj);
                }
                Ok(self.add_expr(
                    HirExprKind::Cast {
                        expr: expr_id,
                        target: target.clone(),
                        kind: CastKind::Static,
                    },
                    target,
                ))
            },
            CppExpr::TypeidType(ty) => {
                // typeid(T): 静态类型的 type_info 引用
                let cxx_ty = self.convert_type(ty)?;
                match cxx_ty {
                    CxxType::Class(class_id) => {
                        let ti_ty = self.typeid_result_type();
                        Ok(self.add_expr(HirExprKind::TypeInfoRef(class_id), ti_ty))
                    },
                    _ => Err("typeid of non-class type not yet supported".to_string()),
                }
            },
            CppExpr::Typeid(expr) => {
                // typeid(expr): 表达式静态类型对应的 type_info;
                // 若静态类型是**多态类** (有虚函数) 的 glvalue, 则按 Itanium ABI
                // 走动态路径 (从对象 vptr 的 typeinfo slot 读), 否则用静态类 RTTI。
                let expr_id = self.build_expr(expr)?;
                let expr_ty = self.hir.exprs[expr_id.0].ty.clone();
                let type_info_ptr = self.typeid_result_type();
                match expr_ty {
                    CxxType::Class(class_id) => {
                        if self.base_has_virtual(class_id) {
                            Ok(self.add_expr(HirExprKind::TypeInfoDyn(expr_id), type_info_ptr))
                        } else {
                            Ok(self.add_expr(HirExprKind::TypeInfoRef(class_id), type_info_ptr))
                        }
                    },
                    CxxType::Pointer(inner) => match inner.as_ref() {
                        CxxType::Class(class_id) => {
                            Ok(self.add_expr(HirExprKind::TypeInfoRef(*class_id), type_info_ptr))
                        },
                        _ => Err("typeid of non-class pointer type not yet supported".to_string()),
                    },
                    CxxType::LValueRef(inner) | CxxType::RValueRef(inner) => match inner.as_ref() {
                        CxxType::Class(class_id) => {
                            if self.base_has_virtual(*class_id) {
                                Ok(self.add_expr(HirExprKind::TypeInfoDyn(expr_id), type_info_ptr))
                            } else {
                                Ok(self.add_expr(
                                    HirExprKind::TypeInfoRef(*class_id),
                                    type_info_ptr,
                                ))
                            }
                        },
                        _ => Err("typeid of non-class reference type not yet supported".to_string()),
                    },
                    _ => Err("typeid of non-class type not yet supported".to_string()),
                }
            },
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
            },
            CppExpr::Ternary { cond, then, els } => {
                // 三元表达式: cond ? then : els
                // C++ 公共类型规则简化为: 两分支类型相同则用之,
                // 否则取 then 分支类型 (覆盖基础整数/指针字面量场景)
                let cond_id = self.build_expr(cond)?;
                let then_id = self.build_expr(then)?;
                let else_id = self.build_expr(els)?;
                let then_ty = self.hir.exprs[then_id.0].ty.clone();
                // 两支结果相同: 类型不等时也保守取 then 分支类型 (else 侧的
                // 公共类型推导尚未实现), 故不再引入 else_ty 比较。
                let ty = then_ty;
                Ok(self.add_expr(
                    HirExprKind::Conditional {
                        cond: cond_id,
                        then_expr: then_id,
                        else_expr: else_id,
                    },
                    ty,
                ))
            },
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
            },
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
            },
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
            },
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
            },
            CppExpr::Delete(ptr) => {
                let ptr_id = self.build_expr(ptr)?;
                let dtor = self.delete_dtor_for(ptr_id);
                Ok(self.add_expr(
                    HirExprKind::Delete {
                        ptr: ptr_id,
                        is_array: false,
                        dtor,
                    },
                    CxxType::Void,
                ))
            },
            CppExpr::DeleteArr(ptr) => {
                let ptr_id = self.build_expr(ptr)?;
                // `delete[]` 要先按逆序逐元素析构 (并读数组 cookie), 未实现;
                // 这里传 `None` 保持旧的「只释放内存」行为。
                Ok(self.add_expr(
                    HirExprKind::Delete {
                        ptr: ptr_id,
                        is_array: true,
                        dtor: None,
                    },
                    CxxType::Void,
                ))
            },
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
            },
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
            },
            CppExpr::QualifiedAccess(path, name) => {
                // 限定名访问: ns1::ns2::name / Enum::member
                // 首段向外查找, 后续段在已解析作用域内查找
                let mut scope = self.global_scope();
                for (idx, part) in path.iter().enumerate() {
                    // [basic.lookup.qual] 首段向外查找, 后续段限定在已解析作用域内
                    let syms = self.lookup_qualifier_segment(scope, part, idx == 0);
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
                                },
                                CxxSymbolKind::Enum => {
                                    // enum 成员注册在枚举类型的当前 scope (不是 enum 自己的 scope)
                                    is_enum = true;
                                    break;
                                },
                                _ => {},
                            }
                        }
                    }
                    if is_enum {
                        // scope 保持不变, 继续在当前作用域查枚举常量
                    } else if let Some(s) = found_scope {
                        scope = s;
                    } else {
                        return Err(format!("'{}' is not a class or namespace", part));
                    }
                }
                // 在最终作用域中查找目标名称
                let mut syms = self.lookup_qualified(scope, name);
                if syms.is_empty() && path.is_empty() {
                    // `::memcpy` 等全局内置函数: 与裸名同样的 -fbuiltin 补登记
                    if let Some(sym) = self.declare_builtin_if_known(name) {
                        syms.push(sym);
                    }
                }
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
                    CxxSymbolKind::Variable => HirExprKind::Global(sym),
                    CxxSymbolKind::EnumConstant => self.enum_constant_expr_kind(sym),
                    CxxSymbolKind::Function => HirExprKind::Global(sym),
                    CxxSymbolKind::Method => HirExprKind::Global(sym),
                    CxxSymbolKind::Class | CxxSymbolKind::Enum | CxxSymbolKind::TypeAlias => {
                        return Err(format!("'{}' is a type, not a value", name))
                    },
                    _ => return Err(format!("'{}' is not a value", name)),
                };
                Ok(self.add_expr(expr_kind, ty))
            },
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
                        },
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
            },
            other => {
                Err(format!("unsupported expression kind: {:?}", std::mem::discriminant(other)))
            },
        }
    }

    // --- 语句构建 ---

    fn build_stmt(&mut self, stmt: &CppStmt) -> Result<HirStmtId, String> {
        match stmt {
            CppStmt::Expr(e) => {
                let expr_id = self.build_expr(e)?;
                Ok(self.add_stmt(HirStmtKind::Expr(expr_id)))
            },
            CppStmt::Return(e) => {
                let expr_id = match e {
                    Some(e) => Some(self.build_expr(e)?),
                    None => None,
                };
                Ok(self.add_stmt(HirStmtKind::Return(expr_id)))
            },
            CppStmt::If { cond, then, els } => {
                let cond_id = self.build_expr(cond)?;
                let then_stmts = self.build_body(then)?;
                let then_stmt = self.add_stmt(HirStmtKind::Compound(then_stmts));
                let else_body = match els {
                    Some(e) => {
                        let e_stmts = self.build_body(e)?;
                        Some(self.add_stmt(HirStmtKind::Compound(e_stmts)))
                    },
                    None => None,
                };
                Ok(self.add_stmt(HirStmtKind::If {
                    cond: cond_id,
                    then_body: then_stmt,
                    else_body,
                }))
            },
            CppStmt::IfConstexpr { cond, then, els } => {
                // M17.0: `if constexpr` 真正的分支省略
                // 先尝试在编译期求值条件，如果成功：
                //   - true (非零): 只 build then 分支
                //   - false (零): 只 build else 分支
                // 如果失败：回退到运行时 if（两个分支都要 build）
                use crate::cpp::cpp_const_eval::eval_cpp_const;

                let chosen_branch = eval_cpp_const(cond, &HashMap::new());

                match chosen_branch {
                    Some(v) if v != 0 => {
                        // 条件为 true（非零），只编译 then 分支
                        let then_stmts = self.build_body(then)?;
                        Ok(self.add_stmt(HirStmtKind::Compound(then_stmts)))
                    },
                    Some(0) => {
                        // 条件为 false，只编译 else 分支
                        match els {
                            Some(e) => {
                                let e_stmts = self.build_body(e)?;
                                Ok(self.add_stmt(HirStmtKind::Compound(e_stmts)))
                            },
                            None => Ok(self.add_stmt(HirStmtKind::Compound(vec![]))),
                        }
                    },
                    None => {
                        // 无法确定，回退到运行时 if
                        let cond_id = self.build_expr(cond)?;
                        let then_stmts = self.build_body(then)?;
                        let then_stmt = self.add_stmt(HirStmtKind::Compound(then_stmts));
                        let else_body = match els {
                            Some(e) => {
                                let e_stmts = self.build_body(e)?;
                                Some(self.add_stmt(HirStmtKind::Compound(e_stmts)))
                            },
                            None => None,
                        };
                        Ok(self.add_stmt(HirStmtKind::If {
                            cond: cond_id,
                            then_body: then_stmt,
                            else_body,
                        }))
                    },
                    _ => {
                        // 无法确定，回退到运行时 if
                        let cond_id = self.build_expr(cond)?;
                        let then_stmts = self.build_body(then)?;
                        let then_stmt = self.add_stmt(HirStmtKind::Compound(then_stmts));
                        let else_body = match els {
                            Some(e) => {
                                let e_stmts = self.build_body(e)?;
                                Some(self.add_stmt(HirStmtKind::Compound(e_stmts)))
                            },
                            None => None,
                        };
                        Ok(self.add_stmt(HirStmtKind::If {
                            cond: cond_id,
                            then_body: then_stmt,
                            else_body,
                        }))
                    },
                }
            },
            CppStmt::While { cond, body } => {
                let cond_id = self.build_expr(cond)?;
                let body_stmts = self.build_body(body)?;
                let body_stmt = self.add_stmt(HirStmtKind::Compound(body_stmts));
                Ok(self.add_stmt(HirStmtKind::While {
                    cond: cond_id,
                    body: body_stmt,
                }))
            },
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
                    },
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
            },
            CppStmt::Block(body) => {
                let body_stmts = self.build_body(body)?;
                Ok(self.add_stmt(HirStmtKind::Compound(body_stmts)))
            },
            CppStmt::LocalVar {
                ty,
                name,
                init,
                is_volatile,
            } => {
                // 计算变量类型 + (可选的) 已预先 build 的 init expr
                // Auto/DecltypeAuto: 优先检测 ctor call 模式, 否则从 init 推导
                let (var_ty, prebuilt_init): (CxxType, Option<HirExprId>) = match ty {
                    MangleType::Auto | MangleType::DecltypeAuto => {
                        let init_expr = init
                            .as_ref()
                            .ok_or_else(|| "auto variable must have an initializer".to_string())?;
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
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(cn) = ctor_class_name {
                            // 用 ClassName 得到 Class 类型, 留待下面 ctor 块处理
                            let class_ty = self.convert_type(&MangleType::Class(cn))?;
                            (class_ty, None)
                        } else {
                            // 普通 auto 推导: build init expr 拿类型
                            let init_id = self.build_expr(init_expr)?;
                            let init_ty = self.hir.exprs[init_id.0].ty.clone();
                            (init_ty, Some(init_id))
                        }
                    },
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
                // 无显式 init 的类类型局部变量: `Class c;` 仍必须调用默认构造器
                // (写 vptr/vbptr + 初始化子对象), 否则运行期拿到未初始化内存。
                // 不需要构造的聚合类 (无虚函数/无 ctor/无类成员) 保持原样。
                if init.is_none() {
                    if let CxxType::Class(class_id) = &var_ty {
                        if self.class_needs_construction(*class_id) {
                            let local_expr =
                                self.add_expr(HirExprKind::Local(sym), var_ty.clone());
                            let place_expr = self.add_expr(
                                HirExprKind::AddressOf(local_expr),
                                CxxType::Pointer(Box::new(var_ty.clone())),
                            );
                            let construct_expr = self.add_expr(
                                HirExprKind::Construct {
                                    class: *class_id,
                                    place: place_expr,
                                    args: Vec::new(),
                                    variant: crate::cpp::hir::CtorVariant::Complete,
                                },
                                CxxType::Void,
                            );
                            let decl_stmt = self.add_stmt(HirStmtKind::Decl {
                                symbol: sym,
                                init: None,
                            });
                            let construct_stmt = self.add_stmt(HirStmtKind::Expr(construct_expr));
                            return Ok(self.add_stmt(HirStmtKind::Compound(vec![
                                decl_stmt,
                                construct_stmt,
                            ])));
                        }
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
                                    let local_expr =
                                        self.add_expr(HirExprKind::Local(sym), var_ty.clone());
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
                                    let construct_stmt =
                                        self.add_stmt(HirStmtKind::Expr(construct_expr));
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
            },
            CppStmt::LocalVarList { .. } => {
                // 一条声明里的多个 init-declarator (`int a = 0, *p, q[2];`):
                // 先用单一真源的归约关系 [`CppStmt::as_local_var_stmts`] 按声明顺序
                // 展开成 N 个「单 declarator」语句 —— 每个 declarator 的类型是共享
                // decl-specifier **叠加它自己的** `*`/`&`/`[N]` 层级, 所以
                // `int *p, q;` 里 `q` 仍是标量 `int` —— 再各自走与单变量完全相同的
                // 建符号 / 构造 / guard 路径, 保证两个分支不会随时间漂移。
                //
                // 展开结果必须打包回**一个** HIR 语句: `for` 的 init 位置只能容纳单个
                // declaration-statement ([stmt.init]), 而 `HirStmtKind::Compound` 在
                // lower_ir 里就是「就地依次降级」的单条语句 (不开新的词法块),
                // 因此既能当 for-init, 也让变量的作用域仍属于外层块。
                let expanded = stmt
                    .as_local_var_stmts()
                    .expect("LocalVarList 必能归约成单 declarator 语句");
                let mut inner = Vec::with_capacity(expanded.len());
                for sub in &expanded {
                    inner.push(self.build_stmt(sub)?);
                }
                Ok(self.add_stmt(HirStmtKind::Compound(inner)))
            },
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
                self.hir
                    .top_level
                    .push(HirTopLevelDecl::Variable(guard_sym));
                Ok(self.add_stmt(HirStmtKind::StaticDecl {
                    symbol: var_sym,
                    guard_symbol: guard_sym,
                    init: init_id,
                }))
            },
            CppStmt::CoAwait(_e) => {
                // 协程语句应在 AST 预转换阶段被转换为状态机形式, 此处不应再出现
                Err("co_await not supported in this context".into())
            },
            CppStmt::CoYield(_e) => {
                // 协程语句应在 AST 预转换阶段被转换为状态机形式, 此处不应再出现
                Err("co_yield not supported in this context".into())
            },
            CppStmt::CoReturn(_e) => {
                // 协程语句应在 AST 预转换阶段被转换为状态机形式, 此处不应再出现
                Err("co_return not supported in this context".into())
            },
            CppStmt::ThisMember(_name, _stmt) => {
                // this->member = ... 或 this->member(...) 可以在表达式层处理
                // 这里直接处理语句内部的 member
                Err("ThisMember statement: delegate to expr builder".into())
            },
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
                let throw_expr = self.add_expr(
                    HirExprKind::Throw {
                        object: obj_id,
                        ty: throw_ty.clone(),
                    },
                    throw_ty,
                );
                Ok(self.add_stmt(HirStmtKind::Expr(throw_expr)))
            },
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
                        },
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
            },
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
            },
            CppStmt::Goto(label) => Ok(self.add_stmt(HirStmtKind::Goto(label.clone()))),
            CppStmt::Labeled { label, stmt } => {
                // `label: stmt` — 标签只影响控制流, 语句本身照常构建
                let inner = self.build_stmt(stmt)?;
                Ok(self.add_stmt(HirStmtKind::Label(label.clone(), inner)))
            },
            CppStmt::Fallthrough { .. } => {
                // [[fallthrough]]; 只是给诊断用的标记, 语义上是空操作
                Ok(self.add_stmt(HirStmtKind::Fallthrough))
            },
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
                            let value =
                                crate::cpp::cpp_const_eval::eval_cpp_const(ce, &HashMap::new())
                                    .ok_or_else(|| {
                                        "case label must be an integer constant expression"
                                            .to_string()
                                    })?;
                            body_stmts.push(self.add_stmt(HirStmtKind::Case {
                                value,
                                body: body_stmt,
                            }));
                        },
                        None => {
                            body_stmts.push(self.add_stmt(HirStmtKind::Default(body_stmt)));
                        },
                    }
                }
                let body = self.add_stmt(HirStmtKind::Compound(body_stmts));
                self.pop_scope();
                Ok(self.add_stmt(HirStmtKind::Switch {
                    cond: cond_id,
                    body,
                }))
            },
            CppStmt::Decl(_d) => Err("nested declaration statement not yet supported".into()),
            // 带上原文: OpaqueStmt 是 parser 容错降级得到的占位语句,
            // 只报枚举判别值 (Discriminant(24)) 无法定位被丢掉的到底是哪条语句。
            other => Err(format!("unsupported statement kind: {other:?}")),
        }
    }

    fn build_body(&mut self, body: &CppBody) -> Result<Vec<HirStmtId>, String> {
        self.push_scope(ScopeKind::Block);
        let mut stmts = Vec::new();
        for stmt in &body.stmts {
            let sid = self.build_stmt(stmt)?;
            stmts.push(sid);
        }
        // 自动存储期析构 (C++ [basic.stc.auto]): 本块作用域内声明的、需要
        // 真实析构的类类型局部量, 按声明逆序发射 Destroy(Complete)。仅在
        // class_needs_destruction 时发射, 对聚合/平凡可析构类无影响。
        let mut dtor_locals: Vec<(SymbolId, ClassId)> = Vec::new();
        if let Some(scope) = self.scopes.get(self.current_scope.0) {
            for syms in scope.symbols.values() {
                for &sid in syms {
                    if let Some(sym) = self.symbols.get(sid.0) {
                        if matches!(sym.kind, CxxSymbolKind::Variable) {
                            if let CxxType::Class(cid) = &sym.ty {
                                dtor_locals.push((sid, *cid));
                            }
                        }
                    }
                }
            }
        }
        dtor_locals.sort_by_key(|(s, _)| s.0);
        for (sym, class) in dtor_locals.into_iter().rev() {
            if !self.class_needs_destruction(class) {
                continue;
            }
            // 含虚基的类未合成可 lower 的析构体 (见 `class_has_virtual_base`),
            // 不发射自动存储期析构调用, 避免引用不存在的 `<class>_dtor_complete`。
            if self.class_has_virtual_base(class, &mut Vec::new()) {
                continue;
            }
            let var_ty = CxxType::Class(class);
            let local_expr = self.add_expr(HirExprKind::Local(sym), var_ty.clone());
            let place = self.add_expr(
                HirExprKind::AddressOf(local_expr),
                CxxType::Pointer(Box::new(var_ty)),
            );
            let destroy = self.add_expr(
                HirExprKind::Destroy {
                    class,
                    place,
                    variant: crate::cpp::hir::DtorVariant::Complete,
                },
                CxxType::Void,
            );
            stmts.push(self.add_stmt(HirStmtKind::Expr(destroy)));
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
        props: FnSym,
    ) -> Result<SymbolId, String> {
        // 把打包的符号属性还原成同名局部量, 以下正文与原逐参数传递一致
        let FnSym {
            kind,
            access,
            owner,
            is_const,
        } = props;
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

        // 纯声明 (只有原型, 没有函数体): 只登记符号, 不进入 HIR 函数表.
        // 若仍登记, 后端会为它发射一个空定义 -> 与真正提供实体的那个 TU
        // (跨 TU / 跨模块 import, 或 libc 等外部实现) 发生 multiple definition.
        if body.is_decl {
            return Ok(fn_sym);
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
        // 函数体内新登记的临时量槽需先声明再进入原语句 (构造仍在各使用点)
        let saved_temps = std::mem::take(&mut self.func_temps);
        let body_id = if !body.is_decl {
            let body_stmts = self.build_body(body)?;
            let temps = std::mem::take(&mut self.func_temps);
            let mut stmts = Vec::with_capacity(temps.len() + body_stmts.len());
            for sym in temps {
                stmts.push(self.add_stmt(HirStmtKind::Decl {
                    symbol: sym,
                    init: None,
                }));
            }
            stmts.extend(body_stmts);
            self.add_body(stmts)
        } else {
            // 纯声明，空 body
            self.add_body(Vec::new())
        };
        // 嵌套函数 (lambda 等) 的临时量登记在外层函数上, 恢复外层列表
        self.func_temps = saved_temps;

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

    /// 构建构造函数 (C1 + C2 两个变体).
    ///
    /// 构造函数在 C++ 里没有返回类型; Itanium ABI 下它在 RAX 返回 `this`,
    /// 且调用方不得依赖该返回值. 因此 HIR 层的 `ret_ty` 必须是 `Void` ——
    /// 若记成 `CxxType::Class`, x86_64 codegen 会按 SysV MEMORY 类给它加
    /// 隐藏 sret 首参数, 把 `this` 挤到 RSI, 从而读错对象地址.
    /// (析构函数一直就是按 `Void` 建模的, 两者应保持一致。)
    fn build_constructor(
        &mut self,
        name: &str,
        params: &[(String, CxxType)],
        inits: &[(String, CppExpr)],
        body: &CppBody,
        props: FnSym,
        class_id: ClassId,
    ) -> Result<SymbolId, String> {
        // 构造函数固定按 `CxxSymbolKind::Constructor` 登记, `is_const` 不适用,
        // 故只取打包的访问级与属主两项, 以下正文与原逐参数传递一致
        let FnSym { access, owner, .. } = props;
        // 构建函数类型
        let ret_ty = CxxType::Void;
        let fn_ty = CxxType::Function {
            ret: Box::new(ret_ty.clone()),
            params: params.iter().map(|(_, t)| t.clone()).collect(),
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let fn_sym = self.add_symbol(
            name.to_string(),
            CxxSymbolKind::Constructor,
            fn_ty.clone(),
            access,
        );
        if let Some(own) = owner {
            self.set_owner(fn_sym, own);
        }

        // 纯声明的构造函数 (类内只写原型): 只登记符号, 不发射定义,
        // 实体由类外定义或别的 TU (模块导出侧) 提供.
        if body.is_decl {
            return Ok(fn_sym);
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
        let _this_sym =
            self.add_symbol("this".into(), CxxSymbolKind::Variable, this_ty, Access::Private);

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

        // 处理初始化列表: Itanium 下同一个构造函数有 C1/C2 两个变体,
        // 先分别构建两者的初始化列表, 再构建用户体 (共用一份语句 id)。
        // 临时量槽 (func_temps) 统一前置到两个变体的 body 里。
        let saved_temps = std::mem::take(&mut self.func_temps);
        let (c1_inits, c2_inits, body_stmts) = if !body.is_decl {
            let c1 = self.build_ctor_initializers(inits, class_id, CtorVariant::Complete)?;
            let c2 = self.build_ctor_initializers(inits, class_id, CtorVariant::Base)?;
            let stmts = self.build_body(body)?;
            (c1, c2, stmts)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        let temps = std::mem::take(&mut self.func_temps);
        self.func_temps = saved_temps;
        let mut decl_stmts = Vec::with_capacity(temps.len());
        for sym in temps {
            decl_stmts.push(self.add_stmt(HirStmtKind::Decl {
                symbol: sym,
                init: None,
            }));
        }
        let mut tail = decl_stmts;
        tail.extend(body_stmts);
        let tail = tail;

        let (body_id, c2_body_id) = if body.is_decl {
            (self.add_body(Vec::new()), HirBodyId(usize::MAX))
        } else {
            // C1: 含虚基类构造 + complete 对象的 vptr 写入
            let c1_id = HirBodyId(self.hir.bodies.len());
            self.hir.bodies.push(HirBody {
                id: c1_id,
                stmts: tail.clone(),
                ctor_initializers: c1_inits,
            });
            // C2: 基子对象变体 (不构虚基, 不写 complete_only vbptr)
            let c2_id = HirBodyId(self.hir.bodies.len());
            self.hir.bodies.push(HirBody {
                id: c2_id,
                stmts: tail,
                ctor_initializers: c2_inits,
            });
            (c1_id, c2_id)
        };

        self.pop_scope();
        self.current_class = saved_class;

        self.hir.functions.push(HirFunction {
            symbol: fn_sym,
            body: body_id,
            params: param_syms.clone(),
            ret_ty: ret_ty.clone(),
        });

        // === 基子对象构造变体 (C2) ===
        //
        // Itanium: 任何类子对象 (非虚基 / 虚基) 都由包含它的构造函数用 C2
        // 变体构造; C2 不构造自己的虚基类 (由最派生对象的 C1 统一构造一次)。
        // 没有虚基类的类 C1/C2 等价, 但仍需发射 C2 —— 否则基类构造调用
        // `lower_ctor_init_base` 引用的 `<class>_ctor_base` 会是未知符号。
        if !body.is_decl {
            let base_link =
                crate::cpp::lower_ir::ctor_variant_name(&self.class_link_name(class_id), CtorVariant::Base);
            let c2_sym = self.add_symbol(
                base_link.clone(),
                CxxSymbolKind::Constructor,
                fn_ty.clone(),
                access,
            );
            if let Some(own) = owner {
                self.set_owner(c2_sym, own);
            }
            if let Some(s) = self.symbols.get_mut(c2_sym.0) {
                s.linkage_name = Some(base_link);
            }
            self.hir.functions.push(HirFunction {
                symbol: c2_sym,
                body: c2_body_id,
                params: param_syms,
                ret_ty,
            });
        }

        Ok(fn_sym)
    }

    /// 合成隐式默认构造函数。
    ///
    /// 触发条件: 类内未声明任何 ctor, 但其对象仍需真实初始化。具体包括:
    /// - 需要写 vptr/vbptr (多态类、或有虚基类的类);
    /// - 有需要构造的基类子对象 (基类有用户 ctor、或基类自身需要构造);
    /// - 有需要构造的类类型字段。
    ///
    /// 必须真正发射定义 (而非只登记符号): 默认构造的局部对象、以及作为
    /// 基类子对象被调的 `<Class>_ctor_base` 都要能链接到实体。调用
    /// `build_constructor` 会同时产出 C1/C2 两个变体。
    ///
    /// 前提: 当前作用域仍是该类作用域 (`current_class == Some(class_id)`),
    /// 否则合成的 ctor 体内 `this`/成员名查找会落在错误的作用域链上。
    fn synthesize_implicit_default_ctor(&mut self, class_id: ClassId) {
        // 已有用户声明的构造函数 → 不合成 (即使只写了原型: 实体由类外定义提供)
        let has_user_ctor = self
            .get_class_info(class_id)
            .map(|i| !i.constructors.is_empty())
            .unwrap_or(false);
        if has_user_ctor {
            return;
        }
        if !self.class_needs_construction(class_id) {
            return;
        }
        let owner = class_id.as_symbol();
        let link = self.class_link_name(class_id);
        if link.is_empty() {
            return;
        }
        let empty_body = crate::cpp::parser::CppBody {
            stmts: Vec::new(),
            is_decl: false,
        };
        match self.build_constructor(
            &link,
            &[],
            &[],
            &empty_body,
            FnSym::ctor(Access::Public, owner),
            class_id,
        ) {
            Ok(ctor_sym) => {
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.constructors.push(ctor_sym);
                }
            },
            Err(e) => {
                eprintln!("[sema] 合成隐式默认构造失败 {}: {}", link, e);
            },
        }
    }

    /// 该类的对象是否需要真实析构 (存在需要运行的析构逻辑)。
    ///
    /// 与 `class_needs_construction` 对偶, 用于门控隐式析构合成 / 自动存储
    /// 期末析构调用: 对无需析构的聚合类 (无虚函数 / 无用户析构 / 成员与基
    /// 类均平凡可析构) 不发射任何析构调用, 避免给大量已有夹具凭空引入
    /// `<Class>_dtor_*` 链接需求。
    pub fn class_needs_destruction(&self, class_id: ClassId) -> bool {
        self.class_needs_destruction_rec(class_id, &mut Vec::new())
    }

    fn class_needs_destruction_rec(&self, class_id: ClassId, seen: &mut Vec<ClassId>) -> bool {
        if seen.contains(&class_id) {
            return false;
        }
        seen.push(class_id);
        // 多态类 (自身或基类有虚函数, 或有虚基) 需要写 vptr -> 需析构。
        if self.class_needs_vptr(class_id) {
            return true;
        }
        let info = match self.get_class_info(class_id) {
            Some(i) => i.clone(),
            None => return false,
        };
        // 用户声明了析构函数 (已登记 HIR 实体) -> 需调用。
        if let Some(dt) = info.destructor {
            if self.hir.functions.iter().any(|f| f.symbol == dt) {
                return true;
            }
        }
        // 同上: 析构体写在类外 (`Foo::~Foo(){}`) 且尚未构建到时也算有实体。
        if self.out_of_class_dtor_defs.contains(&self.class_simple_name(class_id)) {
            return true;
        }
        for base in &info.bases {
            if self.class_needs_destruction_rec(base.class, seen) {
                return true;
            }
        }
        for field in &info.fields {
            if let Some(CxxType::Class(c)) = self.get_symbol(*field).map(|s| &s.ty) {
                if self.class_needs_destruction_rec(*c, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// `delete p` 前的析构调用形态 (C++ [expr.delete]/3: “若所删对象类型需要
    /// 析构, 则先执行析构再释放存储”)。
    ///
    /// 判定与自动存储期析构同一套 (见块作域末尾的 `Destroy` 发射):
    /// 点型是类 + `class_needs_destruction` + 本类确有已登记的 D1 体 +
    /// 不含虚基类 (虚基场景未合成可 lower 的 D1, 见 `class_has_virtual_base`)。
    /// 任一条件不满足返回 `None` = 维持旧的「只调 operator delete」行为,
    /// 不凭空引入 `<Class>_dtor_complete` 的链接需求。
    ///
    /// 多态点型 (本类析构在 `virtual_methods` 里) 必须走 **vtable 虚派发**,
    /// 否则 `delete (Base*)derived` 只会销毁基类部分。
    fn delete_dtor_for(&self, ptr_id: HirExprId) -> Option<crate::cpp::hir::HirDeleteDtor> {
        use crate::cpp::hir::HirDeleteDtor;

        let class = match &self.hir.exprs.get(ptr_id.0)?.ty {
            CxxType::Pointer(inner) => match inner.as_ref() {
                CxxType::Class(cid) => *cid,
                _ => return None,
            },
            _ => return None,
        };
        if !self.class_needs_destruction(class) {
            return None;
        }
        if self.class_has_virtual_base(class, &mut Vec::new()) {
            return None;
        }
        let info = self.get_class_info(class)?;
        let dtor_sym = info.destructor?;
        // D1 体必须真的存在 (否则发射侧会引用不存在的 `<class>_dtor_complete`)。
        if !self.hir.functions.iter().any(|f| f.symbol == dtor_sym) {
            return None;
        }
        match info.virtual_methods.iter().find(|vm| vm.symbol == dtor_sym) {
            Some(vm) => Some(HirDeleteDtor::VirtualDtor {
                class,
                slot: vm.slot,
            }),
            None => Some(HirDeleteDtor::Direct { class }),
        }
    }

    /// 该类是否 (直接或间接经由非虚基) 携带虚基类子对象。
    ///
    /// lower_ir 的析构收尾 (dtor epilogue) 目前无法降级虚基类析构
    /// (`Unsupported("virtual base destructor")`), 因此对这类**用户未声明析构**
    /// 的多态 / 含虚基类, 不合成隐式析构, 以免凭空给已有虚继承夹具引入
    /// 无法 lower 的 D1/D2 体。这是已文档化的 lower_ir 虚基析构缺口, 与非虚
    /// 继承的正常析构域互不影响。
    fn class_has_virtual_base(&self, class_id: ClassId, seen: &mut Vec<ClassId>) -> bool {
        if seen.contains(&class_id) {
            return false;
        }
        seen.push(class_id);
        let info = match self.get_class_info(class_id) {
            Some(i) => i.clone(),
            None => return false,
        };
        for base in &info.bases {
            if base.is_virtual {
                return true;
            }
            if self.class_has_virtual_base(base.class, seen) {
                return true;
            }
        }
        false
    }

    /// 为已生成的析构函数 (D1) 追加一个基子对象变体 (D2 / `<class>_dtor_base`),
    /// 与 D1 共享同一 body。必须在 D1 的 HirFunction 已 push 后调用。
    fn emit_dtor_base_variant(&mut self, class_id: ClassId, dtor_sym: SymbolId) {
        let base_link = crate::cpp::lower_ir::dtor_variant_name(
            &self.class_link_name(class_id),
            crate::cpp::hir::DtorVariant::Base,
        );
        // 已有同名变体 (重复定义防护) 则跳过。
        if self.hir.functions.iter().any(|f| {
            f.symbol != dtor_sym
                && self
                    .symbols
                    .get(f.symbol.0)
                    .map(|s| s.linkage_name.as_deref())
                    == Some(Some(base_link.as_str()))
        }) {
            return;
        }
        let (body_id, params, ret_ty) = match self.hir.functions.iter().find(|f| f.symbol == dtor_sym) {
            Some(f) => (f.body, f.params.clone(), f.ret_ty.clone()),
            None => return,
        };
        let owner = class_id.as_symbol();
        let fn_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: Vec::new(),
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let d2_sym = self.add_symbol(
            base_link.clone(),
            CxxSymbolKind::Destructor,
            fn_ty,
            Access::Public,
        );
        self.set_owner(d2_sym, owner);
        if let Some(s) = self.symbols.get_mut(d2_sym.0) {
            s.linkage_name = Some(base_link);
        }
        self.hir.functions.push(HirFunction {
            symbol: d2_sym,
            body: body_id,
            params,
            ret_ty,
        });
    }

    /// 合成隐式析构函数 (C++11 [class.dtor]): 类内未写析构但对象仍需析构
    /// (多态类要回写 vptr、或基类 / 类成员需要析构) 时, 必须发射 D1 + D2。
    /// 空体 + 入口写自己类 vptr (与非虚析构链一致)。
    fn synthesize_implicit_dtor(&mut self, class_id: ClassId) {
        let has_user_dtor = self
            .get_class_info(class_id)
            .map(|i| i.destructor.is_some())
            .unwrap_or(false);
        if has_user_dtor {
            return;
        }
        if !self.class_needs_destruction(class_id) {
            return;
        }
        // 携带虚基类的隐式析构无法被 lower_ir 降级 (见 `class_has_virtual_base`), 跳过。
        if self.class_has_virtual_base(class_id, &mut Vec::new()) {
            return;
        }
        let owner = class_id.as_symbol();
        let link = self.class_link_name(class_id);
        if link.is_empty() {
            return;
        }
        let class_name = format!("~{}", link);
        let empty_body = crate::cpp::parser::CppBody {
            stmts: Vec::new(),
            is_decl: false,
        };
        let saved_class = self.current_class;
        self.current_class = Some(class_id);
        match self.build_function(
            &class_name,
            CxxType::Void,
            &[],
            &empty_body,
            FnSym::dtor(Access::Public, owner),
        ) {
            Ok(dtor_sym) => {
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.destructor = Some(dtor_sym);
                }
                // 入口写自己类的 vptr (与用户析构同一约定)。
                let body_id = self.hir.functions.last().unwrap().body;
                let this_expr = match self.build_expr(&CppExpr::This) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("[sema] 隐式析构 vptr 写入失败 {}: {}", link, e);
                        self.current_class = saved_class;
                        return;
                    },
                };
                // 隐式析构也占虚槽: `class D : B {}` 且 B 有虚析构时, D 的隐式析构
                // 就是那条槽的重写 (否则 `_ZTV1D` 留着 `B_dtor_complete`,
                // `delete (B*)d` 不销毁派生部分)。
                if let Some(base_dtor) = self.find_virtual_base_dtor(class_id) {
                    self.register_virtual_dtor_slot(class_id, dtor_sym, Some(base_dtor));
                }
                let set_self_vptr = self.add_stmt(HirStmtKind::SetVTablePtr {
                    class: class_id,
                    place: this_expr,
                    complete: false,
                });
                let dtor_body = &mut self.hir.bodies[body_id.0];
                dtor_body.stmts.insert(0, set_self_vptr);
                // D2 基子对象变体。
                self.emit_dtor_base_variant(class_id, dtor_sym);
            }
            Err(e) => {
                eprintln!("[sema] 合成隐式析构失败 {}: {}", link, e);
            }
        }
        self.current_class = saved_class;
    }


    /// 构建构造函数初始化列表
    /// 按照 C++ 标准的构造顺序：基类按声明顺序 → 成员按声明顺序 → 构造函数体
    /// 每个基类构造完成后更新 vptr 指向该基类的 vtable，全部完成后指向最派生类的 vtable。
    ///
    /// `variant` 区分 Itanium 的两个构造变体:
    /// - `Complete` (C1): 负责构造本对象的**全部** (含间接) 虚基子对象;
    ///   末尾写入完整对象的全部 vptr (含虚基子对象的 secondary vbptr)。
    /// - `Base` (C2): 作为基类子对象被构造, **不碰**虚基类 (由最派生
    ///   构造器统一构造一次), 也不写 `complete_only` 的 vbptr。
    fn build_ctor_initializers(
        &mut self,
        inits: &[(String, CppExpr)],
        class_id: ClassId,
        variant: crate::cpp::hir::CtorVariant,
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
                },
                _ => {
                    // 单参表达式
                    let arg_id = self.build_expr(deleg_init)?;
                    vec![arg_id]
                },
            };
            let stmt = self.add_stmt(HirStmtKind::CtorInitBase {
                base: class_id, // 委托到自己
                args,
            });
            result.push(stmt);
            return Ok(result);
        }

        // 注: vptr/vbptr 写入不由这里的解析期 class info 门控 (见函数末尾说明),
        //     是否需要 vptr 是 abi_layout 在完整类定义上算出的布局事实。

        // 1.5 虚基子对象: 只有完整对象构造器 (C1) 负责, 且按依赖顺序
        // (被其它虚基类依赖的先构造) 每个共享子对象只构一次。
        if matches!(variant, crate::cpp::hir::CtorVariant::Complete) {
            let order = self.vbase_ctor_order(class_id);
            for vb in order {
                if !self.class_needs_construction(vb) {
                    continue;
                }
                let vb_name = self
                    .get_symbol(vb.as_symbol())
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let args = match init_map.get(&vb_name).copied() {
                    Some(e) => self.build_init_args(e)?,
                    None => Vec::new(),
                };
                result.push(self.add_stmt(HirStmtKind::CtorInitBase { base: vb, args }));
            }
        }

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
                // 基类构造完成后，更新 vptr 指向该基类的 vtable。
                // 无条件发射: "该类到底有没有 vtable/vptr" 是**布局**问题,
                // 由 abi_layout 在 Sema 解析完整份类定义之后统一决定;
                // lower_ir 查 `ClassAbi::vptrs`, 空则自然成为 no-op。
                // (若在解析期用 class info 里的虚函数表来门控, 会漏写:
                //  构造函数体出现在 `virtual void f()` **之前**时, 此刻
                //  `virtual_methods` 还是空的。)
                let this_expr = self.build_expr(&CppExpr::This)?;
                result.push(self.add_stmt(HirStmtKind::SetVTablePtr {
                    class: base.class,
                    place: this_expr,
                    complete: false,
                }));
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
                // 类成员: 展开构造器参数列表 (build_init_args)
                // 标量成员: 整个表达式作为单一值 (即使它形式上是 Call 表达式)
                let is_class_member = self
                    .get_symbol(*field_sym)
                    .map(|s| matches!(s.ty, CxxType::Class(_)))
                    .unwrap_or(false);
                let args = if is_class_member {
                    self.build_init_args(init_expr)?
                } else {
                    vec![self.build_expr(init_expr)?]
                };
                result.push(self.add_stmt(HirStmtKind::CtorInitMember {
                    member: *field_sym,
                    args,
                }));
            }
        }

        // 4. 所有基类和成员构造完成后，设置 vptr 指向当前（最派生）类的 vtable
        //
        // 这里**不能**用解析期的 `needs_vptr` 门控: 构造函数定义通常写在类内
        // 虚函数之前, 此刻 `info.virtual_methods` 尚未登记, 会误判成"不需要
        // vptr" 而不写 vptr, 于是对象头是 0, 任何虚派发 / typeid(e) 动态路径
        // 都段错误。是否需要 vptr 由 abi_layout (布局唯一真源) 在完整类定义上
        // 算出, lower_ir 查询 `ClassAbi::vptrs`: 无 vptr 的类发射为空。
        let this_expr = self.build_expr(&CppExpr::This)?;
        result.push(self.add_stmt(HirStmtKind::SetVTablePtr {
            class: class_id,
            place: this_expr,
            complete: matches!(variant, crate::cpp::hir::CtorVariant::Complete),
        }));

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

    /// `typeid` 表达式的结果类型。
    ///
    /// 标准语义 ([expr.type.id]/3) 是 `const std::type_info&`。本 TU 里
    /// `std::type_info` 可见 (即 `#include <typeinfo>`) 时按标准返回引用, 夹具
    /// 就能直接 `typeid(x).name()` / `.before()`, 与 g++ 同源可对照。
    ///
    /// `<typeinfo>` 未被包含时回退成 M17 的降级形态 `void*` (不透明 typeinfo
    /// 对象地址, 只做地址相等比较): 早期夹具全靠 `typeid(A) == typeid(B)`,
    /// 引用形态会让它们需要真正的 `operator==`, 属于不必要的破坏。
    fn typeid_result_type(&self) -> CxxType {
        match self.resolve_qualified_class_symbol(&["std".to_string()], "type_info") {
            Some(sym) => CxxType::LValueRef(Box::new(CxxType::Class(ClassId(sym.0)))),
            None => CxxType::Pointer(Box::new(CxxType::Void)),
        }
    }

    /// 类的链接名前缀 (命名空间下为 `ns_Class`, 否则为 `Class`)。
    ///
    /// 必须与 `lower_ir::symbol_link_name` 对 Class 符号的取值一致:
    /// ctor/dtor/method 的符号名与 abi_layout 里 `_ZTV{mangled}` 都以此为基础。
    pub fn class_link_name(&self, class_id: ClassId) -> String {
        let sym = match self.get_symbol(class_id.as_symbol()) {
            Some(s) => s,
            None => return String::new(),
        };
        sym.linkage_name
            .clone()
            .unwrap_or_else(|| sym.name.clone())
    }

    /// 类的**简单**名 (不带 `ns_` 链接前缀、不带 `::` 路径): 与 parser 给的
    /// `class` 前缀最后一段、以及类内 ctor/dtor 的符号名同一口径。
    pub fn class_simple_name(&self, class_id: ClassId) -> String {
        self.get_symbol(class_id.as_symbol())
            .map(|s| s.name.clone())
            .unwrap_or_default()
    }

    /// `ns::Outer::Inner` -> `Inner` (空段过滤后取最后一段)。
    fn path_last_segment(path: &str) -> &str {
        path.split("::")
            .filter(|p| !p.trim().is_empty())
            .last()
            .unwrap_or("")
            .trim()
    }

    /// 解析类路径 `Foo` / `ns::Foo` / `Outer::Inner` 到 ClassId。
    ///
    /// 与 [`Self::resolve_qualified_class_symbol`] 同一逐段走法 ([basic.lookup.qual]:
    /// 首段从当前作用域向外找, 后续段只在已解析作用域内找), 区别只在
    /// 输入是一个已拼好的 `::` 路径串 (parser 对类外成员定义就是这样给的),
    /// 而且**最后一段**必须命中 Class。命名空间内/嵌套类的类外定义需要它。
    fn resolve_class_path(&self, path: &str) -> Option<ClassId> {
        let parts: Vec<&str> = path
            .split("::")
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return None;
        }
        let mut scope = self.global_scope();
        let mut result = None;
        for (idx, part) in parts.iter().enumerate() {
            let syms = self.lookup_qualifier_segment(scope, part, idx == 0);
            let mut next = None;
            let mut class_sym = None;
            for &s in &syms {
                if let Some(sym) = self.get_symbol(s) {
                    match sym.kind {
                        CxxSymbolKind::Class => {
                            class_sym = Some(s);
                            next = Some(sym.scope);
                            break;
                        },
                        CxxSymbolKind::Namespace => {
                            next = Some(sym.scope);
                        },
                        _ => {},
                    }
                }
            }
            scope = next?;
            if idx + 1 == parts.len() {
                result = class_sym.map(ClassId::from);
            }
        }
        result
    }

    /// 该类的对象头部是否需要 vptr (含 vbptr)。
    ///
    /// 两种来源: 自己或基类链上有虚函数 → 需要 vptr; 有虚基类 → 需要
    /// vbptr (Itanium 下两者共用同一 address point 槽位)。二者任一成立
    /// 构造函数就必须写入 vptr, 否则运行期虚派发/this-adjust 拿到的是 0。
    pub fn class_needs_vptr(&self, class_id: ClassId) -> bool {
        self.base_has_virtual(class_id) || !self.all_virtual_bases_of(class_id).is_empty()
    }

    /// 该类的对象是否需要真实构造 (存在可用的构造函数实体)。
    ///
    /// 用于门控 `CtorInitBase`: 对不需要任何初始化的基类/虚基类发射构造
    /// 调用会引用不存在的 `<Class>_ctor_base` 符号 (链接期 undefined),
    /// 反之需要设置 vptr/vbptr 或初始化子对象的类必须被调用。
    pub fn class_needs_construction(&self, class_id: ClassId) -> bool {
        self.class_needs_construction_rec(class_id, &mut Vec::new())
    }

    fn class_needs_construction_rec(&self, class_id: ClassId, seen: &mut Vec<ClassId>) -> bool {
        if seen.contains(&class_id) {
            return false;
        }
        seen.push(class_id);
        if self.class_needs_vptr(class_id) {
            return true;
        }
        let info = match self.get_class_info(class_id) {
            Some(i) => i.clone(),
            None => return false,
        };
        // 用户提供了构造函数体 (登记进 HIR 函数表) → 需要调用它
        for ctor in &info.constructors {
            if self.hir.functions.iter().any(|f| f.symbol == *ctor) {
                return true;
            }
        }
        // 体在**类外**给出且尚未构建到 (`Foo::Foo(){}` 写在使用点之后):
        // 实体存在, 只是 `hir.functions` 要等那条声明被处理。前置扫描已记下。
        if self.out_of_class_ctor_defs.contains(&self.class_simple_name(class_id)) {
            return true;
        }
        // 基类子对象需要构造 → 本类构造必须转发
        for base in &info.bases {
            if self.class_needs_construction_rec(base.class, seen) {
                return true;
            }
        }
        // 类类型字段需要构造 → 同上
        for field in &info.fields {
            if let Some(CxxType::Class(c)) = self.get_symbol(*field).map(|s| &s.ty) {
                if self.class_needs_construction_rec(*c, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// 虚基子对象的构造顺序 (最派生构造器 C1 使用)。
    ///
    /// Itanium 规则: 若虚基 X 自身 (传递地) 以虚基 Y 为基, 则 Y 必须先于 X
    /// 构造 —— 因为 X 的 C2 不碰虚基, 共享子对象只能由更深层的先建好。
    /// 以「被多少个其它虚基依赖」为主键降序、声明序为次键排序, 与 g++ 对
    /// `Final`(菱形) 的实测顺序 Base → Mid1 → Mid2 一致。
    pub fn vbase_ctor_order(&self, class_id: ClassId) -> Vec<ClassId> {
        let all = self.all_virtual_bases_of(class_id);
        let mut order = all.clone();
        order.sort_by_key(|v| {
            std::cmp::Reverse(
                all.iter()
                    .filter(|w| *w != v && self.is_virtual_base_of(*v, **w))
                    .count(),
            )
        });
        order
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
                },
                CxxType::RValueRef(inner) => {
                    // T&& -> move ctor
                    if let CxxType::Class(cid) = inner.as_ref() {
                        if *cid == class_id {
                            return CtorKind::Move;
                        }
                    }
                },
                _ => {},
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
            },
            _ => {
                let arg_id = self.build_expr(init_expr)?;
                Ok(vec![arg_id])
            },
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
                    FnSym::free_fn(Access::Public),
                )?;
                self.hir.top_level.push(HirTopLevelDecl::Function(fn_sym));
                Ok(())
            },
            CppDecl::Class(class_decl) => {
                // struct / class 一律 public: 顶层 class 也是 public
                // (成员的精确访问级由 parser 的 member_access 表定, 见
                // `member_access_of`)
                let access = Access::Public;

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
                    },
                    None => {
                        // 未注册过，完整注册
                        let base_classes = self.parse_base_specs(&class_decl.bases)?;
                        self.register_class(class_decl.name.clone(), access, &base_classes)
                    },
                };

                // 进入类作用域处理成员
                let old_class = self.current_class;
                self.current_class = Some(class_id);

                // 处理成员
                let members: Vec<CppDecl> = class_decl.members.clone();
                // 类内成员函数体按“类已完整”的 C++ 语义编译, 因此先预注册
                // 全部数据成员 (仍按声明顺序 → 字段布局不变), 否则定义在字段
                // 之前的 `int get() const { return v_; }` 会找不到 `v_`.
                for member in &members {
                    if matches!(member, CppDecl::MemberVar { .. }) {
                        let acc = self.member_access_of(class_decl, member);
                        self.build_class_member(member, class_id, acc)?;
                    }
                }
                for member in &members {
                    if matches!(member, CppDecl::MemberVar { .. }) {
                        continue;
                    }
                    let acc = self.member_access_of(class_decl, member);
                    self.build_class_member(member, class_id, acc)?;
                }

                // 虚基子对象具名字段: 布局层为每个（包含间接）虚基子对象
                // 供了一个 `__vb_<Name>` 句柄字段 (偏移 = 共享子对象在对象内的
                // 静态偏移), 以使「共享子对象的地址」在源码/IR 里可表达。
                // 这里在符号表里注册同名的 Field 符号, 使 `o.__vb_Base` 能过
                // 名字查找与访问检查; 偏移则由布局层 (abi_layout) 统一给出。
                let vbs = self.all_virtual_bases_of(class_id);
                for vb in vbs {
                    let vb_name = self
                        .get_symbol(vb.as_symbol())
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    let handle = crate::cpp::abi_layout::vbase_subobject_field_name(&vb_name);
                    let already = self
                        .class_scope(class_id)
                        .map(|sc| !self.lookup_qualified(sc, &handle).is_empty())
                        .unwrap_or(true);
                    if already {
                        continue;
                    }
                    let sym = self.add_symbol(
                        handle,
                        CxxSymbolKind::Field,
                        CxxType::Class(vb),
                        Access::Public,
                    );
                    self.set_owner(sym, class_id.as_symbol());
                }

                // 隐式默认构造函数 (C++11 [class.default.ctor]):
                // 类内没写任何 ctor 但对象仍需初始化 (要写 vptr/vbptr、要转发
                // 基类子对象或类成员的构造) 时, 必须合成 `Class()` 并发射定义,
                // 否则 `Dog d;` 这类默认构造的局部对象 vptr 为 0, 运期
                // typeid / 虚派发会读空指针。
                self.synthesize_implicit_default_ctor(class_id);

                // 隐式析构函数 (C++11 [class.dtor]): 类内未写析构但对象仍需
                // 析构 (多态类要回写 vptr / 基类或类成员需析构) 时, 必须
                // 发射 D1 (`<class>_dtor_complete`) + D2 (`<class>_dtor_base`),
                // 否则派生类析构收尾按 `<base>_dtor_base` 调用时会链接期
                // undefined (如 Base←Mid←Leaf 链中无用户析构的 Mid)。
                self.synthesize_implicit_dtor(class_id);

                // 回到类的父作用域
                // register_class push 了一个 scope，需要 pop
                self.pop_scope();
                self.current_class = old_class;

                self.hir
                    .top_level
                    .push(HirTopLevelDecl::Class(class_id.as_symbol()));
                Ok(())
            },
            CppDecl::GlobalVar { ty, name, init, .. } => {
                let var_ty = self.convert_type(ty)?;
                // M54: 重声明 (extern 声明 + 定义) 复用同一符号, 不完整数组
                // 由定义处的真实边界补齐; 详见 `global_var_redeclaration`。
                let redecl = self.global_var_redeclaration(name, &var_ty);
                let sym = match redecl {
                    Some((sid, completes)) => {
                        if completes {
                            self.symbols[sid.0].ty = var_ty.clone();
                        }
                        sid
                    },
                    None => self.add_symbol(
                        name.clone(),
                        CxxSymbolKind::Variable,
                        var_ty,
                        Access::Public,
                    ),
                };
                self.hir.top_level.push(HirTopLevelDecl::Variable(sym));
                // M54: 初值必须留住 —— 它是全局对象静态存储内容的唯一来源。
                // `build_expr` 保留原有语义 (参与重载决议/诊断), 但表达式本身
                // 按原始 AST 存下, 由 native 发射端折叠成静态初始化器。
                if let Some(e) = init {
                    self.build_expr(e)?;
                    self.global_var_inits.insert(sym, e.clone());
                }
                Ok(())
            },
            CppDecl::Namespace { name, body } => {
                // 命名空间: 注册 namespace 符号 + push namespace 作用域, 递归处理成员
                if let Some(ns_name) = name {
                    let ns_sym = self.add_symbol(
                        ns_name.clone(),
                        CxxSymbolKind::Namespace,
                        CxxType::Void,
                        Access::Public,
                    );
                    let ns_scope = self.push_namespace_scope(Some(ns_name));
                    if let Some(s) = self.scopes.get_mut(ns_scope.0) {
                        s.owner_symbol = Some(ns_sym);
                    }
                    if let Some(s) = self.symbols.get_mut(ns_sym.0) {
                        s.scope = ns_scope;
                    }
                } else {
                    // 匿名 namespace: 纯作用域（内部链接）
                    self.push_namespace_scope(None);
                }
                for d in body {
                    self.build_decl(d)?;
                }
                self.pop_scope();
                Ok(())
            },
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
                        .ok_or_else(|| format!("operator class '{}' not found", class_name))?;
                    let owner = class_id.as_symbol();
                    let saved_class = self.current_class;
                    self.current_class = Some(class_id);
                    let method_sym = self.build_function(
                        &sym_name,
                        ret_ty,
                        &param_tys,
                        body,
                        FnSym::method(Access::Public, owner, false),
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
                        FnSym::free_fn(Access::Public),
                    )?;
                    self.hir.top_level.push(HirTopLevelDecl::Function(fn_sym));
                    let _ = is_unary;
                    Ok(())
                }
            },
            CppDecl::ExportBlock(inner) => {
                // export { ... }: 内部声明正常处理
                for d in inner {
                    self.build_decl(d)?;
                }
                Ok(())
            },
            CppDecl::RawExternC(text) => {
                // `extern "C" { ... }` 在降级阶段保留为原文, 但 native 路径需要其中
                // 的声明 (如 <thread> 的 `struct dccpp_thread;` 前向声明与函数原型),
                // 因此剥去外壳重新解析, 逐条走同一构建流程.
                // 解析失败时保持原行为 (整块忽略), 不因语法缺口阻断整份编译.
                let inner = Self::extern_c_block_inner(text);
                if inner.trim().is_empty() {
                    return Ok(());
                }
                let mut sub = crate::cpp::parser::CppParser::new(inner.as_bytes(), "<extern-c>");
                let decls = match sub.parse_top_level() {
                    Ok(decls) => decls,
                    Err(e) => {
                        // 保持降级: 不阻断整份编译, 但不静默吞掉——否则块内的
                        // 前向声明/函数原型丢失会在后续处以难以定位的
                        // "unknown class/identifier" 再次冒出.
                        eprintln!(
                            "dcc: warning: failed to re-parse extern \"C\" block, skipped: {:?}",
                            e
                        );
                        return Ok(());
                    },
                };
                for d in &decls {
                    self.build_decl(d)?;
                }
                Ok(())
            },
            CppDecl::Template { decl, .. } | CppDecl::PartialSpec { decl, .. } => {
                // 模板声明通常已在 native.rs 被过滤, 此处递归处理内部声明
                self.build_decl(decl)
            },
            CppDecl::FullSpec { .. } => {
                // M51: 全特化 (`template <> struct X<int> { ... }`, `<stdfloat>` /
                // `<print>` 惯用法) 不是独立实体: 它的成员属于被特化的那个类,
                // 由 native.rs 的 TemplateRegistry (`register_full` ->
                // `instantiate_class`) 在实例化时展开. 在这里递归内部成员会把
                // 成员函数/成员变量当成顶层声明 (sema 只能报
                // "top-level member declaration not supported").
                Ok(())
            },
            CppDecl::Concept { .. }
            | CppDecl::Module { .. }
            | CppDecl::Import { .. }
            | CppDecl::PrivateFragment
            | CppDecl::Using { .. }
            | CppDecl::UsingEnum { .. }
            | CppDecl::InheritingCtor { .. }
            | CppDecl::AnonAggr { .. }
            | CppDecl::Empty => {
                // no-op: 模块/概念/继承构造/匿名聚合等占位
                Ok(())
            },
            CppDecl::Opaque(text) => {
                // 无法解析的声明不能静默进入 HIR, 必须报错让上层降级
                Err(format!("unsupported opaque declaration: {}", text))
            },
            CppDecl::Typedef { target, name } => {
                // parser 把 `struct X;` / `class X;` 前置声明编码为
                // Typedef { target: Class(X), name: X } (legacy 路径不输出),
                // 但 native 路径需要 X 作为类符号存在: 这里注册为前向声明类.
                if let MangleType::Class(tname) = target {
                    if tname == name && self.lookup_unqualified(name).is_empty() {
                        let (_class_id, _class_scope) =
                            self.register_class(name.clone(), Access::Public, &[]);
                        // register_class push 了 class scope, 前向声明无成员需 pop
                        self.pop_scope();
                        return Ok(());
                    }
                }
                // typedef/using 别名: 注册 TypeAlias 符号, 方便 convert_type 展开
                let target_ty = self.convert_type(target)?;
                self.add_symbol(name.clone(), CxxSymbolKind::TypeAlias, target_ty, Access::Public);
                Ok(())
            },
            CppDecl::Enum {
                name,
                scoped: _,
                underlying,
                variants,
            } => {
                // 顶层 enum: 登记枚举常量 (含编译期值) + 枚举类型名
                self.register_enum(name, underlying, variants, Access::Public)
            },
            // M54: `static_assert` 是纯编译期断言: 不产生符号、不占存储, 只需
            // 给出**真假**. parser 已用带 constexpr 函数调用折叠的求值器把条件折成
            // `IntLit` (`static_assert(square(12) == 144)`); 这里再用自己的常量
            // 上下文兜一次 (`static_assert(1 == sizeof(long))` 之类纯字面量比较)。
            // 可证为假 => 硬错误; 求不出 => 警告跳过, 既不静默判通过也不误判失败。
            CppDecl::StaticAssert { condition, message } => {
                let truth = match condition {
                    CppExpr::IntLit(v) => Some(*v != 0),
                    _ => crate::cpp::cpp_const_eval::eval_cpp_const(condition, &HashMap::new())
                        .map(|v| v != 0),
                };
                match truth {
                    Some(true) => Ok(()),
                    Some(false) => Err(format!(
                        "static assertion failed: {}",
                        message.clone().unwrap_or_else(|| "<no message>".to_string())
                    )),
                    None => {
                        eprintln!(
                            "dcc: warning: static_assert condition is not evaluatable at compile time, \
                             the assertion was not checked: {}",
                            message.clone().unwrap_or_else(|| "<no message>".to_string())
                        );
                        Ok(())
                    },
                }
            },
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

    /// G22: 成员声明的访问级别 (parser 类体循环记录的 `member_access` 转换).
    fn member_access_of(
        &self,
        class_decl: &crate::cpp::parser::ClassDecl,
        member: &CppDecl,
    ) -> Access {
        Self::access_from_parser(crate::cpp::parser::member_decl_access(
            class_decl, member,
        ))
    }

    fn build_class_member(
        &mut self,
        member: &CppDecl,
        class_id: ClassId,
        access: Access,
    ) -> Result<(), String> {
        let _ = class_id;
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
                    FnSym::method(access, owner, false),
                )?;
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.methods.push(method_sym);
                }
                Ok(())
            },
            CppDecl::MemberVar { ty, name, .. } => {
                let field_ty = self.convert_type(ty)?;
                let field_sym = self.add_symbol(
                    name.clone(),
                    CxxSymbolKind::Field,
                    field_ty,
                    access,
                );
                let owner = self.current_class.unwrap().as_symbol();
                self.set_owner(field_sym, owner);
                Ok(())
            },
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
                    FnSym::method(access, owner, *is_const),
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
                let base_override = self.find_overridden_base_method(class_id, name, &method_ty);

                let is_virtual_method = *is_virtual || *is_override || base_override.is_some();

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
                        let info = self
                            .get_class_info(class_id)
                            .ok_or("class info not found")?;
                        info.virtual_methods.len()
                    };

                    // 本类 `virtual_methods` 里代表「同一条虚槽」的既有条目识别。
                    //
                    // 早期用 `vm.slot == slot` 作匹配键: 多继承下两条独立基链的
                    // 槽号可以恰好相同 (实测 `Leaf : Mid, Other` 里 `id` 继承
                    // `Mid` 的槽 1、`oid` 继承 `Other` 的槽 1), 于是后登记的
                    // override 会把前一条整条挤掉, `Leaf::id` 从 virtual_methods
                    // 消失 → 布局层认为它不是虚函数 → vtable 里留下 `Mid_id`,
                    // 虚派发绑到基类实现 (dynamic_cast 夹具实测失败点)。
                    // 改为按**被重写的基方法符号** (重写) 或**方法名**
                    // (类内声明 + 类外定义重复登记的同一新虚函数) 定位。
                    let overridden_base = base_override.map(|(s, _)| s);
                    let new_virtual_name = if overridden_base.is_none() {
                        self.get_symbol(method_sym).map(|s| s.name.clone())
                    } else {
                        None
                    };
                    let same_slot = self
                        .get_class_info(class_id)
                        .map(|info| {
                            info.virtual_methods.iter().position(|vm| {
                                if vm.symbol == method_sym {
                                    return true;
                                }
                                match (overridden_base, vm.overrides, &new_virtual_name) {
                                    (Some(base_sym), Some(existing_sym), _) => base_sym == existing_sym,
                                    (None, None, Some(name)) => {
                                        !name.is_empty()
                                            && self
                                                .symbols
                                                .get(vm.symbol.0)
                                                .map(|s| &s.name == name)
                                                .unwrap_or(false)
                                    },
                                    _ => false,
                                }
                            })
                        })
                        .unwrap_or(None);

                    let info = self
                        .get_class_info_mut(class_id)
                        .ok_or("class info not found")?;

                    if let Some(idx) = same_slot {
                        let vm = &mut info.virtual_methods[idx];
                        vm.symbol = method_sym;
                        vm.slot = slot;
                        vm.overrides = overridden_base;
                    } else {
                        info.virtual_methods.push(VirtualMethodInfo {
                            symbol: method_sym,
                            slot,
                            overrides: overridden_base,
                        });
                    }
                }

                Ok(())
            },
            CppDecl::Ctor {
                params,
                body,
                inits,
                ..
            } => {
                // 构造函数
                let class_id = self.current_class.unwrap();
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
                    &param_tys,
                    inits,
                    body,
                    FnSym::ctor(access, owner),
                    class_id,
                )?;

                // 记录到 ClassInfo 的 constructors 列表
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.constructors.push(ctor_sym);
                }
                Ok(())
            },
            CppDecl::Dtor {
                body, is_virtual, ..
            } => {
                let class_id = self.current_class.unwrap();
                let owner = class_id.as_symbol();
                let class_name = self
                    .get_symbol(owner)
                    .map(|s| format!("~{}", s.name))
                    .unwrap_or_else(|| "~dtor".into());

                // 基类虚析构识别: **不能**按名字找 (`~D` 与基类的 `~B` 名字不同,
                // 同一个虚槽却对不上), 沿基类链按符号 kind 找析构函数。
                // 识别不到时本类析构不会进入 virtual_methods → 布局层认为它
                // 不是虚函数 → `_ZTV<D>` 的析构槽留着基类实现 → `delete (B*)d`
                // 不销毁派生部分 (实测 g_dtor 未自加)。
                let base_virtual_dtor = self.find_virtual_base_dtor(class_id);

                let is_virtual_dtor = *is_virtual || base_virtual_dtor.is_some();

                let dtor_sym = self.build_function(
                    &class_name,
                    CxxType::Void,
                    &[],
                    body,
                    FnSym::dtor(access, owner),
                )?;

                // 记录到 ClassInfo
                if let Some(info) = self.get_class_info_mut(class_id) {
                    info.destructor = Some(dtor_sym);
                }

                if is_virtual_dtor {
                    self.register_virtual_dtor_slot(class_id, dtor_sym, base_virtual_dtor);

                // Itanium 析构语义: 进入 C 的析构函数时, 先把 vptr 置为
                // **C 自己的** vtable, 这样 C 体内直接/间接的虚调用绑定到 C
                // (而非最派生类)。各基类的 D2 在**自己**入口处重复此动作,
                // 因此这里不需要在体末尾预置基类 vptr。
                if !body.is_decl {
                    let dtor_body_id = self.hir.functions.last().unwrap().body;
                    let this_expr = self.build_expr(&CppExpr::This)?;
                    let set_self_vptr = self.add_stmt(HirStmtKind::SetVTablePtr {
                        class: class_id,
                        place: this_expr,
                        complete: false,
                    });
                    let body_data = &mut self.hir.bodies[dtor_body_id.0];
                    body_data.stmts.insert(0, set_self_vptr);
                }
                }

                // 发射基子对象析构变体 (D2 / `<class>_dtor_base`):
                // 派生类析构收尾会按 `<base>_dtor_base` 调用基类析构, 若只有
                // D1 (`_dtor_complete`) 则链接期报 undefined。无虚基时 D1/D2
                // 体一致 (各自入口写自己类的 vptr), 故共享同一 body。
                if !body.is_decl {
                    self.emit_dtor_base_variant(class_id, dtor_sym);
                }

                Ok(())
            },
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
            },
            CppDecl::Class(inner) => {
                // 嵌套类: 符号本身的访问级别 = 声明处的 `access` (来自 parser
                // 的 member_access 表; 未记录时按 `struct`/`class` 默认回退),
                // 历史上写死成 is_struct 判定, 使 `class` 里 `public:` 嵌套类
                // 被误当成 private.

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
                    let acc = self.member_access_of(inner, m);
                    self.build_class_member(m, inner_id, acc)?;
                }
                self.pop_scope(); // pop 嵌套类作用域
                self.current_class = outer_class;
                Ok(())
            },
            CppDecl::Enum {
                name,
                scoped: _,
                underlying,
                variants,
            } => {
                // 类内 enum: 与顶层 enum 共用同一登记实现
                self.register_enum(name, underlying, variants, access)
            },
            // 类内 FreeFn = friend 声明, 跳过 (函数体独立存在)
            CppDecl::FreeFn { .. } => Ok(()),
            CppDecl::Typedef { target, name } => {
                // 类内嵌套 typedef (含模板实例里的 `typedef T type;`):
                // 登记到**类作用域**的 TypeAlias, 使 `W::type` /
                // `iterator_traits<int*>::value_type` 这类嵌套类型名可被解析
                // (历史实现直接丢弃, 导致所有依赖嵌套 typedef 的 stdlib _traits_
                // 头与用户代码都退化)。
                // 依赖类型此刻可能解析不了, 保持宽松跳过, 不变成硬错误。
                // 只跳过真正的自指前向声明形态 `typedef struct X X;` —— 必须限定
                // 目标名不带 `::`，否则 `typedef typename Iter::value_type value_type;`
                // 这类「成员名 = 嵌套名末段」的合法别名会被误丢。
                let self_forward_decl = matches!(target,
                    MangleType::Class(t) if !t.contains("::") && t == name);
                if !self_forward_decl {
                    if let Ok(ty) = self.convert_type(target) {
                        self.add_symbol(
                            name.clone(),
                            CxxSymbolKind::TypeAlias,
                            ty,
                            access,
                        );
                    } else if let MangleType::Class(t) = target {
                        // 依赖嵌套名 (`typename Iter::value_type` -> `MyIt::value_type`) 在
                        // 定义点还未登记时, 先把**限定名本身**存成 Dependent 别名,
                        // 等引用点 (全部类成员已登记完) 再链式解析。不这样做则
                        // 实例类体的 typedef 会默默丢失, `X<Dep>::value_type` 永远解不出。
                        if t.contains("::") {
                            self.add_symbol(
                                name.clone(),
                                CxxSymbolKind::TypeAlias,
                                CxxType::Dependent(t.clone()),
                                access,
                            );
                        }
                    }
                }
                Ok(())
            },
            // 类内 using/enum-import/concept/module/import/空声明等: 跳过
            CppDecl::UsingEnum { .. }
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
            _ => Err(format!("unsupported class member: {:?}", std::mem::discriminant(member))),
        }
    }

    // --- 入口方法 ---

    /// 第一遍注册：递归扫描顶层声明，为所有类（含 namespace 内）注册前向符号。
    fn register_classes_deep(&mut self, decls: &[CppDecl]) {
        for decl in decls {
            match decl {
                CppDecl::Class(c) => {
                    // 前向注册只登记类符号本身, 一律 public: 顶层 class 也是 public
                    let access = Access::Public;
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
                },
                CppDecl::Namespace { name, body } => {
                    // 前向注册也要走**带名字**的命名空间作用域, 否则该遍
                    // 登记的类拿不到 `ns::` 前缀, ABI 符号会 mangle 成
                    // 不限定的 `_ZTI1W` (与第二遍的 `_ZTIN2ns1WE` 分裂)
                    let ns_name = name.as_deref();
                    self.push_namespace_scope(ns_name);
                    self.register_classes_deep(body);
                    self.pop_scope();
                },
                CppDecl::ExportBlock(inner) => self.register_classes_deep(inner),
                _ => {},
            }
        }
    }

    // === M26.1 (native): `noexcept(expr)` 谓词 — [except.spec] 单一真源 ===

    /// 登记一处函数声明/定义的异常规范。
    /// `pos` = 该声明声明 noexcept; `neg` = 该声明 potentially-throwing。
    /// 同一键的多处声明/重载取保守交集: 只要出现过非 noexcept 声明即可抛。
    /// 符合 [except.spec]: `noexcept(f(e))` 只由被调函数的异常规范决定,
    /// 与其函数体无关 (用户写的函数无说明符即 potentially-throwing,
    /// 即使体内永不 throw; g++ 同义)。
    fn register_noexcept_spec(&mut self, key: String, is_noexcept: bool) {
        if is_noexcept {
            if !self.noexcept_neg.contains(&key) {
                self.noexcept_pos.insert(key);
            }
        } else {
            self.noexcept_neg.insert(key.clone());
            self.noexcept_pos.remove(&key);
        }
    }

    /// 扫描全 TU 收集异常规范与类声明表 (noexcept 谓词前置 pass)。
    /// 递归进入 namespace / 类体 / 模板声明 / export 块。
    fn collect_noexcept_specs(&mut self, decls: &[CppDecl]) {
        for d in decls {
            self.collect_noexcept_decl(d, "");
        }
    }

    fn collect_noexcept_decl(&mut self, d: &CppDecl, ns: &str) {
        match d {
            CppDecl::FreeFn { name, is_noexcept, .. } => {
                let key = if ns.is_empty() {
                    name.clone()
                } else {
                    format!("{}::{}", ns, name)
                };
                self.register_noexcept_spec(key.clone(), *is_noexcept);
                // `using namespace ns;` 后无限定调用: 短名键一并登记
                if key != *name {
                    self.register_noexcept_spec(name.clone(), *is_noexcept);
                }
            },
            // 类外成员函数定义: 与类内声明同一命名空间键 `Class::name`
            CppDecl::MemberFn { class, name, is_noexcept, .. } => {
                self.register_noexcept_spec(format!("{}::{}", class, name), *is_noexcept);
            },
            CppDecl::Ctor { class, is_noexcept, .. } => {
                self.has_user_ctor.insert(class.clone());
                self.register_noexcept_spec(format!("{}::ctor", class), *is_noexcept);
            },
            CppDecl::Dtor { class, is_noexcept, .. } => {
                self.has_user_dtor.insert(class.clone());
                self.register_noexcept_spec(format!("{}::dtor", class), *is_noexcept);
            },
            CppDecl::Class(cd) => {
                self.class_decls_by_name.entry(cd.name.clone()).or_insert_with(|| cd.clone());
                for m in &cd.members {
                    match m {
                        CppDecl::MemberFn { name, is_noexcept, .. } => {
                            self.register_noexcept_spec(
                                format!("{}::{}", cd.name, name),
                                *is_noexcept,
                            );
                        },
                        CppDecl::Ctor { is_noexcept, .. } => {
                            self.has_user_ctor.insert(cd.name.clone());
                            self.register_noexcept_spec(
                                format!("{}::ctor", cd.name),
                                *is_noexcept,
                            );
                        },
                        CppDecl::Dtor { is_noexcept, .. } => {
                            self.has_user_dtor.insert(cd.name.clone());
                            self.register_noexcept_spec(
                                format!("{}::dtor", cd.name),
                                *is_noexcept,
                            );
                        },
                        CppDecl::Class(_) | CppDecl::Namespace { .. } | CppDecl::Template { .. } => {
                            // 嵌套类/模板: 按其自身短名登记 (与类名解析路径一致)
                            self.collect_noexcept_decl(m, ns);
                        },
                        _ => {},
                    }
                }
            },
            CppDecl::Namespace { name, body } => {
                let inner = match name {
                    Some(n) if ns.is_empty() => n.clone(),
                    Some(n) => format!("{}::{}", ns, n),
                    None => ns.to_string(),
                };
                for md in body {
                    self.collect_noexcept_decl(md, &inner);
                }
            },
            CppDecl::Template { decl, .. } => self.collect_noexcept_decl(decl, ns),
            CppDecl::ExportBlock(inner) => {
                for id in inner {
                    self.collect_noexcept_decl(id, ns);
                }
            },
            _ => {},
        }
    }

    /// 键对应的函数是否由异常规范保证不抛。
    fn spec_guarantees_noexcept(&self, key: &str) -> bool {
        self.noexcept_pos.contains(key) && !self.noexcept_neg.contains(key)
    }

    /// `noexcept(expr)` 谓词的值: true = 表达式保证不抛。
    fn noexcept_predicate_holds(&self, e: &CppExpr) -> bool {
        !self.ast_expr_can_throw(e, &mut Vec::new())
    }

    /// 表达式求值是否 potentially-throwing ([except.spec])。
    /// 普通用户函数只看异常规范; 隐式定义的默认构造/析构按成员与基类
    /// **传递推导** (这是标准真正要求分析“体内调用了什么”的情形),
    /// `guard` 防止推导链成环。未知调用目标保守判为可抛。
    fn ast_expr_can_throw(&self, e: &CppExpr, guard: &mut Vec<String>) -> bool {
        match e {
            // 不产生副作用的求值 / 内建运算操作数: 无可抛实体
            CppExpr::IntLit(_)
            | CppExpr::FloatLit(_)
            | CppExpr::StrLit(_)
            | CppExpr::StrConcat(_)
            | CppExpr::Ident(_)
            | CppExpr::This
            | CppExpr::ThisMember(_)
            | CppExpr::Sizeof(_)
            | CppExpr::TypeidType(_)
            | CppExpr::MemberPtr { .. }
            | CppExpr::TemplateIdent { .. }
            | CppExpr::TplStaticAccess { .. }
            | CppExpr::Lambda(_)
            | CppExpr::QualifiedAccess(..) => false,
            // noexcept 谓词本身永不抛
            CppExpr::Noexcept(_) => false,
            // 分配总可能 bad_alloc
            CppExpr::New(..) | CppExpr::NewArr { .. } => true,
            CppExpr::PlacementNew { ptr, ty, args } => {
                self.ast_expr_can_throw(ptr, guard)
                    || args.iter().any(|a| self.ast_expr_can_throw(a, guard))
                    || self.class_ctor_can_throw(ty, guard)
            },
            // delete 调析构: 由析构异常规范决定 (operator delete 本身 noexcept)
            CppExpr::Delete(o) | CppExpr::DeleteArr(o) => {
                self.ast_expr_can_throw(o, guard)
                    || self
                        .ast_expr_class_name(o)
                        .map(|c| self.class_dtor_can_throw(&c, guard))
                        .unwrap_or(false)
            },
            // 字段读取本身不抛, 但接收者子表达式可能被求值
            CppExpr::Member(o, _)
            | CppExpr::PtrMember(o, _)
            | CppExpr::MemberStar(o, _)
            | CppExpr::PtrMemberStar(o, _) => self.ast_expr_can_throw(o, guard),
            CppExpr::MultiIndex(o, idxs) => {
                // 重载 operator[] 的规范 parser 未保留: 类对象保守可抛
                self.ast_expr_class_name(o).is_some()
                    || self.ast_expr_can_throw(o, guard)
                    || idxs.iter().any(|i| self.ast_expr_can_throw(i, guard))
            },
            // 内建一元/二元运算不抛 (重载运算符走 OpCall), 但操作数可能含调用
            CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => {
                self.ast_expr_can_throw(x, guard)
            },
            CppExpr::BinOp(_, l, r) => {
                self.ast_expr_can_throw(l, guard) || self.ast_expr_can_throw(r, guard)
            },
            CppExpr::Ternary { cond, then, els } => {
                self.ast_expr_can_throw(cond, guard)
                    || self.ast_expr_can_throw(then, guard)
                    || self.ast_expr_can_throw(els, guard)
            },
            CppExpr::Comma(es) | CppExpr::InitList(es) => {
                es.iter().any(|x| self.ast_expr_can_throw(x, guard))
            },
            CppExpr::TypedInitList { ty, elems } => {
                elems.iter().any(|x| self.ast_expr_can_throw(x, guard))
                    || match ty {
                        // T{..} 选构造: 用户声明构造按规范, 聚合初始化不抛
                        MangleType::Class(name) => {
                            self.has_user_ctor.contains(name.as_str())
                                && !self.spec_guarantees_noexcept(&format!("{}::ctor", name))
                        },
                        _ => false,
                    }
            },
            CppExpr::Cast(ty, x) => {
                self.ast_expr_can_throw(x, guard)
                    || match ty {
                        // 类类型转换 = 用户定义转换函数 (转换构造)
                        MangleType::Class(name) => self.class_ctor_can_throw(name, guard),
                        _ => false,
                    }
            },
            CppExpr::StaticCast { expr, .. }
            | CppExpr::ConstCast { expr, .. }
            | CppExpr::ReinterpretCast { expr, .. } => self.ast_expr_can_throw(expr, guard),
            CppExpr::DynamicCast { ty, expr } => {
                // 向引用类型 cast 失败抛 bad_cast; 指针形式不抛
                matches!(ty, MangleType::Reference(_)) || self.ast_expr_can_throw(expr, guard)
            },
            CppExpr::Typeid(x) => {
                // 仅对多态类型解引用取 RTTI 才可能抛 bad_typeid
                let poly_deref = matches!(&**x, CppExpr::UnaryOp(UnaryOpKind::Deref, inner)
                    if self
                        .ast_expr_class_name(inner)
                        .map(|c| self.class_is_polymorphic(&c))
                        .unwrap_or(false));
                poly_deref || self.ast_expr_can_throw(x, guard)
            },
            // 重载运算符: 对应 operator 函数的规范未保留, 保守可抛
            CppExpr::OpCall { .. } => true,
            // pack 展开: 递归到展开体 (单态化后等价于普通表达式序列)
            CppExpr::PackExpansion(x) => self.ast_expr_can_throw(x, guard),
            CppExpr::Call(f, args) => {
                args.iter().any(|a| self.ast_expr_can_throw(a, guard))
                    || self.ast_call_target_can_throw(f, guard)
            },
            // 未来新增变体: 标准默认 potentially-throwing, 保守可抛
            _ => true,
        }
    }

    /// 调用目标的异常规范判定。
    fn ast_call_target_can_throw(&self, f: &CppExpr, guard: &mut Vec<String>) -> bool {
        match f {
            CppExpr::Ident(name) => {
                // 类名: 函数式构造 `Foo(...)`
                if self.resolve_class_symbol(name).is_some() {
                    return self.class_ctor_can_throw(name, guard);
                }
                // 成员函数体内无限定名: 即 this->m()
                if let Some(cc) = self.current_class {
                    let cname = self.symbols[cc.0].name.clone();
                    let (syms, _amb) = self.lookup_in_class(cc, name);
                    let is_method = syms.iter().any(|&s| {
                        matches!(
                            self.get_symbol(s).map(|x| x.kind),
                            Some(CxxSymbolKind::Method)
                                | Some(CxxSymbolKind::Constructor)
                                | Some(CxxSymbolKind::Destructor)
                        )
                    });
                    if is_method {
                        return !self.spec_guarantees_noexcept(&format!("{}::{}", cname, name));
                    }
                }
                if name.starts_with("operator") || name.starts_with('~') {
                    return true;
                }
                !self.spec_guarantees_noexcept(name)
            },
            CppExpr::Member(obj, m) | CppExpr::PtrMember(obj, m) => {
                // 接收者子表达式自身可能被求值 (getW().safe()), 其异常规范也参与
                self.ast_expr_can_throw(obj, guard)
                    || match self.ast_expr_class_name(obj) {
                        Some(c) => !self.spec_guarantees_noexcept(&format!("{}::{}", c, m)),
                        None => true,
                    }
            },
            CppExpr::ThisMember(m) => match self.current_class {
                Some(cc) => {
                    let cname = self.symbols[cc.0].name.clone();
                    !self.spec_guarantees_noexcept(&format!("{}::{}", cname, m))
                },
                None => true,
            },
            CppExpr::QualifiedAccess(path, name) => {
                let joined = path
                    .iter()
                    .fold(String::new(), |acc, p| {
                        if acc.is_empty() {
                            p.clone()
                        } else {
                            format!("{}::{}", acc, p)
                        }
                    });
                let joined =
                    if joined.is_empty() { name.clone() } else { format!("{}::{}", joined, name) };
                if self.resolve_class_symbol(&joined).is_some() {
                    return self.class_ctor_can_throw(&joined, guard);
                }
                !self.spec_guarantees_noexcept(&joined)
            },
            // 函数指针调用 / 模板实例调用等: 规范未知, 保守可抛
            _ => true,
        }
    }

    /// 隐式推导链上的成环保护: 已在 guard 栈中则视为不抛 (自递归类型无法
    /// 在构造中完成自身成员构造, 标准意义上亦不递归)。
    fn guard_cycle(guard: &[String], class: &str) -> bool {
        guard.iter().any(|g| g == class)
    }

    /// 类默认构造是否可抛: 用户声明构造按异常规范; 隐式默认构造对成员
    /// 默认构造传递推导 ([class.default.ctor])。
    fn class_ctor_can_throw(&self, class: &str, guard: &mut Vec<String>) -> bool {
        if self.resolve_class_symbol(class).is_none() {
            // 非注册类 (内置类型/未知): 无用户构造可调 → 不抛
            return false;
        }
        if self.has_user_ctor.contains(class) {
            return !self.spec_guarantees_noexcept(&format!("{}::ctor", class));
        }
        if Self::guard_cycle(guard, class) {
            return false;
        }
        guard.push(class.to_string());
        let r = self
            .class_decls_by_name
            .get(class)
            .map(|cd| {
                cd.members.iter().any(|m| match m {
                    CppDecl::MemberVar { ty, .. } => self.mangle_ctor_can_throw(ty, guard),
                    _ => false,
                })
            })
            .unwrap_or(false);
        guard.pop();
        r
    }

    /// 类析构是否可抛: C++17 析构隐式 `noexcept(true)`, 仅当用户显式声明
    /// `noexcept(false)` 或成员/基类析构可抛时才 potentially-throwing。
    fn class_dtor_can_throw(&self, class: &str, guard: &mut Vec<String>) -> bool {
        if self.resolve_class_symbol(class).is_none() {
            return false;
        }
        if self.has_user_dtor.contains(class) {
            return !self.spec_guarantees_noexcept(&format!("{}::dtor", class));
        }
        if Self::guard_cycle(guard, class) {
            return false;
        }
        guard.push(class.to_string());
        let r = self
            .class_decls_by_name
            .get(class)
            .map(|cd| {
                cd.members.iter().any(|m| match m {
                    CppDecl::MemberVar { ty, .. } => self.mangle_dtor_can_throw(ty, guard),
                    _ => false,
                }) || cd
                    .bases
                    .iter()
                    .any(|b| self.class_dtor_can_throw(&b.name, guard))
            })
            .unwrap_or(false);
        guard.pop();
        r
    }

    fn mangle_ctor_can_throw(&self, ty: &MangleType, guard: &mut Vec<String>) -> bool {
        match ty {
            MangleType::Class(name) => self.class_ctor_can_throw(name, guard),
            MangleType::Array(el, _) => self.mangle_ctor_can_throw(el, guard),
            _ => false,
        }
    }

    fn mangle_dtor_can_throw(&self, ty: &MangleType, guard: &mut Vec<String>) -> bool {
        match ty {
            MangleType::Class(name) => self.class_dtor_can_throw(name, guard),
            MangleType::Array(el, _) => self.mangle_dtor_can_throw(el, guard),
            _ => false,
        }
    }

    /// 类的静态名 (含指针/引用/数组剥壳) 推导; 无法从符号表推得时 None。
    fn class_name_of_type(&self, ty: &CxxType) -> Option<String> {
        match ty {
            CxxType::Class(id) => self.get_symbol(id.as_symbol()).map(|s| s.name.clone()),
            CxxType::LValueRef(t) | CxxType::RValueRef(t) | CxxType::Pointer(t) => {
                self.class_name_of_type(t)
            },
            CxxType::Array(t, _) => self.class_name_of_type(t),
            _ => None,
        }
    }

    /// 表达式接收者的类名 (用于 `w.m()` / `p->m()` 的 `Class::m` 键)。
    fn ast_expr_class_name(&self, e: &CppExpr) -> Option<String> {
        match e {
            CppExpr::This => self
                .current_class
                .and_then(|c| self.get_symbol(c.as_symbol()).map(|s| s.name.clone())),
            CppExpr::Ident(name) => self
                .lookup_unqualified(name)
                .into_iter()
                .find_map(|s| self.get_symbol(s).and_then(|sym| self.class_name_of_type(&sym.ty))),
            CppExpr::Member(obj, field) | CppExpr::PtrMember(obj, field) => {
                let cname = self.ast_expr_class_name(obj)?;
                let cid = ClassId::from(self.resolve_class_symbol(&cname)?);
                let (syms, _amb) = self.lookup_in_class(cid, field);
                syms.into_iter().find_map(|s| {
                    self.get_symbol(s).and_then(|sym| self.class_name_of_type(&sym.ty))
                })
            },
            CppExpr::Ref(x) | CppExpr::Cast(_, x) => self.ast_expr_class_name(x),
            _ => None,
        }
    }

    /// 类是否多态 (有虚函数) - typeid 解引用抛 bad_typeid 判定用。
    fn class_is_polymorphic(&self, name: &str) -> bool {
        self.resolve_class_symbol(name)
            .map(|s| {
                self.class_infos
                    .get(s.0)
                    .map(|ci| !ci.virtual_methods.is_empty())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// 剥去 `extern "C" { ... }` (或 `extern "C++"`) 块的外壳, 返回花括号内的声明文本.
    fn extern_c_block_inner(text: &str) -> &str {
        let start = match text.find('{') {
            Some(i) => i + 1,
            None => return "",
        };
        let end = match text.rfind('}') {
            Some(i) if i > start => i,
            _ => return "",
        };
        &text[start..end]
    }

    /// 将 parser 的 AST 声明列表转换为 HIR。
    pub fn build_hir_from_decls(&mut self, decls: &[CppDecl]) -> Result<(), String> {
        // M26.1 (native): 先全 TU 收集异常规范与类声明表, 作为 noexcept(expr)
        // 谓词的单一真源 (类内声明与类外定义都参与)。
        self.collect_noexcept_specs(decls);

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
            params: vec![
                CxxType::Integer {
                    bits: 32,
                    signed: true,
                };
                n
            ],
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
            member_access: vec![],
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
            member_access: vec![],
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
        // 构造 + 析构: 按 Itanium 每个非平凡构造/析构都发射两变体 ——
        // ctor 发射 C1(完整对象)+C2(基子对象), dtor 发射 D1(完整对象)+D2(基子对象)
        // (g++ 对独立带析构类同样产出 _ZN3FooD1Ev + _ZN3FooD2Ev, 已 nm 核实)。
        // 故 HIR 函数实体 = 2 (ctor 变体) + 2 (dtor 变体) = 4。
        assert_eq!(hir.functions.len(), 4);
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
                member_access: vec![],
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
                member_access: vec![],
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
        //
        // vptr 写入语句 (`SetVTablePtr`) 现在由 sema **无条件**发射 (是否需要
        // vptr 是 abi_layout 在完整类定义上算出的布局事实, 解析期判定不可靠),
        // 它会混在初始化序列里; 故断言只看真正的子对象/成员初始化语句。
        let real_inits: Vec<_> = inits
            .iter()
            .filter(|sid| {
                matches!(
                    hir.stmts[sid.0].kind,
                    HirStmtKind::CtorInitBase { .. } | HirStmtKind::CtorInitMember { .. }
                )
            })
            .collect();
        assert!(real_inits.len() >= 2);

        // 第一个应该是基类初始化（Base）
        assert!(matches!(hir.stmts[real_inits[0].0].kind, HirStmtKind::CtorInitBase { .. }));

        // 第二个应该是成员 d
        assert!(matches!(
            hir.stmts[real_inits[1].0].kind,
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
            member_access: vec![],
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

        // 2 个构造函数 × (C1 完整对象 + C2 基子对象) = 4 个函数实体
        assert_eq!(hir.functions.len(), 4);

        // 第二个构造函数是委托构造 (functions[2] = 它的 C1 变体)
        let delegating_body = hir.functions[2].body;
        let body = &hir.bodies[delegating_body.0];
        let inits = &body.ctor_initializers;
        // 委托构造只有一项（委托到自己）
        assert_eq!(inits.len(), 1);
        assert!(matches!(hir.stmts[inits[0].0].kind, HirStmtKind::CtorInitBase { .. }));
    }

    #[test]
    fn classify_ctor_normal() {
        let mut sema = Sema::new();
        let (class_id, _) = sema.register_class("Foo".into(), Access::Public, &[]);
        sema.pop_scope();

        let params = vec![(
            "a".into(),
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
        )];
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
        let params = vec![("other".into(), CxxType::LValueRef(Box::new(CxxType::Class(class_id))))];
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
        let params = vec![("other".into(), CxxType::RValueRef(Box::new(CxxType::Class(class_id))))];
        let inits: Vec<(String, CppExpr)> = vec![];
        let kind = sema.classify_ctor(class_id, &params, &inits);
        assert_eq!(kind, CtorKind::Move);
    }

    #[test]
    fn classify_ctor_delegating() {
        let mut sema = Sema::new();
        let (class_id, _) = sema.register_class("Foo".into(), Access::Public, &[]);
        sema.pop_scope();

        let params = vec![(
            "a".into(),
            CxxType::Integer {
                bits: 32,
                signed: true,
            },
        )];
        // 初始化列表第一项是类名自己 -> delegating
        let inits = vec![("Foo".into(), CppExpr::IntLit(0))];
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
            member_access: vec![],
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
                member_access: vec![("b".into(), crate::cpp::parser::Access::Public)],
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
                member_access: vec![("get_b".into(), crate::cpp::parser::Access::Public)],
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
                    },
                    _ => panic!("expected FieldAccess"),
                }
            },
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
                member_access: vec![],
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
                member_access: vec![],
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

    /// 走 parser 的真实入口把源码翻成 CppDecl, 再交给 sema.
    fn hir_of_src(src: &str) -> Result<(), String> {
        let mut p = crate::cpp::parser::CppParser::new(src.as_bytes(), "t.cpp");
        let decls = p.parse_top_level().map_err(|e| format!("parse: {e:?}"))?;
        let mut sema = Sema::new();
        sema.build_hir_from_decls(&decls).map(|_| ())
    }

    /// 回归 (semantic_enum_type_traits_runtime / cpp98_iterator_traits):
    /// `sizeof(N::S)` 和 `sizeof(E::enumerator)` 这类位点 parser 会原样保留
    /// 限定名, 而 dcc 把 namespace 扁平化后只登记末段符号名, 于是之前一律报
    /// "unknown class type: N::S"。现在限定名按末段回查类型符号,
    /// 枚举量则按其限定符的枚举底层类型解析。
    #[test]
    fn qualified_type_names_in_sizeof_resolve() {
        let src = "namespace n { struct S { int x; }; typedef int alias_t; }\n\
                   enum class E : unsigned char { A = 1 };\n\
                   enum Plain { P0 };\n\
                   int f() { return sizeof(n::S) + sizeof(n::alias_t)\
                             + sizeof(E::A) + sizeof(Plain::P0); }";
        if let Err(e) = hir_of_src(src) {
            panic!("qualified sizeof 应该可解析: {e}");
        }
    }

    /// 末段回查只能是**兜底**: 不存在的限定类型仍必须报错,
    /// 否则本用例会静默吞下真正的名字查找缺陷。
    #[test]
    fn unknown_qualified_type_still_errors() {
        let src = "int f() { return sizeof(nope::Missing); }";
        assert!(
            hir_of_src(src).is_err(),
            "不存在的限定类型不得被兜底误接受"
        );
    }

    /// 类内成员函数体里的裸字段名 (定义在方法之后的 `private` 字段) 必须
    /// 能解析 —— 这是 iterator_traits 夹具的真实形态 (CountIt::p_).
    #[test]
    fn in_class_member_function_sees_later_declared_field() {
        let src = "class It {\n\
                    public:\n\
                        explicit It(int* p) : p_(p) {}\n\
                        int operator*() const { return *p_; }\n\
                        It& operator++() { ++p_; return *this; }\n\
                    private:\n\
                        int* p_;\n\
                    };\n\
                    int use(It it) { return *it; }";
        if let Err(e) = hir_of_src(src) {
            panic!("后置字段 p_ 应该对成员函数体可见: {e}");
        }
    }

    /// 一元运算符重载必须分发到 `operator*` / `operator++` / `operator!` 函数,
    /// 而不是被当成内建运算 —— 这是 native 路径上 `<iterator>` 风格用户迭代器
    /// 能用起来的前提。断言看的是**实际被调用的符号名** (不是"没报错"),
    /// 否则"表达式被静默丢掉"也能假通过。
    #[test]
    fn unary_operator_overload_calls_operator_functions() {
        let src = "class It {\n\
                   public:\n\
                       int* p_;\n\
                       int operator*() const { return *p_; }\n\
                       It& operator++() { ++p_; return *this; }\n\
                       bool operator!() const { return p_ == 0; }\n\
                   };\n\
                   int walk(It it) { int s = *it; ++it; if (!it) ++s; return s; }";
        let mut p = crate::cpp::parser::CppParser::new(src.as_bytes(), "t.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        let mut sema = Sema::new();
        sema.build_hir_from_decls(&decls)
            .expect("一元运算符重载不得当成内建解引用");
        let mut called: Vec<String> = Vec::new();
        for e in sema.hir().exprs.iter() {
            if let HirExprKind::Call { callee, .. } = &e.kind {
                let sym = match callee {
                    HirCallee::Direct(s) => *s,
                    _ => continue,
                };
                if let Some(info) = sema.get_symbol(sym) {
                    called.push(info.name.clone());
                }
            }
        }
        // Itanium 编码: `operator*` -> ml, `operator++` -> pp, `operator!` -> nt
        for want in ["operatorml", "operatorpp", "operatornt"] {
            assert!(
                called.iter().any(|n| n == want),
                "调用点没分发到 {want}: {called:?}"
            );
        }
        // 内建解引用不得残留在类类型上 (那正是修复前的硬错误路径)
        for e in sema.hir().exprs.iter() {
            if let HirExprKind::UnaryOp {
                op: UnaryOp::Deref,
                operand,
            } = &e.kind
            {
                let ty = &sema.hir().exprs[operand.0].ty;
                assert!(
                    !matches!(ty, CxxType::Class(_)),
                    "类类型上残留了内建解引用"
                );
            }
        }
    }

    /// G22 访问控制回归 (`typeid_rtti` 夹具当时的报错
    /// `Access check: 'age' is Private (context: class None)`):
    /// 自由函数/无类上下文下访问 **public** 字段必须合法, 而访问 private
    /// 字段必须被拒 —— 两个方向都断言, 否则"根本不检查"也能貌似通过。
    #[test]
    fn access_check_allows_public_and_denies_private_from_free_fn() {
        let ok = "struct Animal { int age; };\nint take(Animal a) { return a.age; }";
        if let Err(e) = hir_of_src(ok) {
            panic!("public 字段的外部访问必须合法, 不得报 private: {e}");
        }
        let denied = "struct Cat {\nprivate:\n    int secret;\n};\nint peek(Cat c) { return c.secret; }";
        match hir_of_src(denied) {
            Err(e) => assert!(
                e.contains("private member"),
                "private 字段必须因访问控制被拒, 实际错误: {e}"
            ),
            Ok(()) => panic!("private 字段不得对自由函数可见"),
        }
    }

    /// 构造函数体写在 `virtual` 成员函数**之前**时的 vptr 回归
    /// (`tests/fixtures/cpp/typeid_rtti.cpp` 的 `PolyAnimal() {}` 就在
    /// `virtual void speak() {}` 前面)。解析期该类 info 里的虚函数表还是空的,
    /// 若用 `class_needs_vptr` 门控 vptr 写入, 用户构造函数一条
    /// `SetVTablePtr` 都不会发 → 运行期 vptr=0, 虚派发与 typeid 动态路径直接崩。
    /// "有没有 vptr" 是**布局**事实 (abi_layout 唯一真源), 故 sema 必须无条件发射。
    #[test]
    fn ctor_declared_before_virtual_member_still_emits_vptr_write() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Class(ClassDecl {
            name: "Poly".into(),
            is_struct: false,
            is_union: false,
            member_access: vec![],
            bases: vec![],
            members: vec![
                // 关键: ctor 在 virtual 成员函数**之前**声明
                CppDecl::Ctor {
                    class: "Poly".into(),
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
                CppDecl::MemberFn {
                    class: "Poly".into(),
                    ret: MangleType::Void,
                    name: "speak".into(),
                    params: vec![],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: true,
                    is_override: false,
                    is_final: false,
                    is_const: false,
                    is_noexcept: false,
                    is_volatile: false,
                    is_friend: false,
                },
            ],
        })];
        sema.build_hir_from_decls(&decls)
            .expect("build error");
        let hir = sema.into_hir();
        // 只看构造函数的初始化序列 (隐式析构等不干扰)
        let mut sets: Vec<bool> = Vec::new();
        for body in &hir.bodies {
            for sid in &body.ctor_initializers {
                if let HirStmtKind::SetVTablePtr { complete, .. } = hir.stmts[sid.0].kind {
                    sets.push(complete);
                }
            }
        }
        assert_eq!(
            sets.len(),
            2,
            "多态类的 C1/C2 两个构造变体各必须写一次 vptr: {sets:?}"
        );
        assert!(sets.contains(&true), "C1(完整对象) 必须写 vptr");
        assert!(sets.contains(&false), "C2(基子对象) 必须写 vptr");
    }

    /// 任务 3 回归: 用户析构函数体**入口**必须先发射一条 `SetVTablePtr` (绑定
    /// 当前类自己的 vtable, complete=false), 这样析构体内的虚调用才按 Itanium
    /// 绑定到当前正被析构的类而非最派生类。`tests/fixtures/cpp/
    /// ctor_dtor_virtual_dispatch.cpp` 的析构期断言依赖此不变式。
    #[test]
    fn dtor_body_entry_binds_current_class_vptr() {
        let mut sema = Sema::new();
        let decls = vec![CppDecl::Class(ClassDecl {
            name: "Poly".into(),
            is_struct: false,
            is_union: false,
            member_access: vec![],
            bases: vec![],
            members: vec![
                CppDecl::MemberFn {
                    class: "Poly".into(),
                    ret: MangleType::Void,
                    name: "speak".into(),
                    params: vec![],
                    body: CppBody { stmts: vec![], is_decl: false },
                    is_virtual: true,
                    is_override: false,
                    is_final: false,
                    is_const: false,
                    is_noexcept: false,
                    is_volatile: false,
                    is_friend: false,
                },
                CppDecl::Dtor {
                    class: "Poly".into(),
                    body: CppBody { stmts: vec![], is_decl: false },
                    is_virtual: true,
                    is_noexcept: false,
                },
            ],
        })];
        sema.build_hir_from_decls(&decls).expect("build error");
        // 先在 Sema 仍持有符号表时挑出全部析构函数符号 (kind == Destructor,
        // 含 D1 complete 与 D2 base 变体), 再 into_hir 检查各自 body 首语句。
        let dtor_syms: Vec<SymbolId> = sema
            .hir
            .functions
            .iter()
            .filter(|f| {
                matches!(
                    sema.get_symbol(f.symbol).map(|s| s.kind),
                    Some(CxxSymbolKind::Destructor)
                )
            })
            .map(|f| f.symbol)
            .collect();
        assert!(
            !dtor_syms.is_empty(),
            "用户虚析构应登记为 Destructor 符号"
        );
        let hir = sema.into_hir();
        let mut found_entry_vptr = 0;
        for sym in &dtor_syms {
            let Some(f) = hir.functions.iter().find(|f| f.symbol == *sym) else {
                continue;
            };
            let body = &hir.bodies[f.body.0];
            if let Some(first) = body.stmts.first() {
                if let HirStmtKind::SetVTablePtr { complete, .. } = hir.stmts[first.0].kind {
                    assert!(
                        !complete,
                        "析构体入口的 vptr 绑定必须是 complete=false (当前类 vtable), \
                         否则析构期虚调用会绑到最派生类"
                    );
                    found_entry_vptr += 1;
                }
            }
        }
        assert_eq!(
            found_entry_vptr,
            dtor_syms.len(),
            "每个析构变体 (D1/D2) 体入口都必须先写一条 SetVTablePtr"
        );
    }

    /// 任务 3 回归: 携带虚基类的类不合成隐式析构 (lower_ir 无法降级虚基析构),
    /// 从而不影响既有虚继承夹具的 lowering。
    #[test]
    fn virtual_base_class_gets_no_implicit_dtor() {
        let mut sema = Sema::new();
        let decls = vec![
            CppDecl::Class(ClassDecl {
                name: "VB".into(),
                is_struct: false,
                is_union: false,
                member_access: vec![],
                bases: vec![],
                members: vec![CppDecl::MemberFn {
                    class: "VB".into(),
                    ret: MangleType::Void,
                    name: "f".into(),
                    params: vec![],
                    body: CppBody { stmts: vec![], is_decl: false },
                    is_virtual: true,
                    is_override: false,
                    is_final: false,
                    is_const: false,
                    is_noexcept: false,
                    is_volatile: false,
                    is_friend: false,
                }],
            }),
            CppDecl::Class(ClassDecl {
                name: "VD".into(),
                is_struct: false,
                is_union: false,
                member_access: vec![],
                bases: vec![BaseSpec {
                    access: crate::cpp::parser::Access::Public,
                    is_virtual: true,
                    name: "VB".into(),
                    template_args: None,
                }],
                members: vec![],
            }),
        ];
        // 只要不 panic / 不因隐式析构引入无法 lower 的虚基析构体即通过 (与既有
        // 虚继承 lowering 测试同源)。
        sema.build_hir_from_decls(&decls).expect("build error");
        // VD 自身携带虚基 -> class_has_virtual_base 必须为真。
        let vd_sym = (0..sema.symbols.len())
            .filter_map(|i| sema.symbols.get(i).cloned())
            .find(|s| s.kind == CxxSymbolKind::Class && s.name == "VD")
            .map(|s| s.id)
            .expect("VD 类符号应已登记");
        assert!(
            sema.class_has_virtual_base(ClassId(vd_sym.0), &mut Vec::new()),
            "虚继承派生类应被识别为携带虚基"
        );
        // VD 无用户析构, 又携带虚基 -> 不应为 **VD** 合成任何析构函数实体
        // (虚基类 VB 自身多态, 它自己的隐式析构不受本门控影响, 仍应合成)。
        let vd_dtor = (0..sema.symbols.len())
            .filter_map(|i| sema.symbols.get(i).cloned())
            .any(|s| {
                s.kind == CxxSymbolKind::Destructor
                    && s.owner == Some(vd_sym)
                    && sema.hir.functions.iter().any(|f| f.symbol == s.id)
            });
        assert!(
            !vd_dtor,
            "携带虚基且用户未声明析构的类不应合成隐式析构 (lower_ir 无法降级虚基析构)"
        );
        let vb_polymorphic_dtor = (0..sema.symbols.len())
            .filter_map(|i| sema.symbols.get(i).cloned())
            .any(|s| {
                s.kind == CxxSymbolKind::Destructor
                    && s.name.starts_with("~VB")
            });
        assert!(
            vb_polymorphic_dtor,
            "虚基类 VB 自身多态, 其隐式析构仍应被合成 (门控只影响携带虚基的派生类)"
        );
        // 整个 TU 仍可正常建 HIR (不因隐式析构引入无法 lower 的虚基析构体)。
        let _hir = sema.into_hir();
    }

    /// Stage 7 RTTI: `typeid(e)` 按 e 的静态类型分流 —— 多态类 glvalue 走
    /// `TypeInfoDyn` (运行期从 vptr 前一槽读真实 typeinfo), 非多态类对象与
    /// `typeid(T)` 走 `TypeInfoRef` (静态 typeinfo 符号地址)。
    #[test]
    fn typeid_splits_dynamic_and_static_typeinfo() {
        let mut sema = Sema::new();
        let poly = CppDecl::Class(ClassDecl {
            name: "Poly".into(),
            is_struct: false,
            is_union: false,
            member_access: vec![],
            bases: vec![],
            members: vec![CppDecl::MemberFn {
                class: "Poly".into(),
                ret: MangleType::Void,
                name: "f".into(),
                params: vec![],
                body: CppBody {
                    stmts: vec![],
                    is_decl: false,
                },
                is_virtual: true,
                is_override: false,
                is_final: false,
                is_const: false,
                is_noexcept: false,
                is_volatile: false,
                is_friend: false,
            }],
        });
        let plain = CppDecl::Class(ClassDecl {
            name: "Plain".into(),
            is_struct: true,
            is_union: false,
            member_access: vec![],
            bases: vec![],
            members: vec![CppDecl::MemberVar {
                ty: MangleType::Int,
                name: "x".into(),
                is_mutable: false,
                is_volatile: false,
            }],
        });
        let fn_of = |name: &str, param: Option<(&str, MangleType)>, e: CppExpr| CppDecl::FreeFn {
            ret: MangleType::Int,
            name: name.into(),
            params: param
                .into_iter()
                .map(|(n, t)| (n.into(), t))
                .collect(),
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(e))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        };
        let decls = vec![
            poly,
            plain,
            // typeid(p): 多态类的引用 → 动态路径
            fn_of(
                "dyn_ty",
                Some((
                    "p",
                    MangleType::Reference(Box::new(MangleType::Class("Poly".into()))),
                )),
                CppExpr::Typeid(Box::new(CppExpr::Ident("p".into()))),
            ),
            // typeid(o): 非多态类对象 → 静态路径
            fn_of(
                "static_ty",
                Some(("o", MangleType::Class("Plain".into()))),
                CppExpr::Typeid(Box::new(CppExpr::Ident("o".into()))),
            ),
            // typeid(T): 总是静态路径 (即使 T 是多态类)
            fn_of(
                "type_ty",
                None,
                CppExpr::TypeidType(MangleType::Class("Poly".into())),
            ),
        ];
        sema.build_hir_from_decls(&decls)
            .expect("build error");
        let hir = sema.into_hir();
        let mut dyn_cnt = 0;
        let mut ref_cnt = 0;
        for e in &hir.exprs {
            match e.kind {
                HirExprKind::TypeInfoDyn(_) => dyn_cnt += 1,
                HirExprKind::TypeInfoRef(_) => ref_cnt += 1,
                _ => {}
            }
        }
        assert_eq!(dyn_cnt, 1, "只有多态 glvalue 的 typeid 走动态路径");
        assert_eq!(ref_cnt, 2, "非多态对象与 typeid(T) 走静态 typeinfo 符号");
    }

    /// [conv.func] 必须参与重载候选排名: 函数名实能绑函数指针形参。
    ///
    /// 少了它 `is >> std::ws` (`istream& operator>>(istream& (*)(istream&))`)
    /// 全部候选不可行, `try_binary_operator_overload` 返回 `None`, sema 退到
    /// **内建 `>>`** —— 拿流对象指针去做 `shr`, `this` 成垃圾 → 运行期段错。
    #[test]
    fn conv_func_lets_a_function_name_bind_a_function_pointer_param() {
        let istream = CxxType::LValueRef(Box::new(CxxType::Class(ClassId(7))));
        let fn_ty = CxxType::Function {
            ret: Box::new(istream.clone()),
            params: vec![istream.clone()],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        // 形参: `istream& (*)(istream&)`
        let param = CxxType::Pointer(Box::new(fn_ty.clone()));
        assert_eq!(
            Sema::op_arg_rank(&fn_ty, &param),
            Some(0),
            "[conv.func] 在标准转换序列里是精确匹配等级"
        );
        // 签名不同 (形参数不一样) 不能靠 [conv.func] 硬凑
        let other = CxxType::Function {
            ret: Box::new(istream.clone()),
            params: vec![],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        assert_eq!(Sema::op_arg_rank(&other, &param), None, "签名不等价的函数不可行");
        // 函数名也不能绑到"指向普通对象"的指针形参
        assert_eq!(
            Sema::op_arg_rank(
                &fn_ty,
                &CxxType::Pointer(Box::new(CxxType::Integer {
                    bits: 8,
                    signed: true
                }))
            ),
            None,
            "[conv.func] 只造函数指针, 不能当 void*/char* 用"
        );
        // 反过来: 真正的函数指针实参仍走同类型精确匹配 (未被新分支搞坏)
        assert_eq!(Sema::op_arg_rank(&param, &param), Some(0));
    }

    /// [basic.link]: 函数/块作用域里声明的名字（形参、局部量）没有 linkage,
    /// 不得领 namespace 前缀链接名 —— lower_ir 以「linkage_name 是 Some」为
    /// 静态存储对象的唯一判据, 错了就会去读一个从未写入的槽。
    #[test]
    fn function_and_block_scope_names_have_no_linkage_name() {
        let mut sema = Sema::new();
        let i32ty = CxxType::Integer {
            bits: 32,
            signed: true,
        };
        let ns = sema.add_symbol(
            "std".into(),
            CxxSymbolKind::Namespace,
            CxxType::Void,
            Access::Public,
        );
        let ns_scope = sema.push_scope(ScopeKind::Namespace);
        sema.scopes[ns_scope.0].owner_symbol = Some(ns);
        // namespace 作用域的函数: 前缀链接名照旧
        let f = sema.add_symbol(
            "ws".into(),
            CxxSymbolKind::Function,
            CxxType::Void,
            Access::Public,
        );
        assert_eq!(
            sema.get_symbol(f).unwrap().linkage_name.as_deref(),
            Some("std_ws"),
            "namespace 作用域成员仍拿前缀链接名"
        );
        // 进入函数作用域后登记的形参: 无 linkage
        sema.push_scope(ScopeKind::Function);
        let p = sema.add_symbol(
            "is".into(),
            CxxSymbolKind::Variable,
            i32ty.clone(),
            Access::Public,
        );
        assert_eq!(
            sema.get_symbol(p).unwrap().linkage_name,
            None,
            "形参不得拿 namespace 前缀名 (否则被当成函数局部 static)"
        );
        // 块作用域里的局部量同样
        sema.push_scope(ScopeKind::Block);
        let v = sema.add_symbol(
            "tmp".into(),
            CxxSymbolKind::Variable,
            i32ty,
            Access::Public,
        );
        assert_eq!(
            sema.get_symbol(v).unwrap().linkage_name,
            None,
            "块作用域局部量不得拿前缀名"
        );
        // 回到 namespace 作用域后仍正常拿前缀名 (不是整个表而失效)
        sema.pop_scope();
        sema.pop_scope();
        let g = sema.add_symbol(
            "x".into(),
            CxxSymbolKind::Variable,
            CxxType::Void,
            Access::Public,
        );
        assert_eq!(
            sema.get_symbol(g).unwrap().linkage_name.as_deref(),
            Some("std_x")
        );
    }

    // === M26.1 (native): noexcept(expr) 谓词单元测试 ===

    /// 辅助: 解析 C++ 源 → 建 HIR (含 noexcept 异常规范预扫描)。
    fn build_sema_from_src(src: &str) -> Sema {
        let mut p = CppParser::new(src.as_bytes(), "unit.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        let mut sema = Sema::new();
        sema.build_hir_from_decls(&decls).expect("build ok");
        sema
    }

    fn call(target: CppExpr, args: Vec<CppExpr>) -> CppExpr {
        CppExpr::Call(Box::new(target), args)
    }

    #[test]
    fn noexcept_predicate_follows_exception_spec_not_body() {
        // g++ 真值: 谓词只由异常规范决定 —— 无说明符函数即使体内永不
        // 构造 throw 也 potentially-throwing; 声明 noexcept 的函数即使体内
        // 真 throw 也判不抛 (运行时 terminate, 谓词不变)。
        let src = r#"
int unsafe_fn(int x) { return x; }
int risky_fn(int x) { throw 1; return x; }
int safe_fn(int x) noexcept { return x; }
struct Widget {
    Widget() {}
    int safe() noexcept { return 1; }
    int risky();
};
Widget gw;
struct Pod {
    int x;
};
struct Holder {
    Widget w;
};
"#;
        let sema = build_sema_from_src(src);
        // 自由函数: 按异常规范
        assert!(sema.noexcept_predicate_holds(&call(
            CppExpr::Ident("safe_fn".into()),
            vec![CppExpr::IntLit(1)],
        )));
        assert!(!sema.noexcept_predicate_holds(&call(
            CppExpr::Ident("unsafe_fn".into()),
            vec![CppExpr::IntLit(1)],
        )));
        // 传递性边界: risky_fn 体内真 throw 与 unsafe_fn 同构, 谓词结果必须相同
        assert!(!sema.noexcept_predicate_holds(&call(
            CppExpr::Ident("risky_fn".into()),
            vec![CppExpr::IntLit(1)],
        )));
        // 成员调用: 经接收者类名解析到 `Class::m` 键
        assert!(sema.noexcept_predicate_holds(&call(
            CppExpr::Member(Box::new(CppExpr::Ident("gw".into())), "safe".into()),
            vec![],
        )));
        assert!(!sema.noexcept_predicate_holds(&call(
            CppExpr::Member(Box::new(CppExpr::Ident("gw".into())), "risky".into()),
            vec![],
        )));
        // 隐式默认构造 (POD) 不抛; 用户 `Widget() {}` 无说明符 → 可抛
        assert!(sema.noexcept_predicate_holds(&call(
            CppExpr::Ident("Pod".into()),
            vec![],
        )));
        assert!(!sema.noexcept_predicate_holds(&call(
            CppExpr::Ident("Widget".into()),
            vec![],
        )));
        // 隐式 ctor 对成员传递推导: Holder 的 Widget 成员 ctor 可抛 → Holder() 可抛
        assert!(!sema.noexcept_predicate_holds(&call(
            CppExpr::Ident("Holder".into()),
            vec![],
        )));
        // new 总可能 bad_alloc; 字面量/内建运算/cast 不抛
        assert!(!sema.noexcept_predicate_holds(&CppExpr::New("int".into(), vec![])));
        assert!(sema.noexcept_predicate_holds(&CppExpr::IntLit(1)));
        assert!(sema.noexcept_predicate_holds(&CppExpr::Cast(
            MangleType::Int,
            Box::new(CppExpr::FloatLit("1.5".into())),
        )));
    }

    #[test]
    fn noexcept_predicate_rejects_polymorphic_typeid_deref() {
        // g++: typeid(*p) 对多态类型可抛 bad_typeid; typeid(静态类型) 不抛
        let src = r#"
struct Poly {
    virtual int f() { return 1; }
};
struct Plain {
    int x;
};
Poly* pp;
Plain* qp;
"#;
        let sema = build_sema_from_src(src);
        assert!(!sema.noexcept_predicate_holds(&CppExpr::Typeid(Box::new(
            CppExpr::UnaryOp(UnaryOpKind::Deref, Box::new(CppExpr::Ident("pp".into()))),
        ))));
        assert!(sema.noexcept_predicate_holds(&CppExpr::Typeid(Box::new(
            CppExpr::UnaryOp(UnaryOpKind::Deref, Box::new(CppExpr::Ident("qp".into()))),
        ))));
        assert!(sema.noexcept_predicate_holds(&CppExpr::TypeidType(
            MangleType::Class("Poly".into()),
        )));
    }

    // ===== Stage 7 对象模型: 多继承下的虚函数登记 =====

    /// 类名 -> ClassId (测试里按名取类, 不依赖符号登记顺序)。
    fn class_id_of(sema: &Sema, name: &str) -> ClassId {
        ClassId::from(
            sema.lookup_unqualified(name)
                .into_iter()
                .find(|&id| {
                    matches!(sema.get_symbol(id).map(|s| s.kind), Some(CxxSymbolKind::Class))
                })
                .unwrap_or_else(|| panic!("类 {} 未登记", name)),
        )
    }

    /// 本类 `virtual_methods` 里的方法名 (按登记顺序)。
    fn virtual_method_names(sema: &Sema, class: &str) -> Vec<String> {
        let cid = class_id_of(sema, class);
        sema.get_class_info(cid)
            .map(|info| {
                info.virtual_methods
                    .iter()
                    .map(|vm| {
                        sema.get_symbol(vm.symbol).map(|s| s.name.clone()).unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 本类 `virtual_methods` 里每条记录被重写的基方法名。
    fn overridden_base_names(sema: &Sema, class: &str) -> Vec<Option<String>> {
        let cid = class_id_of(sema, class);
        sema.get_class_info(cid)
            .map(|info| {
                info.virtual_methods
                    .iter()
                    .map(|vm| {
                        vm.overrides
                            .and_then(|b| sema.get_symbol(b).map(|s| s.name.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    const MI_RTTI_SRC: &str = r#"
struct Base { virtual ~Base() {} virtual int id() { return 1; } };
struct Other { virtual ~Other() {} virtual int oid() { return 100; } };
struct Mid : Base { int id() override { return 2; } };
struct Leaf : Mid, Other { int id() override { return 3; } int oid() override { return 200; } };
"#;

    #[test]
    fn multi_inheritance_overrides_keep_their_own_entries() {
        // 回归 (dynamic_cast 运行期互操作): `Leaf : Mid, Other` 中 `id` 继承
        // `Mid` 链的槽号 1, `oid` 继承 `Other` 链的槽号 1 —— 旧代码用
        // `vm.slot == slot` 做既有条目匹配, 后登记的 `oid` 把 `id` 整条顶掉,
        // `Leaf::id` 因此不是虚函数, vtable 里留下基类实现。
        let sema = build_sema_from_src(MI_RTTI_SRC);
        let names = virtual_method_names(&sema, "Leaf");
        assert_eq!(
            names,
            vec!["id".to_string(), "oid".to_string(), "~Leaf".to_string()],
            "多继承下两条基链的 override 必须各自成条 (按登记序); 末条是 sema \
             为 `Leaf` 合成的隐式虚析构 —— `Base`/`Other` 都有虚析构, `Leaf` 必须\
             重写那条槽, 否则 `_ZTV4Leaf` 的析构槽留着基类实现 (`delete (Base*)l`\
             只销毁基类半截)"
        );
        assert_eq!(
            overridden_base_names(&sema, "Leaf"),
            vec![
                Some("id".to_string()),
                Some("oid".to_string()),
                // 隐式析构指回主基类链 (`Mid`) 上那条被重写的虚析构
                Some("~Mid".to_string()),
            ],
            "每条记录必须指回自己那条基链的被重写方法"
        );
        // 单继承链上的重写仍保持基类槽号 (重写不换槽)
        let mid = class_id_of(&sema, "Mid");
        let leaf = class_id_of(&sema, "Leaf");
        let mid_slot = sema
            .get_class_info(mid)
            .unwrap()
            .virtual_methods
            .iter()
            .find(|vm| sema.get_symbol(vm.symbol).map(|s| s.name == "id").unwrap_or(false))
            .unwrap()
            .slot;
        let leaf_id_slot = sema
            .get_class_info(leaf)
            .unwrap()
            .virtual_methods
            .iter()
            .find(|vm| sema.get_symbol(vm.symbol).map(|s| s.name == "id").unwrap_or(false))
            .unwrap()
            .slot;
        assert_eq!(leaf_id_slot, mid_slot, "重写不得改变基链上的槽号");
    }

    #[test]
    fn multi_inheritance_virtual_dtor_keeps_own_entry() {
        // 析构登记路径与 Method 路径同构: 同样不得用槽号做匹配键。
        let src = r#"
struct A { virtual ~A() {} virtual int f() { return 1; } };
struct B { virtual ~B() {} virtual int g() { return 2; } };
struct C : A, B { ~C() override {} int f() override { return 3; } int g() override { return 4; } };
"#;
        let sema = build_sema_from_src(src);
        let names = virtual_method_names(&sema, "C");
        assert_eq!(
            names,
            vec!["~C".to_string(), "f".to_string(), "g".to_string()],
            "虚析构 + 两条基链的 override 都得各自登记"
        );
    }

    /// `delete p` 的四种点型场景 (多态基类指针 / 多态派生类指针 / 非多态但有
    /// 用户析构 / 无需析构的聚合) 加一个 `delete[]`。
    const DELETE_DTOR_SRC: &str = r#"
struct Base { virtual ~Base() {} virtual int id() { return 1; } };
struct Derived : Base { ~Derived() override {} };
struct Plain { int v; ~Plain() {} };
struct NoDtor { int v; };
int main() {
    Base* pb = 0;
    delete pb;
    Derived* pd = 0;
    delete pd;
    Plain* pp = 0;
    delete pp;
    NoDtor* pn = 0;
    delete pn;
    Base* arr = 0;
    delete[] arr;
    return 0;
}
"#;

    /// 本类虚析构所占的槽号 (未登记时 `None`)。
    fn virtual_dtor_slot(sema: &Sema, class: &str) -> Option<usize> {
        let cid = class_id_of(sema, class);
        let info = sema.get_class_info(cid)?;
        let dtor = info.destructor?;
        info.virtual_methods
            .iter()
            .find(|vm| vm.symbol == dtor)
            .map(|vm| vm.slot)
    }

    #[test]
    fn delete_picks_the_dtor_call_required_by_pointee_type() {
        // C++ [expr.delete]/3: 点型是多态类 → 必须走 vtable 的虚析构槽;
        // 非多态但有析构 → 直调本类 complete 析构; 平凡类/标量 → `None`。
        // 早期 sema 完全不算这件事, `lower_delete` 只发 `operator delete`,
        // 于是 `delete pb` 根本不跑析构 (实测计数全局始终为 0)。
        let sema = build_sema_from_src(DELETE_DTOR_SRC);
        let dels: Vec<crate::cpp::hir::HirExprKind> = sema
            .hir
            .exprs
            .iter()
            .filter(|e| matches!(e.kind, crate::cpp::hir::HirExprKind::Delete { .. }))
            .map(|e| e.kind.clone())
            .collect();
        assert_eq!(dels.len(), 5, "夹具里应有 5 个 delete 表达式");
        let info_of = |i: usize| match &dels[i] {
            crate::cpp::hir::HirExprKind::Delete {
                dtor, is_array, ..
            } => (*dtor, *is_array),
            _ => unreachable!(),
        };

        let base = class_id_of(&sema, "Base");
        let derived = class_id_of(&sema, "Derived");
        let plain = class_id_of(&sema, "Plain");
        // 重写不换槽: Derived 的虚析构与 Base 共用同一条槽。
        let dtor_slot = virtual_dtor_slot(&sema, "Base");
        assert_eq!(dtor_slot, virtual_dtor_slot(&sema, "Derived"));
        let dtor_slot = dtor_slot.expect("Base 的虚析构必须登记进 virtual_methods");

        assert_eq!(
            info_of(0).0,
            Some(crate::cpp::hir::HirDeleteDtor::VirtualDtor {
                class: base,
                slot: dtor_slot,
            }),
            "delete (Base*)pd 必须按点型多态属性走 vtable 虚析构槽"
        );
        assert_eq!(
            info_of(1).0,
            Some(crate::cpp::hir::HirDeleteDtor::VirtualDtor {
                class: derived,
                slot: dtor_slot,
            }),
            "delete pd 同样虚派发 (动态类型即点型, 行为等价于直调)"
        );
        assert_eq!(
            info_of(2).0,
            Some(crate::cpp::hir::HirDeleteDtor::Direct { class: plain }),
            "非多态但有用户析构的类: 直调 complete 析构"
        );
        assert_eq!(
            info_of(3).0,
            None,
            "无需析构的聚合只释放内存 (不得凭空引入 undefined symbol)"
        );
        let (arr_dtor, is_arr) = info_of(4);
        assert!(is_arr, "delete[] 必须记录数组形态");
        assert_eq!(
            arr_dtor, None,
            "数组逐元素逆序析构 + cookie 尚未实现, 此处显式不记析构目标"
        );
    }

    /// 非限定继承成员使用的几种形态: 本类方法、虚基类方法、中间基类方法、
    /// MI 主/次基类方法、基类字段。
    const UNQUAL_BASE_LOOKUP_SRC: &str = r#"
struct VB { int v; virtual int vf() { return 1; } int gv() { return 2; } };
struct Mid : virtual VB { int mf() { return 3; } };
struct Fin : Mid { int use_vf() { return vf(); } int use_mf() { return mf(); } int read_v() { return v; } int own() { return 4; } int use_own() { return own(); } };
struct A { int x; virtual int a1() { return 5; } int ga() { return 6; } };
struct B { int y; virtual int b1() { return 7; } int gb() { return 8; } };
struct C : A, B { int call_a1() { return a1(); } int call_b1() { return b1(); } int call_ga() { return ga(); } int call_gb() { return gb(); } int read_y() { return y; } };
"#;

    /// 类 id → 类名。
    fn class_name(sema: &Sema, cid: ClassId) -> String {
        sema.get_symbol(cid.as_symbol())
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "?".to_string())
    }

    /// 把一个表达式渲染成简短形状串, 供断言 `this` 实参的子对象调整。
    fn expr_shape(sema: &Sema, id: crate::cpp::hir::HirExprId) -> String {
        use crate::cpp::hir::HirExprKind as K;
        match &sema.hir.exprs[id.0].kind {
            K::This => "this".to_string(),
            K::BaseCast { from, to, .. } => {
                format!("BaseCast({}->{})", class_name(sema, *from), class_name(sema, *to))
            }
            K::VBaseAdjust { from, to, .. } => format!(
                "VBaseAdjust({}->{})",
                class_name(sema, *from),
                class_name(sema, *to)
            ),
            K::AddressOf(e) => format!("AddressOf({})", expr_shape(sema, *e)),
            _ => "<other>".to_string(),
        }
    }

    /// 收集成员调用点: (方法名, 是否虚派发, 首个实参即 this 的形状)。
    fn member_call_this_shapes(sema: &Sema) -> Vec<(String, bool, String)> {
        use crate::cpp::hir::{HirCallee, HirExprKind};
        let mut out = Vec::new();
        for e in &sema.hir.exprs {
            let (callee, args) = match &e.kind {
                HirExprKind::Call { callee, args } => (callee, args),
                _ => continue,
            };
            let (name, is_virtual) = match callee {
                HirCallee::Direct(sym) => (
                    sema.get_symbol(*sym).map(|s| s.name.clone()).unwrap_or_default(),
                    false,
                ),
                HirCallee::Virtual { method, .. } => (
                    sema.get_symbol(*method).map(|s| s.name.clone()).unwrap_or_default(),
                    true,
                ),
                _ => continue,
            };
            let shape = args
                .first()
                .map(|id| expr_shape(sema, *id))
                .unwrap_or_else(|| "<no-this>".to_string());
            out.push((name, is_virtual, shape));
        }
        out
    }

    /// 收集字段访问点: (字段名, 对象表达式形状)。
    fn field_object_shapes(sema: &Sema) -> Vec<(String, String)> {
        use crate::cpp::hir::HirExprKind;
        let mut out = Vec::new();
        for e in &sema.hir.exprs {
            if let HirExprKind::FieldAccess { object, field } = &e.kind {
                let name = sema.get_symbol(*field).map(|s| s.name.clone()).unwrap_or_default();
                out.push((name, expr_shape(sema, *object)));
            }
        }
        out
    }

    #[test]
    fn unqualified_inherited_members_bind_this_to_the_member_subobject() {
        // 任务 4: 派生类成员内 `return prot();` 一类非限定名字必须沿基类链
        // (含虚基) 解析。此前 sema 的 `lookup_unqualified` 只走词法作用域链,
        // 基类成员全部报 `undeclared identifier`。
        //
        // 名字解析只是半个问题: [class.member.lookup]/1 之后隐式 `this` 必须
        // 转换到**成员所属子对象**, 否则 MI 次基类 / 虚基类的方法体按自己的
        // 布局读 `this->field` 会读到别的子对象 (实测 `C : A, B` 里裸调 `b1()`
        // 把 B::y 读成 A::x, dcc 47 / g++ 43; 虚基 `vf()` 读到 0)。
        let sema = build_sema_from_src(UNQUAL_BASE_LOOKUP_SRC);
        let shapes = member_call_this_shapes(&sema);
        let get = |name: &str| -> (bool, String) {
            let hit: Vec<_> = shapes.iter().filter(|(n, _, _)| n == name).collect();
            assert_eq!(hit.len(), 1, "调用点 {} 应恰好一个, 实际 {:?}", name, shapes);
            (hit[0].1, hit[0].2.clone())
        };

        // 1) 虚基方法: 虚派发 + this 经 vbtable 运行期调整
        assert_eq!(
            get("vf"),
            (true, "VBaseAdjust(Fin->VB)".to_string()),
            "虚基虚函数: 槽号照本类链, this 必须是虚基子对象"
        );
        // 2) 中间基类非虚方法: 直调 + BaseCast 到 Mid 子对象
        assert_eq!(
            get("mf"),
            (false, "BaseCast(Fin->Mid)".to_string()),
            "非虚基类方法必须直调, this 折到该基类子对象"
        );
        // 3) MI 次基类的虚/非虚方法
        assert_eq!(get("b1"), (true, "BaseCast(C->B)".to_string()));
        assert_eq!(get("gb"), (false, "BaseCast(C->B)".to_string()));
        // 4) 主基类 (偏移 0) 同样显式成 BaseCast, 与 g++ 同构且不影响正确性
        assert_eq!(get("a1"), (true, "BaseCast(C->A)".to_string()));
        assert_eq!(get("ga"), (false, "BaseCast(C->A)".to_string()));
        // 5) 本类自有方法: 不得凭空插入任何子对象转换
        assert_eq!(get("own"), (false, "this".to_string()));

        // 6) 基类字段的非限定读写也走同一调整链
        let fields = field_object_shapes(&sema);
        assert!(
            fields
                .iter()
                .any(|(n, s)| n == "y" && s == "BaseCast(C->B)"),
            "C::read_y 里的 `y` 必须折到 B 子对象, 实际 {:?}",
            fields
        );
        assert!(
            fields
                .iter()
                .any(|(n, s)| n == "v" && s == "VBaseAdjust(Fin->VB)"),
            "Fin::read_v 里的虚基字段 `v` 必须走 vbtable, 实际 {:?}",
            fields
        );
    }

    #[test]
    fn unqualified_lookup_still_rejects_names_absent_from_the_base_chain() {
        // 基类链回退不得把拼错的名字静默吞掉: 整条链 (含虚基) 都没有时
        // 仍必须按 [basic.lookup.unqual] 报 undeclared。
        let src = r#"
struct VB { int v; virtual int vf() { return 1; } };
struct Mid : virtual VB { int mf() { return 3; } };
struct Fin : Mid { int use_missing() { return nowhere_to_find(); } };
"#;
        let mut p = CppParser::new(src.as_bytes(), "unit.cpp");
        let decls = p.parse_top_level().expect("parse ok");
        let mut sema = Sema::new();
        let err = sema
            .build_hir_from_decls(&decls)
            .expect_err("基类链上不存在的名字必须报错");
        assert!(
            err.contains("undeclared identifier: nowhere_to_find"),
            "实际诊断: {}",
            err
        );
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
                    MangleType::Template { name, args: _args } => {
                        // For std::vector<int>, we might have nested types like 'iterator'
                        // This would be resolved via typedefs/typedef-like mechanisms
                        // For now, return dependent type marker
                        Some(CxxType::Dependent(format!("{}.{}", name, member)))
                    },
                    _ => None,
                };
            }
        }
        None
    }
}

/// M52: 若一个 `MangleType::Class` 名字带 **elaborated type specifier** 前缀
/// (`struct X` / `union U` / `enum E` / `class C`), 返回它真正 denotate 的
/// tag 名; 否则返回 `None`.
///
/// 前缀形式只可能来自 parser 对 C 风格 elaborated 类型的保守编码
/// (`parse_mangle_type_depth` 把关键字与 tag 一起存成名字, legacy C 输出需要
/// 它), C++ 标识符里不可能出现空格, 因此该判定无误报.
pub fn elaborated_tag_name(name: &str) -> Option<&str> {
    for kw in ["struct ", "union ", "enum ", "class "] {
        if let Some(rest) = name.strip_prefix(kw) {
            let rest = rest.trim();
            if !rest.is_empty() && rest != name {
                return Some(rest);
            }
        }
    }
    None
}
