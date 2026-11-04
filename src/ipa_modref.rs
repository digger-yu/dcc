// src/ipa_modref.rs - IPA modref (过程间 modify/reference 分析)
//
// 目的: 让 `__builtin_constant_p(V)` 在"V 是地址被取用的局部、且其地址逃逸点
// 全部是**已证明不会写该位置**的调用实参"时折叠成 1 (修复 gcc-dg modref-2.c)。
//
// 第一性原理:
//   * 一个指针形参 p 的"被写穿" (mod) 意味: 函数的执行可能通过 p 写它指向的对象。
//   * 摘要 mods[f][i] 必须是被写形参集的**超集** (漏报 ⇒ 误判某局部未改写 ⇒ 误折
//     constant_p ⇒ 静默误编译)。因此遍历对拿不准的形态一律保守 (记 opaque)。
//   * 调用点: 若被调者 g 不写第 i 个指针形参 (mods[g][i]==false 且 g 不 opaque),
//     则把 `&V` 作为第 i 个实参传给 g 不会改写 V。
//   * 一个局部 V 可证为常量, 仅当: V 是整型标量、常量初值、从不被直接写、本函数无
//     不可归因的存储、且 V 的地址**每一次**被取用都是上述"安全实参" (保住
//     `int x=1; __builtin_constant_p(x)==0`: x 未被取址 ⇒ 不折)。
//
// 只折 `__builtin_constant_p(纯 Ident)`; 不做内存常量传播, 不碰 SSA 管线。

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Program, Stmt, Type, VarDecl};
use crate::const_eval::{self, ConstValue};
use crate::ir::Module;

/// 一个函数的 modref 摘要。
#[derive(Clone, Default)]
pub struct ModRefSummary {
    /// mods[i]: 可能透过第 i 个形参写内存 (直接或经调用传递)。
    pub mods: Vec<bool>,
    /// 函数写的内存无法归因到任何形参 (不可归因的存储 / 未知被调者 / 取形参地址)。
    pub opaque: bool,
    /// escape[i]: 第 i 个形参的**指针值**可能流出到未知场所 (存进全局/其它
    /// 指针、返回、传给会逃逸的被调者、进 asm)。指针一流出, 其指向对象之后
    /// 就可能被任何未知指针改写 —— 这才是 `__builtin_constant_p` 该保守的
    /// 判据 (modref-5: `*ptr = 1` 这类不可归因存储本身伤不到未逃逸的局部)。
    pub escape: Vec<bool>,
    pub n_params: usize,
    /// param_is_ptr[i]: 第 i 个形参是否指针类型 (调用点安全判据用)。
    pub param_is_ptr: Vec<bool>,
}

/// 指针值的来源 (只看来源, 保守)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prov {
    Param(usize),
    Unknown,
}

/// 一处存储 (赋值/自增等左值) 的目标基址归类。
enum StoreBase {
    ViaParam(usize),
    ViaObject,
    Unknown,
}

/// 遍历上下文: 一个"指针值"正在经历什么。
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    /// 值被存进某处 (赋给对象/初值/返回值/asm) ⇒ 指针被存起来, pointee 之后可能被写。
    Store,
    /// 指针在**读**位置被解引用 (Deref / ->f / p[i] 读) ⇒ ref, 不是 mod。
    DerefRead,
    /// 普通值上下文 (算术/比较/分支条件)。
    Value,
}

/// 一次调用的一个实参的静态信息。
#[derive(Clone, Default)]
struct CallArg {
    /// 实参指针值是否派生自本函数某形参 ( Prov::Param(j) )。
    src_param: Option<usize>,
    /// 实参是否形如 `&Ident{name}` (取某局部/全局的地址)。
    addr_name: Option<String>,
}

/// 一次调用点。
#[derive(Clone)]
struct CallSite {
    /// 被调者名; None = 未知/间接/内建/不在 known 表。
    callee: Option<String>,
    args: Vec<CallArg>,
}

/// 单函数扫描得到的事实。
#[derive(Default)]
struct BodyFacts {
    direct_mods: BTreeSet<usize>,
    /// 形参指针值逃逸到未知场所的下标 (存全局/返回/传给逃逸被调者/asm)。
    escape_params: BTreeSet<usize>,
    opaque_store: bool,
    calls: Vec<CallSite>,
    addr_of_counts: HashMap<String, usize>,
    object_writes: HashMap<String, usize>,
    local_decl_counts: HashMap<String, usize>,
    /// 出现在任一 `asm` 操作数子树里的**全部**具名对象。
    ///
    /// 为什么需要单独一张表：`"m"`/`"=m"`/`"+m"` 这类内存约束把对象的**地址**直接交给
    /// 未知代码，而 AST 上根本没有 `&` —— `addr_of_counts` 看不见它。没有这张表，
    /// 「从不被取址 ⇒ 值必定」那条捷径就会对 asm 输错 `__builtin_constant_p`。
    asm_named: BTreeSet<String>,
}

/// 这个名字是内联器造出来的临时量（`__inl<N>$<形参>`、`__inl<N>$rv`、
/// `__inlexp<N>$rv`、`__inl<N>_l<局部>_<序号>`，见 `opt::function_inline`）。
///
/// 为什么要按名字划这条线：「地址从未逃逸 ⇒ 值必定就是初值」这个推论对源码里的局部
/// 同样成立，但它的**答案不该被代回器改写**。`int x = 1; if (__builtin_constant_p(x) != 0)`
/// 在 gcc 里会随 -O 变（-O0 答 0、-O1 以上答 1），而本仓的契约是**档位无关**
/// （`scripts/check_opt_equivalence.sh` 就是这条契约的判据，语料 `constp.c` 断言 0）。
/// 内联临时量不在此列：它根本不是源码里的对象，而是「调用点那个实参」的副本，
/// gcc 对它的答案恒为 1（内核 `BUILD_BUG_ON` 族就靠这一点折掉）。
fn is_inline_generated(name: &str) -> bool {
    name.starts_with("__inl")
}

fn is_ptr(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(_))
}

fn is_int_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::Char
            | Type::SChar
            | Type::UChar
            | Type::Char8
            | Type::Char16
            | Type::Char32
            | Type::Short
            | Type::UShort
            | Type::Int
            | Type::UInt
            | Type::Long
            | Type::ULong
            | Type::LongLong
            | Type::ULongLong
            | Type::Int128
            | Type::UInt128
            | Type::Enum(..)
    )
}

/// 指针值的来源 (保守: 只认形参、cast/comma/assign 透传、指针算术基址、同源 Cond)。
fn ptr_prov(e: &Expr, params: &HashMap<String, usize>) -> Prov {
    match e {
        Expr::Ident { name, .. } => params
            .get(&name.name)
            .copied()
            .map(Prov::Param)
            .unwrap_or(Prov::Unknown),
        Expr::Cast { e, .. } => ptr_prov(e, params),
        Expr::Comma { r, .. } => ptr_prov(r, params),
        Expr::Assign { r, .. } => ptr_prov(r, params),
        Expr::Binop {
            op: BinOp::Add | BinOp::Sub,
            l,
            r,
            ..
        } => {
            if is_ptr(&l.ty()) {
                ptr_prov(l, params)
            } else if is_ptr(&r.ty()) {
                ptr_prov(r, params)
            } else {
                Prov::Unknown
            }
        },
        Expr::Cond { then_, else_, .. } => {
            let a = ptr_prov(then_, params);
            let b = ptr_prov(else_, params);
            if a == b {
                a
            } else {
                Prov::Unknown
            }
        },
        _ => Prov::Unknown,
    }
}

/// 归类一处存储的左值目标。
fn classify_store_target(l: &Expr, params: &HashMap<String, usize>) -> StoreBase {
    match l {
        // 改写指针变量本身, 不是写穿 (void f(int*p){p=0;} 不 mod *p).
        Expr::Ident { .. } => StoreBase::ViaObject,
        Expr::Deref { e, .. } => match ptr_prov(e, params) {
            Prov::Param(j) => StoreBase::ViaParam(j),
            Prov::Unknown => StoreBase::Unknown,
        },
        Expr::Member {
            expr,
            is_arrow: true,
            ..
        } => match ptr_prov(expr, params) {
            Prov::Param(j) => StoreBase::ViaParam(j),
            Prov::Unknown => StoreBase::Unknown,
        },
        Expr::Member {
            expr,
            is_arrow: false,
            ..
        } => classify_store_target(expr, params),
        Expr::Index { arr, .. } => {
            if matches!(arr.ty(), Type::Array(_, _)) {
                classify_store_target(arr, params)
            } else {
                match ptr_prov(arr, params) {
                    Prov::Param(j) => StoreBase::ViaParam(j),
                    Prov::Unknown => StoreBase::Unknown,
                }
            }
        },
        _ => StoreBase::Unknown,
    }
}

/// 记录一次"指针值在 ctx 中被使用"对 direct_mods 的贡献。
fn note(ctx: Ctx, prov: Prov, facts: &mut BodyFacts) {
    if let (Ctx::Store, Prov::Param(j)) = (ctx, prov) {
        // 指针值被存进某处 (赋给对象/返回值/asm 输出): pointee 之后可能被写,
        // 且指针本身也流出了本函数的可控范围 ⇒ 同时记 mod 与 escape。
        facts.direct_mods.insert(j);
        facts.escape_params.insert(j);
    }
}

/// 把一条 asm 操作数子树里出现的**所有**具名对象收进 `asm_named`。
///
/// 为什么不是只收根名字：内核的 asm 操作数常常是 `(p + off)`、`*(&v->a)`、
/// `container_of(...)` 这一类算式，里面每一个名字都可能被内存约束或算式里的取址
/// 交给未知代码。宁可少判一个常量，也不能多判一个。
fn collect_asm_ident_names(e: &Expr, facts: &mut BodyFacts) {
    match e {
        Expr::Ident { name, .. } => {
            facts.asm_named.insert(name.name.clone());
        },
        Expr::Member { expr, .. }
        | Expr::Index { arr: expr, .. }
        | Expr::AddrOf { e: expr, .. }
        | Expr::Deref { e: expr, .. }
        | Expr::Cast { e: expr, .. }
        | Expr::Unary { e: expr, .. }
        | Expr::Real { e: expr, .. }
        | Expr::Imag { e: expr, .. }
        | Expr::SizeofExpr { e: expr, .. }
        | Expr::AlignofExpr { e: expr, .. }
        | Expr::PostInc { e: expr, .. }
        | Expr::PostDec { e: expr, .. }
        | Expr::PreInc { e: expr, .. }
        | Expr::PreDec { e: expr, .. } => collect_asm_ident_names(expr, facts),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. } => {
            collect_asm_ident_names(l, facts);
            collect_asm_ident_names(r, facts);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_asm_ident_names(cond, facts);
            collect_asm_ident_names(then_, facts);
            collect_asm_ident_names(else_, facts);
        },
        Expr::Call { callee, args, .. } => {
            collect_asm_ident_names(callee, facts);
            for a in args {
                collect_asm_ident_names(a, facts);
            }
        },
        _ => {},
    }
}

/// 若左值是一个具名对象 (Ident/聚合根的 Ident), 记一次 object_write。
fn note_object_write(l: &Expr, facts: &mut BodyFacts) {
    match l {
        Expr::Ident { name, .. } => {
            *facts.object_writes.entry(name.name.clone()).or_insert(0) += 1;
        },
        Expr::Member {
            expr,
            is_arrow: false,
            ..
        } => note_object_write(expr, facts),
        Expr::Index { arr, .. } if matches!(arr.ty(), Type::Array(_, _)) => {
            note_object_write(arr, facts)
        },
        _ => {},
    }
}

/// 处理一次"存储左值" (赋值/自增自减的目标): 归类 + 记 object_write。
fn handle_store_target(l: &Expr, params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    match classify_store_target(l, params) {
        StoreBase::ViaParam(j) => {
            facts.direct_mods.insert(j);
        },
        StoreBase::Unknown => {
            facts.opaque_store = true;
        },
        StoreBase::ViaObject => {
            note_object_write(l, facts);
        },
    }
}

fn scan_expr(e: &Expr, ctx: Ctx, params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    match e {
        Expr::AddrOf { e: inner, .. } => {
            if let Expr::Ident { name, .. } = inner.as_ref() {
                *facts.addr_of_counts.entry(name.name.clone()).or_insert(0) += 1;
                // 取形参的地址 ⇒ 形参指针可被改指向 ⇒ 保守记 opaque。
                if params.contains_key(&name.name) {
                    facts.opaque_store = true;
                    // 形参地址流出, 之后任何未知指针都可能拿到它 ⇒ 逃逸。
                    if let Some(j) = params.get(&name.name) {
                        facts.escape_params.insert(*j);
                    }
                }
            }
            scan_expr(inner, Ctx::Value, params, facts);
        },
        Expr::Ident { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::LabelAddr { .. } => {
            note(ctx, ptr_prov(e, params), facts);
        },
        Expr::Cast { e: inner, .. } => scan_expr(inner, ctx, params, facts),
        Expr::Comma { l, r, .. } => {
            scan_expr(l, Ctx::Value, params, facts);
            scan_expr(r, ctx, params, facts);
        },
        Expr::Assign { l, r, .. } => {
            handle_store_target(l, params, facts);
            scan_expr(r, Ctx::Store, params, facts);
            scan_expr(l, Ctx::Value, params, facts);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            scan_expr(cond, Ctx::Value, params, facts);
            scan_expr(then_, ctx, params, facts);
            scan_expr(else_, ctx, params, facts);
        },
        Expr::Binop { l, r, .. } => {
            scan_expr(l, Ctx::Value, params, facts);
            scan_expr(r, Ctx::Value, params, facts);
        },
        Expr::Unary { e: inner, .. } => scan_expr(inner, Ctx::Value, params, facts),
        Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. } => {
            handle_store_target(inner, params, facts);
            scan_expr(inner, Ctx::Value, params, facts);
        },
        Expr::Deref { e: inner, .. } => {
            // 解引用的**结果**是 pointee 的值, 不是 inner 指针本身。赋值右值
            // 的 Ctx::Store 只说明"这个值被存走", 与 inner 指针是否被存无关
            // (`*a = *b` 的右值 `*b` 只是**读** b 的 pointee)。旧写法把 Store
            // ctx 透传给 note, 于是 `void copy(int *a, int *b) { *a = *b; }`
            // 的形参 b 被误记成"可能被写" ⇒ 调用点 `copy(&a,&b)` 不再是安全
            // 实参 ⇒ modref-5 的 b 无法证明不变。故 note 一律按 Value。
            note(Ctx::Value, ptr_prov(inner, params), facts);
            scan_expr(inner, Ctx::DerefRead, params, facts);
        },
        Expr::Member { expr, is_arrow, .. } => {
            if *is_arrow {
                // 同上: `p->f` 读的是 pointee 的字段, p 仅被读。
                note(Ctx::Value, ptr_prov(expr, params), facts);
                scan_expr(expr, Ctx::DerefRead, params, facts);
            } else {
                scan_expr(expr, ctx, params, facts);
            }
        },
        Expr::Index { arr, idx, .. } => {
            scan_expr(idx, Ctx::Value, params, facts);
            if matches!(arr.ty(), Type::Array(_, _)) {
                scan_expr(arr, ctx, params, facts);
            } else {
                // 同上: `p[i]` 读 pointee, p 仅被读。
                note(Ctx::Value, ptr_prov(arr, params), facts);
                scan_expr(arr, Ctx::DerefRead, params, facts);
            }
        },
        Expr::Call { callee, args, .. } => {
            scan_call(callee, args, params, facts);
        },
        Expr::SizeofExpr { e: inner, .. } | Expr::AlignofExpr { e: inner, .. } => {
            scan_expr(inner, Ctx::Value, params, facts);
        },
        Expr::Real { e: inner, .. } | Expr::Imag { e: inner, .. } => {
            scan_expr(inner, Ctx::Value, params, facts);
        },
        Expr::VaArg { ap, .. } => scan_expr(ap, Ctx::Value, params, facts),
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            // asm 输入/输出都是"指针/值逃逸点": 输出视为 Store (note 会同时记
            // mod+escape), 输入保守记 opaque; 输入里的形参派生指针同样可能
            // 被 asm 存到任何地方 ⇒ 记 escape。
            // 操作数子树里出现的具名对象全部进 `asm_named`（内存约束不发 `&`）。
            for o in outputs {
                collect_asm_ident_names(o, facts);
                scan_expr(o, Ctx::Store, params, facts);
            }
            for i in inputs {
                collect_asm_ident_names(i, facts);
            }
            if !inputs.is_empty() {
                facts.opaque_store = true;
            }
            for i in inputs {
                if let Prov::Param(j) = ptr_prov(i, params) {
                    facts.escape_params.insert(j);
                }
                scan_expr(i, Ctx::Value, params, facts);
            }
        },
        // 复合字面量/初始化列表/语句表达式/其它: 保守递归子节点 (Value)。
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                scan_init_item(it, params, facts);
            }
        },
        Expr::StmtExpr { block, .. } => scan_block(block, params, facts),
        Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::VaArgPack { .. }
        | Expr::Offsetof { .. } => {},
    }
}

fn scan_init_item(
    it: &crate::ast::InitItem,
    params: &HashMap<String, usize>,
    facts: &mut BodyFacts,
) {
    use crate::ast::InitItem;
    match it {
        InitItem::Expr(e) => scan_expr(e, Ctx::Store, params, facts),
        InitItem::Designated { value, .. } => scan_expr(value, Ctx::Store, params, facts),
    }
}

fn scan_call(callee: &Expr, args: &[Expr], params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    let callee_name = match callee {
        Expr::Ident { name, .. } => Some(name.name.clone()),
        _ => None,
    };
    let mut site = CallSite {
        callee: callee_name,
        args: Vec::with_capacity(args.len()),
    };
    for a in args {
        let src_param = match ptr_prov(a, params) {
            Prov::Param(j) => Some(j),
            Prov::Unknown => None,
        };
        let addr_name = match a {
            Expr::AddrOf { e, .. } => match e.as_ref() {
                Expr::Ident { name, .. } => Some(name.name.clone()),
                _ => None,
            },
            _ => None,
        };
        site.args.push(CallArg {
            src_param,
            addr_name,
        });
        scan_expr(a, Ctx::Value, params, facts);
    }
    scan_expr(callee, Ctx::Value, params, facts);
    facts.calls.push(site);
}

fn scan_stmt(s: &Stmt, params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    match s {
        Stmt::Expr { e, .. } => scan_expr(e, Ctx::Value, params, facts),
        Stmt::Return { val: Some(e), .. } => scan_expr(e, Ctx::Store, params, facts),
        Stmt::Return { val: None, .. } => {},
        Stmt::If {
            cond, then_, else_, ..
        } => {
            scan_expr(cond, Ctx::Value, params, facts);
            scan_stmt(then_, params, facts);
            if let Some(e) = else_ {
                scan_stmt(e, params, facts);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            scan_expr(cond, Ctx::Value, params, facts);
            scan_stmt(body, params, facts);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                scan_stmt(i, params, facts);
            }
            if let Some(c) = cond {
                scan_expr(c, Ctx::Value, params, facts);
            }
            if let Some(st) = step {
                scan_expr(st, Ctx::Value, params, facts);
            }
            scan_stmt(body, params, facts);
        },
        Stmt::Block { b, .. } => scan_block(b, params, facts),
        Stmt::Switch { expr, prelude, .. } => {
            scan_expr(expr, Ctx::Value, params, facts);
            for it in prelude {
                scan_item(it, params, facts);
            }
        },
        Stmt::Label { stmt, .. } => scan_stmt(stmt, params, facts),
        Stmt::GotoIndirect { expr, .. } => scan_expr(expr, Ctx::Value, params, facts),
        Stmt::Invoke { call, .. } => scan_stmt(call, params, facts),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            scan_expr(object, Ctx::Value, params, facts);
            scan_expr(selector, Ctx::Value, params, facts);
        },
        Stmt::AsmGoto { inputs, .. } => {
            // asm 可任意写内存 ⇒ 不可归因; 递归输入以捕获嵌套调用/取址。
            facts.opaque_store = true;
            for i in inputs {
                collect_asm_ident_names(i, facts);
                scan_expr(i, Ctx::Value, params, facts);
            }
        },
        Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Goto { .. }
        | Stmt::Fallthrough { .. } => {},
    }
}

fn scan_block(b: &Block, params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    for it in &b.items {
        scan_item(it, params, facts);
    }
}

fn scan_item(it: &BlockItem, params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    match it {
        BlockItem::Stmt(s) => scan_stmt(s, params, facts),
        BlockItem::Decl(d) => scan_decl(d, params, facts),
    }
}

fn scan_decl(d: &Decl, params: &HashMap<String, usize>, facts: &mut BodyFacts) {
    match d {
        Decl::Var(v) => {
            *facts
                .local_decl_counts
                .entry(v.name.name.clone())
                .or_insert(0) += 1;
            if let Some(init) = &v.init {
                scan_expr(init, Ctx::Store, params, facts);
            }
        },
        Decl::Func(nested) => {
            // 嵌套函数可通过 upvar 捕获并写外层对象 ⇒ 不可归因。
            let _ = nested;
            facts.opaque_store = true;
        },
        Decl::GlobalAsm {
            outputs, inputs, ..
        } => {
            for o in outputs {
                scan_expr(o, Ctx::Store, params, facts);
            }
            if !inputs.is_empty() {
                facts.opaque_store = true;
            }
        },
        _ => {},
    }
}

/// 形参名 -> 下标。
fn param_map(f: &FuncDecl) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for (i, p) in f.params.iter().enumerate() {
        if let Some(id) = &p.name {
            m.insert(id.name.clone(), i);
        }
    }
    m
}

fn scan_body(f: &FuncDecl) -> BodyFacts {
    let params = param_map(f);
    let mut facts = BodyFacts::default();
    if let Some(body) = &f.body {
        scan_block(body, &params, &mut facts);
    }
    facts
}

/// 函数是否可进 known 表 (有体、非变参、非 K&R、非 weak/alias/copy、非嵌套)。
fn is_analyzable(f: &FuncDecl) -> bool {
    f.body.is_some()
        && !f.is_variadic
        && !f.is_old_style
        && !f.is_weak
        && !f.is_extern
        && f.alias_name.is_none()
        && !f.is_nested
        && !f.name.name.is_empty()
}

/// 计算所有函数的 modref 摘要 (不动点, 处理递归)。
fn compute_summaries(
    prog: &Program,
) -> (HashMap<String, ModRefSummary>, HashMap<String, BodyFacts>) {
    // known 表: 函数名 -> (decl 序, n_params, param_is_ptr).
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut n_params: HashMap<String, usize> = HashMap::new();
    let mut param_is_ptr: HashMap<String, Vec<bool>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if is_analyzable(f) {
                known.insert(f.name.name.clone());
                order.push(f.name.name.clone());
                n_params.insert(f.name.name.clone(), f.params.len());
                param_is_ptr
                    .insert(f.name.name.clone(), f.params.iter().map(|p| is_ptr(&p.ty)).collect());
            }
        }
    }

    // 扫描每个 known 函数得 facts.
    let mut facts: HashMap<String, BodyFacts> = HashMap::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if known.contains(&f.name.name) {
                facts.insert(f.name.name.clone(), scan_body(f));
            }
        }
    }

    // 初始化摘要.
    let mut mods: HashMap<String, BTreeSet<usize>> = HashMap::new();
    let mut escapes: HashMap<String, BTreeSet<usize>> = HashMap::new();
    let mut opaque: HashMap<String, bool> = HashMap::new();
    for name in &order {
        let bf = &facts[name];
        mods.insert(name.clone(), bf.direct_mods.clone());
        escapes.insert(name.clone(), bf.escape_params.clone());
        opaque.insert(name.clone(), bf.opaque_store);
    }

    // 反向调用图 (只收 callee ∈ known 的边), 用于变更时重入队调用方.
    let mut callers: HashMap<String, BTreeSet<String>> = HashMap::new();
    for name in &order {
        for site in &facts[name].calls {
            if let Some(g) = &site.callee {
                if known.contains(g) {
                    callers.entry(g.clone()).or_default().insert(name.clone());
                }
            }
        }
    }

    // 工作list不动点.
    let cap = order.len() * 8 + 16;
    let mut queue: VecDeque<String> = order.iter().cloned().collect();
    let mut in_queue: BTreeSet<String> = order.iter().cloned().collect();
    let mut rounds = 0usize;
    while let Some(fname) = queue.pop_front() {
        in_queue.remove(&fname);
        rounds += 1;
        if rounds > cap {
            break;
        }
        let bf = &facts[&fname];
        let mut add_mods: BTreeSet<usize> = BTreeSet::new();
        let mut add_escapes: BTreeSet<usize> = BTreeSet::new();
        let mut set_opaque = false;
        for site in &bf.calls {
            match &site.callee {
                None => {
                    // 未知/间接被调者: 传出的每个形参派生指针都可能被写.
                    for a in &site.args {
                        if let Some(j) = a.src_param {
                            add_mods.insert(j);
                            // 指针进了未知被调者, 它想存哪存哪 ⇒ 逃逸。
                            add_escapes.insert(j);
                        }
                    }
                    set_opaque = true;
                },
                Some(g) if known.contains(g) => {
                    let g_opaque = opaque[g];
                    let g_mods = &mods[g];
                    let g_escapes = &escapes[g];
                    let g_np = n_params[g];
                    for (i, a) in site.args.iter().enumerate() {
                        let param_writes = g_opaque || (i < g_np && g_mods.contains(&i));
                        if param_writes {
                            if let Some(j) = a.src_param {
                                add_mods.insert(j);
                            }
                        }
                        // 被调者把第 i 个形参的指针值传出 ⇒ 调用方的形参
                        // (指针值来自它) 同样逃逸 (传递性)。
                        if i < g_np && g_escapes.contains(&i) {
                            if let Some(j) = a.src_param {
                                add_escapes.insert(j);
                            }
                        }
                    }
                },
                Some(_) => {
                    // 已知存在但不是 analyzable (extern 声明等): 当作未知.
                    for a in &site.args {
                        if let Some(j) = a.src_param {
                            add_mods.insert(j);
                            add_escapes.insert(j);
                        }
                    }
                    set_opaque = true;
                },
            }
        }
        let mut changed = false;
        {
            let cur = mods.get_mut(&fname).unwrap();
            for j in add_mods {
                if cur.insert(j) {
                    changed = true;
                }
            }
        }
        {
            let cur = escapes.get_mut(&fname).unwrap();
            for j in add_escapes {
                if cur.insert(j) {
                    changed = true;
                }
            }
        }
        if set_opaque && !opaque[&fname] {
            opaque.insert(fname.clone(), true);
            changed = true;
        }
        if changed {
            if let Some(cs) = callers.get(&fname) {
                for c in cs {
                    if in_queue.insert(c.clone()) {
                        queue.push_back(c.clone());
                    }
                }
            }
        }
    }

    let mut summaries: HashMap<String, ModRefSummary> = HashMap::new();
    for name in &order {
        let m: Vec<bool> = (0..n_params[name])
            .map(|i| mods[name].contains(&i))
            .collect();
        let esc: Vec<bool> = (0..n_params[name])
            .map(|i| escapes[name].contains(&i))
            .collect();
        summaries.insert(
            name.clone(),
            ModRefSummary {
                mods: m,
                opaque: opaque[name],
                escape: esc,
                n_params: n_params[name],
                param_is_ptr: param_is_ptr[name].clone(),
            },
        );
    }
    (summaries, facts)
}

/// 每个"安全实参"的计数: `&V` 作为已知不写该形参、且不让该指针逃逸的调用实参
/// 出现次数。
fn safe_arg_counts(
    bf: &BodyFacts,
    summaries: &HashMap<String, ModRefSummary>,
) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for site in &bf.calls {
        let Some(g) = &site.callee else { continue };
        let Some(sum) = summaries.get(g) else {
            continue;
        };
        for (i, a) in site.args.iter().enumerate() {
            let Some(name) = &a.addr_name else { continue };
            let param_ok = i < sum.n_params
                && sum.param_is_ptr.get(i).copied().unwrap_or(false)
                && !sum.mods.get(i).copied().unwrap_or(true)
                && !sum.escape.get(i).copied().unwrap_or(true);
            if param_ok && !sum.opaque {
                *counts.entry(name.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// 判定函数 F 内哪些局部可证为编译期常量, 返回 名字 -> 常量值。
///
/// 为什么不再因「本函数有不可归因存储」就整体放弃: 不可归因存储 (`*p = v`,
/// p 是来源不明的指针) 只能写到**地址逃逸过的**对象; 而本函数的候选局部
/// 要求每一次取址都是安全实参 (被调者既不写也不让该指针逃逸), 地址从未
/// 流出 ⇒ 未知指针拿不到它 ⇒ 不可归因存储伤不到它 (modref-5 的
/// `*ptr = 1` 与 `barrier()` 正是这一形状: b 的地址只安全地进过 copy)。
fn proven_const_locals(
    f: &FuncDecl,
    bf: &BodyFacts,
    summaries: &HashMap<String, ModRefSummary>,
) -> HashMap<String, i128> {
    let safe = safe_arg_counts(bf, summaries);
    let param_names: BTreeSet<String> = f
        .params
        .iter()
        .filter_map(|p| p.name.as_ref().map(|id| id.name.clone()))
        .collect();

    // 先把函数里**所有**局部声明收齐（不收指针进去不行：收集器与判据必须同寿命），
    // 再过结构筛子。结构筛子完全不看初值能不能折——那一条挪到了下面的不动点里，
    // 为的是接住「初值里读另一个已证常量」的链：`int a = 2; int b = a * 3;`，以及强制
    // 内联拼接出的 `__inl$_nr = __inl$_bit`（形状表 `.work_tmp/constp_shapes.c` 的 c15：
    // gcc=1、旧 dcc=0）。同一条链在 `opt.rs::subst_cand_reads_items` 里已有实现（它只
    // 服务 asm 立即数操作数），这里复用同一个替换器 `subst_proven_locals`，不另升第二套规则。
    let mut all_decls: Vec<&VarDecl> = Vec::new();
    if let Some(body) = &f.body {
        collect_vars_in_block(body, &mut all_decls);
    }
    // 排障：`DCC_CONSTP_TRACE=1` 打出每个候选被拒的原因。没有它只能看到「constp 返回 0」
    // 这一句，而背后有六条筛子 + 一个不动点，逐条猜测会很慢（本轮已付过学费）。
    let ctrace = constp_trace();
    let passes = |v: &VarDecl| -> bool {
        let name = &v.name.name;
        macro_rules! deny {
            ($why:expr) => {{
                if ctrace {
                    eprintln!("CONSTP-REJ| {} {} :: {}", f.name.name, name, $why);
                }
                return false;
            }};
        }
        // 1. 非 extern、非 volatile.
        if v.is_extern || v.quals.is_volatile() {
            deny!("extern/volatile");
        }
        // 2. 整型标量.
        if !is_int_scalar(&v.ty) {
            deny!("非整型标量");
        }
        // 3. 不是形参名 (排除遮蔽)。
        //
        // 「本函数内只声明一次」这一条不再写在这里：内联会把同一个 `static` 函数体
        // 拼接进同一个 caller 两次（`get_thr_limit()` 同时被 `init_a/init_b` 调、两者又
        // 一起被内联），两份 `({ auto ux = …; })` 带着**同一个**声明名落地，于是这条
        // 「只声明一次」一律失败、候选全丢。改为在下面的不动点里按名字分组：**同名多份
        // 声明只要全部证成同一个常量，替换依旧安全**（同值，读哪一份都一样）。
        if param_names.contains(name) {
            deny!("形参同名");
        }
        // 4. 必须有初值（能不能折到不动点那一轮里再试）.
        if v.init.is_none() {
            deny!("无初值");
        }
        // 5+6. 两条都能证明「这个局部的值就是初值」：
        //   (a) 原口径：被取址，但**全部**取用点都是安全实参（被调者不写它）；
        //   (b) 「隔离」口径：从来没被取址过（`addr == 0`），且不在任何 asm
        //       操作数里（内存约束不发 `&`，只能单独记）—— 但**只对inline 造出的
        //       临时量**成立，见 [`is_inline_generated`] 的理由。
        // 内核那一族 `BUILD_BUG_ON`/`compiletime_assert` 的「常量化判定」完全押在
        // `__builtin_constant_p(<内联出来的局部>)` 上（实测 `mm/shmem.o` 的 9 条
        // `__compiletime_assert_NNN`），(b) 就是为它们加的：形参被内联成一个有字面量
        // 初值的局部，对 gcc 而言那次替换已经发生，所以答 1。
        let addr = bf.addr_of_counts.get(name).copied().unwrap_or(0);
        if bf.asm_named.contains(name) {
            deny!("出现在 asm 操作数里");
        }
        let isolated = addr == 0 && is_inline_generated(name);
        let via_safe_args = addr != 0 && addr == safe.get(name).copied().unwrap_or(0);
        if !(isolated || via_safe_args) {
            deny!("地址流出去向不明");
        }
        // 7. 从未被直接写.
        if bf.object_writes.get(name).copied().unwrap_or(0) != 0 {
            deny!("被写过");
        }
        true
    };
    let cands: Vec<&VarDecl> = all_decls.into_iter().filter(|v| passes(v)).collect();

    // 不动点：先把「初值不含任何候选读」的直接算出来，再把含有已证候选的那几条接上。
    // 每轮至少新增一条才会继续，故必然终止（候选数有限且已入表的不重复推）。
    //
    // **按名字分组**：同名多份声明必须全部折到同一个常量才入表（见上面第 3 条的理由）。
    // 只要有一份折不动或算出来的值不同，这个名字就永远不入表——宁可不折，不能折错。
    let mut by_name: HashMap<&str, Vec<&VarDecl>> = HashMap::new();
    for v in &cands {
        by_name.entry(v.name.name.as_str()).or_default().push(v);
    }
    let mut out: HashMap<String, i128> = HashMap::new();
    loop {
        let mut grew = false;
        for (name, decls) in &by_name {
            if out.contains_key(*name) {
                continue;
            }
            let mut agreed: Option<i128> = None;
            let mut all_ok = true;
            for v in decls {
                let Some(init) = &v.init else {
                    all_ok = false;
                    break;
                };
                let probe = subst_proven_locals(init, &out);
                match const_eval::eval_full(&probe, false) {
                    Ok(ConstValue::Int(cv)) => match agreed {
                        None => agreed = Some(cv),
                        Some(prev) if prev == cv => {},
                        Some(_) => {
                            all_ok = false;
                            break;
                        },
                    },
                    _ => {
                        all_ok = false;
                        break;
                    },
                }
            }
            if all_ok {
                if let Some(cv) = agreed {
                    out.insert((*name).to_string(), cv);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    if ctrace {
        let mut keys: Vec<&String> = out.keys().collect();
        keys.sort();
        for k in keys {
            eprintln!("CONSTP-OK| {} {}={}", f.name.name, k, out[k]);
        }
    }
    out
}

/// `DCC_CONSTP_TRACE` 只查一次环境变量：`fold_constant_p_expr` 会为每个
/// `__builtin_constant_p` 调一次，逐次 `var_os` 在全树上是白扔的开销。
fn constp_trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DCC_CONSTP_TRACE").is_some())
}

/// 收一整个块树里的局部 `Decl::Var`（包含嵌套块；不递归进嵌套函数的体，
/// 那些名字不属于本函数的作用域）。
///
/// **初始化式也要下探**：强制内联落在「声明初值」这一位置时，callee 的体被包成
/// `int *p = ({ …__inl1$…; … });` —— 形参临时量声明在那个**语句表达式**里，而它
/// 正是 `__builtin_constant_p(形参)` 唯一的答案来源。旧写法在这里只 `out.push(v)`，
/// 于是 `.work_tmp/cp3.c` 那一形（内联 + `restart:` 标号的 `__rhashtable_lookup`）
/// 的临时量连候选表都进不了 ⇒ constp 一律答 0 ⇒ `BUILD_BUG_ON` 变成真引用
/// （实测 `fs/xattr.o`/`fs/pidfs.o`/`kernel/events/hw_breakpoint.o` 各留 2–4 条
/// `__compiletime_assert_473/487`）。
fn collect_vars_in_block<'a>(b: &'a Block, out: &mut Vec<&'a VarDecl>) {
    collect_vars_in_items(&b.items, out)
}

fn collect_vars_in_stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a VarDecl>) {
    match s {
        Stmt::Block { b, .. } => collect_vars_in_block(b, out),
        Stmt::Expr { e, .. } => collect_vars_in_expr(e, out),
        Stmt::Return { val: Some(e), .. } => collect_vars_in_expr(e, out),
        Stmt::Return { val: None, .. } => {},
        Stmt::If {
            cond, then_, else_, ..
        } => {
            collect_vars_in_expr(cond, out);
            collect_vars_in_stmt(then_, out);
            if let Some(e) = else_ {
                collect_vars_in_stmt(e, out);
            }
        },
        Stmt::While { cond, body, .. } => {
            collect_vars_in_expr(cond, out);
            collect_vars_in_stmt(body, out);
        },
        Stmt::Do { body, cond, .. } => {
            collect_vars_in_expr(cond, out);
            collect_vars_in_stmt(body, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_vars_in_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_vars_in_expr(c, out);
            }
            if let Some(st) = step {
                collect_vars_in_expr(st, out);
            }
            collect_vars_in_stmt(body, out);
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            collect_vars_in_expr(expr, out);
            collect_vars_in_items(prelude, out);
            for c in cases {
                collect_vars_in_block(&c.body, out);
            }
        },
        Stmt::Label { stmt, .. } => collect_vars_in_stmt(stmt, out),
        _ => {},
    }
}

fn collect_vars_in_items<'a>(items: &'a [BlockItem], out: &mut Vec<&'a VarDecl>) {
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => {
                out.push(v);
                if let Some(e) = &v.init {
                    collect_vars_in_expr(e, out);
                }
            },
            BlockItem::Stmt(s) => collect_vars_in_stmt(s, out),
            _ => {},
        }
    }
}

/// 语句表达式 `({ ... })` 里的声明也算本函数的局部。
///
/// 内核 `min()/max()/clamp()` 的 `__careful_cmp_once` 就是
/// `({ auto ux = (x); auto uy = (y); __cmp(op, ux, uy); })`。那份 `ux/uy` 收不进候选表，
/// `statically_true((long long)uy >= 0)` 就永远拿不到 1，`__sign_use` 退化成
/// 2 ⇒ `__types_ok` 判 0 ⇒ `BUILD_BUG_ON_MSG` 真的发出一条
/// `__compiletime_assert_NNN` 引用（全树链接期就是一堆未定义引用）。
///
/// 实测（`-O2`，夹具 `.work_tmp/constp_stmt_shape.c`）：函数体顶层的两形 gcc/dcc 都是
/// 1，坐在 `({...})` 里的两形 gcc=1、dcc=0。首例现场是
/// `arch/x86/kernel/cpu/mce/amd.c` 的 `min(thr_limit /*u32*/, THRESHOLD_MAX /*int 0xFFF*/)`。
fn collect_vars_in_expr<'a>(e: &'a Expr, out: &mut Vec<&'a VarDecl>) {
    crate::opt::visit_expr_stmtexprs(e, &mut |items| collect_vars_in_items(items, out));
}

// ---- __builtin_constant_p 折叠 ----

/// 把 proven 常量局部替换成字面量 (仅用于 __builtin_constant_p 实参判定)。
fn subst_proven_locals(e: &Expr, proven: &HashMap<String, i128>) -> Expr {
    match e {
        Expr::Ident { name, ty, span } => {
            if let Some(v) = proven.get(&name.name) {
                return Expr::Int {
                    v: *v,
                    span: span.clone(),
                    ty: ty.clone(),
                };
            }
            e.clone()
        },
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op: *op,
            l: Box::new(subst_proven_locals(l, proven)),
            r: Box::new(subst_proven_locals(r, proven)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Unary {
            op,
            e: inner,
            span,
            ty,
        } => Expr::Unary {
            op: *op,
            e: Box::new(subst_proven_locals(inner, proven)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Cast {
            ty: cty,
            e: inner,
            span,
        } => Expr::Cast {
            ty: cty.clone(),
            e: Box::new(subst_proven_locals(inner, proven)),
            span: span.clone(),
        },
        _ => e.clone(),
    }
}

fn fold_constant_p_expr(e: &mut Expr, proven: &HashMap<String, i128>) {
    match e {
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if name.name == "__builtin_constant_p" && args.len() == 1 {
                    // 在 AST 层把 __builtin_constant_p 完全解析 (必须赶在 SSA/SCCP 之前:
                    // 否则 SCCP 会把非常量实参的值替换成常量, codegen 再误判成常量表达式)。
                    // 1) 实参本身是编译期常量表达式; 2) 实参是 proven 局部, 或整个表达式
                    //    把 proven 局部替换成字面量后成为常量 (modref-5:
                    //    `__builtin_constant_p (b == 2)`, b 已证不变)。
                    // 二者皆非 ⇒ 0 (与 GCC 对"变量实参返回 0"一致, 也是 constp.c 的断言)。
                    let is_c = const_eval::is_constant_p_arg(&args[0]);
                    let proven_local = matches!(&args[0], Expr::Ident { name: vn, .. } if proven.contains_key(&vn.name));
                    let folded_expr_const = !is_c
                        && !proven_local
                        && const_eval::is_constant(&subst_proven_locals(&args[0], proven));
                    let v = if is_c || proven_local || folded_expr_const {
                        1
                    } else {
                        0
                    };
                    if v == 0 && constp_trace() {
                        let mut keys: Vec<&String> = proven.keys().collect();
                        keys.sort();
                        eprintln!(
                            "CONSTP-ZERO| proven({})={:?} arg={:?}",
                            keys.len(),
                            keys,
                            &args[0]
                        );
                    }
                    *e = Expr::Int {
                        v,
                        span: span.clone(),
                        ty: ty.clone(),
                    };
                    return;
                }
            }
            // 未折: 仍递归子节点 (嵌套调用).
            fold_constant_p_expr(callee, proven);
            for a in args.iter_mut() {
                fold_constant_p_expr(a, proven);
            }
        },
        Expr::Assign { l, r, .. } => {
            fold_constant_p_expr(l, proven);
            fold_constant_p_expr(r, proven);
        },
        Expr::Binop { l, r, .. } => {
            fold_constant_p_expr(l, proven);
            fold_constant_p_expr(r, proven);
        },
        Expr::Unary { e: inner, .. } => fold_constant_p_expr(inner, proven),
        Expr::Cast { e: inner, .. } => fold_constant_p_expr(inner, proven),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            fold_constant_p_expr(cond, proven);
            fold_constant_p_expr(then_, proven);
            fold_constant_p_expr(else_, proven);
        },
        Expr::Comma { l, r, .. } => {
            fold_constant_p_expr(l, proven);
            fold_constant_p_expr(r, proven);
        },
        Expr::Index { arr, idx, .. } => {
            fold_constant_p_expr(arr, proven);
            fold_constant_p_expr(idx, proven);
        },
        Expr::Member { expr, .. } => fold_constant_p_expr(expr, proven),
        Expr::Deref { e: inner, .. } => fold_constant_p_expr(inner, proven),
        Expr::AddrOf { e: inner, .. } => fold_constant_p_expr(inner, proven),
        Expr::SizeofExpr { e: inner, .. } | Expr::AlignofExpr { e: inner, .. } => {
            fold_constant_p_expr(inner, proven)
        },
        Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. } => fold_constant_p_expr(inner, proven),
        Expr::Real { e: inner, .. } | Expr::Imag { e: inner, .. } => {
            fold_constant_p_expr(inner, proven)
        },
        Expr::VaArg { ap, .. } => fold_constant_p_expr(ap, proven),
        // **语句表达式必须下探**：内核 `min()/max()/READ_ONCE/…` 那一族把
        // `__builtin_constant_p(<局部>)` 坐在 `({...})` 里（实测 `lib/kstrtox.o` 的 11 条
        // `__compiletime_assert_NNN` 全仍活着，站点条件就是 `__sign_use` 里的
        // `__builtin_constant_p((long long)__UNIQUE_ID_x__197 …)`）。不下探这一臂，上面的
        // `proven` 表再准也轮不到它跑。臂表与 `opt.rs::fold_branches_in_expr` 保持同一份
        // 形状（那两个 walker 一旦覆盖不一致，就会出现「同一个构造一条路径看得到、
        // 另一条看不到」这种最难查的不对称）。这里不拆子表达式，只折 constp 本身。
        Expr::StmtExpr { block, .. } => fold_constant_p_block(block, proven),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    crate::ast::InitItem::Expr(x) => fold_constant_p_expr(x, proven),
                    crate::ast::InitItem::Designated { value, .. } => {
                        fold_constant_p_expr(value, proven)
                    },
                }
            }
        },
        _ => {},
    }
}

fn fold_constant_p_stmt(s: &mut Stmt, proven: &HashMap<String, i128>) {
    match s {
        Stmt::Expr { e, .. } => fold_constant_p_expr(e, proven),
        Stmt::Return { val: Some(e), .. } => fold_constant_p_expr(e, proven),
        Stmt::Return { val: None, .. } => {},
        Stmt::If {
            cond, then_, else_, ..
        } => {
            fold_constant_p_expr(cond, proven);
            fold_constant_p_stmt(then_, proven);
            if let Some(e) = else_ {
                fold_constant_p_stmt(e, proven);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            fold_constant_p_expr(cond, proven);
            fold_constant_p_stmt(body, proven);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                fold_constant_p_stmt(i, proven);
            }
            if let Some(c) = cond {
                fold_constant_p_expr(c, proven);
            }
            if let Some(st) = step {
                fold_constant_p_expr(st, proven);
            }
            fold_constant_p_stmt(body, proven);
        },
        Stmt::Block { b, .. } => fold_constant_p_block(b, proven),
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            fold_constant_p_expr(expr, proven);
            for it in prelude.iter_mut() {
                fold_constant_p_item(it, proven);
            }
            // 只走 prelude 而不下探臂体，等于「switch 里的 constp 没人看」（臂体
            // 坐的恰好是 `({...})` 展开后的那一堆语句）。
            for c in cases.iter_mut() {
                fold_constant_p_block(&mut c.body, proven);
            }
        },
        Stmt::Label { stmt, .. } => fold_constant_p_stmt(stmt, proven),
        Stmt::Invoke { call, .. } => fold_constant_p_stmt(call, proven),
        _ => {},
    }
}

fn fold_constant_p_block(b: &mut Block, proven: &HashMap<String, i128>) {
    for it in b.items.iter_mut() {
        fold_constant_p_item(it, proven);
    }
}

fn fold_constant_p_item(it: &mut BlockItem, proven: &HashMap<String, i128>) {
    match it {
        BlockItem::Stmt(s) => fold_constant_p_stmt(s, proven),
        BlockItem::Decl(d) => {
            if let Decl::Var(v) = d {
                if let Some(init) = &mut v.init {
                    fold_constant_p_expr(init, proven);
                }
            }
        },
    }
}

/// 入口: 对整个 module 跑 IPA modref + 折叠 __builtin_constant_p。
pub fn run(module: &mut Module, level: u8) {
    if level < 2 {
        return;
    }
    let prog = module.source_mut();
    let (summaries, facts) = compute_summaries(prog);
    // 按 decl 序确定性处理.
    let names: Vec<String> = prog
        .decls
        .iter()
        .filter_map(|d| {
            if let Decl::Func(f) = d {
                Some(f.name.name.clone())
            } else {
                None
            }
        })
        .collect();
    // 先算每个函数的 proven (不可变借用 facts/summaries).
    let mut proven_by_fn: HashMap<String, HashMap<String, i128>> = HashMap::new();
    for decl in &prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(bf) = facts.get(&f.name.name) {
                let p = proven_const_locals(f, bf, &summaries);
                if !p.is_empty() {
                    proven_by_fn.insert(f.name.name.clone(), p);
                }
            }
        }
    }
    let _ = names;
    // 再折叠 (可变借用). 对**所有**函数体执行: 即便无 proven 局部, 也要把剩余的
    // __builtin_constant_p(非常量实参) 解析成 0 —— 否则它会漏到 SSA, 被 SCCP 把实参
    // 值替换成常量后再被 codegen 误判成常量表达式 (constp.c 的 int x=1 正是此坑)。
    let empty: HashMap<String, i128> = HashMap::new();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let p = proven_by_fn.get(&f.name.name).unwrap_or(&empty);
            if let Some(body) = &mut f.body {
                fold_constant_p_block(body, p);
            }
        }
    }
}
