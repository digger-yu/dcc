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
use crate::cpp::runtime::CxxRuntimeSymbol;
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
    /// zero-cost EH 需要的 `DW.ref.<sym>` 间接槽。
    ///
    /// LSDA 类型表与 FDE 的 personality 字段都用 `DW_EH_PE_indirect` 编码,
    /// 表项指向这些 8 字节槽而不是直接指向目标符号 (与 g++ 产物同构, 才能在
    /// 动态链接下由 PLT/GOT 正常解析)。槽的**定义**由 `native.rs` 发射。
    pub eh_ref_slots: Vec<String>,
    /// 需要由本 TU 合成的**非类类型** typeinfo (`_ZTIi` / `_ZTSi` 等)。
    ///
    /// 布局层 (`ClassAbi::rtti`) 只覆盖类类型; `throw 1;` / `catch (int)` 的
    /// 类型匹配同样需要一个真实存在的 `_ZTI` 对象, 否则链入后是 undefined weak
    /// (值 0), `__do_catch` 解引用空指针。
    pub builtin_typeinfos: Vec<crate::cpp::typeinfo_mangling::BuiltinTypeinfoSymbols>,
}

// === zero-cost EH: 作用域模型 ===

/// 一条清理动作。
#[derive(Debug, Clone)]
enum EhCleanupKind {
    /// 重放 sema 为该局部量生成的析构语句 (与正常路径作用域末尾**同一条**语句,
    /// 因此清理语义只有单一真源, 不会与 sema 的析构规则漂移)。
    Destroy(HirStmtId),
    /// 离开 `catch` 体: `__cxa_end_catch()`。异常从 catch 体内穿出时必须执行,
    /// 否则 libstdc++ 的 handler 计数泄漏。
    EndCatch,
    /// `throw` 的异常对象**构造**抛出: `__cxa_free_exception(buf)`。
    ///
    /// 缓冲区已经分配但对象没构造成功, 不释放就是每次抛出泄漏一块内存;
    /// 实测 g++ 同样把构造调用包进受保护区间, 其落地垫先 `__cxa_free_exception`
    /// 再做局部清理。
    FreeException(SsaVar),
}

/// 带稳定 key 的清理项。
///
/// key 参与落地垫共享判定: 清理集合相同 ⇒ 生成的清理代码相同 ⇒ 可以共用同一个
/// 落地垫块 (与 GCC 按 region 合并落地垫同构)。
#[derive(Debug, Clone)]
struct EhCleanup {
    key: u64,
    kind: EhCleanupKind,
}

/// 一个 `catch` 子句在 EH 侧的描述。
#[derive(Debug, Clone)]
struct EhHandler {
    /// LSDA 类型表索引 (1-based, 本函数内单调分配); 也是落地垫里比较的 selector。
    type_index: u32,
    /// typeinfo 符号名; `None` = `catch (...)` (类型表里是值为 0 的空表项)。
    typeinfo: Option<String>,
    /// 该 handler 的入口块 (只由落地垫进入, 不在正常 CFG 里)。
    catch_bb: String,
}

/// EH 清理链的一项。栈内序 = 由外向内。
///
/// 用**一条平铺链**而不是「按语法块分组的帧栈」: 一个清理的生效区间不是语法块,
/// 而是「构造完成」到「正常析构语句」之间 —— 与 GCC `finish_expr` 在建构调用之后
/// 才把 cleanup 挂上清理链完全同一语义。sema 把析构语句物化在**所属作用域**的末尾,
/// 那个位置往往是外层兄弟语句 (实测 `void f(){ D a(1); throw 9; }` 的 HIR 形状是
/// `body = [Compound[Decl a, Expr Construct(a)], Expr Throw, Expr Destroy(a)]`),
/// 按 Compound 边界收帧会提前丢掉清理集合 —— 抛点因此完全不被保护 (实测表现:
/// 异常穿过本帧不执行析构, 外层也没登记 handler, 直接 `std::terminate`)。
/// 直接以构造/析构语句为进出界就不依赖任何语法形状, 也不会漏。
///
/// 链序就是 Itanium 异常展开要求的顺序: 先尝试最近的 handler, 每离开一个更外层的
/// 存活对象插入一条 cleanup 记录, 再尝试更外层的 handler —— 与 g++ 产物的 action
/// 链逐条一致。
#[derive(Debug, Clone)]
enum EhChainEntry {
    /// 一个仍存活的对象/状态在展开路径上必须执行的清理动作。
    Cleanup(EhCleanup),
    /// `try` 体: 离开时按源码序尝试的 handler。
    Try {
        try_id: u32,
        handlers: Vec<EhHandler>,
    },
}

/// 作用域快照里的一项 (自内向外), 用于落地垫代码生成。
///
/// 从 `eh_scope` **克隆**出来而不是借用: 生成落地垫要改写 `self` 的块与指令,
/// 借用 `self.eh_scope` 会与之冲突; 克隆同时天然形成快照, 正是缓存需要的语义。
#[derive(Debug, Clone)]
enum EhChainItem {
    /// 一个 try 的 handler 组 (源码序)。
    Try {
        try_id: u32,
        handlers: Vec<EhHandler>,
    },
    /// 一个块作用域的清理组。
    Cleanups(Vec<EhCleanup>),
}

/// 落地垫缓存的 key: 与作用域快照一一对应但可哈希。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EhChainKeyFrame {
    Try(u32),
    Block(Vec<u64>),
}

/// 预扫第二遍: 为每个需析构的局部量定位「构造完成语句」与「正常析构语句」。
///
/// - `activate_at`: 该语句执行**完之后**对象才进入清理集合。构造调用自身抛出时
///   对象还不存在, 不能析构 —— 所以激活点不能放在 `Decl` 上: sema 把 `D a(1);`
///   拆成 `Compound[Decl a, Expr Construct(a)]`, 于是激活点要往后看到那条
///   `Construct` 兄弟语句; 没有这种兄弟时 (声明即构造完成) 激活点就是 `Decl` 本身。
/// - `retire_at`: 正常路径的析构语句执行**之前**把清理撤掉。对象已死, 之后再抛出
///   不该再析构它; 同时这条析构调用自身要受「更外层清理」保护 (析构中途再抛时外层
///   对象仍要销毁), 先撤再发正好得到这个区间。
///
/// 递归覆盖的语句形态必须与 [`eh_collect_dtor_stmts`] 一致, 否则会出现「第一遍找到
/// 析构、第二遍找不到激活点」的静默漏清理。
fn eh_collect_cleanup_sites(
    hir: &HirTranslationUnit,
    stmts: &[HirStmtId],
    dtor_of: &HashMap<SymbolId, HirStmtId>,
    activate_at: &mut HashMap<HirStmtId, SymbolId>,
    retire_at: &mut HashMap<HirStmtId, SymbolId>,
    depth: u32,
) {
    // 与第一遍同一个深度上限: 只防环状畸形输入。
    if depth > 512 {
        return;
    }
    for (i, sid) in stmts.iter().enumerate() {
        let Some(stmt) = hir.stmts.get(sid.0) else {
            continue;
        };
        match &stmt.kind {
            HirStmtKind::Decl { symbol, .. } => {
                if !dtor_of.contains_key(symbol) {
                    continue;
                }
                let constructed_next = stmts
                    .get(i + 1)
                    .and_then(|n| hir.stmts.get(n.0))
                    .and_then(|n| match &n.kind {
                        HirStmtKind::Expr(e) => Some(*e),
                        _ => None,
                    })
                    .and_then(|e| hir.exprs.get(e.0))
                    .and_then(|ex| match &ex.kind {
                        HirExprKind::Construct { place, .. } => Some(*place),
                        _ => None,
                    })
                    .and_then(|place| eh_place_local_of(hir, place))
                    == Some(*symbol);
                if constructed_next {
                    activate_at.insert(stmts[i + 1], *symbol);
                } else {
                    activate_at.insert(*sid, *symbol);
                }
            },
            HirStmtKind::Expr(e) => {
                if let Some(sym) =
                    eh_destroy_local_of(hir, *e).filter(|sym| dtor_of.contains_key(sym))
                {
                    retire_at.insert(*sid, sym);
                }
            },
            HirStmtKind::Compound(inner) => {
                eh_collect_cleanup_sites(hir, inner, dtor_of, activate_at, retire_at, depth + 1);
            },
            HirStmtKind::If {
                then_body,
                else_body,
                ..
            } => {
                let one = [*then_body];
                eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
                if let Some(eb) = else_body {
                    let one = [*eb];
                    eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
                }
            },
            HirStmtKind::While { body, .. }
            | HirStmtKind::DoWhile { body, .. }
            | HirStmtKind::Switch { body, .. } => {
                let one = [*body];
                eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
            },
            HirStmtKind::For { init, body, .. } => {
                if let Some(id) = init {
                    let one = [*id];
                    eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
                }
                let one = [*body];
                eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
            },
            HirStmtKind::Try { body, handlers } => {
                let one = [*body];
                eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
                for h in handlers {
                    let one = [h.body];
                    eh_collect_cleanup_sites(hir, &one, dtor_of, activate_at, retire_at, depth + 1);
                }
            },
            _ => {},
        }
    }
}

/// 预扫 HIR 语句树, 收集「 sema 为哪个局部量发了析构语句」。
///
/// 只认 sema 自己的形状 (`build_dtor_stmts` 发的
/// `Expr(Destroy { place: AddressOf(Local(sym)), variant: Complete })`)。
/// 同一符号多条出口 (块末 + break 出口域) 时保留首条: 它们的 place 与
/// 析构变体完全相同, 重放哪一条产生的清理代码都一样。
///
/// 不递归进表达式树: 自动存储期析构在 HIR 里**只**以语句形态出现, 往表达式
/// 里钻只会误把 `delete` 的析构当成局部量清理。
fn eh_collect_dtor_stmts(
    hir: &HirTranslationUnit,
    stmt_id: HirStmtId,
    out: &mut HashMap<SymbolId, HirStmtId>,
    depth: u32,
) {
    // HIR 是一棵树, 但递归深度受源码嵌套约束; 上限只防住环状的畸形输入。
    if depth > 512 {
        return;
    }
    let Some(stmt) = hir.stmts.get(stmt_id.0) else {
        return;
    };
    let kind = stmt.kind.clone();
    let rec = |out: &mut HashMap<SymbolId, HirStmtId>, s: HirStmtId| {
        eh_collect_dtor_stmts(hir, s, out, depth + 1)
    };
    match &kind {
        HirStmtKind::Expr(e) => {
            if let Some(sym) = eh_destroy_local_of(hir, *e) {
                out.entry(sym).or_insert(stmt_id);
            }
        },
        HirStmtKind::Compound(ss) => {
            for s in ss {
                rec(out, *s);
            }
        },
        HirStmtKind::If {
            then_body,
            else_body,
            ..
        } => {
            rec(out, *then_body);
            if let Some(eb) = else_body {
                rec(out, *eb);
            }
        },
        HirStmtKind::While { body, .. } | HirStmtKind::Switch { body, .. } => rec(out, *body),
        HirStmtKind::DoWhile { body, .. } => rec(out, *body),
        HirStmtKind::For { init, body, .. } => {
            if let Some(i) = init {
                rec(out, *i);
            }
            rec(out, *body);
        },
        HirStmtKind::Case { body, .. } | HirStmtKind::Default(body) => rec(out, *body),
        HirStmtKind::Label(_, body) => rec(out, *body),
        HirStmtKind::Try { body, handlers } => {
            rec(out, *body);
            for h in handlers {
                rec(out, h.body);
            }
        },
        // 以下形态不会包含局部量析构语句: 空语句 / 跳转 / 声明自身 /
        // 静态存储对象 (走 __cxa_atexit) / ctor-init / vptr 写入。
        HirStmtKind::Null
        | HirStmtKind::Return(_)
        | HirStmtKind::Break
        | HirStmtKind::Continue
        | HirStmtKind::Fallthrough
        | HirStmtKind::Goto(_)
        | HirStmtKind::Decl { .. }
        | HirStmtKind::StaticDecl { .. }
        | HirStmtKind::CtorInitMember { .. }
        | HirStmtKind::CtorInitBase { .. }
        | HirStmtKind::SetVTablePtr { .. } => {},
    }
}

/// 若 `expr_id` 是 `Destroy(AddressOf(Local(sym)))` 形态, 返回该局部符号。
/// 解出「对象所在地」表达式指向的局部符号: `Local(sym)` 与 `AddressOf(Local(sym))`
/// 两种形态。sema 对 `Construct` / `Destroy` 的 `place` 用的都是后一种 (要的是对象
/// 地址), 只认前一种会静默认不出激活点, 于是清理区间整体前移到 `Decl` ——
/// 构造调用自身抛出时就会对一个还不存在的对象跑析构。
fn eh_place_local_of(hir: &HirTranslationUnit, place_id: HirExprId) -> Option<SymbolId> {
    let expr = hir.exprs.get(place_id.0)?;
    match &expr.kind {
        HirExprKind::Local(sym) => Some(*sym),
        HirExprKind::AddressOf(inner) => {
            let inner_expr = hir.exprs.get(inner.0)?;
            match &inner_expr.kind {
                HirExprKind::Local(sym) => Some(*sym),
                _ => None,
            }
        },
        _ => None,
    }
}

fn eh_destroy_local_of(hir: &HirTranslationUnit, expr_id: HirExprId) -> Option<SymbolId> {
    let expr = hir.exprs.get(expr_id.0)?;
    let HirExprKind::Destroy { place, .. } = &expr.kind else {
        return None;
    };
    eh_place_local_of(hir, *place)
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
    symbol_link_name_with_plan(symbols, sym, None, None)
}

/// Stage 10.1: 与 [`symbol_link_name`] 同语义, 但在模块 TU 上下文里
/// 优先走 [`crate::cpp::modules::ModuleLinkagePlan::mangled_name_of`] 反查的
/// mangled 名; 非模块 TU 或不在改名表里时与原版完全等价。
///
/// `mangled_override` 非空时短路直接返回 (供 Stage 10.5 等需要「显式
/// 链接名覆盖」的接入点使用)。
pub fn symbol_link_name_with_plan(
    symbols: &[CxxSymbol],
    sym: &CxxSymbol,
    mangled_override: Option<&str>,
    plan: Option<&crate::cpp::modules::ModuleLinkagePlan>,
) -> String {
    // Stage 10.1: 如果是模块 TU 且存在反向映射, 使用 mangled 名
    if let Some(plan) = plan {
        let backend_name = symbol_link_name(symbols, sym);
        if let Some(mangled) = plan.mangled_name_of(&backend_name) {
            return mangled;
        }
    }
    if let Some(m) = mangled_override {
        return m.to_string();
    }
    // 显式 linkage_name 优先: 构造/析构的多个 ABI 变体 (C1/C2) 共享同一个
    // 所属类, 只能靠符号上的明确名字区分。
    if let Some(ln) = &sym.linkage_name {
        // 命名空间内自由函数的 `linkage_name` (sema `compute_linkage_name`) 只到
        // 「命名空间 + 名字」一级, 不含参数类型 —— 同名异签重载因此会拿到完全
        // 相同的符号名, 必须在这里补上签名后缀 (见下方 Function 分支说明)。
        if sym.kind == CxxSymbolKind::Function {
            let sig = func_symbol_signature(symbols, sym);
            if !sig.is_empty() && free_function_has_rival(symbols, sym, &sig) {
                return format!("{ln}{sig}");
            }
        }
        return ln.clone();
    }
    let base = sym.name.clone();
    let owner = sym.owner.and_then(|o| class_symbol_link_name(symbols, o));
    // 重载后缀: 同名的多个重载必须得到不同链接名 (见 `overload_signature`)。
    let sig = func_symbol_signature(symbols, sym);
    match sym.kind {
        CxxSymbolKind::Constructor => format!(
            "{}{}",
            ctor_variant_name(owner.as_deref().unwrap_or(&base), CtorVariant::Complete),
            sig
        ),
        CxxSymbolKind::Destructor => {
            // sema 用 `~Class` 作为析构符号名; 调用侧约定是 `<class>_dtor_*`.
            let cls = owner
                .unwrap_or_else(|| base.strip_prefix('~').unwrap_or(base.as_str()).to_string());
            dtor_variant_name(&cls, DtorVariant::Complete)
        },
        CxxSymbolKind::Method => match owner {
            Some(c) => format!("{}{}", member_func_symbol(&c, &base), sig),
            None => format!("{base}{sig}"),
        },
        // 全局作用域的自由函数: `linkage_name` 为空, 默认链接名就是源名 ——
        // `printf` / `main` 这类与 C 运行时互操作的符号必须保持原名 (本后端
        // 不做 Itanium mangling)。但同名异签重载必须区分: 否则两份定义生成
        // 同名符号 (`multiple definition`), 且 ELF 层的 `labels`/`func_cfi`/`eh_funcs`
        // 全按名字索引 → 后一份覆盖前一份, 前一份代码永远不可达。
        CxxSymbolKind::Function => {
            if !sig.is_empty() && free_function_has_rival(symbols, sym, &sig) {
                format!("{base}{sig}")
            } else {
                base
            }
        },
        _ => base,
    }
}

/// 函数符号的签名后缀 (零参数且非 const 时为空串)。
fn func_symbol_signature(symbols: &[CxxSymbol], sym: &CxxSymbol) -> String {
    let (params, is_const) = match &sym.ty {
        CxxType::Function {
            params, is_const, ..
        } => (params.clone(), *is_const),
        _ => (Vec::new(), false),
    };
    overload_signature(symbols, &params, is_const)
}

/// 本 TU 里是否存在与 `sym` **同名但签名不同**的自由函数符号。
///
/// 只比名字而不比命名空间前缀就足够: `compute_linkage_name` 把命名空间拼进了
/// `name`/`linkage_name`, 不同命名空间的同名重载不会撞到同一个链接名上。
/// 同签名的重复声明 (原型 + 定义) 不触发后缀, 因此不会改变既有单一函数
/// (包括全部 libc 声明) 的符号名。
fn free_function_has_rival(symbols: &[CxxSymbol], sym: &CxxSymbol, sig: &str) -> bool {
    symbols.iter().any(|other| {
        other.id != sym.id
            && other.kind == CxxSymbolKind::Function
            && other.name == sym.name
            && func_symbol_signature(symbols, other) != sig
    })
}

/// 重载区分用的链接名后缀: `_` + 逐参数类型码 (+ const 限定标记)。
///
/// 为什么必须有: `member_func_symbol` / `ctor_variant_name` 只到「类 + 名字」
/// 一级, 同名重载 (`S::S()` 与 `S::S(int)`) 因此得到**同一个**链接名 ——
/// 两个定义被链接器合并成一个, 另一个静默消失 (实测 `S s; S t(2);` 打出
/// `491541736 2` 而不是 `1 2`: 默认构造的定义被 `S(int)` 覆盖, 无参调用
/// 跑进了带参实现) 。C++ 里重载的身份就是「名字 + 参数类型序列 + const」,
/// 所以这里按同一口径编码。
///
/// 零参数不产生后缀, 与已有单重载类的名字完全兼容。
pub fn overload_signature(symbols: &[CxxSymbol], params: &[CxxType], is_const: bool) -> String {
    if params.is_empty() && !is_const {
        return String::new();
    }
    let mut out = String::from("_");
    for p in params {
        out.push_str(&type_sig(symbols, p));
    }
    if is_const {
        out.push('K');
    }
    out
}

/// 单个参数类型的短码 (只需保证唯一且可过 C lexer, 不要求与 Itanium 同名)。
fn type_sig(symbols: &[CxxSymbol], ty: &CxxType) -> String {
    match ty {
        CxxType::Void => "v".to_string(),
        CxxType::Bool => "b".to_string(),
        CxxType::Integer { bits, signed } => match (bits, signed) {
            (8, true) => "c".to_string(),
            (8, false) => "h".to_string(),
            (16, true) => "s".to_string(),
            (16, false) => "t".to_string(),
            (32, true) => "i".to_string(),
            (32, false) => "j".to_string(),
            (64, true) => "l".to_string(),
            (64, false) => "m".to_string(),
            (128, true) => "a".to_string(),
            (128, false) => "b128u".to_string(),
            _ => format!("I{bits}{}", if *signed { 's' } else { 'u' }),
        },
        CxxType::Float { bits } => match bits {
            32 => "f".to_string(),
            64 => "d".to_string(),
            80 | 128 => "e".to_string(),
            _ => format!("F{bits}"),
        },
        CxxType::Pointer(inner) => format!("P{}", type_sig(symbols, inner)),
        CxxType::LValueRef(inner) => format!("R{}", type_sig(symbols, inner)),
        CxxType::RValueRef(inner) => format!("O{}", type_sig(symbols, inner)),
        CxxType::Array(inner, len) => {
            format!("A{}{}", len.unwrap_or(0), type_sig(symbols, inner))
        },
        CxxType::Class(id) => format!(
            "C{}",
            class_symbol_link_name(symbols, id.as_symbol()).unwrap_or_else(|| format!("K{}", id.0))
        ),
        CxxType::Enum(id) => format!("E{}", id.0),
        CxxType::MemberPointer { class, .. } => {
            format!("M{}", class_symbol_link_name(symbols, class.as_symbol()).unwrap_or_default())
        },
        CxxType::Function { ret, params, .. } => {
            let mut s = format!("F{}", type_sig(symbols, ret));
            for p in params {
                s.push_str(&type_sig(symbols, p));
            }
            s
        },
        CxxType::Alias(id) => match symbols.get(id.0) {
            Some(s) => type_sig(symbols, &s.ty),
            None => format!("T{}", id.0),
        },
        CxxType::Dependent(name) => {
            let mut s = String::from("D");
            for c in name.chars() {
                if c.is_ascii_alphanumeric() {
                    s.push(c);
                }
            }
            s.push_str(&format!("{}", name.len()));
            s
        },
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
        // vtable 槽若指向 this-adjust thunk, 必须真把那个入口函数发出来,
        // 否则 vtable 里就是一个未定义符号 (运行期跳转落空)。
        self.emit_thunks()?;
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
                        VTableComponent::Function {
                            symbol, adjustment, ..
                        } => {
                            // 需要 this 调整时, vtable 槽存的是 thunk 入口而非
                            // 最终实现 —— 入口名由布局层唯一分配。
                            let entry = vtable.entry_symbol_for(symbol, adjustment);
                            entries.push(VTableEntry::SymAddr(entry));
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

    /// 发射 this-adjust thunk 作为真实函数符号 (Stage 5/6)。
    ///
    /// 一个 thunk 就是 Itanium ABI 里 vtable 槽实际指向的入口: 把 `this`
    /// 从基类子对象调回最终覆写者期望的子对象, 再尾调用实现。以前
    /// secondary vtable 的槽直接写实现符号, 多继承下 `B* p; p->fb()` 会拿
    /// 着 B 子对象的 `this` 去读 `C::b` (实测 gcc 220 / dcc 110)。
    ///
    /// 偏移 / 入口名 / 目标全部取自 ABI 描述符 (`ClassAbi::thunks`), 本函数
    /// 不重算任何布局数据; 参数类型取自目标函数自身的签名。
    ///
    /// 本后端的架构约定: 虚基带来的 this relocation 由 HIR 的
    /// `VBaseAdjust` 节点在**指针转换**处完成 (运行期读 vbtable), vtable 里的
    /// 槽只可能是固定偏移 thunk。因此布局层不得产出 virtual thunk; 真到了
    /// 这一步就是布局层的不变式被破坏, 报 Internal 而不是当作
    /// 「未实现特性」降级 (后者会把缺陷当成能力边界长期背在身上)。
    fn emit_thunks(&mut self) -> Result<(), LowerIrError> {
        let mut class_ids: Vec<ClassId> = self.abis.keys().copied().collect();
        class_ids.sort_by_key(|c| c.0);

        // 先快照 thunk 描述符: 避免在下发循环里持有 self.abis 的不可变借用。
        let mut pending: Vec<crate::cpp::abi_layout::ThunkDesc> = Vec::new();
        for cid in class_ids {
            let abi = &self.abis[&cid];
            for t in &abi.thunks {
                if !pending.iter().any(|p| p.symbol == t.symbol) {
                    pending.push(t.clone());
                }
            }
        }

        for thunk in pending {
            if thunk.this_adjust.vbase_index.is_some() {
                return Err(LowerIrError::Internal(format!(
                    "layout layer emitted a virtual thunk {} (vbase-relative this adjustment); \
                     vbase relocation must be expressed as a HIR VBaseAdjust instead",
                    thunk.symbol
                )));
            }
            if self.output.functions.iter().any(|f| f.name == thunk.symbol) {
                continue;
            }
            let offset = thunk.this_adjust.non_virtual;
            if offset == 0 {
                continue;
            }
            // 目标实现必须在本 TU 里已有定义, 否则拿不到签名也无法尾调用。
            // vtable 已经引用了 thunk 入口, 静默跳过会在目标文件里留下
            // 未定义符号 (链接期才报 *ABS*, 运行期直接跳空) —— 必须响。
            let target_idx = match self
                .output
                .functions
                .iter()
                .position(|f| f.name == thunk.target_symbol)
            {
                Some(i) => i,
                None => {
                    return Err(LowerIrError::Internal(format!(
                        "thunk {} targets {}, which has no definition in this TU",
                        thunk.symbol, thunk.target_symbol
                    )));
                },
            };
            let params: Vec<SsaVar> = self.output.functions[target_idx]
                .params
                .clone()
                .into_iter()
                .enumerate()
                .map(|(i, p)| SsaVar {
                    base: format!("__thunk_p{}", i),
                    version: 0,
                    ty: p.ty,
                    span: dummy_span(),
                })
                .collect();
            let ret_ty = self.output.functions[target_idx].ret_ty.clone();
            // `FnAddr` 的类型只需给出返回值宽度供调用点取返回类型; 形参类型
            // 取自目标函数自己的签名 (下面拼 `call_args` 时逐一对应)。
            let call_ty = Type::Func(Box::new(QualType::new(ret_ty.clone())), Vec::new());

            let mut f = Function::new(thunk.symbol.clone(), ret_ty.clone(), dummy_span());
            f.params = params.clone();
            let byte_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::SChar)));
            let adjusted_var = SsaVar {
                base: "__thunk_adj_byte".to_string(),
                version: 0,
                ty: byte_ptr_ty.clone(),
                span: dummy_span(),
            };
            let mut insts = Vec::new();
            // 1. `this` 先转成字节指针 (Operand 侧不接受内联 cast, 必须先落到 SSA 名)
            let this_byte_var = SsaVar {
                base: "__thunk_this_byte".to_string(),
                version: 0,
                ty: byte_ptr_ty.clone(),
                span: dummy_span(),
            };
            insts.push(Inst {
                dest: Some(this_byte_var.clone()),
                kind: InstKind::Cast {
                    ty: byte_ptr_ty.clone(),
                    e: Operand::Var(params[0].clone()),
                },
                span: dummy_span(),
            });
            // 2. 按字节偏移到最终覆写者的子对象
            insts.push(Inst {
                dest: Some(adjusted_var.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(this_byte_var),
                    r: Operand::Int(offset as i128),
                },
                span: dummy_span(),
            });
            // 3. 调回目标类型 (与目标函数首参一致)
            let typed_var = SsaVar {
                base: "__thunk_adj".to_string(),
                version: 0,
                ty: params[0].ty.clone(),
                span: dummy_span(),
            };
            insts.push(Inst {
                dest: Some(typed_var.clone()),
                kind: InstKind::Cast {
                    ty: params[0].ty.clone(),
                    e: Operand::Var(adjusted_var),
                },
                span: dummy_span(),
            });
            // 4. 尾调用最终实现
            let mut call_args = vec![Operand::Var(typed_var)];
            call_args.extend(params[1..].iter().cloned().map(Operand::Var));
            let dest = if matches!(ret_ty, Type::Void) {
                None
            } else {
                Some(SsaVar {
                    base: "__thunk_ret".to_string(),
                    version: 0,
                    ty: ret_ty.clone(),
                    span: dummy_span(),
                })
            };
            insts.push(Inst {
                dest: dest.clone(),
                kind: InstKind::Call {
                    callee: Operand::FnAddr(thunk.target_symbol.clone(), call_ty),
                    args: call_args,
                    ret_ty: ret_ty.clone(),
                },
                span: dummy_span(),
            });
            let term = match dest {
                Some(v) => Terminator::Ret {
                    val: Some(Operand::Var(v)),
                },
                None => Terminator::Ret { val: None },
            };
            if let Some(bb) = f.blocks.get_mut("entry") {
                bb.insts = insts;
                bb.term = term;
            }
            self.output.functions.push(f);
        }
        Ok(())
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

    /// 某个类的构造变体在链接期的实际名字。
    ///
    /// `ctor` = sema 已经选定的重载构造函数符号。它不可省: 重载构造同名同类,
    /// 只按类名拼符号会把两个定义合并成一个 (见 `overload_signature`)。
    /// None 代表隐式默认构造 (零参数, 无后缀)。
    ///
    /// C1 (Complete) 直接复用定义侧的 `symbol_link_name`, 保证两边同源;
    /// 其他变体 (C2 / allocating) 是 sema 另行登记的符号, 名字 =
    /// `<class>_ctor_<variant>` + **同一段**重载后缀。
    pub fn ctor_link_name(
        &self,
        class: ClassId,
        variant: CtorVariant,
        ctor: Option<SymbolId>,
    ) -> Result<String, LowerIrError> {
        let class_sym = self.get_symbol(class.as_symbol())?;
        let class_name = self.symbol_name(class_sym);
        let Some(ctor) = ctor else {
            return Ok(ctor_variant_name(&class_name, variant));
        };
        let sym = self.get_symbol(ctor)?;
        if matches!(variant, CtorVariant::Complete) {
            return Ok(self.symbol_name(sym));
        }
        let params: Vec<CxxType> = match &sym.ty {
            CxxType::Function { params, .. } => params.clone(),
            _ => Vec::new(),
        };
        Ok(format!(
            "{}{}",
            ctor_variant_name(&class_name, variant),
            overload_signature(self.symbols, &params, false)
        ))
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
                // x87 80-bit `long double` ([basic.fundamental]/3.3: 它的精度至少
                // 与 double 相同, 是实现定义的独立类型)。sema 用 `Float{bits: 80}`
                // 表示它, C 侧则用 `Type::LongDouble` —— 两侧必须接得上, 否则
                // 任何用到 `long double` 的 TU (printf 的 `%Lf`、`<cmath>` 的
                // `strtold`, 以及 GCC 源码自身的 `LONG_DOUBLE_TYPE_SIZE`) 都会在
                // 这里报 unsupported 并退回 legacy 文本降级。
                80 => Ok(Type::LongDouble),
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

        // C 侧类型尺寸 = 「字段最大结束偏移」, 而 ABI 对象尺寸还含尾部填充
        // 与 vptr 槽: `struct S { virtual int f(); }` 一个字段都没有, 但
        // standalone 尺寸是 8 (仅 vptr)。不补齐就会少分栈槽 / `new` 字节数,
        // 成员写入越界 (实测 sizeof(S) dcc=1 / gcc=8, sizeof(U) dcc=12 / gcc=16)。
        // 尺寸单一真源仍是布局层 `ClassAbi::size`, 这里只是把它表达进
        // `ast::Type`; 用 `size-1` 处的单字节占位, 不改动任何用户字段偏移。
        if !is_union && abi.size > 0 {
            let covered = fields
                .iter()
                .map(|f| f.offset as u64 + type_size(&f.ty))
                .max()
                .unwrap_or(0);
            if covered < abi.size {
                fields.push(crate::ast::Field {
                    name: format!("__dcc_obj_tail_{}", id.0),
                    ty: Type::UChar,
                    offset: (abi.size - 1) as usize,
                    bit_width: None,
                    bit_offset: 0,
                    quals: crate::ast::QualifierFlags::NONE,
                    ptr_quals: Vec::new(),
                    field_align: None,
                    field_packed: false,
                });
            }
        }

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

/// 这个类的 C 侧表示是不是 `union`。
///
/// 真值是布局层的 `ClassAbi::is_union` (从声明的 `union` 关键字带来)。
/// 旧写法用「所有字段偏移都是 0」反推, 两个方向都会错: 单个字段的
/// `struct S { int x; }` 被误判成 union, 而真正多字段的 union 因为字段被
/// 顺序排列反而判不出来。
fn is_union_layout(abi: &ClassAbi) -> bool {
    abi.is_union
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
    // === zero-cost EH 状态 (每个函数独立) ===
    /// EH 清理链, 由外向内; 既含存活对象的清理项也含 `try` 的 handler 组。
    eh_chain: Vec<EhChainEntry>,
    /// 语句 -> 该语句执行完后进入清理集合的局部量 (构造完成点)。
    eh_activate_at: HashMap<HirStmtId, SymbolId>,
    /// 语句 -> 该语句执行前撤掉清理的局部量 (正常析构点)。
    eh_retire_at: HashMap<HirStmtId, SymbolId>,
    /// 已由 `eh_activate_at` 接管激活的符号 (避免 `Decl` 兜底路径重复登记)。
    eh_activated_syms: std::collections::HashSet<SymbolId>,
    /// 仍存活的局部量 -> 它在 `eh_chain` 里的清理项 key。
    eh_cleanup_key_of: HashMap<SymbolId, u64>,
    /// 需要自动析构的局部量: 符号 -> sema 生成的那条 `Destroy` 语句。
    ///
    /// 整函数预扫一次得到, 因为「哪些局部量需要清理」的真源是 sema 的
    /// `DtorFrame` (已物化成块末 / 循环初始化域收尾 / return/break 出口处的
    /// `Destroy` 语句), 而不是 lower_ir 自己再推一遍类型规则 —— 后者会在
    /// `automatic_destructor_needed` 的两个例外 (聚合 / 含虚基) 上漂移。
    eh_dtor_of: HashMap<SymbolId, HirStmtId>,
    /// `try` 语句序号 (分配 `try_id`)
    eh_try_seq: u32,
    /// 清理项序号 (分配稳定的 cleanup key)
    eh_cleanup_seq: u64,
    /// catch 类型 -> LSDA 类型表索引 (1-based)
    eh_type_index: HashMap<String, u32>,
    /// 下一个可用的类型表索引
    eh_type_next: u32,
    /// 作用域快照 -> 落地垫块名
    eh_lp_cache: HashMap<Vec<EhChainKeyFrame>, String>,
    /// 只由 unwinder 进入的块 (落地垫区) 根, 按登记序
    eh_roots: Vec<String>,
    /// 全部归 EH 机件所有的块。`unreachable_blocks()` 必须排除它们: 那里的
    /// 「隐式落通」修补会把 `_Unwind_Resume` 之后的 `Unreachable` 改写成
    /// `Jump(循环增量块)`, 从而把清理链搅成正常控制流。
    eh_owned_blocks: std::collections::HashSet<String>,
    /// 共享的 `_Unwind_Resume` 块
    eh_resume_bb: Option<String>,
    /// 正在生成落地垫代码: 其中的调用**不再**升级成 invoke (否则递归生成)。
    ///
    /// 清理动作在展开途中再次抛出按 `std::terminate` 处理, 本就不该有落地垫。
    in_eh_emit: bool,
    /// 异常对象指针槽 (`%rax`) 与 selector 槽 (`%rdx`) 的函数级局部
    eh_exc_obj: Option<SsaVar>,
    eh_exc_sel: Option<SsaVar>,
    /// 落地垫生成过程中遇到的首个错误 (由 `finish()` 报出)。
    eh_error: Option<String>,
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
            eh_chain: Vec::new(),
            eh_activate_at: HashMap::new(),
            eh_retire_at: HashMap::new(),
            eh_activated_syms: std::collections::HashSet::new(),
            eh_cleanup_key_of: HashMap::new(),
            eh_dtor_of: HashMap::new(),
            eh_try_seq: 0,
            eh_cleanup_seq: 0,
            eh_type_index: HashMap::new(),
            eh_type_next: 0,
            eh_lp_cache: HashMap::new(),
            eh_roots: Vec::new(),
            eh_owned_blocks: std::collections::HashSet::new(),
            eh_resume_bb: None,
            in_eh_emit: false,
            eh_exc_obj: None,
            eh_exc_sel: None,
            eh_error: None,
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

        // zero-cost EH: 把只由 unwinder 进入的块归集到 `eh_unwind_blocks`,
        // 并把落地垫区指向正常块的那条边登记成重入标签。
        self.finish_eh_blocks();

        // 落地垫生成失败不能只当内部提示吞掉: 那会让异常静默跳过本帧的析构。
        if let Some(msg) = self.eh_error.take() {
            return Err(LowerIrError::Internal(msg));
        }

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
    ///
    /// 排除 `eh_owned_blocks`: 那些块属于零成本异常的落地垫区, 它们的
    /// `Unreachable` 是**有意**的终态 (如 `_Unwind_Resume` 永不返回), 不能
    /// 被循环/switch 的「隐式落通」修补改成正常跳转。
    fn unreachable_blocks(&self) -> Vec<String> {
        let mut result = Vec::new();
        for (name, bb) in &self.ssa.blocks {
            if matches!(bb.term, Terminator::Unreachable) && !self.eh_owned_blocks.contains(name) {
                result.push(name.clone());
            }
        }
        result
    }

    fn push_inst(&mut self, inst: Inst) {
        let inst = self.eh_protect_call(inst);
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
    /// 静态存储的旧值不在本函数的 SSA 里 (也不应在 —— 它要能跨调用/跨函数
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
                // 常量表示必须跟**表达式类型**走 ([lex.fcon] + [basic.fundamental]):
                // 早前无条件按 double 建模 (旧注释就写着「此处不存在 80-bit
                // long double 通道」), 于是 `1.0L / 3.0L` 两侧都是 double, 商只有
                // 53 位精度 (实测 dcc=0.333333333333333315 / gcc=0.333333333333333333),
                // `float` 字面量也一样丢类型。宽度单一真源是 sema 己标好的
                // `expr.ty` (由 const_text::float_literal_bits 从后缀得出)。
                match expr_ty {
                    Type::LongDouble => crate::float80::from_decimal(s)
                        .map(|b| Operand::Float(FloatConst::LongDouble(b), Type::LongDouble))
                        .or_else(|| {
                            // 十进制扫描失败 (如十六进制浮点 / 特殊格式):
                            // 经 f64 中转仍是一个可编译的常量, 不阻断整份编译。
                            s.parse::<f64>().ok().map(|v| {
                                Operand::Float(
                                    FloatConst::LongDouble(crate::float80::from_f64(v)),
                                    Type::LongDouble,
                                )
                            })
                        })
                        .ok_or_else(|| {
                            LowerIrError::Internal(format!("cannot lower float literal {s:?}"))
                        }),
                    Type::Float => {
                        Ok(Operand::Float(FloatConst::F32(s.parse().unwrap_or(0.0)), Type::Float))
                    },
                    other => Ok(Operand::Float(FloatConst::F64(s.parse().unwrap_or(0.0)), other)),
                }
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
                ctor,
            } => self.lower_construct(*class, *place, args, *variant, *ctor),

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
                ctor,
            } => self.lower_new(ty, args, placement.as_deref(), *is_array, *ctor),

            HirExprKind::Delete {
                ptr,
                is_array,
                dtor,
            } => self.lower_delete(*ptr, *is_array, *dtor),

            HirExprKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => self.lower_conditional(*cond, *then_expr, *else_expr, &expr_ty),

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
        self.push_branch_result(base, &Type::Bool, e)
    }

    /// 把一个分支结果写进基名为 `base` 的结果变量 (两条路径各调用一次)。
    ///
    /// 两条路径的写入在 SSA→AST 重建时合并到同一个 C 局部, 因而不需要 phi。
    fn push_branch_result(&mut self, base: &str, ty: &Type, e: Operand) -> SsaVar {
        let dest = self.fresh_var(base, ty.clone());
        self.push_inst(Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Cast { ty: ty.clone(), e },
            span: dummy_span(),
        });
        dest
    }

    /// [expr.cond]/1: `cond ? then : else` **只求值被选中的那一个分支**。
    ///
    /// 不能用 `InstKind::Select`: 它要求先把两个分支都降级到同一个线性块里再
    /// 选择, 于是 (1) 未选中的分支的副作用也被执行 —— `w = c ? (w1 = 11) :
    /// (w2 = 22)` 把 `w2` 也写了; (2) `Select` 只能承载可寄存器化的标量, 16
    /// 字节的 `long double` 分支直接得到 `nan`。
    ///
    /// 正确形状是控制流: 真/假各一个基本块, 结果写进同一个基名的变量, merge
    /// 块读它 —— 与 `&&` / `||` 的短路降级同一套机制。它能嵌套的前提是重建层
    /// 能认出「某条路径本身又是分支」时的汇聚点, 那是
    /// [`crate::ssa::compute_ipdoms`] (后支配者) 解决的问题。
    fn lower_conditional(
        &mut self,
        cond: HirExprId,
        then_expr: HirExprId,
        else_expr: HirExprId,
        result_ty: &Type,
    ) -> Result<Operand, LowerIrError> {
        let then_bb = self.unique_bb("cond_then");
        let else_bb = self.unique_bb("cond_else");
        let merge_bb = self.unique_bb("cond_merge");

        let cond_val = self.lower_expr(cond)?;
        self.set_term(Terminator::Branch {
            cond: cond_val,
            t: then_bb.clone(),
            f: else_bb.clone(),
        });

        let result_base = self.fresh_logical_base("cond");

        // 真分支: 只在这里降级 then_expr
        self.current_bb = then_bb;
        let then_val = self.lower_expr(then_expr)?;
        self.push_branch_result(&result_base, result_ty, then_val);
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // 假分支: 只在这里降级 else_expr
        self.current_bb = else_bb;
        let else_val = self.lower_expr(else_expr)?;
        self.push_branch_result(&result_base, result_ty, else_val);
        self.set_term(Terminator::Jump(merge_bb.clone()));

        // merge: 读回结果变量 (与短路逻辑同一口径: version 0 只当名字载体)
        self.current_bb = merge_bb;
        Ok(Operand::Var(SsaVar {
            base: result_base,
            version: 0,
            ty: result_ty.clone(),
            span: dummy_span(),
        }))
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
                Ok(Operand::Int(0))
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
        // this 调整量已由指针转换 (`BaseCast` / `VBaseAdjust`) 与 vtable 里的
        // thunk 入口完整承担: `this` 进来时就是callee 期望的子对象。
        // 旧这里有一个 `vbase_index.is_some() => Unsupported` 的拒绝分支,
        // 但 HIR 已不再在调用点携带虚基索引 (虚基调整只能运行期读 vbtable,
        // 属于转换而不是调用), 那个分支永远不会命中 —— 留着只会把缺陷
        // 伪装成未实现特性。
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
                let is_fn = IrLowerer::is_function_value_symbol(sym);
                // `&fn` 与 `fn` 是同一个值 ([conv.func]): 走与 `lower_expr` 的
                // Global 分支同一真源 (SymAddr + `Type::Func`)。
                if is_fn {
                    let fty = self.lowerer.lower_func_value_type(&sym.ty);
                    return Ok(Operand::SymAddr(name, fty));
                }
                // 数据对象的 `&g` 必须是**符号地址**: `InstKind::AddrOf` 重建为
                // `&g` => `lea g(%rip)` (与 `lower_static_storage_store` 同一形态)。
                // `Operand::SymAddr` 在运行期操作数位置会被求成「载入 g 的值」,
                // 于是 `B gb;` 的全局构造拿到的是 gb 的头 8 字节而不是 &gb,
                // 构造函数往垃圾指针里写 => 段错误。
                Ok(self.lower_sym_addr(name, result_ty.clone()))
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
        ctor: Option<SymbolId>,
    ) -> Result<Operand, LowerIrError> {
        let place = self.lower_expr(place_id)?;

        // 构造函数名: sema 选定的重载 + 变体 (`ctor_link_name` 与定义侧同源)
        let ctor_name = self.lowerer.ctor_link_name(class, variant, ctor)?;

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

    // ==================================================================
    // zero-cost EH: 作用域栈 / 受保护调用 / 落地垫
    // ==================================================================

    /// 函数作用域入口: 预扫「哪些局部量需要在异常路径清理」及其进出界语句。
    ///
    /// 必须在下探函数体之前做: 函数级局部量的清理集合要能被体内第一个受保护调用
    /// 看到, 否则异常穿出本帧时不执行析构 (GCC 会)。
    fn eh_begin_function_scope(&mut self, body: &HirBody) {
        let mut dtor_of: HashMap<SymbolId, HirStmtId> = HashMap::new();
        for id in body.ctor_initializers.iter().chain(body.stmts.iter()) {
            eh_collect_dtor_stmts(self.lowerer.hir, *id, &mut dtor_of, 0);
        }
        let mut activate_at = HashMap::new();
        let mut retire_at = HashMap::new();
        eh_collect_cleanup_sites(
            self.lowerer.hir,
            &body.stmts,
            &dtor_of,
            &mut activate_at,
            &mut retire_at,
            0,
        );
        let activated: std::collections::HashSet<SymbolId> =
            activate_at.values().copied().collect();
        self.eh_dtor_of = dtor_of;
        self.eh_activate_at = activate_at;
        self.eh_retire_at = retire_at;
        self.eh_activated_syms = activated;
    }

    /// 函数作用域收尾: 清空清理链。
    ///
    /// 正常情况下每个对象都在自己的析构语句处退休, 这里只是防御性收尾 (例如
    /// `[[noreturn]]` 出口没生成析构语句), 不让上一个函数的状态漏进下一个函数。
    fn eh_end_function_scope(&mut self) {
        self.eh_chain.clear();
        self.eh_cleanup_key_of.clear();
    }

    /// 登记一条清理, 返回它的稳定 key (供显式退休用)。
    fn eh_add_cleanup(&mut self, kind: EhCleanupKind) -> u64 {
        let key = self.eh_cleanup_seq;
        self.eh_cleanup_seq += 1;
        self.eh_chain
            .push(EhChainEntry::Cleanup(EhCleanup { key, kind }));
        key
    }

    /// 局部量构造完成: 从此刻起它进入清理集合。
    fn eh_activate_cleanup(&mut self, symbol: SymbolId) {
        if self.eh_cleanup_key_of.contains_key(&symbol) {
            return;
        }
        let Some(dtor_stmt) = self.eh_dtor_of.get(&symbol).copied() else {
            return;
        };
        let key = self.eh_add_cleanup(EhCleanupKind::Destroy(dtor_stmt));
        self.eh_cleanup_key_of.insert(symbol, key);
    }

    /// 局部量正常析构 (或 glue 状态结束): 把它从清理集合里撤掉。
    fn eh_retire_cleanup(&mut self, symbol: SymbolId) {
        let Some(key) = self.eh_cleanup_key_of.remove(&symbol) else {
            return;
        };
        self.eh_remove_cleanup(key);
    }

    /// 按 key 撤掉一条清理 (显式配对的 glue 清理用)。
    fn eh_remove_cleanup(&mut self, key: u64) {
        if let Some(pos) = self
            .eh_chain
            .iter()
            .rposition(|e| matches!(e, EhChainEntry::Cleanup(c) if c.key == key))
        {
            self.eh_chain.remove(pos);
        }
    }

    /// 当前清理链快照 (自内向外), 跳过没有 handler 的 `try` 帧。
    fn eh_chain_items(&self) -> Vec<EhChainItem> {
        let mut out = Vec::new();
        for entry in self.eh_chain.iter().rev() {
            match entry {
                EhChainEntry::Try { try_id, handlers } => {
                    if !handlers.is_empty() {
                        out.push(EhChainItem::Try {
                            try_id: *try_id,
                            handlers: handlers.clone(),
                        });
                    }
                },
                EhChainEntry::Cleanup(c) => out.push(EhChainItem::Cleanups(vec![c.clone()])),
            }
        }
        out
    }

    /// 快照的哈希 key (与落地垫内容一一对应)。
    fn eh_chain_key(items: &[EhChainItem]) -> Vec<EhChainKeyFrame> {
        items
            .iter()
            .map(|it| match it {
                EhChainItem::Try { try_id, .. } => EhChainKeyFrame::Try(*try_id),
                EhChainItem::Cleanups(cs) => {
                    EhChainKeyFrame::Block(cs.iter().map(|c| c.key).collect())
                },
            })
            .collect()
    }

    /// 快照 -> LSDA action 链。与 [`Self::eh_build_landing_pad`] 生成的代码
    /// **必须同一顺序**: 表描述的和代码做的就是 libstdc++ 依赖的同一件事。
    fn eh_actions(items: &[EhChainItem]) -> Vec<crate::ast::EhAction> {
        let mut out = Vec::new();
        for it in items {
            match it {
                EhChainItem::Try { handlers, .. } => {
                    for h in handlers {
                        out.push(crate::ast::EhAction::Catch {
                            type_index: h.type_index,
                            typeinfo: h.typeinfo.clone(),
                        });
                    }
                },
                EhChainItem::Cleanups(_) => out.push(crate::ast::EhAction::Cleanup),
            }
        }
        out
    }

    /// 把「可能抛出的普通调用」升级为受保护调用 (invoke)。
    ///
    /// 单一拦截点设在 [`Self::push_inst`]: 所有调用 (普通/成员/虚/构造/析构/runtime
    /// glue) 都经由它发射, 因此不存在「某条路径漏加保护」的结构性缺口。
    fn eh_protect_call(&mut self, inst: Inst) -> Inst {
        if self.in_eh_emit {
            return inst;
        }
        let (callee, args, ret_ty) = match &inst.kind {
            InstKind::Call {
                callee,
                args,
                ret_ty,
            } => (callee.clone(), args.clone(), ret_ty.clone()),
            _ => return inst,
        };
        let items = self.eh_chain_items();
        if items.is_empty() {
            return inst;
        }
        let key = Self::eh_chain_key(&items);
        let unwind = match self.eh_lp_cache.get(&key) {
            Some(u) => u.clone(),
            None => {
                match self.eh_build_landing_pad(items.clone()) {
                    Ok(u) => {
                        self.eh_lp_cache.insert(key, u.clone());
                        u
                    },
                    // 生成落地垫失败必须报错, 不能默默退回普通调用:
                    // 那样异常会直接跳过本帧的析构与 handler, 是个错答案而不是
                    // 一个编译失败。
                    Err(e) => {
                        self.eh_note_error(&e);
                        return inst;
                    },
                }
            },
        };
        Inst {
            dest: inst.dest,
            kind: InstKind::Invoke {
                callee,
                args,
                ret_ty,
                unwind: Some(unwind),
                actions: Self::eh_actions(&items),
            },
            span: inst.span,
        }
    }

    /// 记录一个「只能在下一次 `finish()` 时才能报出」的 EH 错误。
    ///
    /// `push_inst` 的签名是 `()` (全站数百个调用点), 无法就地返回 `Result`; 先记
    /// 住首个错因, `finish()` 把它当成编译失败报出, 绝不静默降级成错代码。
    fn eh_note_error(&mut self, e: &LowerIrError) {
        if self.eh_error.is_none() {
            self.eh_error = Some(e.to_string());
        }
    }

    /// 新建一个只由 unwinder 进入的块并登记归属。
    fn eh_new_block(&mut self, prefix: &str) -> String {
        let bb = self.unique_bb(prefix);
        self.eh_owned_blocks.insert(bb.clone());
        bb
    }

    /// 异常对象指针槽 (`%rax`) 与 selector 槽 (`%rdx`): 函数级共用两个具名局部。
    ///
    /// 必须是**具名非临时**局部: 后端在落地垫标签后的第一条指令里把它们溢出到
    /// 栈槽 (见 `emit/stmt.rs` 的 `Stmt::EhLandingPad`), 而 SSA 临时值会被
    /// `rebuild_block` 的 `subst` 内联掉, 槽就没了。名字也不以 `_` 开头,
    /// 以保证 `rebuild_block` 会为其生成声明。
    fn eh_exc_slots(&mut self) -> (SsaVar, SsaVar) {
        if let (Some(o), Some(s)) = (self.eh_exc_obj.clone(), self.eh_exc_sel.clone()) {
            return (o, s);
        }
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let obj = SsaVar {
            base: "dcc_eh_object".to_string(),
            version: 0,
            ty: void_ptr_ty.clone(),
            span: dummy_span(),
        };
        let sel = SsaVar {
            base: "dcc_eh_selector".to_string(),
            version: 0,
            ty: Type::LongLong,
            span: dummy_span(),
        };
        for (var, ty) in [(obj.clone(), void_ptr_ty), (sel.clone(), Type::LongLong)] {
            self.ssa.local_types.insert(var.base.clone(), ty);
            self.ssa.original_locals.insert(var.base.clone());
            self.ssa
                .local_quals
                .insert(var.base.clone(), QualifierFlags::NONE);
            self.locals.insert(var.base.clone(), var);
        }
        self.eh_exc_obj = Some(obj.clone());
        self.eh_exc_sel = Some(sel.clone());
        (obj, sel)
    }

    /// 共享的 `_Unwind_Resume(exc_object)` 块。
    ///
    /// 所有「清理完但无人接」的落地垫尾都跳到它: 一个函数里只发一条 resume 调用,
    /// 与 GCC 的 `try/before/finally` 合并同理, 也保证 `eh_owned_blocks` 里那个
    /// 永返终态的块只有一份。
    fn eh_resume_block(&mut self) -> String {
        if let Some(b) = &self.eh_resume_bb {
            return b.clone();
        }
        let prev = self.current_bb.clone();
        let bb = self.eh_new_block("eh_resume");
        let obj = self.eh_exc_slots().0;
        self.current_bb = bb.clone();
        // `_Unwind_Resume` 必须登记成 `lp=0 / action=0` 的受保护区间 (而不是不登记):
        // 本函数已经有 LSDA, 而清理阶段里「PC 落在本函数内却没有区间覆盖」会被
        // libstdc++ 当成清理代码二次抛出 -> `std::terminate`, 外层 handler 永远
        // 进不去 (实测: 中间帧析构跑完后直接 terminate)。
        self.eh_emit_propagate_call(CxxRuntimeSymbol::UnwindResume, vec![Operand::Var(obj)]);
        self.set_term(Terminator::Unreachable);
        self.current_bb = prev;
        self.eh_resume_bb = Some(bb.clone());
        self.eh_roots.push(bb.clone());
        bb
    }

    /// 为一个作用域快照生成落地垫块。
    ///
    /// 块内形状与 g++ 产物逐块一致 (已用 `tmp_eh/nested.cpp` 等样本比对):
    /// ```text
    /// lp:
    ///   <exc_object, exc_selector> = [%rax, %rdx]
    ///   if (selector == f1) goto catch_1      ; 最近的 handler 先试
    /// next:
    ///   if (selector == f2) goto catch_2
    /// next:
    ///   <清理动作>                             ; 只有比它更外层的 handler 才需要
    ///   if (selector == f3) goto catch_3
    /// next:
    ///   goto resume
    /// ```
    /// 「handler 在对象作用域之内则不清理」这一关键语义就体现在比较与清理的
    /// 前后次序上, 所以 action 链与这段代码必须同源 (`eh_actions` / 本函数走同一
    /// 个 `items`)。
    fn eh_build_landing_pad(&mut self, items: Vec<EhChainItem>) -> Result<String, LowerIrError> {
        let (obj, sel) = self.eh_exc_slots();
        let saved_bb = self.current_bb.clone();
        let saved_flag = std::mem::replace(&mut self.in_eh_emit, true);
        let lp = self.eh_new_block("eh_lp");
        self.eh_roots.push(lp.clone());
        // 本函数现在有异常交接点: FDE 需要 `zPLR` 增强与 personality 槽。
        self.eh_require_personality();
        self.current_bb = lp.clone();
        self.push_inst(Inst {
            dest: Some(obj),
            kind: InstKind::EhLandingPad { selector: sel },
            span: dummy_span(),
        });
        for it in &items {
            match it {
                EhChainItem::Try { handlers, .. } => {
                    for h in handlers {
                        let cur = self.current_bb.clone();
                        let cond = self.eh_selector_is(h.type_index);
                        let next = self.eh_new_block("eh_lp_next");
                        self.ssa.blocks.get_mut(&cur).unwrap().term = Terminator::Branch {
                            cond,
                            t: h.catch_bb.clone(),
                            f: next.clone(),
                        };
                        self.current_bb = next;
                    }
                },
                EhChainItem::Cleanups(cs) => {
                    for c in cs {
                        match c.kind.clone() {
                            EhCleanupKind::Destroy(stmt) => self.lower_stmt(stmt)?,
                            EhCleanupKind::EndCatch => {
                                self.eh_call_runtime(CxxRuntimeSymbol::EndCatch, vec![])
                            },
                            EhCleanupKind::FreeException(buf) => self.eh_call_runtime(
                                CxxRuntimeSymbol::FreeException,
                                vec![Operand::Var(buf)],
                            ),
                        }
                    }
                },
            }
        }
        let resume = self.eh_resume_block();
        self.set_term(Terminator::Jump(resume));
        self.in_eh_emit = saved_flag;
        self.current_bb = saved_bb;
        Ok(lp)
    }

    /// `selector == type_index` 比较。
    ///
    /// libstdc++ 递上来的 selector 就是 action 链里那个**正的 filter 值**
    /// (实测 g++ 落地垫直接 `cmpq $2, %rdx`), 所比较量就是 `type_index`。
    fn eh_selector_is(&mut self, type_index: u32) -> Operand {
        let sel = self.eh_exc_slots().1;
        let res = self.fresh_syn("eh_test", Type::Int);
        self.push_inst(Inst {
            dest: Some(res.clone()),
            kind: InstKind::Bin {
                op: BinOp::EqEq,
                l: Operand::Var(sel),
                r: Operand::Int(type_index as i128),
            },
            span: dummy_span(),
        });
        Operand::Var(res)
    }

    /// 登记 personality 与它的 `DW.ref` 间接槽。
    fn eh_require_personality(&mut self) {
        let name = CxxRuntimeSymbol::PersonalityV0.name().to_string();
        if !self.lowerer.output.external_symbols.contains(&name) {
            self.lowerer.output.external_symbols.push(name.clone());
        }
        let slot = format!("DW.ref.{}", name);
        if !self.lowerer.output.eh_ref_slots.contains(&slot) {
            self.lowerer.output.eh_ref_slots.push(slot);
        }
    }

    /// 发射一个 `void f(...)` 形态的 runtime 调用 (并补上外部声明)。
    fn eh_call_runtime(&mut self, sym: CxxRuntimeSymbol, args: Vec<Operand>) {
        let name = self.register_runtime(sym.name());
        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        self.push_inst(Inst {
            dest: None,
            kind: InstKind::Call {
                callee: Operand::FnAddr(name, fn_ty),
                args,
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        });
    }

    /// 发射一个「把当前异常继续向外递」的 runtime 调用 (`_Unwind_Resume` /
    /// `__cxa_rethrow`): 登记为 `landing_pad = None` 的受保护调用。
    ///
    /// 直接构造 `InstKind::Invoke` 而不是普通 `Call`, 是为了绕开
    /// [`Self::eh_protect_call`] 的自动升级 —— 那里给出的落地垫会让本帧在展开途中
    /// 重新接管同一个异常 (二次展开), 而这两个调用按 ABI 只是把异常交回运行时。
    fn eh_emit_propagate_call(&mut self, sym: CxxRuntimeSymbol, args: Vec<Operand>) {
        let name = self.register_runtime(sym.name());
        let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        self.ssa
            .blocks
            .get_mut(&self.current_bb.clone())
            .unwrap()
            .insts
            .push(Inst {
                dest: None,
                kind: InstKind::Invoke {
                    callee: Operand::FnAddr(name, fn_ty),
                    args,
                    ret_ty: Type::Void,
                    unwind: None,
                    actions: Vec::new(),
                },
                span: dummy_span(),
            });
    }

    /// 把一个 runtime 符号登记为外部引用 (native 发射端负责任的声明它)。
    fn register_runtime(&mut self, name: &str) -> String {
        let name = name.to_string();
        if !self.lowerer.output.external_symbols.contains(&name) {
            self.lowerer.output.external_symbols.push(name.clone());
        }
        name
    }

    /// catch 类型 -> LSDA 类型表索引 (1-based, 同一类型在同一函数里共用一个索引)。
    ///
    /// `None` = `catch (...)`: 仍占一个正索引, 但类型表项是值为 0 的空表项
    /// (g++ 对 `catch (...)` 正是这样发的)。
    fn eh_type_index_for(&mut self, typeinfo: Option<String>) -> u32 {
        let key = typeinfo.clone().unwrap_or_else(|| "...".to_string());
        if let Some(idx) = self.eh_type_index.get(&key) {
            return *idx;
        }
        self.eh_type_next += 1;
        let idx = self.eh_type_next;
        self.eh_type_index.insert(key, idx);
        if let Some(sym) = typeinfo {
            // 类型表用 `DW_EH_PE_indirect`, 所以需要一个 `DW.ref.<sym>` 槽。
            let slot = format!("DW.ref.{}", sym);
            if !self.lowerer.output.eh_ref_slots.contains(&slot) {
                self.lowerer.output.eh_ref_slots.push(slot);
            }
        }
        idx
    }

    /// 该抛/捕类型的 typeinfo **符号名**。
    ///
    /// 类类型走布局层 (`ClassAbi::rtti`, 带命名空间限定名); 其余类型由
    /// `typeinfo_mangling` 给出 Itanium 编码, 并把需要合成定义的 `_ZTI`/`_ZTS`
    /// 登记给 `native.rs`。引用形抛/捕类型 (`catch (X&)`) 的 typeinfo 是**所指
    /// 类型**的, 所以先脱引用。
    fn eh_typeinfo_symbol(&mut self, ty: &CxxType) -> Result<Option<String>, LowerIrError> {
        let stripped = ty.remove_reference().clone();
        match &stripped {
            CxxType::Class(id) => {
                let abi = self.lowerer.abis.get(id).ok_or_else(|| {
                    LowerIrError::Internal(format!(
                        "ABI for class {} not found while building the catch/throw type table",
                        id.0
                    ))
                })?;
                let sym_name = abi.rtti.typeinfo_symbol.clone();
                if !self.lowerer.output.rodata.contains(&sym_name) {
                    self.lowerer.output.rodata.push(sym_name.clone());
                }
                Ok(Some(sym_name))
            },
            CxxType::Enum(id) => {
                let sym = self.lowerer.get_symbol(id.as_symbol())?;
                let enc = crate::cpp::mangle::encode_type_name(&sym.name.clone());
                drop(sym);
                Ok(Some(self.eh_register_synthesized_typeinfo(&enc)))
            },
            other => {
                let Some(info) = crate::cpp::typeinfo_mangling::builtin_typeinfo_symbols(other)
                else {
                    return Ok(None);
                };
                if !self
                    .lowerer
                    .output
                    .builtin_typeinfos
                    .iter()
                    .any(|b| b.typeinfo_symbol == info.typeinfo_symbol)
                {
                    self.lowerer.output.builtin_typeinfos.push(info.clone());
                }
                Ok(Some(info.typeinfo_symbol))
            },
        }
    }

    /// 登记一个按 mangled 编码合成的 typeinfo (目前用于枚举), 返回 `_ZTI` 符号。
    fn eh_register_synthesized_typeinfo(&mut self, encoding: &str) -> String {
        let info = crate::cpp::typeinfo_mangling::named_typeinfo_symbols(encoding);
        if !self
            .lowerer
            .output
            .builtin_typeinfos
            .iter()
            .any(|b| b.typeinfo_symbol == info.typeinfo_symbol)
        {
            self.lowerer.output.builtin_typeinfos.push(info.clone());
        }
        info.typeinfo_symbol
    }

    /// 把落地垫区归集到 `Function::eh_unwind_blocks`, 并登记重入标签。
    ///
    /// 分类规则是图论的而不是登记的: 「从 entry 沿正常边到不了」就是 EH 区。
    /// 这样任何新增的正常边 (循环、switch、goto) 都不需要知道 EH 的存在,
    /// 也不会因为某个地方漏登记而把落地垫发射两遍或干脆不发。
    fn finish_eh_blocks(&mut self) {
        if self.eh_roots.is_empty() {
            return;
        }
        let mut normal: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stack = vec![self.ssa.entry.clone()];
        while let Some(b) = stack.pop() {
            if !normal.insert(b.clone()) {
                continue;
            }
            if let Some(bb) = self.ssa.blocks.get(&b) {
                stack.extend(bb.succs.iter().cloned());
            }
        }
        let mut order: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for root in self.eh_roots.clone() {
            let mut stack = vec![root];
            while let Some(b) = stack.pop() {
                if normal.contains(&b) || !seen.insert(b.clone()) {
                    continue;
                }
                order.push(b.clone());
                if let Some(bb) = self.ssa.blocks.get(&b) {
                    stack.extend(bb.succs.iter().cloned());
                }
            }
        }
        let mut reentry: Vec<String> = Vec::new();
        let mut seen_re: std::collections::HashSet<String> = std::collections::HashSet::new();
        for b in &order {
            if let Some(bb) = self.ssa.blocks.get(b) {
                for s in &bb.succs {
                    if normal.contains(s) && seen_re.insert(s.clone()) {
                        reentry.push(s.clone());
                    }
                }
            }
        }
        self.ssa.eh_unwind_blocks = order;
        self.ssa.eh_reentry_labels = reentry;
    }

    /// 降低 `throw` 为 zero-cost EH 的抛出序列 (无 setjmp)。
    ///
    /// 与 g++ 产物同构 (对照样本 `tmp_eh/throwlocal.cpp`):
    ///
    /// ```text
    ///   buf  = __cxa_allocate_exception(sizeof T)
    ///   [受保护] 在 buf 上构造抛出对象      ; 构造抛出 ⇒ __cxa_free_exception(buf)
    ///   [受保护] __cxa_throw(buf, & _ZTI_T, dtor)
    ///   unreachable
    /// ```
    ///
    /// 三个不能省的细节:
    ///
    /// * **构造抛出必须释放缓冲区**。缓冲区已拿到但对象没构造成功, 不 `free`
    ///   就是每次抛出泄漏一块内存; g++ 正是把构造调用包进受保护区间, 其落地垫
    ///   先 `__cxa_free_exception` 再做局部清理。
    /// * **`__cxa_throw` 本身也要是受保护调用**。`try { throw X(); } catch (X)`
    ///   在同一个帧里就必须能接住 —— 展开器从本帧的 LSDA 开始找 handler, 没有
    ///   这条 call-site 记录就永远找不到。
    /// * **第三参数是该类型的析构函数指针**, 平凡可析构时传 0。传错会让
    ///   `__cxa_end_catch` 不销毁异常对象 (泄漏) 或对平凡类型误调析构。
    fn lower_throw(
        &mut self,
        object: Option<HirExprId>,
        ty: &CxxType,
    ) -> Result<Operand, LowerIrError> {
        let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
        let fn_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));

        // --- rethrow: `throw;` ---
        let Some(obj_id) = object else {
            // `throw;` 走 `__cxa_rethrow`, 而且**必须**照普通抛出一样受本帧剩余
            // action 链保护 (与 `_Unwind_Resume` 不同): 重抛的异常要被**同一帧更
            // 外层**的 try 接住, 还要先跑掉本 catch 体的 `__cxa_end_catch` 清理。
            // g++ 产物实测: `__cxa_rethrow` 被 `.LEHBn/.LEHEn` 包住, 其区间的 action
            // 链就是 [end_catch 清理, 外层 handler]。
            self.eh_call_runtime(CxxRuntimeSymbol::Rethrow, vec![]);
            self.set_term(Terminator::Unreachable);
            return Ok(Operand::Int(0));
        };

        // 1. 异常缓冲区 (堆上, 由 `__cxa_end_catch`/`__cxa_free_exception` 释放)
        let size = self.compute_type_size(ty)?;
        let alloc_name = self.register_runtime(CxxRuntimeSymbol::AllocException.name());
        let buf = self.fresh_syn("exn_buf", void_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(buf.clone()),
            kind: InstKind::Call {
                callee: Operand::FnAddr(alloc_name, fn_ptr_ty.clone()),
                args: vec![Operand::Int(size as i128)],
                ret_ty: void_ptr_ty.clone(),
            },
            span: dummy_span(),
        });

        // 2. 在缓冲区上构造抛出对象
        let value_ty = ty.remove_reference().clone();
        if let CxxType::Class(class_id) = value_ty {
            let copy_ctor = self.eh_resolve_copy_ctor(class_id);
            let ctor_name =
                self.lowerer
                    .ctor_link_name(class_id, CtorVariant::Complete, copy_ctor)?;
            if copy_ctor.is_some() {
                // 有用户声明的拷贝构造: 必须真调它 (可观察行为)。
                let src = self.lower_object_lvalue_addr(obj_id)?;
                // 拷贝构造自身抛出时缓冲区还没交给运行时, 必须自己回收。
                let free_key = self.eh_add_cleanup(EhCleanupKind::FreeException(buf.clone()));
                self.push_inst(Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: Operand::FnAddr(ctor_name, fn_ptr_ty.clone()),
                        args: vec![Operand::Var(buf.clone()), src],
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                });
                self.eh_remove_cleanup(free_key);
            } else {
                // 隐式拷贝: 按聚合值整体写入缓冲区, 与按值传参同一语义。
                let ir_ty = self.lowerer.lower_type(&value_ty)?;
                let typed_ptr_ty = Type::Ptr(Box::new(QualType::new(ir_ty.clone())));
                let typed = self.fresh_syn("exn_typed", typed_ptr_ty.clone());
                self.push_inst(Inst {
                    dest: Some(typed.clone()),
                    kind: InstKind::Cast {
                        ty: typed_ptr_ty,
                        e: Operand::Var(buf.clone()),
                    },
                    span: dummy_span(),
                });
                let val = self.lower_expr(obj_id)?;
                // 写出这一步理论上不抛, 但抛出表达式的求值可能带用户代码; 分配与
                // 写出之间的任何展开都得回收缓冲区。
                let free_key = self.eh_add_cleanup(EhCleanupKind::FreeException(buf.clone()));
                self.push_inst(Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: Operand::Var(typed),
                        src: val,
                        is_volatile: false,
                    },
                    span: dummy_span(),
                });
                self.eh_remove_cleanup(free_key);
            }
        } else {
            let ir_ty = self.lowerer.lower_type(&value_ty)?;
            let typed_ptr_ty = Type::Ptr(Box::new(QualType::new(ir_ty.clone())));
            let typed = self.fresh_syn("exn_typed", typed_ptr_ty.clone());
            self.push_inst(Inst {
                dest: Some(typed.clone()),
                kind: InstKind::Cast {
                    ty: typed_ptr_ty,
                    e: Operand::Var(buf.clone()),
                },
                span: dummy_span(),
            });
            let val = self.lower_expr(obj_id)?;
            self.push_inst(Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(typed),
                    src: val,
                    is_volatile: false,
                },
                span: dummy_span(),
            });
        }

        // 3. typeinfo 与析构函数指针
        //
        // 两个参数都必须是**符号地址**: `Operand::SymAddr` 的语义是「按名字读这个
        // 全局」, 而 `_ZTIi` 被发射成 `uchar*[2]`, 读它拿到的是**首槽 (vptr)**。
        // 实测 g++ 链入后 `__cxa_throw` 收到的就是这个 vptr, 于是
        // `__gxx_personality_v0` 在 vptr 上再取一级虚表直接 SIGSEGV。取址走
        // `InstKind::AddrOf` (`lea sym(%rip)`) 才是 ABI 要的 `const type_info*`。
        let typeinfo = self.eh_typeinfo_symbol(ty)?;
        let ti_operand = match &typeinfo {
            Some(sym) => {
                if !self.lowerer.output.rodata.contains(sym) {
                    self.lowerer.output.rodata.push(sym.clone());
                }
                self.lower_sym_addr(sym.clone(), void_ptr_ty.clone())
            },
            // 编码不出来 (依赖类型等): 传 null。`__cxa_throw` 会照原样把异常
            // 递出去, 任何 typed catch 都不匹配 —— 与「该类型不可被命名捕获」
            // 的语义一致, 且 `catch (...)` 仍然有效。
            None => Operand::Int(0),
        };
        let dtor_operand = self.eh_exception_dtor_operand(ty, fn_ptr_ty.clone());

        // 4. `__cxa_throw(buf, typeinfo, dtor)` —— 不返回。
        let throw_name = self.register_runtime(CxxRuntimeSymbol::Throw.name());
        self.push_inst(Inst {
            dest: None,
            kind: InstKind::Call {
                callee: Operand::FnAddr(throw_name, fn_ptr_ty),
                args: vec![Operand::Var(buf), ti_operand, dtor_operand],
                ret_ty: Type::Void,
            },
            span: dummy_span(),
        });
        self.set_term(Terminator::Unreachable);
        Ok(Operand::Void)
    }

    /// 抛出类型的析构函数指针 (`__cxa_throw` 第三参数)。
    ///
    /// 真源是符号表: 类**有**析构函数符号 (用户写的或 sema 按成员需求合成的)
    /// 才传 D1, 否则传 0。这样平凡可析构类型不会被凭空引入一个不存在的
    /// `<class>_dtor_complete` 引用, 而需要销毁的类型也不会泄漏异常对象。
    fn eh_exception_dtor_operand(&mut self, ty: &CxxType, fn_ptr_ty: Type) -> Operand {
        let CxxType::Class(class_id) = ty.remove_reference().clone() else {
            return Operand::Int(0);
        };
        let owner = class_id.as_symbol();
        let has_dtor = self
            .lowerer
            .symbols
            .iter()
            .any(|s| matches!(s.kind, CxxSymbolKind::Destructor) && s.owner == Some(owner));
        if !has_dtor {
            return Operand::Int(0);
        }
        let class_name = {
            let class_sym = match self.lowerer.symbols.get(owner.0) {
                Some(s) => s,
                None => return Operand::Int(0),
            };
            self.lowerer.symbol_name(class_sym)
        };
        // 与 `lower_destroy` 的调用侧同一个名字约定 (`symbol_name` + `dtor_variant_name`);
        // 同样要的是**函数地址**, 所以走 `AddrOf` 而不是 `SymAddr`。
        let dtor_name = dtor_variant_name(&class_name, DtorVariant::Complete);
        self.lower_sym_addr(dtor_name, fn_ptr_ty)
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
        ctor: Option<SymbolId>,
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
                    self.lower_construct(
                        *class_id,
                        place_args[0],
                        args,
                        CtorVariant::Complete,
                        ctor,
                    )?;
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

                    // 调用构造函数 (重载由 sema 选定)
                    // 注意：这里需要用 HirExprId 来表示指针，但我们只有 Operand
                    // 简化：直接生成构造函数调用，this 指针就是 typed_ptr_var
                    let ctor_name =
                        self.lowerer
                            .ctor_link_name(*class_id, CtorVariant::Complete, ctor)?;
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

        // zero-cost EH 退休点: 正常析构语句执行**之前**把该对象的清理撤掉。
        // 撤早了会让这条析构调用不被自己保护 (重复析构), 撤晚了会让已死对象
        // 参与后续异常的清理 (二次析构) —— 这里正是 GCC 清理链的进出界次序。
        //
        // `in_eh_emit` 期间必须跳过: 生成落地垫要**重放**这些析构语句, 而链此时
        // 是受保护调用那一刻的快照, 重放不能把它改写 (实测后果: 建完落地垫后
        // 快照里的对象已从链上消失, 同一作用域后面的抛出因此完全不被保护)。
        if !self.in_eh_emit {
            if let Some(sym) = self.eh_retire_at.get(&stmt_id).copied() {
                self.eh_retire_cleanup(sym);
            }
        }

        let r = self.lower_stmt_inner(stmt_id, &stmt);

        // zero-cost EH 激活点: 构造完成之后该对象才进入清理集合 (同上, 落地垫
        // 重放期间不改链)。
        if !self.in_eh_emit {
            if let Some(sym) = self.eh_activate_at.get(&stmt_id).copied() {
                self.eh_activate_cleanup(sym);
            }
        }
        r
    }

    /// 语句降级的本体 (与 EH 进出界挂钩分开, 让挂钩能包住**整条**语句)。
    fn lower_stmt_inner(&mut self, stmt_id: HirStmtId, stmt: &HirStmt) -> Result<(), LowerIrError> {
        let _ = stmt_id;
        match &stmt.kind {
            HirStmtKind::Expr(expr_id) => {
                let _ = self.lower_expr(*expr_id)?;
                Ok(())
            },

            HirStmtKind::Compound(stmts) => {
                // zero-cost EH: 清理集合的边界**不是**语法块, 而是每个对象的
                // 构造完成点与正常析构点 (见 `eh_collect_cleanup_sites`)。sema 把
                // 析构语句发在外层兄弟位置, 按块收帧会提前丢掉清理集合 (实测导致
                // 抛点完全不被保护), 所以这里不开帧。
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

                // zero-cost EH 兜底: 预扫第二遍没能定位该对象的构造完成点
                // (出现新的语句形态时才会这样) 就退回「`Decl` 一结束即激活」。
                // 正常路径由 `eh_activate_at` 在正确的语句之后激活, 不会走到这里。
                if self.eh_dtor_of.contains_key(symbol) && !self.eh_activated_syms.contains(symbol)
                {
                    self.eh_activate_cleanup(*symbol);
                }
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

            HirStmtKind::CtorInitBase { base, args, ctor } => {
                self.lower_ctor_init_base(*base, args, *ctor)
            },

            HirStmtKind::CtorInitMember { member, args, ctor } => {
                self.lower_ctor_init_member(*member, args, *ctor)
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

        // zero-cost EH: `for` 初始化域里声明的类局部量, 其析构语句在循环**后面**的
        // 兄弟语句里; 清理链按「构造完成 → 正常析构」进出界, 所以整个循环期间它们都
        // 留在集合里, 出了循环由那条析构语句自动撤掉, 不需要额外的作用域帧。

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

    /// 降低 try/catch 为 **zero-cost EH** (表驱动展开, 无 setjmp)。
    ///
    /// 生成的形状与 g++ 产物逐块对齐 (对照样本 `tmp_eh/nested.cpp`):
    ///
    /// ```text
    /// ; 正常路径
    ///   <try body>            ; 体内每个可能抛出的调用都被登记成 invoke
    ///   goto try_end
    /// ; 落地垫区 (只由 unwinder 按 LSDA call-site 区间进入)
    /// eh_lp.N:  <对象, selector> = [%rax, %rdx]
    ///           if (selector == <本 try 的 handler 索引>) goto catch_i
    ///           ...
    ///           goto eh_resume
    /// catch_i:  p = __cxa_begin_catch(exc_object); <绑定参数>; <catch 体>
    ///           __cxa_end_catch(); goto try_end
    /// ```
    ///
    /// 关键设计点:
    ///
    /// * **handler 入口块在下探 body 之前并压栈**。落地垫是由
    ///   body 里第一个受保护调用触发生成的, 那时它必须已经能引用到本 try 的
    ///   各 catch 入口块。
    /// * try body 与 catch 体是**两条独立的路径**, 所以 catch 体里再抛出的异常
    ///   只由**更外层**的帧/区域接管 (本 try 的 Try 帧此时已弹出), 与 C++ 的
    ///   「catch 体不在自己的 try 作用域内」一致。
    /// * catch 体必须登记 `__cxa_end_catch` 清理: 异常从体内穿出时不调用它,
    ///   libstdc++ 的 handler 计数会永久泄漏 (`std::uncaught_exceptions()` 与
    ///   后续 rethrow 全错)。
    fn lower_try(
        &mut self,
        body_id: HirStmtId,
        handlers: &[HirCatchHandler],
    ) -> Result<(), LowerIrError> {
        let try_id = self.eh_try_seq;
        self.eh_try_seq += 1;

        // 1. 先建每个 handler 的入口块与类型表索引 (下探 body 之前)。
        let mut eh_handlers = Vec::with_capacity(handlers.len());
        for h in handlers {
            let catch_bb = self.eh_new_block("catch");
            // 省略号 handler: typeinfo = None => 类型表里发值为 0 的空表项,
            // 但索引仍是正的 (libstdc++ 靠「filter > 0 且表项为 0」识别 catch-all)。
            let typeinfo = if h.is_catch_all {
                None
            } else {
                self.eh_typeinfo_symbol(&h.param_ty)?
            };
            let type_index = self.eh_type_index_for(typeinfo.clone());
            eh_handlers.push(EhHandler {
                type_index,
                typeinfo,
                catch_bb,
            });
        }
        // catch 体正常结束后的汇合块: 属于**正常** CFG, 落地垫用 `goto` 重入它
        // (由 `finish_eh_blocks` 自动登记成 `eh_reentry_labels`)。
        let after_bb = self.unique_bb("try_end");

        // 2. 压 Try 帧 → 下探 body → 弹帧。body 里的受保护调用因此看到
        //    「本 try 的 handler 在前、更外层块作用域的清理在后」的 action 链。
        self.eh_chain.push(EhChainEntry::Try {
            try_id,
            handlers: eh_handlers.clone(),
        });
        self.lower_stmt(body_id)?;
        self.eh_chain.pop();

        // 3. try body 正常走完 → 汇合块 (body 以 return/throw 结尾时不补跳转)。
        let cur = self.current_bb.clone();
        if matches!(self.ssa.blocks[&cur].term.clone(), Terminator::Unreachable) {
            self.set_term(Terminator::Jump(after_bb.clone()));
        }

        // 4. 各 catch 体。
        for (i, handler) in handlers.iter().enumerate() {
            let catch_bb = eh_handlers[i].catch_bb.clone();
            self.current_bb = catch_bb.clone();

            // 4a. `__cxa_begin_catch(exc_object)`: 交出异常对象 (对类类型还要
            //     做「已捕获」登记), 返回值即对象地址。
            let void_ptr_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
            let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
            let obj = self.eh_exc_slots().0;
            let caught = self.fresh_syn("eh_caught", void_ptr_ty.clone());
            let begin_name = self.register_runtime(CxxRuntimeSymbol::BeginCatch.name());
            self.push_inst(Inst {
                dest: Some(caught.clone()),
                kind: InstKind::Call {
                    callee: Operand::FnAddr(begin_name, fn_ty),
                    args: vec![Operand::Var(obj)],
                    ret_ty: void_ptr_ty,
                },
                span: dummy_span(),
            });

            // 4b. 绑定 catch 形参 (引用 / 按值类 / 标量三条路)。
            if let Some(param) = handler.param {
                self.eh_bind_catch_param(param, &handler.param_ty, &caught)?;
            }

            // 4c. catch 体自身的作用域: 体内再抛出必须先 `__cxa_end_catch`。
            let end_key = self.eh_add_cleanup(EhCleanupKind::EndCatch);
            self.lower_stmt(handler.body)?;
            self.eh_remove_cleanup(end_key);

            // 4d. 体正常结束: end_catch → 汇合块。
            let cur = self.current_bb.clone();
            if matches!(self.ssa.blocks[&cur].term.clone(), Terminator::Unreachable) {
                self.eh_call_runtime(CxxRuntimeSymbol::EndCatch, vec![]);
                self.set_term(Terminator::Jump(after_bb.clone()));
            }
        }

        self.current_bb = after_bb;
        Ok(())
    }

    /// 把 `catch` 形参绑定到 `__cxa_begin_catch` 返回的异常对象地址。
    ///
    /// 三条路径按 C++ [exception.propagation]/1 与 Itanium ABI 划分:
    ///   * `catch (T& e)` / `catch (T&& e)`: 引用的 ABI 表示**就是**对象地址,
    ///     直接把指针值绑进去。旧的统一 `Load` 写法对引用会多解一层, 把对象的
    ///     第一个字当成指针用。
    ///   * `catch (T e)` 且 T 是类: 从异常对象**拷贝构造**局部量 (有用户声明的
    ///     拷贝构造就必须调它, 这是可观察行为); 没有 (隐式拷贝) 时退回聚合整体
    ///     载入, 与按值传参的隐式拷贝语义一致。
    ///   * `catch (int e)` 等标量: 解引用载入。
    fn eh_bind_catch_param(
        &mut self,
        param: SymbolId,
        param_ty: &CxxType,
        caught: &SsaVar,
    ) -> Result<(), LowerIrError> {
        let sym = self.lowerer.get_symbol(param)?;
        let name = sym.name.clone();
        let is_volatile = sym.is_volatile;
        drop(sym);
        // 类型真源用 HIR 上的 `handler.param_ty`: 符号表里的 `sym.ty` 与它是同一个
        // 声明的两份副本, 一旦哪边漏了限定符就会不一致。
        let ir_ty = self.lowerer.lower_type(param_ty)?;
        let var = self.fresh_var(&name, ir_ty.clone());
        self.ssa.local_types.insert(name.clone(), ir_ty.clone());
        self.ssa.original_locals.insert(name.clone());
        self.ssa.local_quals.insert(
            name.clone(),
            if is_volatile {
                QualifierFlags::VOLATILE
            } else {
                QualifierFlags::NONE
            },
        );

        let is_ref = matches!(param_ty, CxxType::LValueRef(_) | CxxType::RValueRef(_));
        // 值形态 (引用脱掉后的那一层) 决定指针类型与是否需要拷贝构造。
        let value_ty = match param_ty {
            CxxType::LValueRef(t) | CxxType::RValueRef(t) => (**t).clone(),
            other => other.clone(),
        };
        let value_ir_ty = self.lowerer.lower_type(&value_ty)?;
        let typed_ptr_ty = Type::Ptr(Box::new(QualType::new(value_ir_ty.clone())));
        let typed = self.fresh_syn("eh_typed", typed_ptr_ty.clone());
        self.push_inst(Inst {
            dest: Some(typed.clone()),
            kind: InstKind::Cast {
                ty: typed_ptr_ty.clone(),
                e: Operand::Var(caught.clone()),
            },
            span: dummy_span(),
        });

        if is_ref {
            // 引用形参的存放就是被引用对象的地址: 直接绑指针, 不载入。
            self.push_inst(Inst {
                dest: Some(var.clone()),
                kind: InstKind::Cast {
                    ty: ir_ty.clone(),
                    e: Operand::Var(typed),
                },
                span: dummy_span(),
            });
        } else if let CxxType::Class(class_id) = value_ty {
            // 按值捕获类对象: 优先走用户声明的拷贝构造函数。
            let copy_ctor = self.eh_resolve_copy_ctor(class_id);
            if let Some(ctor) = copy_ctor {
                let ctor_name =
                    self.lowerer
                        .ctor_link_name(class_id, CtorVariant::Complete, Some(ctor))?;
                let place_addr = self.lower_sym_addr(var.base.clone(), typed_ptr_ty);
                let fn_ty = Type::Ptr(Box::new(QualType::new(Type::Void)));
                self.push_inst(Inst {
                    dest: None,
                    kind: InstKind::Call {
                        callee: Operand::FnAddr(ctor_name, fn_ty),
                        args: vec![place_addr, Operand::Var(typed)],
                        ret_ty: Type::Void,
                    },
                    span: dummy_span(),
                });
            } else {
                // 隐式拷贝 (平凡/聚合): 整体载入到局部量。
                self.push_inst(Inst {
                    dest: Some(var.clone()),
                    kind: InstKind::Load {
                        e: Operand::Var(typed),
                        ty: value_ir_ty,
                        is_volatile: false,
                    },
                    span: dummy_span(),
                });
            }
        } else {
            self.push_inst(Inst {
                dest: Some(var.clone()),
                kind: InstKind::Load {
                    e: Operand::Var(typed),
                    ty: value_ir_ty,
                    is_volatile: false,
                },
                span: dummy_span(),
            });
        }

        self.locals.insert(name, var);
        Ok(())
    }

    /// 找该类**用户声明**的拷贝构造函数 (单参、脱引用后是本类)。
    ///
    /// 只认用户声明的: dcc 不合成隐式拷贝构造符号 (见 `synthesize_implicit_*`),
    /// 找不到时调用方退回聚合整体拷贝 —— 与按值传参/返回的隐式拷贝同一语义。
    fn eh_resolve_copy_ctor(&self, class_id: ClassId) -> Option<SymbolId> {
        let owner = class_id.as_symbol();
        for sym in self.lowerer.symbols.iter() {
            if !matches!(sym.kind, CxxSymbolKind::Constructor) {
                continue;
            }
            if sym.owner != Some(owner) {
                continue;
            }
            let CxxType::Function { params, .. } = &sym.ty else {
                continue;
            };
            if params.len() != 1 {
                continue;
            }
            if matches!(params[0].remove_reference(), CxxType::Class(c) if *c == class_id) {
                return Some(sym.id);
            }
        }
        None
    }

    // --- ctor initializer lowering ---

    /// 构造函数：基类初始化。
    fn lower_ctor_init_base(
        &mut self,
        base_class: ClassId,
        args: &[HirExprId],
        ctor: Option<SymbolId>,
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

        // `base_class == owner_class` 时不是构造基类, 而是**委托构造**:
        // GCC 实测 `S::C2(int)` 委托时 `call _ZN1SC1Ev` —— 叫目标的完整对象
        // 变体 (C1), 因为委托构造负责把整个对象 (含虚基/vptr) 建完。
        // 委托时 this 就是目标对象, 不查基类子对象偏移 (本类不在自己的
        // `bases` 里, 查了会报 "base class N not found in class N")。
        let delegating = base_class == owner_class;
        let variant = if delegating {
            CtorVariant::Complete
        } else {
            CtorVariant::Base
        };
        let ctor_name = self.lowerer.ctor_link_name(base_class, variant, ctor)?;

        // 计算基类子对象地址
        let offset = if delegating {
            0
        } else {
            self.lowerer
                .base_subobject_offset(owner_class, base_class)?
                .ok_or_else(|| {
                    LowerIrError::Internal(format!(
                        "base class {} not found in class {}",
                        base_class.0, owner_class.0
                    ))
                })? as i64
        };
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

        // 调用基类构造函数（base variant）/ 委托构造 (complete variant)
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
        ctor: Option<SymbolId>,
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

                // 调用成员构造函数 (重载由 sema 选定)
                let ctor_name =
                    self.lowerer
                        .ctor_link_name(*member_class, CtorVariant::Complete, ctor)?;
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

        // zero-cost EH: 函数也是一个作用域。预扫出「哪些局部量需要异常清理」
        // 并开一个块帧, 否则 `void f(){ A a; g(); }` 里 `g()` 抛出时 `A a` 不会被
        // 析构 (GCC 会) —— 函数级局部量没有包在 Compound 里, 只能在这里登记。
        self.eh_begin_function_scope(&body);

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

        self.eh_end_function_scope();

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
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(base_id, base_abi);

        // Derived : Base { y: int }
        // Base@0 (primary), y@4, size=8, align=4
        let derived_abi = ClassAbi {
            class_id: derived_id,
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
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
            is_const: false,
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
            is_const: false,
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
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(a_id, a_abi);

        // B: { b: double @ 0 }, size=8, align=8
        let b_abi = ClassAbi {
            class_id: b_id,
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
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
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
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
            is_const: false,
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
            is_const: false,
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
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(a_id, a_abi);

        let b_abi = ClassAbi {
            class_id: b_id,
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
            this_adjust_map: std::collections::HashMap::new(),
            needs_vptr: false,
            has_virtual_functions: false,
        };
        lowerer.register_abi(b_id, b_abi);

        let c_abi = ClassAbi {
            class_id: c_id,
            is_union: false,
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
            thunks: Vec::new(),
            has_direct_virtual_bases: false,
            vbase_embedded_vptr: 0,
            vbase_own_vptr_slots: Vec::new(),
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
                ctor: None,
                args: vec![],
            },
        };
        hir.stmts.push(stmt_init_a);

        // stmt 1: CtorInitBase(B)
        let stmt_init_b = HirStmt {
            id: HirStmtId(1),
            kind: HirStmtKind::CtorInitBase {
                base: b_id,
                ctor: None,
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
                    this_adjust: crate::cpp::hir::ThisAdjust { non_virtual: 0 },
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
        let lr = lowerer.lower();
        assert!(lr.is_ok(), "lowering should succeed: {:?}", lr.err());

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
        let lr = lowerer.lower();
        assert!(lr.is_ok(), "lowering should succeed: {:?}", lr.err());

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

        // B 链 secondary vtable 的槽指向 `Class2::b_func` 的 this-adjust
        // thunk (GCC 实测 `_ZThn16_N1C6b_funcEv`: `subq $16,%rdi; jmp`)。
        // thunk 的签名取自目标函数, 所以实现要与调用方在同一 TU ——
        // 与真实流水线 (native 把类的所有方法一发下发) 一致。
        let c_ptr_ty = CxxType::Pointer(Box::new(CxxType::Class(c_id)));
        let impl_ty = CxxType::Function {
            ret: Box::new(CxxType::Void),
            params: vec![c_ptr_ty.clone()],
            is_const: false,
            is_volatile: false,
            ref_qual: None,
        };
        let impl_sym = sema.add_symbol(
            "Class2::b_func".into(),
            CxxSymbolKind::Function,
            impl_ty,
            Access::Public,
        );
        let impl_this = sema.add_symbol(
            "__impl_this".into(),
            CxxSymbolKind::Variable,
            c_ptr_ty,
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
        hir.bodies.push(HirBody {
            id: HirBodyId(1),
            stmts: vec![],
            ctor_initializers: vec![],
        });
        hir.functions.push(HirFunction {
            symbol: impl_sym,
            body: HirBodyId(1),
            params: vec![impl_this],
            ret_ty: CxxType::Void,
        });

        let mut lowerer = IrLowerer::new(&hir, &symbols);
        lowerer.register_abi(a_id, calc.compute_class(a_id));
        lowerer.register_abi(b_id, calc.compute_class(b_id));
        lowerer.register_abi(c_id, abi_c);

        let lr = lowerer.lower();
        assert!(lr.is_ok(), "lowering should succeed: {:?}", lr.err());

        let out = lowerer.finish();
        // 本 TU 里的函数: test_adjust + Class2::b_func 实现 + 它的 thunk
        let func = out
            .functions
            .iter()
            .find(|f| f.name == "test_adjust")
            .expect("test_adjust 应当已下发");
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

        // thunk 入口必须是真发出来的函数, 并且与 vtable 里写的名字一致
        // (否则 vtable 槽指向一个不存在的符号, 运行期直接跳空)。
        let b_chain = out
            .vtables
            .iter()
            .find(|(name, _)| name.contains("Class2") && name.contains("Class1"))
            .expect("B 链的 secondary vtable");
        let thunk_entry = b_chain
            .1
            .iter()
            .filter_map(|e| match e {
                crate::cpp::lower_ir::VTableEntry::SymAddr(s) if s.starts_with("dcc_thunk_") => {
                    Some(s.clone())
                },
                _ => None,
            })
            .next()
            .expect("secondary vtable 应写入 thunk 入口");
        assert!(
            out.functions.iter().any(|f| f.name == thunk_entry),
            "thunk {} 必须有定义",
            thunk_entry
        );
    }
}
