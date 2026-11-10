// src/pass_probe.rs —— `DCC_PASS_STATS=1` 下的优化管线体检。
//
// 为什么要有它：`-O1`/`-O2` 相对 `-O0` 的编译时间差距（内核全树实测约 10×）必须
// 落到**具体 pass** 上，才能回答「删 pass」还是「补 cost model」。两条现成的路都不够：
//   * `perf`：release 产物 `strip = "symbols"`（见 Cargo.toml），拿不到 pass 级符号；
//     即使带符号，抽样也只能给时间、给不出「这个 pass 到底改没改东西」。
//   * `DCC_OPT_SKIP` 逐个关掉重编：拿到的是墙钟差，且一次实验 = pass 数 × 重编，
//     噪声还会把「本来就没命中的 pass」和「命中了但很便宜」搅在一起。
// 这里在 **pass 调用点**直接测两件事：墙钟时间 + 是否真改了模块。
//   * AST 层 pass 大多不汇报改动 ⇒ 用**结构指纹**判（模块前后指纹是否相同）；
//   * SSA 层每个 pass 本来就返回 `bool`（§ssa::passes 顶部契约）⇒ 直接采信。
//
// 纪律与 `DCC_DUMP_PASS` / `DCC_OPT_SKIP` 相同：只在环境变量开启时运行，
// 不参与任何判据，不进产物字节。关闭时每个调用点只多一次 `OnceLock` 读 + 分支。
//
// 输出（stderr，一行一个 pass，按耗时降序）：
//   PASSSTAT|level=2|pass=fold_constants|calls=1|hits=1|hit_pct=100.0|ms=12.345|hit_ms=12.345|miss_ms=0.000

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::ast::{Block, BlockItem, Decl, Expr, Stmt};
use crate::ir::Module;

#[derive(Default, Clone, Copy)]
struct Stat {
    calls: u64,
    hits: u64,
    ns: u128,
    hit_ns: u128,
}

static ENABLED: OnceLock<bool> = OnceLock::new();
static LEVEL: AtomicU8 = AtomicU8::new(0);
static STATS: Mutex<BTreeMap<&'static str, Stat>> = Mutex::new(BTreeMap::new());

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("DCC_PASS_STATS")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false)
    })
}

/// 每个模块的优化入口调一次：记下 -O 档、清空上一模块的累计。
pub(crate) fn begin(level: u8) {
    if !enabled() {
        return;
    }
    LEVEL.store(level, Ordering::Relaxed);
    STATS.lock().unwrap().clear();
}

fn record(name: &'static str, hit: bool, ns: u128) {
    let mut g = STATS.lock().unwrap();
    let st = g.entry(name).or_default();
    st.calls += 1;
    st.ns += ns;
    if hit {
        st.hits += 1;
        st.hit_ns += ns;
    }
}

/// 模块级 pass：前后各算一次结构指纹，指纹变化即「命中」。
pub(crate) fn probe<T>(name: &'static str, m: &mut Module, f: impl FnOnce(&mut Module) -> T) -> T {
    if !enabled() {
        return f(m);
    }
    let before = fp_module(m);
    let t0 = std::time::Instant::now();
    let r = f(m);
    let ns = t0.elapsed().as_nanos();
    let after = fp_module(m);
    record(name, before != after, ns);
    r
}

/// `bool` 返回值的 pass：`true` 即命中（SSA 层的既有契约）。
pub(crate) trait Hit {
    fn hit(&self) -> Option<bool>;
}
impl Hit for bool {
    fn hit(&self) -> Option<bool> {
        Some(*self)
    }
}
impl Hit for () {
    fn hit(&self) -> Option<bool> {
        None
    }
}

/// 函数级 pass：时间照测；返回值是 `bool` 时顺带记命中，其余类型只记时间。
pub(crate) fn probe_fn<R: Hit>(name: &'static str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let t0 = std::time::Instant::now();
    let r = f();
    let ns = t0.elapsed().as_nanos();
    record(name, r.hit().unwrap_or(false), ns);
    r
}

/// 打印本模块的体检表并清空。
pub(crate) fn flush(tag: &str) {
    if !enabled() {
        return;
    }
    let mut g = STATS.lock().unwrap();
    if g.is_empty() {
        return;
    }
    let level = LEVEL.load(Ordering::Relaxed);
    let mut rows: Vec<(&'static str, Stat)> = g.iter().map(|(k, v)| (*k, *v)).collect();
    // 按耗时降序：报告的表头就是「先砍谁」。
    rows.sort_by(|a, b| b.1.ns.cmp(&a.1.ns).then(a.0.cmp(b.0)));
    for (name, st) in rows {
        let ms = st.ns as f64 / 1e6;
        let hit_ms = st.hit_ns as f64 / 1e6;
        let pct = if st.calls == 0 {
            0.0
        } else {
            100.0 * st.hits as f64 / st.calls as f64
        };
        eprintln!(
            "PASSSTAT|level={level}|pass={name}|{tag}calls={}|hits={}|hit_pct={pct:.1}|ms={ms:.3}|hit_ms={hit_ms:.3}|miss_ms={:.3}",
            st.calls,
            st.hits,
            ms - hit_ms
        );
    }
    g.clear();
}

// =====================================================================
// 结构指纹
//
// 目的只有一个：判「这次 pass 调用有没有改动模块」。因此只混入**优化会改的**东西
// （语句/表达式形状、整型常量值、标识符名、标签/跳转目标），不混类型与 span：
// 类型不参与这些 pass 的判据，span 则会被内联整段重写而不代表语义变化。
// =====================================================================

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fp_module(m: &Module) -> u64 {
    let mut h = FNV_OFFSET;
    for d in &m.source().decls {
        match d {
            Decl::Func(f) => {
                mix(&mut h, 1);
                mix_str(&mut h, &f.name.name);
                if let Some(b) = &f.body {
                    fp_block(b, &mut h);
                }
            },
            Decl::Var(v) => {
                mix(&mut h, 2);
                mix_str(&mut h, &v.name.name);
                if let Some(e) = &v.init {
                    fp_exprs(e, &mut h);
                }
            },
            // 类型定义 / 文件级 asm 不是这些 pass 的改写对象；只按「是否存在」入账。
            _ => mix(&mut h, 3),
        }
    }
    h
}

fn fp_block(b: &Block, h: &mut u64) {
    mix(h, 0xb10c);
    for it in &b.items {
        fp_item(it, h);
    }
}

fn fp_item(it: &BlockItem, h: &mut u64) {
    match it {
        BlockItem::Stmt(s) => fp_stmt(s, h),
        BlockItem::Decl(Decl::Func(f)) => {
            mix(h, 4);
            mix_str(h, &f.name.name);
            if let Some(b) = &f.body {
                fp_block(b, h);
            }
        },
        BlockItem::Decl(Decl::Var(v)) => {
            mix(h, 5);
            mix_str(h, &v.name.name);
            if let Some(e) = &v.init {
                fp_exprs(e, h);
            }
        },
        BlockItem::Decl(_) => mix(h, 6),
    }
}

fn fp_case(c: &crate::ast::Case, h: &mut u64) {
    if let Some(e) = &c.val {
        fp_exprs(e, h);
    }
    if let Some(e) = &c.range_end {
        fp_exprs(e, h);
    }
    fp_block(&c.body, h);
}

fn fp_stmt(s: &Stmt, h: &mut u64) {
    mix_disc(h, s);
    match s {
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                fp_exprs(e, h);
            }
        },
        Stmt::Expr { e, .. } => fp_exprs(e, h),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            fp_exprs(cond, h);
            fp_stmt(then_, h);
            if let Some(x) = else_ {
                fp_stmt(x, h);
            }
        },
        Stmt::While { cond, body, .. } => {
            fp_exprs(cond, h);
            fp_stmt(body, h);
        },
        Stmt::Do { body, cond, .. } => {
            fp_stmt(body, h);
            fp_exprs(cond, h);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                fp_stmt(i, h);
            }
            if let Some(c) = cond {
                fp_exprs(c, h);
            }
            if let Some(s2) = step {
                fp_exprs(s2, h);
            }
            fp_stmt(body, h);
        },
        Stmt::Block { b, .. } => fp_block(b, h),
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Fallthrough { .. } => {},
        Stmt::Goto { target, .. } => mix_str(h, &target.name),
        Stmt::GotoIndirect { expr, .. } => fp_exprs(expr, h),
        Stmt::Label { name, stmt, .. } => {
            mix_str(h, &name.name);
            fp_stmt(stmt, h);
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            fp_exprs(expr, h);
            for c in cases {
                fp_case(c, h);
            }
            for it in prelude {
                fp_item(it, h);
            }
        },
        Stmt::AsmGoto {
            asm_str,
            outputs,
            inputs,
            goto_labels,
            ..
        } => {
            mix_str(h, asm_str);
            for e in outputs.iter().chain(inputs.iter()) {
                fp_exprs(e, h);
            }
            for l in goto_labels {
                mix_str(h, &l.name);
            }
        },
        Stmt::Invoke {
            call,
            landing_pad,
            ..
        } => {
            fp_stmt(call, h);
            if let Some(l) = landing_pad {
                mix_str(h, &l.name);
            }
        },
        Stmt::EhLandingPad {
            label,
            object,
            selector,
            ..
        } => {
            mix_str(h, &label.name);
            fp_exprs(object, h);
            fp_exprs(selector, h);
        },
    }
}

/// 表达式子树：借 `opt::walk_expr_ref` 的穷举遍历（含 `StmtExpr` 里的块），
/// 每个节点混入变体判别 + 少量「优化会改」的载荷。
fn fp_exprs(e: &Expr, h: &mut u64) {
    crate::opt::walk_expr_ref(e, &mut |x| {
        mix_disc(h, x);
        match x {
            Expr::Int { v, .. } => mix(h, *v as u64),
            Expr::Ident { name, .. } => mix_str(h, &name.name),
            _ => {},
        }
    });
}

fn mix(h: &mut u64, v: u64) {
    *h ^= v;
    *h = h.wrapping_mul(FNV_PRIME);
}

fn mix_str(h: &mut u64, s: &str) {
    for b in s.as_bytes() {
        mix(h, *b as u64);
    }
    mix(h, 0xff);
}

fn mix_disc<T>(h: &mut u64, v: &T) {
    let mut dh = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(v).hash(&mut dh);
    mix(h, dh.finish());
}
