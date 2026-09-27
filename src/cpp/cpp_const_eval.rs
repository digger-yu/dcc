//! M17.0 + Phase 3/Enhanced: 编译期常量求值引擎 (完整实现 v3)
//!
//! ## Current Implementation Coverage
//! 🎯 **G28**: `__builtin_is_constant_evaluated()` ✅ COMPLETE  
//! 🎯 **G26**: if constexpr semantics ✅ COMPLETE
//! 🎯 **G25**: constexpr function calls framework ✅ COMPLETE
//! 🎯 **G24**: type traits infrastructure ✅ COMPLETE
//! 🎯 **G27**: consteval enforcement ✅ COMPLETE (Turn 2)
//! 🎯 **G25 completion**: constexpr 函数体真解释执行 ✅ COMPLETE
//!    (可变作用域栈 / 赋值与就地改写副作用 / for-while 迭代累积 /
//!     switch 分支 / 经 `func_table` 的嵌套与递归调用折叠)
//!
//! ### Core Capabilities
//! - Context-aware constexpr evaluation
//! - Dead code elimination for if constexpr
//! - **Constexpr function invocation with full body interpretation**
//!   (参数绑定、局部变量、循环、递归; 见 `eval_constexpr_function_body`)
//! - **Consteval runtime call detection and error emission**
//! - 完整控制流建模: if / for / while / switch / block, 且**副作用不丢**
//!   (无法在编译期完成的循环/分支一律报错, 不静默按"已跑完"处理)
//! - 栈式词法作用域: 块内声明屏蔽外层, 写入落在真正声明它的那一帧
//! - Type size/alignment queries via TypeInfoRepository

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::cpp::parser::{BinOpKind, CppExpr, SizeofArg, UnaryOpKind};

// ============================================================================
// Phase 3: Data Structures
// ============================================================================

/// Represents a constexpr function definition available for evaluation
pub struct ConstexprFunction {
    /// Function name
    pub name: String,
    /// Parameter names
    pub params: Vec<String>,
    /// Return type (string representation)
    pub return_type: String,
    /// Whether this must be evaluated at compile time
    pub is_consteval: bool,
    /// Body statements (to evaluate)
    pub body: Vec<CppStmt>,
}

/// Represents a violation of consteval requirements
#[derive(Debug, Clone)]
pub struct ConstevalViolation {
    /// Function name that was called at runtime
    pub func_name: String,
    /// Error message describing the violation
    pub message: String,
}

impl ConstevalViolation {
    pub fn new(func_name: &str) -> Self {
        Self {
            func_name: func_name.to_string(),
            message: format!(
                "consteval function '{}' must be evaluated at compile time",
                func_name
            ),
        }
    }
}

/// Statement types for constexpr evaluation context
#[derive(Debug, Clone)]
pub enum CppStmt {
    /// Expression statement
    Expr(CppExpr),
    /// Variable declaration (for scoping)
    Decl(String, Option<CppExpr>),
    /// Block containing multiple statements
    Block(Vec<Box<CppStmt>>),
    /// If statement (non-constexpr - emitted as runtime if)
    If(Box<CppExpr>, Vec<Box<CppStmt>>, Option<Vec<Box<CppStmt>>>),
    /// For loop (init → cond → body → step, 与 C++ 语义一致)
    For(Option<Box<CppStmt>>, Option<CppExpr>, Option<Box<CppStmt>>, Vec<Box<CppStmt>>),
    /// Return statement
    Return(Option<CppExpr>),
    /// While loop
    While(Option<CppExpr>, Vec<Box<CppStmt>>),
    /// Switch statement with constant expressions
    Switch(Option<CppExpr>, Vec<(Option<CppExpr>, Vec<Box<CppStmt>>)>), // Each arm: None = default, Some(expr) = case expr
    /// 具名变量赋值: `name = value;`
    ///
    /// `CppExpr` 来自 parser, 本身没有赋值节点, 所以"写变量"这一副作用必须在
    /// 语句层显式建模; 写入落到**真正声明该变量的作用域帧**, 从而跨语句/跨迭代
    /// 累积 (`a = b;` / `sum = sum + a;`)。名字在作用域栈里不存在时退化为在
    /// 当前最内层帧新建绑定 (解释器保持宽容, 不做名字查找诊断)。
    Assign { name: String, value: CppExpr },
    /// 具名变量就地增减: `name += delta;` (`delta` 取负即 `--` / `-=`)
    ///
    /// 与 `Assign` 同样落到声明帧, 供 `for (...; ...; ++i)` 这类步进使用。
    Adjust { name: String, delta: i128 },
    /// `break;` —— 跳出最近的 while/for; 位于 switch 体内则结束该 switch。
    Break,
    /// `continue;` —— 进入最近循环的下一步进 (for) 或下一轮条件判断 (while)。
    Continue,
}

// ============================================================================
// G25 completion: 解释器期作用域栈与控制流信号
// ============================================================================

/// constexpr 循环迭代硬上限.
///
/// 超限一律**报错**而非静默当作"循环已正常结束": 后者会把没跑完的副作用当成
/// 真值交付给上层 (旧实现的 100 次空转就是这么丢副作用的).
const MAX_CONSTEXPR_LOOP_ITERATIONS: usize = 100_000;

/// 解释器期局部变量作用域栈 (G25: 可变 + 按块屏蔽).
///
/// - `frames[0]` 是最外层帧: 装函数形参绑定, 以及从
///   [`ConstexprContext::locals`] 克隆进来的外部常量 (模板参数 / 命名空间作用域
///   `constexpr`)。callee 在栈上的写入只影响自己的副本, 不会漏回 caller。
/// - 每进入一个 block / 循环体 push 一帧, 退出时 pop —— 内层同名声明随帧
///   销毁, 外层绑定不受影响 (C++ 词法屏蔽语义)。
/// - **查找**由内向外 (最内层优先); **写入**落到真正声明它的那一帧,
///   所以块内 `x = 5;` 能改外层 `x`, 块内 `int x = 5;` 不能。
#[derive(Debug, Clone, Default)]
struct LocalsStack {
    /// 由外向内的作用域帧; 始终至少保留一帧 (见 [`LocalsStack::new`]).
    frames: Vec<HashMap<String, i128>>,
}

impl LocalsStack {
    /// 空栈 (仅一个顶层空帧).
    fn new() -> Self {
        Self { frames: vec![HashMap::new()] }
    }

    /// 以外部已知常量为顶层帧建栈 (克隆, 因此 callee 的改写不回流).
    fn with_base(base: &HashMap<String, i128>) -> Self {
        Self { frames: vec![base.clone()] }
    }

    /// 进入一个新词法作用域.
    fn push_scope(&mut self) {
        self.frames.push(HashMap::new());
    }

    /// 退出当前作用域, 销毁本层声明 (屏蔽变量在此失效).
    fn pop_scope(&mut self) {
        // 顶层帧永不弹出, 保证后续 declare 总有落脚处.
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// 由内向外查名字.
    fn lookup(&self, name: &str) -> Option<i128> {
        self.frames.iter().rev().find_map(|f| f.get(name).copied())
    }

    /// 在当前最内层帧声明一个变量 (同名即屏蔽外层).
    fn declare(&mut self, name: String, value: i128) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name, value);
        }
    }

    /// 向已存在的绑定写入 (落在声明它的那一帧); 未找到返回 false.
    fn assign_existing(&mut self, name: &str, value: i128) -> bool {
        for frame in self.frames.iter_mut().rev() {
            if frame.contains_key(name) {
                frame.insert(name.to_string(), value);
                return true;
            }
        }
        false
    }

    /// 赋值: 已有绑定则改写, 否则在当前最内层帧新建.
    fn assign_or_declare(&mut self, name: &str, value: i128) {
        if !self.assign_existing(name, value) {
            self.declare(name.to_string(), value);
        }
    }

    /// 摊平为单张表 (外层先写, 内层覆盖), 供 legacy 表达式求值器复用.
    ///
    /// constexpr 函数体内变量数量很小, 这里 O(变量数) 的克隆不是热路径瓶颈,
    /// 换来的是"表达式求值只有一种入口 (`eval_cpp_const`)"的单一真源.
    fn merged(&self) -> HashMap<String, i128> {
        let mut out = HashMap::new();
        for frame in self.frames.iter() {
            for (k, v) in frame {
                out.insert(k.clone(), *v);
            }
        }
        out
    }
}

/// 语句序列的执行流向.
///
/// 旧实现用 `Option<i128>` 只能表达"return 了没", `break`/`continue` 无法从
/// if/block 体内递升到循环处, 故引入本枚举 (仅本模块可见).
enum StmtFlow {
    /// 顺序执行到末尾 (或已跳出当前构造)
    Finish,
    /// 待外层循环/switch 消费的 `break;`
    Break,
    /// 待外层循环消费的 `continue;`
    Continue,
    /// `return value;`
    Return(i128),
}

// 语句层求值的返回语义 (贯穿本模块 `eval_*` 系列, 类型即 `ConstEvalResult`):
//   - `Err`      = 求值过程中命中了无法完成的 constexpr 调用 (递归超深 / consteval 违规);
//   - `Ok(None)` = 表达式含无法折叠的子形式 (沿用 legacy 求值器语义, 不新增错误)。
// 两者含义不同, 不可合并: 上层据此区分"不是常量"与"折叠失败".

/// switch 的分支表: `(case 表达式, 分支体)`, case 为 None 即 `default`.
/// 抽别名是为了避开 `clippy::type_complexity` (嵌套两层的元组切片).
type SwitchArm = (Option<CppExpr>, Vec<Box<CppStmt>>);

/// 就地改写类别: 由 [`decode_mutation`] 从表达式树解码而来.
#[derive(Debug, Clone)]
enum MutKind {
    /// `x = e`
    Set(CppExpr),
    /// `x op= e` (op 保存的是**已剥后缀**的二元运算符, 如 `Add`)
    Compound(BinOpKind, CppExpr),
    /// `++x` / `x++` / `--x` / `x--` (带符号增量)
    Delta(i128),
}

/// 把"就地修改局部变量"的表达式拆成 `(变量名, 改写方式)`.
///
/// 只认左值是裸标识符的形态 (`CppExpr` 无赋值节点, 也不建模下标/成员改写);
/// 其它表达式返回 None, 调用方按纯表达式处理.
fn decode_mutation(e: &CppExpr) -> Option<(String, MutKind)> {
    match e {
        CppExpr::UnaryOp(op, inner) => {
            let delta = inc_dec_delta(*op)?;
            let name = ident_name(inner)?;
            Some((name.clone(), MutKind::Delta(delta)))
        },
        CppExpr::BinOp(op, lhs, rhs) => {
            let name = ident_name(lhs)?;
            // 复合赋值: 剥出普通二元运算符后交给 `MutKind::Compound`
            assign_base_op(*op)
                .map(|base| (name.clone(), MutKind::Compound(base, (**rhs).clone())))
        },
        _ => None,
    }
}

/// 表达式是否是个裸标识符, 是则返回变量名.
fn ident_name(e: &CppExpr) -> Option<&String> {
    match e {
        CppExpr::Ident(name) => Some(name),
        _ => None,
    }
}

/// 增减运算符 → 带符号增量 (非增减运算符返回 None).
fn inc_dec_delta(op: UnaryOpKind) -> Option<i128> {
    match op {
        UnaryOpKind::PreInc | UnaryOpKind::PostInc => Some(1),
        UnaryOpKind::PreDec | UnaryOpKind::PostDec => Some(-1),
        _ => None,
    }
}

/// 赋值运算符 → 对应的普通二元运算符 (`=` 本身返回 None, 由 `MutKind::Set` 走).
fn assign_base_op(op: BinOpKind) -> Option<BinOpKind> {
    match op {
        BinOpKind::AddAssign => Some(BinOpKind::Add),
        BinOpKind::SubAssign => Some(BinOpKind::Sub),
        BinOpKind::MulAssign => Some(BinOpKind::Mul),
        BinOpKind::DivAssign => Some(BinOpKind::Div),
        BinOpKind::ModAssign => Some(BinOpKind::Mod),
        BinOpKind::ShlAssign => Some(BinOpKind::Shl),
        BinOpKind::ShrAssign => Some(BinOpKind::Shr),
        BinOpKind::AndAssign => Some(BinOpKind::And),
        BinOpKind::OrAssign => Some(BinOpKind::Or),
        BinOpKind::XorAssign => Some(BinOpKind::Xor),
        _ => None,
    }
}

/// 取值转成 `CppExpr::IntLit` 载体 (它是 i64).
///
/// constexpr 折叠的整型值域远小于 i64; 越界只可能来自病态输入, 此时饱和到
/// i64 端点好于 panic (解释器不得因常量溢出而中断整次编译).
fn i128_to_i64(v: i128) -> i64 {
    i64::try_from(v).unwrap_or_else(|_| if v.is_negative() { i64::MIN } else { i64::MAX })
}

/// 造一个整型字面量节点 (宽度适配见 [`i128_to_i64`]).
fn int_lit_expr(v: i128) -> CppExpr {
    CppExpr::IntLit(i128_to_i64(v))
}

/// 循环迭代超限的统一诊断 (for / while 共用措辞).
fn loop_limit_error() -> String {
    format!(
        "constexpr loop exceeded the {}-iteration bound; side effects cannot be dropped silently",
        MAX_CONSTEXPR_LOOP_ITERATIONS
    )
}

/// 取被调函数名: `f(...)` 与 `f<int>(...)` 都归一到表里可能用的键.
fn constexpr_callee_name(callee: &CppExpr) -> Option<&str> {
    match callee {
        CppExpr::Ident(name) => Some(name),
        CppExpr::TemplateIdent { name, .. } => Some(name),
        _ => None,
    }
}

/// 在 constexpr 函数表里查一个调用目标: 先按全名, 再按 `::` 尾段短名
/// (表里可能以 `std::foo` 或 `foo` 为键, 两边都允许命中).
fn lookup_constexpr_func<'t>(
    table: &'t HashMap<String, ConstexprFunction>,
    name: &str,
) -> Option<&'t ConstexprFunction> {
    if let Some(f) = table.get(name) {
        return Some(f);
    }
    let short = name.rsplit("::").next()?;
    table.get(short)
}

// ============================================================================
// Phase 3: Type Information Repository
// ============================================================================

/// Type information repository for constexpr sizeof/alignof queries
pub struct TypeInfoRepository {
    /// Primitive type sizes (hardcoded for common types)
    pub primitive_sizes: HashMap<String, usize>,
    /// Class type sizes (populated from abi_layout during sema phase)
    pub class_sizes: HashMap<String, usize>,
    /// Class aligns (populated from abi_layout during sema phase)
    pub class_aligns: HashMap<String, usize>,
}

impl TypeInfoRepository {
    /// Create new repository with standard primitive type sizes (64-bit Linux)
    pub fn new() -> Self {
        let mut primitive_sizes = HashMap::new();
        primitive_sizes.insert("char".to_string(), 1);
        primitive_sizes.insert("bool".to_string(), 1);
        primitive_sizes.insert("_Bool".to_string(), 1);
        primitive_sizes.insert("short".to_string(), 2);
        primitive_sizes.insert("int".to_string(), 4);
        primitive_sizes.insert("long".to_string(), 8);
        primitive_sizes.insert("float".to_string(), 4);
        primitive_sizes.insert("double".to_string(), 8);
        primitive_sizes.insert("void*".to_string(), 8);
        primitive_sizes.insert("void**".to_string(), 8);

        TypeInfoRepository {
            primitive_sizes,
            class_sizes: HashMap::new(),
            class_aligns: HashMap::new(),
        }
    }

    /// Get size of a builtin/primitive type
    pub fn get_primitive_size(&self, type_name: &str) -> Option<usize> {
        self.primitive_sizes.get(type_name).copied()
    }

    /// Register a class type's size and alignment
    pub fn register_class(&mut self, name: String, size: usize, align: usize) {
        self.class_sizes.insert(name.clone(), size);
        self.class_aligns.insert(name, align);
    }

    /// Get size of any type (class or primitive)
    pub fn size_of(&self, type_name: &str) -> Option<usize> {
        // Try class types first (more specific)
        if let Some(&size) = self.class_sizes.get(type_name) {
            return Some(size);
        }

        // Fall back to primitive types
        self.primitive_sizes.get(type_name).copied()
    }

    /// Get alignment requirement of any type
    pub fn align_of(&self, type_name: &str) -> Option<usize> {
        // For primitives, use natural alignment
        match self.size_of(type_name)? {
            1 => Some(1),
            2 => Some(2),
            4 => Some(4),
            8 => Some(8),
            _ => Some(8), // Default alignment for larger types
        }
    }
}

impl Default for TypeInfoRepository {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Phase 3: Enhanced Context
// ============================================================================

/// Phase 3 enhanced context for constexpr evaluation
pub struct ConstexprContext<'a> {
    /// Whether we're in a constant evaluation context
    pub in_constant_context: bool,
    /// Local variables map
    pub locals: &'a HashMap<String, i128>,
    /// Function table for constexpr calls
    pub func_table: Option<&'a HashMap<String, ConstexprFunction>>,
    /// Type info repository for sizeof queries
    pub type_info: Option<&'a TypeInfoRepository>,
    /// Recursion depth tracking
    current_depth: usize,
    max_recursion_depth: usize,
    /// Track violations (consteval calls at runtime)
    pub violations: Vec<ConstevalViolation>,
}

impl<'a> ConstexprContext<'a> {
    pub fn new(in_constant_context: bool, locals: &'a HashMap<String, i128>) -> Self {
        ConstexprContext {
            in_constant_context,
            locals,
            func_table: None,
            type_info: None,
            current_depth: 0,
            max_recursion_depth: 64,
            violations: Vec::new(),
        }
    }

    /// Create with function table support
    pub fn with_functions(
        in_constant_context: bool,
        locals: &'a HashMap<String, i128>,
        funcs: &'a HashMap<String, ConstexprFunction>,
    ) -> Self {
        ConstexprContext {
            in_constant_context,
            locals,
            func_table: Some(funcs),
            type_info: None,
            current_depth: 0,
            max_recursion_depth: 64,
            violations: Vec::new(),
        }
    }

    /// Create with type info repository for sizeof queries
    pub fn with_type_info(
        in_constant_context: bool,
        locals: &'a HashMap<String, i128>,
        type_info: &'a TypeInfoRepository,
    ) -> Self {
        ConstexprContext {
            in_constant_context,
            locals,
            func_table: None,
            type_info: Some(type_info),
            current_depth: 0,
            max_recursion_depth: 64,
            violations: Vec::new(),
        }
    }

    pub fn is_constant_evaluated(&self) -> bool {
        self.in_constant_context
    }

    /// Check recursion limit before calling a function
    pub fn check_recursion_limit(&self, _func_name: &str) -> Result<(), String> {
        if self.current_depth >= self.max_recursion_depth {
            return Err(format!(
                "constexpr recursion limit exceeded (depth {} > {})",
                self.current_depth, self.max_recursion_depth
            ));
        }
        Ok(())
    }

    /// Enter a function call scope
    pub fn enter_call(&mut self) {
        self.current_depth += 1;
    }

    /// Exit a function call scope
    pub fn exit_call(&mut self) {
        if self.current_depth > 0 {
            self.current_depth -= 1;
        }
    }

    /// Record a consteval violation
    pub fn record_violation(&mut self, func_name: &str) {
        self.violations.push(ConstevalViolation::new(func_name));
    }

    /// Check if there are any violations
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    /// Get violations as a result string (or None if none)
    pub fn violations_message(&self) -> Option<String> {
        if self.violations.is_empty() {
            None
        } else {
            let messages: Vec<_> = self.violations.iter().map(|v| v.message.clone()).collect();
            Some(format!("consteval violations:\n{}", messages.join("\n")))
        }
    }

    /// Clear all violations (for new evaluation round)
    pub fn clear_violations(&mut self) {
        self.violations.clear();
    }

    /// Get type info repository (if available)
    pub fn get_type_info(&self) -> Option<&TypeInfoRepository> {
        self.type_info
    }
}

impl Default for ConstexprContext<'static> {
    fn default() -> Self {
        // Use LazyLock for thread-safe static initialization
        static EMPTY: LazyLock<HashMap<String, i128>> = LazyLock::new(HashMap::new);
        Self::new(false, &EMPTY)
    }
}

// ============================================================================
// Phase 3/Enhanced: Constexpr Function Body Execution Engine
// ============================================================================

/// Represents the result of a constexpr function evaluation
#[derive(Debug)]
pub struct ConstValue {
    pub value: i128,
}

impl ConstValue {
    pub fn new(val: i128) -> Self {
        Self { value: val }
    }
}

/// Result type for constexpr function execution
pub type ConstEvalResult<T> = Result<T, String>;

impl ConstexprContext<'_> {
    /// Evaluate a complete constexpr function body
    ///
    /// This is the core implementation of G25 completion:
    /// - Executes statements sequentially through the function body
    /// - Manages local variable bindings in a **mutable, scope-stacked** way
    ///   (见 [`LocalsStack`]): 声明/赋值/循环迭代的副作用都能跨语句保留
    /// - Extracts and propagates return values correctly
    /// - Supports nested blocks with lexical shadowing
    /// - Tracks recursion depth to prevent infinite recursion attacks
    /// - 体内对 `func_table` 里其它 constexpr 函数的调用会被递归折叠求值
    ///
    /// 外部常量可见性: `self.locals` 会被克隆进 callee 的最外层帧, 因此函数体
    /// 能读到模板参数 / 命名空间作用域 `constexpr`; 因为是克隆, callee 对同名
    /// 变量的改写不会漏回 caller。
    ///
    /// # Arguments
    /// * `func` - The constexpr function definition
    /// * `args` - Actual argument values passed to the function
    ///
    /// # Returns
    /// * `Ok(i128)` - The return value of the function
    /// * `Err(String)` - Error message describing the failure
    ///
    /// # Example
    ///
    /// 循环版 fib (`int a=0,b=1; for(int i=0;i<n;++i){int t=a+b; a=b; b=t;} return a;`):
    ///
    /// ```rust
    /// use std::collections::HashMap;
    ///
    /// use dcc::cpp::cpp_const_eval::{ConstexprContext, ConstexprFunction, CppStmt};
    /// use dcc::cpp::parser::{BinOpKind, CppExpr, UnaryOpKind};
    ///
    /// // 语句装箱与标识符构造的小助手, 让下面的手建工种读起来接近源码
    /// let sb = |s: CppStmt| Box::new(s);
    /// let var = |n: &str| CppExpr::Ident(n.to_string());
    /// let bin = |op: BinOpKind, l: CppExpr, r: CppExpr| {
    ///     CppExpr::BinOp(op, Box::new(l), Box::new(r))
    /// };
    /// let decl = |n: &str, v: i64| CppStmt::Decl(n.to_string(), Some(CppExpr::IntLit(v)));
    ///
    /// // 体内一句: int t = a + b;  a = b;  b = t;
    /// let loop_body = vec![
    ///     sb(CppStmt::Decl(
    ///         "t".to_string(),
    ///         Some(bin(BinOpKind::Add, var("a"), var("b"))),
    ///     )),
    ///     sb(CppStmt::Assign { name: "a".to_string(), value: var("b") }),
    ///     sb(CppStmt::Assign { name: "b".to_string(), value: var("t") }),
    /// ];
    /// // for (int i = 0; i < n; ++i) { ... }
    /// let for_stmt = CppStmt::For(
    ///     Some(sb(decl("i", 0))),
    ///     Some(bin(BinOpKind::Lt, var("i"), var("n"))),
    ///     Some(sb(CppStmt::Expr(CppExpr::UnaryOp(
    ///         UnaryOpKind::PreInc,
    ///         Box::new(var("i")),
    ///     )))),
    ///     loop_body,
    /// );
    /// let body = vec![
    ///     decl("a", 0),
    ///     decl("b", 1),
    ///     for_stmt,
    ///     CppStmt::Return(Some(var("a"))),
    /// ];
    /// let fib = ConstexprFunction {
    ///     name: "fib".to_string(),
    ///     params: vec!["n".to_string()],
    ///     return_type: "int".to_string(),
    ///     is_consteval: false,
    ///     body,
    /// };
    ///
    /// let outer: HashMap<String, i128> = HashMap::new();
    /// let mut ctx = ConstexprContext::new(true, &outer);
    /// assert_eq!(ctx.eval_constexpr_function_body(&fib, &[10]).unwrap(), 55);
    /// ```
    pub fn eval_constexpr_function_body(
        &mut self,
        func: &ConstexprFunction,
        args: &[i128],
    ) -> ConstEvalResult<i128> {
        // Check recursion limit before entering
        self.check_recursion_limit(&func.name)?;

        // G27: For consteval functions, ensure we're in constant evaluation context
        if func.is_consteval && !self.in_constant_context {
            self.record_violation(&func.name);
            return Err(self.violations_message().unwrap_or_default());
        }

        // 实参个数与形参不匹配时不得默默丢弃多余形参: 那会让后面的调用
        // 在"参数未绑定"的假象上折叠出一个错值.
        if func.params.len() != args.len() {
            return Err(format!(
                "constexpr call '{}' got {} argument(s) but takes {} parameter(s)",
                func.name,
                args.len(),
                func.params.len()
            ));
        }

        // Enter function call scope
        self.enter_call();

        // 顶层帧 = 外部已知常量的副本; 形参绑定写在这同一帧里
        let mut sc = LocalsStack::with_base(self.locals);
        for (param, arg_val) in func.params.iter().zip(args.iter()) {
            sc.declare(param.clone(), *arg_val);
        }

        // Execute function body and extract return value
        let flow = self.eval_constexpr_body(&func.body, &mut sc);

        // Exit function call scope (错误路径上也要配对, 否则同一次编译里
        // 后续调用会背到一个虚高的递归深度)
        self.exit_call();

        match flow? {
            StmtFlow::Return(value) => Ok(value),
            // 整条路径跑完仍无 return: 不是常量表达式
            StmtFlow::Finish => {
                Err("constexpr function must have a return value".to_string())
            },
            // 顶层的 break/continue 无循环可跳 → 在 C++ 里就是语法错误,
            // 这里同样不能当成"正常跑完"交付结果.
            StmtFlow::Break | StmtFlow::Continue => Err(format!(
                "constexpr function '{}' has a stray break/continue outside any loop",
                func.name
            )),
        }
    }

    /// Evaluate a sequence of statements sequentially
    ///
    /// This is the main dispatch loop for constexpr function execution:
    /// - 语句按序执行, 局部状态经**可变**的 `sc` 作用域栈线程化 (旧实现以
    ///   `&HashMap` 传递并在每层 `locals.clone()`, 任何写入都会在下一条语句丢失)
    /// - Handles control flow (if / for / while / switch / block)
    /// - Extracts and propagates return values from Return nodes
    /// - break/continue 以 [`StmtFlow`] 递升到最近的循环 / switch 处
    ///
    /// # Arguments
    /// * `body` - List of statements to execute
    /// * `sc` - 当前的可变局部作用域栈
    ///
    /// # Returns
    /// * `Ok(StmtFlow)` - 序列结束 / break / continue / return(值)
    /// * `Err(String)` - 命中了无法在编译期完成的 constexpr 调用
    fn eval_constexpr_body(
        &mut self,
        body: &[CppStmt],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        // Process statements sequentially - O(body size) amortized complexity
        for stmt in body {
            let flow = self.run_one_stmt(stmt, sc)?;
            // Finish 之外 (return / break / continue) 都要中断本层顺序执行
            if !matches!(flow, StmtFlow::Finish) {
                return Ok(flow);
            }
        }
        // End of statement list without a jump
        Ok(StmtFlow::Finish)
    }

    /// 执行单条语句 —— 所有 `CppStmt` 变体的唯一派发点.
    fn run_one_stmt(
        &mut self,
        stmt: &CppStmt,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        match stmt {
            CppStmt::Expr(expr) => self.run_expr_stmt(expr, sc),

            CppStmt::Decl(name, init_expr) => {
                // 变量声明写入**当前最内层帧**: 同名即屏蔽外层.
                // 无初始化式按值初始化 (算术类型 = 0); 有初始化式却求不出值一律
                // 报错 —— 静默跳过会让后续语句在一条不存在的绑定上继续跑.
                let value = match init_expr {
                    Some(init) => self.eval_expr_required(init, sc)?,
                    None => 0,
                };
                sc.declare(name.clone(), value);
                Ok(StmtFlow::Finish)
            },

            CppStmt::Assign { name, value } => {
                let v = self.eval_expr_required(value, sc)?;
                // 写入落到真正声明它的那一帧, 因此循环体内的 `a = b;` 能跨迭代保留
                sc.assign_or_declare(name, v);
                Ok(StmtFlow::Finish)
            },

            CppStmt::Adjust { name, delta } => {
                // 未声明过的名字按 0 起算 (解释器保持宽容; 真实诊断属于 sema)
                let cur = sc.lookup(name).unwrap_or(0);
                sc.assign_or_declare(name, cur.wrapping_add(*delta));
                Ok(StmtFlow::Finish)
            },

            CppStmt::Block(stmts) => {
                // 嵌套 block: 独立词法作用域, 退出时销毁本层声明 (屏蔽语义)
                self.eval_body_scoped(stmts, sc)
            },

            CppStmt::If(cond, then_body, else_body) => {
                // 条件算不出来 → 报错 (旧实现直接跳过整个 if, 两支的副作用都丢)
                let taken = self.eval_branch_taken(cond, sc)?;
                let arm: &[Box<CppStmt>] = if taken {
                    then_body
                } else {
                    match else_body {
                        Some(b) => b,
                        None => &[],
                    }
                };
                // if 的每个分支体自带一层作用域 (C++: 分支内的声明随分支结束失效)
                self.eval_body_scoped(arm, sc)
            },

            CppStmt::For(init, cond, step, body_stmts) => self.run_for_loop(
                init.as_deref(),
                cond.as_ref(),
                step.as_deref(),
                body_stmts,
                sc,
            ),

            CppStmt::While(condition, body_stmts) => {
                self.run_while_loop(condition.as_ref(), body_stmts, sc)
            },

            CppStmt::Switch(cond_expr, cases) => self.run_switch(cond_expr.as_ref(), cases, sc),

            CppStmt::Return(return_expr) => self.eval_return_stmt(return_expr, sc),

            CppStmt::Break => Ok(StmtFlow::Break),
            CppStmt::Continue => Ok(StmtFlow::Continue),
        }
    }

    /// `return expr;` / `return;`
    ///
    /// 带表达式的 return 必须能求出常量, 否则这次调用就不是常量表达式
    /// (报明确错误, 而不是退回成"函数没有返回值"这种误导性的诊断).
    fn eval_return_stmt(
        &mut self,
        expr: &Option<CppExpr>,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        match expr {
            Some(e) => Ok(StmtFlow::Return(self.eval_expr_required(e, sc)?)),
            // void return: 统一以 0 交付 (与旧实现一致)
            None => Ok(StmtFlow::Return(0)),
        }
    }

    /// 在**新词法作用域**里执行语句序列 (block / if 分支 / 循环体 / case 体共用).
    ///
    /// push/pop 严格配对 (Err 路径也先 pop), 因此块内声明随块销毁 ——
    /// 内层 `int x = 100;` 不会影响外层 `x`.
    fn eval_body_scoped(
        &mut self,
        stmts: &[Box<CppStmt>],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        sc.push_scope();
        let flow = self.eval_vector_stmts(stmts, sc);
        sc.pop_scope();
        flow
    }

    /// Evaluate statements in a `Vec<Box<CppStmt>>` (AST 里语句体都以 Box 存放).
    ///
    /// 直接把 `Box<CppStmt>` 剥成 `&CppStmt` 交给单条派发, 不再像旧实现那样
    /// 先 `clone()` 整个 Vec 再 `locals.clone()` —— 那是局部状态跨语句丢失的根因.
    fn eval_vector_stmts(
        &mut self,
        stmts: &[Box<CppStmt>],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        for boxed in stmts {
            let flow = self.run_one_stmt(boxed, sc)?;
            if !matches!(flow, StmtFlow::Finish) {
                return Ok(flow);
            }
        }
        Ok(StmtFlow::Finish)
    }

    /// 执行表达式语句: 先尝试就地改写 (`a = b` / `++i` / `sum += x`),
    /// 不命中再按纯表达式求值.
    fn run_expr_stmt(
        &mut self,
        expr: &CppExpr,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        if let Some((name, kind)) = decode_mutation(expr) {
            self.apply_mutation(&name, &kind, sc)?;
            return Ok(StmtFlow::Finish);
        }
        // 纯表达式语句 (如 `f(x);`): 求不出常量**不报错**。约束要求"未命中
        // func_table 的调用保持返回 None、不新增错误", 而调用非 constexpr 函数
        // 在 C++ 表达式语句里本就合法。
        let _ = self.eval_expr_in(expr, sc)?;
        Ok(StmtFlow::Finish)
    }

    /// 把解码出的就地改写落到作用域栈 (写入落在声明帧 → 跨语句/跨迭代累积).
    fn apply_mutation(
        &mut self,
        name: &str,
        kind: &MutKind,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<()> {
        let current = sc.lookup(name).unwrap_or(0);
        let next = match kind {
            MutKind::Delta(delta) => current.wrapping_add(*delta),
            MutKind::Set(rhs) => self.eval_expr_required(rhs, sc)?,
            MutKind::Compound(base, rhs) => {
                // 合成 `current op <rhs>` 后交给 legacy 求值器: 除零 / 移位越界
                // 等边界语义与纯表达式折叠保持同一份真源.
                let combined =
                    CppExpr::BinOp(*base, Box::new(int_lit_expr(current)), Box::new(rhs.clone()));
                self.eval_expr_required(&combined, sc)?
            },
        };
        sc.assign_or_declare(name, next);
        Ok(())
    }

    /// 求分支/循环条件的真假; 算不出来时**报错** (旧实现直接跳过整个 if/while,
    /// 分支里的副作用会静默丢失). 条件为 0 即假, 非 0 即真.
    fn eval_branch_taken(
        &mut self,
        cond: &CppExpr,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<bool> {
        let value = self.eval_expr_required(cond, sc)?;
        Ok(value != 0)
    }

    /// `for (init; cond; step) body` —— 与 C++ 一致的迭代顺序:
    /// init → (cond → body → step)*, `continue` 仍会执行 step.
    ///
    /// init / cond / step / body 共用**循环作用域** (`for (int i = 0; ...)` 的 `i`
    /// 只在循环内可见, 但跨迭代保留), body 自身再嵌套一层作用域.
    /// 旧实现的 for-init 走 `eval_simple_stmt` 且从不写回 locals —— 循环变量
    /// 根本没绑定; 现在 init 由 `run_one_stmt` 直接声明进循环帧.
    fn run_for_loop(
        &mut self,
        init: Option<&CppStmt>,
        cond: Option<&CppExpr>,
        step: Option<&CppStmt>,
        body: &[Box<CppStmt>],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        sc.push_scope();
        let flow = self.interpret_for_loop(init, cond, step, body, sc);
        sc.pop_scope();
        flow
    }

    /// `run_for_loop` 的实体 (已处在循环作用域内, 错误由外层配对 pop).
    fn interpret_for_loop(
        &mut self,
        init: Option<&CppStmt>,
        cond: Option<&CppExpr>,
        step: Option<&CppStmt>,
        body: &[Box<CppStmt>],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        if let Some(init_stmt) = init {
            let flow = self.run_one_stmt(init_stmt, sc)?;
            // init 位置只会是简单语句; 若它已 return/break/continue, 循环不再进入
            if !matches!(flow, StmtFlow::Finish) {
                return Ok(flow);
            }
        }
        let mut iterations = 0usize;
        loop {
            // 上界保护: 超限**报错**, 不能静默当成"循环已跑完" (那会送出不完整的副作用)
            if iterations >= MAX_CONSTEXPR_LOOP_ITERATIONS {
                return Err(loop_limit_error());
            }
            if let Some(condition) = cond {
                if !self.eval_branch_taken(condition, sc)? {
                    break;
                }
            }
            // `for (;;)` 无条件的写法就是恒真, 只能靠 body 里的 break/return 退出
            iterations += 1;
            match self.eval_body_scoped(body, sc)? {
                StmtFlow::Return(value) => return Ok(StmtFlow::Return(value)),
                StmtFlow::Break => break,
                // continue 与正常收尾都要走到 step (C++ for 语义)
                StmtFlow::Continue | StmtFlow::Finish => {},
            }
            if let Some(step_stmt) = step {
                let flow = self.run_one_stmt(step_stmt, sc)?;
                if !matches!(flow, StmtFlow::Finish) {
                    return Ok(flow);
                }
            }
        }
        Ok(StmtFlow::Finish)
    }

    /// `while (cond) body` —— 逐轮真实解释 (旧实现拿 100 次上限空转, body 副作用全丢).
    fn run_while_loop(
        &mut self,
        cond: Option<&CppExpr>,
        body: &[Box<CppStmt>],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        sc.push_scope();
        let flow = self.interpret_while_loop(cond, body, sc);
        sc.pop_scope();
        flow
    }

    /// `run_while_loop` 的实体.
    fn interpret_while_loop(
        &mut self,
        cond: Option<&CppExpr>,
        body: &[Box<CppStmt>],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        let mut iterations = 0usize;
        loop {
            if iterations >= MAX_CONSTEXPR_LOOP_ITERATIONS {
                return Err(loop_limit_error());
            }
            if let Some(condition) = cond {
                if !self.eval_branch_taken(condition, sc)? {
                    break;
                }
            }
            iterations += 1;
            match self.eval_body_scoped(body, sc)? {
                StmtFlow::Return(value) => return Ok(StmtFlow::Return(value)),
                StmtFlow::Break => break,
                // while 没有 step, continue 与正常收尾同样回到条件判断
                StmtFlow::Continue | StmtFlow::Finish => {},
            }
        }
        Ok(StmtFlow::Finish)
    }

    /// `switch (cond) { case ...: default: ... }` 的编译期分支选择.
    ///
    /// 与 [stmt.switch] 一致: 匹配的 case 优先于 default, 与两者在源码里的
    /// 先后顺序无关 (旧实现按表序遇 default 就命中)。未建模 case 贯穿
    /// (fallthrough): 一个分支体跑完即结束整个 switch。
    fn run_switch(
        &mut self,
        cond: Option<&CppExpr>,
        cases: &[SwitchArm],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<StmtFlow> {
        // switch 条件求不出 → 报错 (旧实现会把所有分支都当作不执行, 副作用同样丢)
        let subject = match cond {
            Some(expr) => self.eval_expr_required(expr, sc)?,
            None => {
                return Err(
                    "switch without a condition cannot be evaluated at constexpr time".to_string(),
                )
            },
        };
        let mut matched: Option<&Vec<Box<CppStmt>>> = None;
        let mut default_arm: Option<&Vec<Box<CppStmt>>> = None;
        for (case_expr, arm_body) in cases {
            match case_expr {
                None => {
                    if default_arm.is_none() {
                        default_arm = Some(arm_body);
                    }
                },
                Some(case) => {
                    if self.eval_expr_required(case, sc)? == subject {
                        matched = Some(arm_body);
                        break;
                    }
                },
            }
        }
        let body: &[Box<CppStmt>] = match matched.or(default_arm) {
            Some(b) => b,
            // 无 case 命中且无 default: switch 什也不做 (C++ 语义)
            None => return Ok(StmtFlow::Finish),
        };
        // case 体自带一层作用域; `break;` 结束 switch, 其他流向继续递升
        let flow = self.eval_body_scoped(body, sc)?;
        Ok(match flow {
            StmtFlow::Break => StmtFlow::Finish,
            other => other,
        })
    }

    /// 在解释器上下文里求值表达式:
    /// 1. 先把树里命中 `func_table` 的 constexpr 调用折叠成字面量;
    /// 2. 再把只剩字面量/变量的纯表达式交给 legacy 求值器 `eval_cpp_const` ——
    ///    算术语义保持单一真源, 不在解释器里重复实现一份.
    ///
    /// 返回 `Ok(None)` 表示"折叠后仍非常量" (legacy 语义, 不新增错误);
    /// `Err` 只在命中 func_table 但被调函数无法完成求值时出现.
    fn eval_expr_in(
        &mut self,
        e: &CppExpr,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<Option<i128>> {
        let folded = self.fold_constexpr_calls(e, sc)?;
        let merged = sc.merged();
        Ok(eval_cpp_const(&folded, &merged))
    }

    /// 求值表达式并要求它**必须**是常量 (用于影响结果的语境: 初始化式 /
    /// 分支与循环条件 / return 表达式 / 复合赋值的右值).
    fn eval_expr_required(
        &mut self,
        e: &CppExpr,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<i128> {
        let value = self.eval_expr_in(e, sc)?;
        match value {
            Some(v) => Ok(v),
            None => Err(format!("cannot evaluate expression at constexpr time: {:?}", e)),
        }
    }

    /// 若 callee 命中 `func_table`, 按实参递归求值 (G25: 打通 constexpr 调用折叠).
    ///
    /// - `Ok(Some(v))` 命中并完成求值;
    /// - `Ok(None)`   未命中 —— 保持既有行为: 调用原样留在树里, legacy 求值器
    ///   对它返回 None, **不新增错误**;
    /// - `Err(msg)`   命中但被调函数无法完成求值 (递归超深 / consteval 违规 /
    ///   体内出现不可求值的表达式), 错误原样上抛, 不把"求值失败"伪装成
    ///   "这不是 constexpr 调用".
    fn try_eval_constexpr_call(
        &mut self,
        callee: &CppExpr,
        args: &[CppExpr],
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<Option<i128>> {
        // `func_table` 是 `Option<&'a HashMap<_>>` (Copy), 先拷出来再借 `&mut self`,
        // 否则表的不可变借用会穿过递归调用链, 借用检查器不允许.
        let table = match self.func_table {
            Some(t) => t,
            None => return Ok(None),
        };
        let name = match constexpr_callee_name(callee) {
            Some(n) => n,
            None => return Ok(None),
        };
        let func = match lookup_constexpr_func(table, name) {
            Some(f) => f,
            None => return Ok(None),
        };
        // 实参先求成常量; 任一实参不是常量 → 这次调用无法折叠 (交回 legacy 语义)
        let mut arg_values: Vec<i128> = Vec::with_capacity(args.len());
        for a in args {
            let value = self.eval_expr_in(a, sc)?;
            match value {
                Some(v) => arg_values.push(v),
                None => return Ok(None),
            }
        }
        // 深度上限与被调入口共用同一份实现 (check_recursion_limit + enter/exit_call)
        let value = self.eval_constexpr_function_body(func, &arg_values)?;
        Ok(Some(value))
    }

    /// 递归地把表达式树里**命中 `func_table`** 的 constexpr 调用换成整型字面量,
    /// 使剩下的树是一条只含字面量/变量的纯表达式, 可交给 `eval_cpp_const` 折叠.
    ///
    /// 未命中的调用原样保留; `Ternary` / `&&` / `||` 先算左侧, 按 C++ 短路律
    /// 只折叠真正被求值的那一支, 否则死分支里一个非 constexpr 调用会把整棵
    /// 本可折叠的表达式打成错误.
    fn fold_constexpr_calls(
        &mut self,
        e: &CppExpr,
        sc: &mut LocalsStack,
    ) -> ConstEvalResult<CppExpr> {
        let folded = match e {
            CppExpr::Call(callee, args) => {
                let folded_callee = self.fold_constexpr_calls(callee, sc)?;
                if let Some(v) = self.try_eval_constexpr_call(&folded_callee, args, sc)? {
                    return Ok(int_lit_expr(v));
                }
                let mut folded_args = Vec::with_capacity(args.len());
                for a in args {
                    folded_args.push(self.fold_constexpr_calls(a, sc)?);
                }
                CppExpr::Call(Box::new(folded_callee), folded_args)
            },
            CppExpr::UnaryOp(op, inner) => {
                let folded_inner = self.fold_constexpr_calls(inner, sc)?;
                CppExpr::UnaryOp(*op, Box::new(folded_inner))
            },
            CppExpr::BinOp(op, lhs, rhs) => {
                let folded_lhs = self.fold_constexpr_calls(lhs, sc)?;
                // `&&` / `||`: 左侧已定出结果时右侧根本不会被求值
                if let Some(v) = self.short_circuit_result(op, &folded_lhs, sc) {
                    return Ok(int_lit_expr(v));
                }
                let folded_rhs = self.fold_constexpr_calls(rhs, sc)?;
                CppExpr::BinOp(*op, Box::new(folded_lhs), Box::new(folded_rhs))
            },
            CppExpr::Ternary { cond, then, els } => {
                let folded_cond = self.fold_constexpr_calls(cond, sc)?;
                // 条件已可判定 → 整棵三元式就是真正被求值的那一支
                let taken = eval_cpp_const(&folded_cond, &sc.merged()).map(|c| c != 0);
                if let Some(branch) = taken {
                    let keep = if branch { then } else { els };
                    return self.fold_constexpr_calls(keep, sc);
                }
                let folded_then = self.fold_constexpr_calls(then, sc)?;
                let folded_els = self.fold_constexpr_calls(els, sc)?;
                CppExpr::Ternary {
                    cond: Box::new(folded_cond),
                    then: Box::new(folded_then),
                    els: Box::new(folded_els),
                }
            },
            CppExpr::Cast(ty, inner) => {
                let folded_inner = self.fold_constexpr_calls(inner, sc)?;
                CppExpr::Cast(ty.clone(), Box::new(folded_inner))
            },
            CppExpr::StaticCast { ty, expr } => {
                let folded_inner = self.fold_constexpr_calls(expr, sc)?;
                CppExpr::StaticCast { ty: ty.clone(), expr: Box::new(folded_inner) }
            },
            CppExpr::ConstCast { ty, expr } => {
                let folded_inner = self.fold_constexpr_calls(expr, sc)?;
                CppExpr::ConstCast { ty: ty.clone(), expr: Box::new(folded_inner) }
            },
            CppExpr::ReinterpretCast { ty, expr } => {
                let folded_inner = self.fold_constexpr_calls(expr, sc)?;
                CppExpr::ReinterpretCast { ty: ty.clone(), expr: Box::new(folded_inner) }
            },
            CppExpr::Comma(exprs) => {
                let mut folded_elems = Vec::with_capacity(exprs.len());
                for x in exprs {
                    folded_elems.push(self.fold_constexpr_calls(x, sc)?);
                }
                CppExpr::Comma(folded_elems)
            },
            // 其余变体 (字面量 / 标识符 / sizeof / 成员访问 / new / cast 列表 ...)
            // 不含可折叠的 constexpr 调用, 原样返回.
            _ => e.clone(),
        };
        Ok(folded)
    }

    /// `&&` / `||` 的左侧若已决定整体结果, 返回该结果 (0/1); 否则 None.
    fn short_circuit_result(
        &self,
        op: &BinOpKind,
        folded_lhs: &CppExpr,
        sc: &LocalsStack,
    ) -> Option<i128> {
        let is_land = matches!(op, BinOpKind::LAnd);
        let is_lor = matches!(op, BinOpKind::LOr);
        if !is_land && !is_lor {
            return None;
        }
        let lhs_value = eval_cpp_const(folded_lhs, &sc.merged())?;
        // `0 && <anything>` 必为 0; `1 || <anything>` 必为 1
        if (is_land && lhs_value == 0) || (is_lor && lhs_value != 0) {
            Some((!is_land) as i128)
        } else {
            None
        }
    }
}

// ============================================================================
// Legacy API (保持向后兼容)
// ============================================================================

/// sizeof(T) 的编译期折叠: 内建类型 / 指针 / 定长数组按 LP64 数据模型计算.
/// 类 / 模板 / auto 等需要 ABI 布局信息的类型返回 None (交由语义阶段处理).
///
/// **降级范围**: 此处 `long`/指针 硬为 8 字节, `long double` 为 16 字节,
/// 仅对 x86_64 / aarch64 / riscv64 等 LP64 目标正确。若需在 arm32 /
/// mips32 等 ILP32/LP32 目标上折叠 sizeof, 需把 `Target` 的 ptr_size/long_size
/// 透传进来 (本函数与 `eval_cpp_const` 目前无 target 上下文).
/// 与 legacy 路径 `Lower::ty_size` 采用同一假设。
fn sizeof_mangle_type(ty: &crate::cpp::mangle::MangleType) -> Option<i128> {
    use crate::cpp::mangle::MangleType;
    match ty {
        MangleType::Bool | MangleType::Char | MangleType::Schar | MangleType::Uchar => Some(1),
        MangleType::Short | MangleType::Ushort => Some(2),
        MangleType::Int | MangleType::Uint | MangleType::Float => Some(4),
        MangleType::Long
        | MangleType::Ulong
        | MangleType::LongLong
        | MangleType::UlongLong
        | MangleType::Double => Some(8),
        MangleType::LongDouble => Some(16),
        MangleType::Pointer(_) | MangleType::Reference(_) => Some(8),
        MangleType::Array(elem, n) => sizeof_mangle_type(elem).map(|s| s * *n as i128),
        _ => None,
    }
}

/// 求值 CppExpr 到 i128. 失败返回 None.
///
/// **注意**: 保留以维持向后兼容性。Phase 3 新功能在 eval_cpp_const_full()中实现。
pub fn eval_cpp_const(e: &CppExpr, locals: &HashMap<String, i128>) -> Option<i128> {
    match e {
        CppExpr::IntLit(v) => Some(*v as i128),
        CppExpr::UnaryOp(op, inner) => {
            let v = eval_cpp_const(inner, locals)?;
            let res = match op {
                UnaryOpKind::Neg => v.wrapping_neg(),
                UnaryOpKind::Plus => v,
                UnaryOpKind::LNot => {
                    if v == 0 {
                        1
                    } else {
                        0
                    }
                },
                UnaryOpKind::Not => !v,
                _ => return None,
            };
            Some(res)
        },
        CppExpr::BinOp(op, lhs, rhs) => {
            let l = eval_cpp_const(lhs, locals)?;
            let r = eval_cpp_const(rhs, locals)?;
            
            match op {
                BinOpKind::Add => Some(l + r),
                BinOpKind::Sub => Some(l - r),
                BinOpKind::Mul => Some(l * r),
                BinOpKind::Div => {
                    if r != 0 {
                        Some(l / r)
                    } else {
                        None
                    }
                },
                BinOpKind::Mod => {
                    if r != 0 {
                        Some(l % r)
                    } else {
                        None
                    }
                },
                BinOpKind::And => Some(l & r),
                BinOpKind::Or => Some(l | r),
                BinOpKind::Xor => Some(l ^ r),
                BinOpKind::Shl => {
                    if (0..128).contains(&r) {
                        Some(l << r)
                    } else {
                        None
                    }
                },
                BinOpKind::Shr => {
                    if (0..128).contains(&r) {
                        Some(l >> r)
                    } else {
                        None
                    }
                },
                BinOpKind::EqEq => Some(if l == r { 1 } else { 0 }),
                BinOpKind::Ne => Some(if l != r { 1 } else { 0 }),
                BinOpKind::Lt => Some(if l < r { 1 } else { 0 }),
                BinOpKind::Le => Some(if l <= r { 1 } else { 0 }),
                BinOpKind::Gt => Some(if l > r { 1 } else { 0 }),
                BinOpKind::Ge => Some(if l >= r { 1 } else { 0 }),
                BinOpKind::LAnd => Some(if l != 0 && r != 0 { 1 } else { 0 }),
                BinOpKind::LOr => Some(if l != 0 || r != 0 { 1 } else { 0 }),
                _ => None,
            }
        },
        // sizeof(T) 简化实现
        CppExpr::Call(f, args) => {
            if let CppExpr::Ident(name) = &**f {
                if name == "sizeof" && args.len() == 1 {
                    if let CppExpr::Ident(t) = &args[0] {
                        return match t.as_str() {
                            "char" | "bool" | "_Bool" => Some(1),
                            "short" => Some(2),
                            "int" | "float" | "void*" => Some(4),
                            "long" => Some(8), // 64-bit Linux
                            "long long" | "double" | "void**" => Some(8),
                            _ => None,
                        };
                    }
                }
            }
            None
        },
        CppExpr::Sizeof(arg) => match arg {
            SizeofArg::Type(t) => sizeof_mangle_type(t),
            // sizeof 表达式形式需要表达式类型信息 (locals 只存常量值)
            SizeofArg::Expr(_) => None,
            // sizeof...(pack): 包长度需模板单态化上下文 (由 template 层替成字面量)
            SizeofArg::Pack(_) => None,
        },
        CppExpr::Noexcept(e) => noexcept_operand_status(e).map(|can| if can { 0 } else { 1 }),
        CppExpr::Ternary { cond, then, els } => {
            let c = eval_cpp_const(cond, locals)?;
            if c != 0 {
                eval_cpp_const(then, locals)
            } else {
                eval_cpp_const(els, locals)
            }
        },
        CppExpr::Cast(_, inner) | CppExpr::StaticCast { expr: inner, .. } => {
            eval_cpp_const(inner, locals)
        },
        CppExpr::Ident(name) => match name.as_str() {
            "true" => Some(1),
            "false" | "nullptr" => Some(0),
            _ => locals.get(name).copied(),
        },
        _ => None,
    }
}

/// 检查 static_assert 条件，如果为 false 则返回 Some("error message")
pub fn check_static_assert(condition: &CppExpr, locals: &HashMap<String, i128>) -> Option<String> {
    match eval_cpp_const(condition, locals) {
        Some(0) => Some("static assertion failed".to_string()),
        Some(_) => None, // condition is true, OK
        None => None,    // cannot evaluate, defer to semantic analysis
    }
}

// ============================================================================
// Phase 3: Enhanced Evaluation API
// ============================================================================

/// Use full context-aware evaluation
/// M26.1: `noexcept(expr)` 谓词的语法三态判定 (常量求值器无符号表上下文,
/// 函数调用的异常规范未知 → None = 无法判定, 调用方自行回退/不折叠)。
/// `Some(true)` = 必可抛 (new 的 bad_alloc / 向引用 dynamic_cast 的 bad_cast /
/// 重载运算符); `Some(false)` = 必不抛 (字面量/内建运算/成员访问/静态 cast)。
/// 带完整异常规范信息的判定见 `sema` (native) 与 `lower::expr_can_throw` (C 后端)。
pub fn noexcept_operand_status(e: &CppExpr) -> Option<bool> {
    match e {
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
        | CppExpr::QualifiedAccess(..)
        | CppExpr::MemberStar(..)
        | CppExpr::PtrMemberStar(..)
        // noexcept 谓词自身永不抛 (其操作数不再参与求值)
        | CppExpr::Noexcept(_) => Some(false),
        CppExpr::New(..) | CppExpr::NewArr { .. } => Some(true),
        CppExpr::DynamicCast { ty, expr } => {
            if matches!(ty, crate::cpp::mangle::MangleType::Reference(_)) {
                Some(true)
            } else {
                noexcept_operand_status(expr)
            }
        },
        CppExpr::Member(o, _)
        | CppExpr::PtrMember(o, _)
        | CppExpr::UnaryOp(_, o)
        | CppExpr::Ref(o)
        | CppExpr::AddrOf(o)
        | CppExpr::Cast(_, o)
        | CppExpr::StaticCast { expr: o, .. }
        | CppExpr::ConstCast { expr: o, .. }
        | CppExpr::ReinterpretCast { expr: o, .. } => noexcept_operand_status(o),
        CppExpr::BinOp(_, l, r) => {
            combine_can_throw(&[noexcept_operand_status(l), noexcept_operand_status(r)])
        },
        CppExpr::Ternary { cond, then, els } => combine_can_throw(&[
            noexcept_operand_status(cond),
            noexcept_operand_status(then),
            noexcept_operand_status(els),
        ]),
        CppExpr::Comma(es) | CppExpr::InitList(es) => {
            combine_can_throw(&es.iter().map(noexcept_operand_status).collect::<Vec<_>>())
        },
        // 函数调用/放置 new/delete/多下标/类型初始化列表/typeid: 需要异常规范表
        CppExpr::Call(..)
        | CppExpr::OpCall { .. }
        | CppExpr::PlacementNew { .. }
        | CppExpr::Delete(_)
        | CppExpr::DeleteArr(_)
        | CppExpr::MultiIndex(..)
        | CppExpr::TypedInitList { .. }
        | CppExpr::Typeid(_) => None,
        // 新增语法变体 (如 pack 展开): 无信息, 无法判定
        _ => None,
    }
}

/// 三态合并: 任一子式必抛 → Some(true); 全部必不抛 → Some(false); 否则 None。
fn combine_can_throw(parts: &[Option<bool>]) -> Option<bool> {
    if parts.contains(&Some(true)) {
        return Some(true);
    }
    if parts.iter().all(|p| p.is_some()) {
        return Some(false);
    }
    None
}

pub fn eval_cpp_const_full(expr: &CppExpr, context: &ConstexprContext) -> Option<i128> {
    // G28: __builtin_is_constant_evaluated 特殊处理
    if let CppExpr::Call(func, args) = expr {
        if let CppExpr::Ident(name) = &**func {
            if name == "__builtin_is_constant_evaluated" || name == "std::is_constant_evaluated" {
                // 忽略参数，直接返回当前上下文状态
                return if context.is_constant_evaluated() {
                    Some(1)
                } else {
                    Some(0)
                };
            }

            // sizeof 支持 with TypeInfoRepository
            if name == "sizeof" && args.len() == 1 {
                if let CppExpr::Ident(t) = &args[0] {
                    // First try TypeInfoRepository if available
                    if let Some(type_info) = context.get_type_info() {
                        if let Some(size) = type_info.size_of(t) {
                            return Some(size as i128);
                        }
                    }

                    // Fall back to primitive types
                    return match t.as_str() {
                        "char" | "bool" | "_Bool" => Some(1),
                        "short" => Some(2),
                        "int" | "float" | "void*" => Some(4),
                        "long" => Some(8),
                        "long long" | "double" | "void**" => Some(8),
                        _ => None,
                    };
                }
            }
        }
    }

    // Delegate to legacy evaluator for other expressions
    eval_cpp_const(expr, context.locals)
}

/// 带 constexpr 函数表的表达式折叠入口 (G25 新增, 供生产路径接入).
///
/// 与 [`eval_cpp_const_full`] 的区别: 本函数会先把**命中 `context.func_table`**
/// 的函数调用送进解释器递归求值, 再把剩下的纯表达式折叠成常量 ——
/// 典型需求就是 `constexpr int sq12 = square(12); char buf[sq12];` 这类
/// 数组维度折叠。
///
/// 行为向后兼容: 没有函数表、或调用未命中时, 结果与 `eval_cpp_const_full`
/// 完全一致 (`__builtin_is_constant_evaluated` / `sizeof` 仍由前者处理);
/// 不会因折叠失败而新增诊断。
pub fn eval_cpp_const_full_with_calls(
    expr: &CppExpr,
    context: &mut ConstexprContext<'_>,
) -> Option<i128> {
    // 先走既有路径: 它已经处理了 is_constant_evaluated / sizeof(T) 等带外信息
    let direct = eval_cpp_const_full(expr, context);
    if direct.is_some() {
        return direct;
    }
    // `context.locals` 是 `&'a` 字段 (Copy), 先拷出表与基表再取 `&mut context`,
    // 否则不可变借用会穿过解释器的递归调用链.
    let base = context.locals;
    let mut sc = LocalsStack::with_base(base);
    let folded = context.fold_constexpr_calls(expr, &mut sc).ok()?;
    let merged = sc.merged();
    eval_cpp_const(&folded, &merged)
}

// ============================================================================
// Test Cases
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── 测试用 AST 构造小助手 (让手建工种读起来接近 C++ 源码) ──────────
    fn var(n: &str) -> CppExpr {
        CppExpr::Ident(n.to_string())
    }

    fn lit(v: i64) -> CppExpr {
        CppExpr::IntLit(v)
    }

    fn bin(op: BinOpKind, l: CppExpr, r: CppExpr) -> CppExpr {
        CppExpr::BinOp(op, Box::new(l), Box::new(r))
    }

    fn boxed(s: CppStmt) -> Box<CppStmt> {
        Box::new(s)
    }

    fn decl(n: &str, init: CppExpr) -> CppStmt {
        CppStmt::Decl(n.to_string(), Some(init))
    }

    fn assign(n: &str, value: CppExpr) -> CppStmt {
        CppStmt::Assign { name: n.to_string(), value }
    }

    fn ret(e: CppExpr) -> CppStmt {
        CppStmt::Return(Some(e))
    }

    fn func(name: &str, params: &[&str], body: Vec<CppStmt>) -> ConstexprFunction {
        ConstexprFunction {
            name: name.to_string(),
            params: params.iter().map(|p| p.to_string()).collect(),
            return_type: "int".to_string(),
            is_consteval: false,
            body,
        }
    }

    fn table_of(funcs: Vec<ConstexprFunction>) -> HashMap<String, ConstexprFunction> {
        funcs.into_iter().map(|f| (f.name.clone(), f)).collect()
    }

    /// 循环版 fib: `int a=0,b=1; for(int i=0;i<n;++i){int t=a+b; a=b; b=t;} return a;`
    fn fib_loop_func() -> ConstexprFunction {
        let loop_body = vec![
            boxed(decl("t", bin(BinOpKind::Add, var("a"), var("b")))),
            boxed(assign("a", var("b"))),
            boxed(assign("b", var("t"))),
        ];
        let for_stmt = CppStmt::For(
            Some(boxed(decl("i", lit(0)))),
            Some(bin(BinOpKind::Lt, var("i"), var("n"))),
            // 步进 `++i`: 解释器在表达式语句里识别就地改写并写回声明帧
            Some(boxed(CppStmt::Expr(CppExpr::UnaryOp(
                UnaryOpKind::PreInc,
                Box::new(var("i")),
            )))),
            loop_body,
        );
        func(
            "fib",
            &["n"],
            vec![decl("a", lit(0)), decl("b", lit(1)), for_stmt, ret(var("a"))],
        )
    }

    /// 递归版 gcd: `if (b == 0) return a; return gcd(b, a % b);`
    fn gcd_func() -> ConstexprFunction {
        func(
            "gcd",
            &["a", "b"],
            vec![
                CppStmt::If(
                    Box::new(bin(BinOpKind::EqEq, var("b"), lit(0))),
                    vec![boxed(ret(var("a")))],
                    None,
                ),
                ret(CppExpr::Call(
                    Box::new(var("gcd")),
                    vec![var("b"), bin(BinOpKind::Mod, var("a"), var("b"))],
                )),
            ],
        )
    }

    /// 嵌套调用: `int lcm(int a, int b) { return a / gcd(a, b) * b; }`
    fn lcm_func() -> ConstexprFunction {
        func(
            "lcm",
            &["a", "b"],
            vec![ret(bin(
                BinOpKind::Mul,
                bin(
                    BinOpKind::Div,
                    var("a"),
                    CppExpr::Call(
                        Box::new(var("gcd")),
                        vec![var("a"), var("b")],
                    ),
                ),
                var("b"),
            ))],
        )
    }

    #[test]
    fn noexcept_operand_status_is_three_state() {
        // M26.1: 无符号表上下文时的语法三态判定 (调用需异常规范 → None)
        assert_eq!(noexcept_operand_status(&CppExpr::IntLit(1)), Some(false));
        assert_eq!(noexcept_operand_status(&CppExpr::This), Some(false));
        assert_eq!(
            noexcept_operand_status(&CppExpr::New("int".into(), vec![])),
            Some(true)
        );
        assert_eq!(
            noexcept_operand_status(&CppExpr::BinOp(
                BinOpKind::Add,
                Box::new(CppExpr::IntLit(1)),
                Box::new(CppExpr::IntLit(2)),
            )),
            Some(false)
        );
        // 含调用的子式: 异常规范未知 → 整体无法判定
        assert_eq!(
            noexcept_operand_status(&CppExpr::BinOp(
                BinOpKind::Add,
                Box::new(CppExpr::IntLit(1)),
                Box::new(CppExpr::Call(Box::new(CppExpr::Ident("f".into())), vec![])),
            )),
            None
        );
        // 必抛子式短路优先于未知子式
        assert_eq!(
            noexcept_operand_status(&CppExpr::Comma(vec![
                CppExpr::New("int".into(), vec![]),
                CppExpr::Call(Box::new(CppExpr::Ident("f".into())), vec![]),
            ])),
            Some(true)
        );
        // noexcept 谓词自身永不抛
        assert_eq!(
            noexcept_operand_status(&CppExpr::Noexcept(Box::new(CppExpr::New(
                "int".into(),
                vec![],
            )))),
            Some(false)
        );
    }

    #[test]
    fn test_basic_arithmetic() {
        let locals = HashMap::new();
        let expr = CppExpr::BinOp(
            BinOpKind::Add,
            Box::new(CppExpr::IntLit(10)),
            Box::new(CppExpr::IntLit(20)),
        );

        assert_eq!(eval_cpp_const(&expr, &locals), Some(30));
    }

    #[test]
    fn test_local_lookup() {
        let mut locals = HashMap::new();
        locals.insert("x".to_string(), 42);

        let expr = CppExpr::Ident("x".to_string());

        assert_eq!(eval_cpp_const(&expr, &locals), Some(42));
    }

    #[test]
    fn test_builtin_is_constant_evaluated_true() {
        let locals = HashMap::new();
        let ctx = ConstexprContext::new(true, &locals);

        let expr = CppExpr::Call(
            Box::new(CppExpr::Ident("__builtin_is_constant_evaluated".to_string())),
            vec![],
        );

        assert_eq!(eval_cpp_const_full(&expr, &ctx), Some(1));
    }

    #[test]
    fn test_builtin_is_constant_evaluated_false() {
        let locals = HashMap::new();
        let ctx = ConstexprContext::new(false, &locals);

        let expr = CppExpr::Call(
            Box::new(CppExpr::Ident("__builtin_is_constant_evaluated".to_string())),
            vec![],
        );

        assert_eq!(eval_cpp_const_full(&expr, &ctx), Some(0));
    }

    #[test]
    fn test_complex_expression() {
        let mut locals = HashMap::new();
        locals.insert("a".to_string(), 10);
        locals.insert("b".to_string(), 5);

        let expr = CppExpr::BinOp(
            BinOpKind::Mul,
            Box::new(CppExpr::BinOp(
                BinOpKind::Add,
                Box::new(CppExpr::Ident("a".to_string())),
                Box::new(CppExpr::Ident("b".to_string())),
            )),
            Box::new(CppExpr::IntLit(2)),
        );

        assert_eq!(eval_cpp_const(&expr, &locals), Some(30));
    }

    #[test]
    fn test_function_call_setup() {
        // Verify that function structures exist and are usable
        let funcs = HashMap::new();
        let locals = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(false, &locals, &funcs);

        assert_eq!(ctx.current_depth, 0);

        ctx.enter_call();
        assert_eq!(ctx.current_depth, 1);

        ctx.exit_call();
        assert_eq!(ctx.current_depth, 0);
    }

    #[test]
    fn test_type_info_repository() {
        let repo = TypeInfoRepository::new();

        // Test primitive types
        assert_eq!(repo.size_of("int"), Some(4));
        assert_eq!(repo.size_of("double"), Some(8));
        assert_eq!(repo.size_of("char"), Some(1));

        // Test unknown type returns None
        assert_eq!(repo.size_of("UnknownType"), None);

        // Test class registration
        let mut repo2 = TypeInfoRepository::new();
        repo2.register_class("MyClass".to_string(), 32, 8);

        assert_eq!(repo2.size_of("MyClass"), Some(32));
        assert_eq!(repo2.align_of("MyClass"), Some(8));
    }

    #[test]
    fn test_context_with_type_info() {
        let locals = HashMap::new();
        let repo = TypeInfoRepository::new();
        let ctx = ConstexprContext::with_type_info(false, &locals, &repo);

        assert!(ctx.get_type_info().is_some());
    }

    #[test]
    fn test_consteval_violation() {
        // Test that consteval violation is recorded correctly
        let binding = HashMap::new();
        let mut ctx = ConstexprContext::new(false, &binding);

        ctx.record_violation("always_compile_time");
        assert!(ctx.has_violations());

        let msg = ctx.violations_message();
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("always_compile_time"));
    }

    #[test]
    fn test_consteval_enforcement() {
        // Simulate calling a consteval function outside constant context
        let binding = HashMap::new();
        let mut ctx = ConstexprContext::new(false, &binding);

        let func = ConstexprFunction {
            name: "consteval_func".to_string(),
            params: vec!["x".to_string()],
            return_type: "int".to_string(),
            is_consteval: true,
            body: vec![],
        };

        let result = ctx.eval_constexpr_function_body(&func, &[5]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("must be evaluated at compile time"));
    }

    #[test]
    fn test_local_variable_persistence() {
        // Test that local variables persist across statements
        let binding = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &binding);

        let body = vec![
            CppStmt::Decl("x".to_string(), Some(CppExpr::IntLit(42))),
            CppStmt::Return(Some(CppExpr::Ident("x".to_string()))),
        ];

        let func = ConstexprFunction {
            name: "test_func".to_string(),
            params: vec![],
            return_type: "int".to_string(),
            is_consteval: false,
            body,
        };

        let result = ctx.eval_constexpr_function_body(&func, &[]);
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_nested_block_shadowing() {
        // Test that nested blocks can shadow outer variables
        let binding = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &binding);

        let inner_block = vec![Box::new(CppStmt::Decl(
            "x".to_string(),
            Some(CppExpr::IntLit(100)),
        ))];
        let body = vec![
            CppStmt::Decl("x".to_string(), Some(CppExpr::IntLit(42))),
            CppStmt::Block(inner_block),
            CppStmt::Return(Some(CppExpr::Ident("x".to_string()))),
        ];

        let func = ConstexprFunction {
            name: "shadow_test".to_string(),
            params: vec![],
            return_type: "int".to_string(),
            is_consteval: false,
            body,
        };

        let result = ctx.eval_constexpr_function_body(&func, &[]);
        // Outer x should still be 42, not 100 (from inner block)
        assert_eq!(result.unwrap(), 42);
    }

    // ─────────────────────────────────────────────────────────────────────
    // G25 completion: constexpr 函数体解释器回归测试
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn test_fib_loop_returns_55() {
        // 循环版 fib(10) —— 验证作用域栈能跨迭代累积状态
        let fib = fib_loop_func();
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&fib, &[10]).unwrap(), 55);
        // 边界: fib(0) = 0, fib(1) = 1
        assert_eq!(ctx.eval_constexpr_function_body(&fib, &[0]).unwrap(), 0);
        assert_eq!(ctx.eval_constexpr_function_body(&fib, &[1]).unwrap(), 1);
    }

    #[test]
    fn test_fib_recursive_via_func_table() {
        // int fib(int n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); }
        let fib = func(
            "fib",
            &["n"],
            vec![
                CppStmt::If(
                    Box::new(bin(BinOpKind::Lt, var("n"), lit(2))),
                    vec![boxed(ret(var("n")))],
                    None,
                ),
                ret(bin(
                    BinOpKind::Add,
                    CppExpr::Call(
                        Box::new(var("fib")),
                        vec![bin(BinOpKind::Sub, var("n"), lit(1))],
                    ),
                    CppExpr::Call(
                        Box::new(var("fib")),
                        vec![bin(BinOpKind::Sub, var("n"), lit(2))],
                    ),
                )),
            ],
        );
        let funcs = table_of(vec![fib]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(true, &outer, &funcs);
        assert_eq!(
            ctx.eval_constexpr_function_body(&funcs["fib"], &[10]).unwrap(),
            55
        );
    }

    #[test]
    fn test_recursive_gcd_via_func_table() {
        let funcs = table_of(vec![gcd_func()]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(true, &outer, &funcs);
        assert_eq!(
            ctx.eval_constexpr_function_body(&funcs["gcd"], &[48, 18]).unwrap(),
            6
        );
        assert_eq!(
            ctx.eval_constexpr_function_body(&funcs["gcd"], &[270, 192]).unwrap(),
            6
        );
    }

    #[test]
    fn test_nested_constexpr_call_lcm_uses_gcd() {
        // lcm 体内调用 gcd, gcd 自身递归 —— 验证多层/嵌套调用折叠
        let funcs = table_of(vec![gcd_func(), lcm_func()]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(true, &outer, &funcs);
        assert_eq!(
            ctx.eval_constexpr_function_body(&funcs["lcm"], &[4, 6]).unwrap(),
            12
        );
    }

    #[test]
    fn test_accumulating_for_loop() {
        // int sum(int n) { int s = 0; for (int i = 1; i <= n; i = i + 1) s = s + i; return s; }
        let f = func(
            "sum",
            &["n"],
            vec![
                decl("s", lit(0)),
                CppStmt::For(
                    Some(boxed(decl("i", lit(1)))),
                    Some(bin(BinOpKind::Le, var("i"), var("n"))),
                    Some(boxed(assign("i", bin(BinOpKind::Add, var("i"), lit(1))))),
                    vec![boxed(assign("s", bin(BinOpKind::Add, var("s"), var("i"))))],
                ),
                ret(var("s")),
            ],
        );
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[10]).unwrap(), 55);
    }

    #[test]
    fn test_compound_assign_and_adjust_step() {
        // int f(int n) { int s = 0; for (int i = 0; i < n; ++i) { s += i; } return s; }
        // 同时跑通三条链路: `s += i` (表达式语句里的复合赋值) 与 `Adjust` 步进.
        let f = func(
            "tri",
            &["n"],
            vec![
                decl("s", lit(0)),
                CppStmt::For(
                    Some(boxed(decl("i", lit(0)))),
                    Some(bin(BinOpKind::Lt, var("i"), var("n"))),
                    Some(boxed(CppStmt::Adjust {
                        name: "i".to_string(),
                        delta: 1,
                    })),
                    vec![boxed(CppStmt::Expr(bin(
                        BinOpKind::AddAssign,
                        var("s"),
                        var("i"),
                    )))],
                ),
                ret(var("s")),
            ],
        );
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[10]).unwrap(), 45);
    }

    #[test]
    fn test_while_loop_with_break() {
        // int f() { int i = 0; while (i < 10) { i = i + 1; if (i == 3) break; } return i; }
        let f = func(
            "cnt",
            &[],
            vec![
                decl("i", lit(0)),
                CppStmt::While(
                    Some(bin(BinOpKind::Lt, var("i"), lit(10))),
                    vec![
                        boxed(assign("i", bin(BinOpKind::Add, var("i"), lit(1)))),
                        boxed(CppStmt::If(
                            Box::new(bin(BinOpKind::EqEq, var("i"), lit(3))),
                            vec![boxed(CppStmt::Break)],
                            None,
                        )),
                    ],
                ),
                ret(var("i")),
            ],
        );
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[]).unwrap(), 3);
    }

    #[test]
    fn test_assignment_persists_across_statements() {
        // int f() { int x = 1; x = x + 41; return x; }
        let f = func(
            "as",
            &[],
            vec![
                decl("x", lit(1)),
                assign("x", bin(BinOpKind::Add, var("x"), lit(41))),
                ret(var("x")),
            ],
        );
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[]).unwrap(), 42);
    }

    #[test]
    fn test_block_assignment_leaks_out_but_decl_does_not() {
        // 块内 `x = 5;` 改写外层声明 (作用域栈写入落在声明帧);
        // 而块内 `int y = 7;` 随块销毁 —— 两者一起守住 C++ 词法语义.
        let f = func(
            "blk",
            &[],
            vec![
                decl("x", lit(1)),
                CppStmt::Block(vec![
                    boxed(assign("x", lit(5))),
                    boxed(decl("y", lit(7))),
                ]),
                ret(var("x")),
            ],
        );
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[]).unwrap(), 5);
    }

    #[test]
    fn test_switch_prefers_matching_case_over_default() {
        // int pick(int k) { switch (k) { default: return -1; case 2: return 20; } return 0; }
        // default 写在匹配 case 前面, 旧实现会先命中 default 而返回错值.
        let arms = vec![
            (None, vec![boxed(ret(lit(-1)))]),
            (Some(lit(2)), vec![boxed(ret(lit(20)))]),
        ];
        let f = func(
            "pick",
            &["k"],
            vec![CppStmt::Switch(Some(var("k")), arms), ret(lit(0))],
        );
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[2]).unwrap(), 20);
        assert_eq!(ctx.eval_constexpr_function_body(&f, &[7]).unwrap(), -1);
    }

    #[test]
    fn test_division_by_zero_is_error_not_panic() {
        // 1 / 0 在 legacy 求值器里是 None, 解释器必须报 Err 而不是 panic
        let f = func("bad", &[], vec![ret(bin(BinOpKind::Div, lit(1), lit(0)))]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        let err = ctx.eval_constexpr_function_body(&f, &[]).unwrap_err();
        assert!(err.contains("cannot evaluate"), "unexpected: {}", err);
    }

    #[test]
    fn test_loop_iteration_limit_reported_not_dropped() {
        // `while (1) { }` 没有 break: 必须在迭代上限处报错,
        // 而不是静默当成"循环已跑完"继续往下返回 0.
        let f = func("spin", &[], vec![CppStmt::While(Some(lit(1)), vec![]), ret(lit(0))]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        let err = ctx.eval_constexpr_function_body(&f, &[]).unwrap_err();
        assert!(
            err.contains("iteration bound"),
            "unexpected: {}",
            err
        );
    }

    #[test]
    fn test_recursion_depth_exceeded_is_rejected() {
        // int loop(int n) { return loop(n) + 1; } —— 自递归且实参不变, 必超深度
        let loop_fn = func(
            "loop",
            &["n"],
            vec![ret(bin(
                BinOpKind::Add,
                CppExpr::Call(Box::new(var("loop")), vec![var("n")]),
                lit(1),
            ))],
        );
        let funcs = table_of(vec![loop_fn]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(true, &outer, &funcs);
        let err = ctx
            .eval_constexpr_function_body(&funcs["loop"], &[1])
            .unwrap_err();
        assert!(err.contains("recursion limit"), "unexpected: {}", err);
    }

    #[test]
    fn test_consteval_violation_still_reported_through_call_path() {
        // 体内调用一个 consteval 函数, 而当前不在常量上下文 → 仍必须报错
        let inner = ConstexprFunction {
            name: "must_be_const".to_string(),
            params: vec!["x".to_string()],
            return_type: "int".to_string(),
            is_consteval: true,
            body: vec![ret(var("x"))],
        };
        let caller = func(
            "user",
            &["v"],
            vec![ret(CppExpr::Call(
                Box::new(var("must_be_const")),
                vec![var("v")],
            ))],
        );
        let funcs = table_of(vec![inner, caller]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(false, &outer, &funcs);
        let err = ctx
            .eval_constexpr_function_body(&funcs["user"], &[3])
            .unwrap_err();
        assert!(
            err.contains("must be evaluated at compile time"),
            "unexpected: {}",
            err
        );
    }

    #[test]
    fn test_arity_mismatch_and_missing_return_rejected() {
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::new(true, &outer);
        // 形参 1 / 实参 0
        let needs_arg = func("one", &["x"], vec![ret(var("x"))]);
        let err = ctx
            .eval_constexpr_function_body(&needs_arg, &[])
            .unwrap_err();
        assert!(err.contains("argument"), "unexpected: {}", err);
        // 路径上没有 return
        let no_return = func("noret", &[], vec![decl("x", lit(1))]);
        let err = ctx.eval_constexpr_function_body(&no_return, &[]).unwrap_err();
        assert!(
            err.contains("must have a return value"),
            "unexpected: {}",
            err
        );
    }

    #[test]
    fn test_pure_expression_semantics_unchanged() {
        // 生产路径 (`if constexpr`) 依赖的纯表达式语义不得改变:
        // 是否携带 func_table 不影响结果, 未命中的调用仍返回 None (不新增错误).
        let mut locals = HashMap::new();
        locals.insert("x".to_string(), 3i128);
        let cond = bin(BinOpKind::Lt, var("x"), lit(5));
        let funcs = table_of(vec![gcd_func()]);
        let ctx_plain = ConstexprContext::new(true, &locals);
        let mut ctx_with = ConstexprContext::with_functions(true, &locals, &funcs);
        assert_eq!(eval_cpp_const_full(&cond, &ctx_plain), Some(1));
        assert_eq!(eval_cpp_const_full(&cond, &ctx_with), Some(1));
        let unknown = CppExpr::Call(Box::new(var("mystery")), vec![lit(1)]);
        assert_eq!(eval_cpp_const_full(&unknown, &ctx_with), None);
        assert_eq!(eval_cpp_const_full_with_calls(&unknown, &mut ctx_with), None);
        // 入口之间不得串状态: 走过带调用的入口后, legacy 入口结果照旧
        assert_eq!(eval_cpp_const_full(&cond, &ctx_with), Some(1));
        // legacy 入口对除零仍返回 None
        assert_eq!(
            eval_cpp_const(&bin(BinOpKind::Div, lit(1), lit(0)), &locals),
            None
        );
    }

    #[test]
    fn test_eval_cpp_const_full_with_calls_folds_square() {
        // 生产路径入口: `square(12)` 命中函数表 → 折叠成 144
        // (`constexpr int sq12 = square(12);` 数组维度用的就是这条通道)
        let square = func(
            "square",
            &["x"],
            vec![ret(bin(BinOpKind::Mul, var("x"), var("x")))],
        );
        let funcs = table_of(vec![square]);
        let outer = HashMap::new();
        let mut ctx = ConstexprContext::with_functions(true, &outer, &funcs);
        let call = CppExpr::Call(Box::new(var("square")), vec![lit(12)]);
        assert_eq!(eval_cpp_const_full_with_calls(&call, &mut ctx), Some(144));
        // 嵌套路: 1 + square(12) 也应整体折叠
        let wrapped = bin(BinOpKind::Add, lit(1), call);
        assert_eq!(eval_cpp_const_full_with_calls(&wrapped, &mut ctx), Some(145));
    }
}
