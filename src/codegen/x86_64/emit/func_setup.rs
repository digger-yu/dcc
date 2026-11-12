// src/codegen/x86_64/emit/func_setup.rs
// Per-function setup helpers: frame bookkeeping, local analysis walks and
// small register/stack helpers used by compile_func. Manually migrated from
// the pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::abi::layout::LayoutMap;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::Codegen;
use std::collections::{HashMap, HashSet};

/// 作用域感知的变量槽分配器（§5.12 frame economy / 报告 §5.26）。
///
/// 旧 `pre_register_vars` 把函数体摊平后再单调累加水位：每个内联副本的局部变量都
/// **永久**占一格。`perf_event_task_tick` 因此有 11736B 变量区（1467 槽）⇒ 单帧
/// 0x2e80，在 16KB IRQ 栈上一个 `call` 就打穿守卫页（gcc 同函数零帧）。
///
/// C 的块作用域保证「已退出的块」的槽可以复用，所以这里按块边界保存-恢复水位；
/// 函数体顶层不回收（形参与顶层局部同帧，参数在峰值之下另行分配）。两条保守规则：
/// * 同名变量在全函数共用一格（`var_map` 以名字索引，发射期分不出作用域），
///   所以出现次数 >1 的名字在走作用域前**钉在**顶层，不参与回收；
/// * `switch` 的 prelude 与所有 case 共用一个作用域（C 语义：声明对后续 case 可见）。
struct ScopedVarAlloc<'a> {
    var_map: &'a mut HashMap<String, (i64, Type)>,
    maxima: &'a HashMap<String, (i64, Type)>,
    pinned: &'a HashSet<String>,
    static_local_map: &'a crate::static_locals::StaticLocalMap,
    func_name: &'a str,
    temp_slots: i32,
    /// 帧头里 callee-saved 保存槽的个数（§5.42；`compute_callee_plan` 的 popcount）。
    /// 槽数与 temp 槽区一起把变量区推下去，所以每个声明槽的偏移都跟着它走。
    cs_slots: i32,
    kasan_rz_slots: i32,
    over_align: usize,
    /// 当前水位（槽数，向下生长）
    total: i64,
    /// 历史最高水位（含已被回收的块）
    peak: i64,
    spill: i64,
}

/// 「有寄存器家的变量还占不占帧槽」（`DCC_FRAME_SKIP_HOMED`，只读一次）。
///
/// 规划期给体内**每一个**声明都留一个槽（[`ScopedVarAlloc`]），因为那时还不知道
/// 谁会拿到寄存器家。但 `v.home == Location::Reg(_)` 的变量在发射期每一次读写都
/// 直接走号，`var_offset` 根本不会被问到它 —— 槽是白留的。
///
/// 为什么值得（报告 §5.39）：`__schedule` 实测帧 992 B 里 `var` 占 776 B（97 槽），
/// 而反汇编只触及其中 24 个槽；同一函数 gcc 是 96 B。缺口不是「局部变量多」，是
/// 「内联展开的临时量每个都占一格，而它们绝大多数住在寄存器里」。
///
/// 安全性靠 `var_offset` 的 fail-closed 兜底：真有一个 Reg 家的名字被问到帧偏移，
/// 那里报 `E1134` 让本次编译失败，而不是静默按 `Type::Int` 猜一个槽读错宽度。
fn skip_homed_frame_slots() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DCC_FRAME_SKIP_HOMED").is_some())
}

/// 逐笔帧槽分配追踪（`DCC_SLOT_TRACE`，只读一次）。
fn slot_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DCC_SLOT_TRACE").is_some())
}

impl<'a> ScopedVarAlloc<'a> {
    /// 给一个声明分槽（同名已在 `var_map` 里就不动）。对齐圆整与 KASAN 红区口径
    /// 与原扁平实现逐字一致。
    fn alloc_one(&mut self, v: &VarDecl) {
        let name = &v.name.name;
        if slot_trace_on() || std::env::var_os("DCC_SLOT_HOME").is_some() {
            eprintln!(
                "SLOT-HOME {} name={} home={:?} skip={}",
                self.func_name,
                name,
                v.home,
                skip_homed_frame_slots()
            );
        }
        let (slots, alloc_ty) = match self.maxima.get(name) {
            Some((s, t)) => (*s, t.clone()),
            None => (Codegen::var_alloc_slots(&v.ty), v.ty.clone()),
        };
        if self.var_map.contains_key(name) {
            return;
        }
        // §5.39 实验：**已经拿到物理寄存器家**（`Location::Reg`）的变量不再占一个
        // 帧槽。它的每一次读写都走寄存器，`var_offset` 在发射期不会被问到它；
        // 真的问到了会在 `var_offset` 报 `E1134`（fail-closed），不是静默错值。
        //
        // 为什么值得：`__schedule` 实测 var=776 B（97 槽）里绝大多数是内联展开留下
        // 的 `__inlN$_p` 临时量，而反汇编只触及 24 个槽。gcc 对同一函数是 96 B。
        if skip_homed_frame_slots()
            && matches!(v.home, Some(crate::location::Location::Reg(_)))
        {
            return;
        }
        let mut used = self.total.saturating_mul(8);
        let total_before = self.total;
        let hdr = LayoutMap::hdr_of(self.temp_slots as i64, self.cs_slots as i64);
        // 过对齐局部：把游标上圆整, 使其基址
        // -(used+hdr)-(slots-1)*8 成为该变量有效对齐的整数倍。
        //
        // 以前这里只处理 `va > 16`（那时整帧会把 %r13 动态对齐到 va）。§5.42 把帧头
        // 从 `48+8t` 换成 `8+8cs+8t` 后，16 字节对齐的局部原先靠「hdr 与
        // (slots-1)*8 恰好凑成 16 倍数」的巧合被打破了（实测
        // `struct_field_aligned.c`：hdr=32、4 个槽 ⇒ 基址 -56，退码 6）。
        // 对齐是类型要求，不能依赖帧头的数值巧合 ⇒ va > 8 一律圆整。
        let va = v.alignment.unwrap_or(0).max(v.ty.align()).max(8) as i64;
        if va > 8 {
            let target = hdr + (slots.saturating_sub(1) * 8);
            let rem = (used + target).rem_euclid(va);
            if rem != 0 {
                used += va - rem;
            }
        }
        let off = -(used + hdr) - (slots.saturating_sub(1) * 8);
        self.var_map.insert(name.clone(), (off, alloc_ty));
        // 诊断窗口（`DCC_SLOT_TRACE`）：帧水位的每一笔都要能对账。实测 `__schedule` 的
        // `var=608`（76 槽）里只有 38 个不同偏移、且最深那一格在 -688 而前一笔只到 -384
        // ——没有逐笔水位就去猜「哪一笔把 _total 推上去」，那是编故事。
        if slot_trace_on() {
            eprintln!(
                "SLOT {} name={} off={} slots={} total_before={} total_after={}",
                self.func_name,
                name,
                off,
                slots,
                total_before,
                total_before.saturating_add(slots),
            );
        }
        let advanced = used.saturating_sub(self.total.saturating_mul(8));
        self.total = self.total.saturating_add(advanced / 8).saturating_add(slots);
        self.total = self.total.saturating_add(self.kasan_rz_slots as i64);
        if self.total > self.peak {
            self.peak = self.total;
        }
        for j in 0..self.kasan_rz_slots {
            let rz_name = format!("__kasan_rz_{}_{}", name, j);
            // 红区紧贴对象**下方**一个槽起算：`off` 已经是对象的最低槽，再减 `slots*8`
            // 就是往下多跳一个对象（实测 `struct S s; char b[8];` 里多槽变量的最后
            // 一条红区正好落在下一个变量 `b` 的槽上，libc 的 `memcpy` 被报
            // use-after-poison）。`self.total` 的推进是 `slots + kasan_rz_slots` 个连续
            // 槽，所以红区第 j 条必须是 `off - 8 - 8j`。
            let rz_off = off - 8 - j as i64 * 8;
            self.var_map.entry(rz_name).or_insert((rz_off, Type::Void));
        }
    }

    fn decl(&mut self, v: &VarDecl) {
        Codegen::count_spill_slot(v, &mut self.spill);
        if !self.pinned.contains(&v.name.name) {
            self.alloc_one(v);
        }
        // 初始化式里也能嵌着语句表达式块（`int r = ({…})`）：那里的声明属于同一帧。
        if let Some(init) = &v.init {
            self.expr(init);
        }
    }

    /// 一个块。`release=false` 用于函数体顶层 / switch 全体 / for 的 init 块：
    /// 这些声明的作用域比块本身长。
    fn block(&mut self, items: &[BlockItem], release: bool) {
        let base = self.total;
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.is_extern && v.init.is_none() {
                        continue;
                    }
                    if v.is_static
                        && self.static_local_map.is_static_local(self.func_name, &v.name.name)
                    {
                        continue;
                    }
                    self.decl(v);
                },
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => self.stmt(s),
            }
        }
        if release {
            self.total = base;
        }
    }

    /// 语句子树。穷举写：新增 `Stmt` 变体会直接编不过（与收集器同一条纪律）。
    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block { b, .. } => self.block(&b.items, true),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                self.expr(cond);
                self.stmt(then_);
                if let Some(e) = else_ {
                    self.stmt(e);
                }
            },
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                self.expr(cond);
                self.stmt(body);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                // `for (int i = 0; …)` 的 init 声明作用域覆盖整个循环：块形 init 必须
                // 在循环作用域内分配、循环结束才回收（按普通块走会提前释放）。
                let base = self.total;
                if let Some(i) = init {
                    match i.as_ref() {
                        Stmt::Block { b, .. } => self.block(&b.items, false),
                        other => self.stmt(other),
                    }
                }
                if let Some(c) = cond {
                    self.expr(c);
                }
                if let Some(st) = step {
                    self.expr(st);
                }
                self.stmt(body);
                self.total = base;
            },
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                // 整个 switch 一个作用域（C 语义：声明对后续 case 可见）。
                let base = self.total;
                self.expr(expr);
                self.block(prelude, false);
                for case in cases {
                    self.block(&case.body.items, false);
                }
                self.total = base;
            },
            Stmt::Label { stmt, .. } => self.stmt(stmt),
            Stmt::Invoke { call, .. } => self.stmt(call),
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => self.expr(e),
            Stmt::GotoIndirect { expr, .. } => self.expr(expr),
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    self.expr(x);
                }
            },
            Stmt::Return { val: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. }
            | Stmt::EhLandingPad { .. } => {},
        }
    }

    /// 表达式子树里的 `({...})` 块也能带声明。穷举写：新增 `Expr` 变体编不过。
    fn expr(&mut self, e: &Expr) {
        use crate::ast::Expr as E;
        match e {
            E::StmtExpr { block, .. } => self.block(&block.items, true),
            E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::Ident { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => self.expr(x),
            E::Offsetof { indices, .. } => {
                for x in indices {
                    self.expr(x);
                }
            },
            E::Binop { l, r, .. }
            | E::Assign { l, r, .. }
            | E::Comma { l, r, .. }
            | E::Index { arr: l, idx: r, .. } => {
                self.expr(l);
                self.expr(r);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                self.expr(cond);
                self.expr(then_);
                self.expr(else_);
            },
            E::Call { callee, args, .. } => {
                self.expr(callee);
                for x in args {
                    self.expr(x);
                }
            },
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        InitItem::Expr(x) => self.expr(x),
                        InitItem::Designated { designator, value } => {
                            self.designator(designator);
                            self.expr(value);
                        },
                    }
                }
            },
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    self.expr(x);
                }
            },
        }
    }

    fn designator(&mut self, d: &Designator) {
        match d {
            Designator::Field(_) => {},
            Designator::Index(x) => self.expr(x),
            Designator::Range(a, b) => {
                self.expr(a);
                self.expr(b);
            },
            Designator::Chain(items) => {
                for sub in items {
                    self.designator(sub);
                }
            },
        }
    }
}

/// 用户写了不是寄存器的寄存器名（与 `report_internal_bug` 不同：这是**用户代码错误**，
/// 不是后端不变量破坏）。
const DIAG_BAD_REG_NAME: &str = "E0316";

/// 发射期才登记的那族帧内对象的**需求清单**（§11.23 步 2）。
///
/// 四族的名字都要到发射期才知道：`__compound_lit_<pos>` 带发射序、`__vmsize_<key>` 带类型
/// 标签、`va_start` 的 ap 槽带实参名、`<agg_member_tmp>` 是发射器自己的暂存名 —— 它们都不在
/// `pre_register_vars` 能预知的名字集合里，但与局部变量共用**同一条**向下生长的变量区水位。
///
/// 为什么这份清单现在是「可枚举」的：`var_offset` 的懒注册兜底已被 `E1134` 封死（源码里来的
/// 名字必须在规划期登记完），剩下的新名字 `register_var` 就只有这四族，而每一族都住在源码
/// AST 的某个站点上 ⇒ 上界可以静态算准；算少了就是 `E1133`（账面超出）。
#[derive(Default)]
pub struct LateNeed {
    /// 每个复合字面量站点一份（名字含发射序 ⇒ 站点之间不复用槽）。
    compound_slots: i64,
    /// VM 记录类型的尺寸元信息键（发射期按 `struct:<tag>` / `union:<tag>` 复用一槽）。
    vmsize_keys: Vec<String>,
    /// `va_start` 的 ap 候选名（按名字复用；全局 ap 在发射点走 rip 相对，不占帧槽）。
    ap_names: Vec<String>,
    /// 聚合成员赋值的一般 RHS 暂存（固定名，全函数共用一槽）。
    agg_tmp: bool,
    /// 本函数里出现过的**最大向量字节数**（不是晚分配带的输入，是向量临时槽带的）。
    ///
    /// `vector_temp_base(slot, vec_size)` 把槽按 `slot * round8(vec_size)` 摆放，所以
    /// 整条带必须容得下「若干个最大向量」；固定 512B 只够 32 个 16B 槽或 4 个 128B 槽，
    /// 256B 向量的第二个槽就溢出到 bf_rmw/trampoline 带了。尺寸判据单源于
    /// `abi::layout::vec_band_reserve`。
    ///
    /// 它搭本结构那次穷举遍历的车而不是另起一个扫树器：`late_need_type` 已经逐节点看到
    /// 每个表达式与声明的类型（连 VLA 维度里的子类型都进去），新增一个遍历器只会多一处
    /// 「静默少扫一棵子树」的机会。
    max_vec_bytes: usize,
    /// 本函数是否需要帧底那条 **complex / 聚合 rvalue 物化带**（§5.12）。
    ///
    /// 搭 `max_vec_bytes` 同一趟穷举扫描：带为 0 后「漏扫一种来源」的代价由
    /// `LayoutMap::cx_rvalue_off` 的断言兑底（当场报错，不静默写坏相邻区域）。
    needs_cx_band: bool,
    /// 本函数里**嵌套最深的那条位域 RMW 链**的层数（0 = 没有写位域 / 位域自增的站点）。
    /// 它是 BF RMW 带的尺寸输入，单源于 `abi::layout::bf_rmw_band_reserve`（§5.12）。
    bf_rmw_depth: usize,
    /// 已展开过的 `Arc<Vec<Field>>` 字段表地址。
    ///
    /// 内核类型图是 DAG：`struct list_head` 这类枢纽被成千上万个结构体引用，`Ptr` 层
    /// 又无条件下探 ⇒ 按路径数展开是**指数级**。实测 `net/ipv4/tcp_input.o` 在 codegen
    /// 阶段长时间不出产物，gdb 栈一直停在 `late_need_type` 的 Struct 分支。同一份字段表
    /// 只需扫一遍：本函数收集的是「最大向量字节数 + VLA 尺寸站点」，重复扫描不产生新信息。
    ty_seen: HashSet<usize>,
}

impl LateNeed {
    /// 按 `gen_vm_type_bind` 的口径登记一个记录类型（两条判据都住在
    /// `Codegen::vm_size_key` / `Codegen::vm_record_needs_size_slot`，本函数不重答）。
    fn push_vmsize(&mut self, ty: &Type) {
        let Some(key) = Codegen::vm_size_key(ty) else {
            return;
        };
        if !Codegen::vm_record_needs_size_slot(ty) {
            return;
        }
        // 发射期同一个 key 只对应一槽（`vm_struct_size_slots` 命中就复用），所以这里去重
        let already = self.vmsize_keys.iter().any(|k| k == &key);
        if !already {
            self.vmsize_keys.push(key);
        }
    }

    /// 块级 struct/union 定义的那次绑定（`gen_vm_struct_bind` 把 `StructDef` 塑成
    /// `Type::Struct(名字, 字段, 对齐, 非 packed)` 再走同一个入口），键拼法与那里一致。
    fn push_vmsize_def(&mut self, tag: &str, fields: &[crate::ast::Field], is_union: bool) {
        let prefix = if is_union { "union" } else { "struct" };
        if !fields.iter().any(|f| f.ty.has_vla()) {
            return;
        }
        let key = format!("{}:{}", prefix, tag);
        if !self.vmsize_keys.iter().any(|k| k == &key) {
            self.vmsize_keys.push(key);
        }
    }

    /// `va_start` 的 ap 候选名（发射期按名字复用，多个 `va_start(ap,…)` 只一槽）。
    fn push_ap(&mut self, name: String) {
        if !self.ap_names.iter().any(|n| n == &name) {
            self.ap_names.push(name);
        }
    }
}

impl Codegen {
    pub fn reset_reg_tracker(&mut self) {
        // 占用真相只有一份：清的是 `RegBank` 里的两个位图与钉住集合。
        self.reg_bank.reset();
        // file-scope 的 `register asm` 绑定在 compile_func 开头就已经写进
        // reg_bindings, 而上面一行把占用清空。不重新 claim 就等于宣告
        // 「本函数不使用这些寄存器」, 序言里对应的保存会被 patch_callee_saves
        // 改成 nop —— 调用者存放在里面的值就此消失 (本地声明那条路径在绑定处
        // 当场 claim, 因为它的时机晚于本函数)。
        let bound: Vec<u8> = self
            .homes
            .values()
            .filter_map(|(l, _)| l.reg())
            .map(|r| r.n)
            .collect();
        for reg in bound {
            self.reg_bank.claim(reg);
        }
    }

    /// P12 Phase 5.2a：把「本函数里能把控制流带出保存/恢复区间的 asm goto」clobber 掉的
    /// 池号从临时值发号器里拿掉。
    ///
    /// 为什么不能只在发射点处理：守护是一对 `push`/`pop`，而 `asm goto` 的跳出路径上不
    /// 经过那个 `pop`。所以必须在函数入口就知道这件事（H1），不然第一个表达式已经把值
    /// 放进了那个号。
    ///
    /// 判据本身只有一份（`crate::ast_asm::escaping_asm_clobbers`）。分配器这一侧今天**不需要**
    /// 再读它：带 `Label`/`AsmGoto` 的函数根本进不了 SSA（`ssa::structure::is_structured`），
    /// 所以一个号也不会被自动绑到这些号上 —— 那条前提由用例
    /// `ssa::regalloc::tests::escaping_asm_functions_do_not_reach_the_allocator` 钉住；
    /// 它一变红，补判据的位置就只可能在本函数与 `plan_for` 这两处。两处各自写一遍
    /// 遍历就会漂移，而漂移的方向就是静默错值。
    /// 发射开始**之前**的一次 AST 事实收集（两件事共用同一次遍历）：
    ///   * 能跳出守护区间的 `asm goto` 所 clobber 的池号 ⇒ 禁作溅出槽；
    ///   * 模板里出现过 `std` ⇒ 本单元保留 `rep movs/stos` 前的 `cld`（见
    ///     [`crate::codegen::x86_64::Codegen::asm_sets_direction`]）。
    pub fn ban_escaping_asm_regs(&mut self, body: Option<&crate::ast::Block>) {
        let Some(b) = body else { return };
        let facts = crate::ast_asm::collect_asm_facts(b);
        // 单调或：见字段注释里「为什么不做每函数重置」。
        self.asm_sets_direction |= facts.sets_direction;
        for name in facts.clobbers {
            // 名字→号走本后端已有的那一份解析（`register asm` 与 clobber 同一口径）；
            // `ban` 对池外的号（caller-saved）直接返 false，所以 `"rcx"` 这种最常见的
            // clobber 不会改变本函数能用的溅出号。
            if let Some(reg) = Self::parse_clobber_reg_name(&name) {
                self.reg_bank.ban(reg);
            }
        }
    }

    pub fn stack_reserve_bytes(&self) -> i64 {
        // §5.12：恒返 0。
        //
        // 以前分两种：`red_zone` 目标 0/48，`-mno-red-zone`（内核）再加 128。两个数
        // 都没人按它寻址：
        //   * 那 48B「3 个 16 字节向量槽」与向量临时带重复计费 —— 向量临时槽的唯一
        //     入口是 `vector_temp_base` → `LayoutMap::vec_temp`，住在 `vec_reserve` 那条
        //     带里（`max_vec_bytes > 0` 时它至少 512B）；
        //   * 那 128B 也买不到「红区」：本后端把它加进 `frame_size`，于是它落在
        //     **%rsp 以上**的帧内空隙里，不是硬件/被调者会去碰的那 128 字节。对 16KB
        //     内核栈就是一个函数白贴 128B。
        //
        // 为什么本项与「帧底那摞带是否装得下」无关（这点容易误判，本轮就这么碎过一次
        // 帧）：带底约束 `cx+va+sc+8+vec+bf ≥ trampoline 实测用量` 里 `frame_size` 与
        // `stack_reserve` 全部抵消，本项既不提供也不拿走分隔量。真正托着 trampoline 的
        // 是 cx/bf/vec 三条带，见 `abi/layout.rs::BF_RMW_RESERVE` 与 `cx_band_reserve` 的
        // 门控条件。
        0
    }

    /// 用户写的寄存器文本名 → 号。名字表只有一份（`abi/regnames.rs`）：本函数以前是
    /// 三张同事实表里的那一张（它额外认识 APX r16-r31，并接受 `%` 前缀与大写），
    /// 现在保留同样的接受口径，只把「哪个名字是哪个号」的事实交给唯一名表。
    /// 接受集的分寸（哪些写法有 `%` 形态）由表里的 `percent_ok` 逐条描述，
    /// 所以合并不扩大也不缩小语言。
    pub fn parse_reg_name(name: &str) -> Option<u8> {
        crate::codegen::x86_64::abi::regnames::lookup_reg_name(
            name,
            &crate::codegen::x86_64::abi::regnames::NAME_FORMS_USER,
        )
        .map(|e| e.num)
    }

    /// clobber 名单里的文本名 → 号。与上面同一张表，**只差 APX 号**：`-mapxf` 下
    /// gcc 确实接受 `::: "r16"`，而 `register asm("r16")` 它是硬错误（全局寄存器
    /// 变量不能占 REX2 编码的号）。两者共用一个口径必有一侧与 gcc 相反，
    /// 所以 `ban_escaping_asm_regs` 走本函数。
    pub fn parse_clobber_reg_name(name: &str) -> Option<u8> {
        crate::codegen::x86_64::abi::regnames::lookup_reg_name(
            name,
            &crate::codegen::x86_64::abi::regnames::NAME_FORMS_CLOBBER,
        )
        .map(|e| e.num)
    }

    /// 文件作用域 `register asm` 绑到「会被调用毁掉」的寄存器时的警告码。与
    /// `DIAG_RESERVED_REG` 同一家族（031x = 寄存器名/绑定），但不占 E0317：那是一个
    /// 号码本身在本 target 不存在的错误，不是一句「能不能存住值」的告警。
    pub(crate) const DIAG_CALL_CLOBBERED_GLOBAL_BINDING: &'static str = "W0319";

    /// 文件作用域的 `register asm` 绑定如果落在一个会被调用毁掉的寄存器上，按 gcc 的
    /// 原话警告（Phase 5.1 前置三的第一块：这个事实以前只有编译器知道）。
    ///
    /// 为什么不保存、也不升成错误：gcc 就是这个形状（警告完照样把值放在那个寄存器里，
    /// 调用会毁掉它）。实测本端 -O0 的产物与 gcc 逐条一致（`mov %rax,%r11` … `call sink`
    /// … `mov %r11,%rax`），缺的只有这句话。
    ///
    /// **只适用于文件作用域**：函数内的 `register asm` 在 gcc 文档里明说不保证跨调用存活，
    /// 实测 gcc 对零诊断；本函数也只由 `compile_rel` 里那个文件作用域登记点调用。
    /// 消息正文与 gcc 逐字相同（用例可以直接 `contains` 两侧输出），变量名与号进 hint。
    pub(crate) fn diag_call_clobbered_global_binding(
        &mut self,
        v: &VarDecl,
        home: &crate::location::Location,
    ) {
        let Some(reg) = home.reg() else { return };
        if !crate::codegen::x86_64::abi::regbank::is_call_clobbered(reg.n) {
            return;
        }
        // Phase 3.2: 文本从家反查（`gp_name_of_num` 是号→名唯一入口），不再独立读
        // `register_name` —— 与判据同源。
        let text = crate::codegen::x86_64::abi::regnames::gp_name_of_num(reg.n).unwrap_or("");
        self.diagnostics.push(crate::diagnostics::Diagnostic {
            sev: crate::diagnostics::Severity::Warning,
            code: Self::DIAG_CALL_CLOBBERED_GLOBAL_BINDING,
            msg: "call-clobbered register used for global register variable".to_string(),
            span: v.span.clone(),
            hint: Some(format!(
                "'{name}' 绑在 `{text}` 上，而它不是 callee-saved：任何函数调用都会毁掉它。\
                 要跨调用保活就换 rbx/r12-r15，或改回普通全局变量",
                name = v.name.name
            )),
            source_line: None,
        });
    }

    /// 发一条 callee-saved 保存/恢复，并按「**真的发了**」登记补丁点。
    ///
    /// `patch_callee_saves` 的回填口径是「从记录的起点往后写 4 个 0x90」，而
    /// `emit_insn` 可能整条不发（P13 阶段6 的块内等价事实会把「槽里已经是这个号」的
    /// 恢复当恒等删掉）。一条没发的指令如果照样登记补丁点，那 4 个 0x90 就落在
    /// **下一条指令**上，把 `mov %r13,%rsp` 涂成 nop —— 实测症状是尾声只剩
    /// `pop %rbp; ret`、程序 SIGSEGV。所以补丁点只能与「字节真的在了」同生同灭。
    pub(crate) fn emit_callee_save_site(&mut self, bit: u8, insn: Insn) -> bool {
        let pos = self.pos();
        let handled = self.emit_insn(&insn);
        // `emit_insn` 对「被块内等价事实整条删掉」也返回 true（那是「已处理」而不是
        // 「发了字节」），所以判据只能是 `code` 真的长长了。
        if handled && self.pos() > pos {
            self.callee_save_patch_sites.push((pos, bit));
            true
        } else {
            false
        }
    }

    /// 回填本函数序言/尾声的 callee-saved 保存/恢复指令.
    /// 单遍编译下序言先于函数体发射, 无法预知函数体是否使用这些寄存器;
    /// 函数体生成完毕后 `RegBank::used_mask` 已完整, 将未使用寄存器的
    /// 4 字节 mov 改写为 4×0x90 (nop), 消除叶子函数的冗余保存/恢复.
    /// 尾声可能在每个 return 处内联发射, 统一在此处回填, 不依赖生成顺序.
    /// sites_base: compile_func 开头记录的 sites 起始下标 (嵌套函数隔离).
    pub fn patch_callee_saves(&mut self, sites_base: usize) {
        let used = self.reg_bank.used_mask();
        // §5.42 fail-closed：发射期用到的号必须是计划的一部分。
        // 图外号一旦被 claim/发出，帧头就没有它的保存槽 —— 此时把补丁点 nop 掉
        // 就是静默毁调用者。报错而不是“就势扩图”：帧大小、变量区顶都已按计划发射。
        //
        // 除「收尾时还握着」的号以外，还要算上 `plan_violation`：一个图外号可能被
        // claim 过又释放（`used_mask` 就不再含它），但存进去的值已经落在那个号里，
        // 帧头却没有槽。两个读数取并集才是「本函数碰过图外池号」的完整事实。
        let touched = used | self.reg_bank.plan_violation();
        let stray = touched & !self.callee_plan;
        if stray != 0 {
            self.report_internal_bug(
                "E1137",
                format!(
                    "本函数用了未规划的 callee-saved 号（used={:#06b} touched={:#06b} plan={:#06b}）：\
                     帧头没有它的保存槽，序言/尾声已按计划发射",
                    used, touched, self.callee_plan
                ),
                "把这类号的来源收进 `compute_callee_plan`（homes / 局部声明 / asm 固定约束），\
                 不得静默扩图",
            );
        }
        for (pos, bit) in self.callee_save_patch_sites.drain(sites_base..) {
            if used & bit == 0 {
                let p = pos as usize;
                self.code[p..p + 4].fill(0x90);
            }
        }
    }

    pub fn emit_store_rax_r13(&mut self, off: i64) {
        self.store_stack(0, off);
    }

    /// 用户文本名 → 家（后端事实：名字表只有 `parse_reg_name` 这一份）。
    /// 名字不是本目标已知的寄存器 ⇒ **报错**，而不是静默不绑。
    ///
    /// 实测过的静默后果（P12 Phase 4.1d 要消除的缺陷）：
    /// `register long gv asm("nosuchreg");` 在改前退出码 0，而产物里 `gv` 出现 **0 次** ——
    /// 全局变量因收集阶段 `continue` 不进 .data/.bss，又因 `rel_symbols` 判
    /// `register_name.is_some()` 不发符号，于是这个对象**整个消失**；而 gcc 在这里是
    /// `error: invalid register name for ‘gv’`。「一个名字没认出来」不可能比「声明不存在」
    /// 更接近用户意图，所以只能报错。
    pub(crate) fn resolve_asm_reg(&mut self, v: &VarDecl) -> Option<crate::location::Location> {
        let text = v.register_name.as_deref()?;
        if let Some(loc) = Self::parse_reg_home(text) {
            // GNU C 的局部寄存器变量只在「asm 操作数引用它」时才是可观察的约定
            // （GCC 文档的 supported use）；声明处有初值、又从不进 asm 操作数的绑定
            // 只剩普通局部变量的值语义，按普通变量处理（不钉号，保留号也不再是错误）。
            // `asmgoto-5.c` 的 14 个 `register int vN asm("rXX") = aN` 就是这个形状：
            // 14 个号把 GPR 占满，而 GCC 在这里根本不把它当回事（-O2 直接通过）。
            //
            // `v.home.is_none()` 把**分配器写回的** register_name（`apply_bindings`
            // 给自动绑定补的文本）排除在外：那一位只是家表的中间通道，不是用户请求。
            if v.home.is_none() && !self.asm_binding_is_observable(v) {
                return None;
            }
            // 名字认得，但这个号是本端自己的帧基座：拿它当家会让所有 `%r13+disp` 寻址
            // 一起失效。而今天它不报错：实测同一个 `register long v asm("r13")` 在 -O0
            // 真发出 `mov %rax,%r13`，在 -O2 则被静默降级成栈槽（`mov %rax,-0x48(%r13)`）
            // —— 同一份源两个档位两个答案，那比报错坏得多。
            if let Some(why) = self.reserved_reg_reason(&loc) {
                self.diag_reserved_reg(&v.name.name, text, why, &v.span);
                return None;
            }
            return Some(loc);
        }
        self.diagnostics.push(crate::diagnostics::Diagnostic {
            sev: crate::diagnostics::Severity::Error,
            code: DIAG_BAD_REG_NAME,
            msg: format!("invalid register name for '{}': '{}'", v.name.name, text),
            span: v.span.clone(),
            hint: Some("register asm(\"rN\") 里的 N 必须是本目标已知的寄存器名".to_string()),
            source_line: None,
        });
        None
    }

    /// 用户写的 `register asm("...")` 绑定是否有可观察的语义（`resolve_asm_reg` 的降级开关）。
    ///
    /// 两个来源，任一成立就必须钉号：
    ///   1. 名字出现在某条内联汇编的操作数里 —— GCC 保证这个操作数就落在指定的号；
    ///   2. 声明处没有初值 —— 首次读可能早于任何赋值，读的是那个号里的现值
    ///      （只读别名形态：`register unsigned long current_stack_pointer asm("rsp")`）。
    ///
    /// 其余形状按普通变量处理：值全部来自初始化式或后续赋值，住处不可观察。
    /// 判据只需要「名字在不在 asm 操作数集合里」，集合由 `collect_asm_operand_names`
    /// 在发射开始前收齐（并行 worker 也能自己收）。
    fn asm_binding_is_observable(&self, v: &VarDecl) -> bool {
        v.init.is_none() || self.asm_operand_names.contains(&v.name.name)
    }

    /// 保留号（栈指针 / 帧基址 / 帧指针）不得当变量之家：诊断码也只定在这里一处
    /// （`asm/emit.rs` 的 clobber 侧引用同一个常量）。
    pub(crate) const DIAG_RESERVED_REG: &'static str = "E0318";

    /// 同一条 asm 的两个输出被绑到同一块存储（`asm/opmap.rs` 的 fail-closed 闸门）。
    /// 与 `DIAG_RESERVED_REG` 同一家族（031x/032x = 寄存器名与操作数绑定），但不走
    /// `E0311` 那条「指令编不出来」的透传通道：这里编得出来，错的是**绑定**，
    /// 拿 `--asm-fallback=gas` 兼掉只会把一个后端缺陷变成没人看得到的默认行为。
    pub(crate) const DIAG_ASM_OPERAND_CONFLICT: &'static str = "E0321";

    /// 批 4 开关：`DCC_DISPTAB_RBP=0` 关掉「跳表基址常驻 `%rbp`」。
    ///
    /// 它是**归因用的闸门**，不是「要不要开」的悬案：默认开，关掉后产物与本轮之前逐字节
    /// 相同（`scripts/ab_pair.sh` 靠它做同轮配对）。env 只读一次，理由与 `blockopt::level()`
    /// 同一条：发射途中换规则会造出没人能复现的产物。
    fn disptab_rbp_on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| match std::env::var("DCC_DISPTAB_RBP") {
            Ok(v) => matches!(v.trim(), "1" | "true" | "yes" | "on"),
            Err(_) => true,
        })
    }

    /// 本函数要不要把一张跳表的基址常驻 `%rbp`，要的话是哪张（mangled 符号名）。
    ///
    /// 四条互斥全部是「`%rbp` 的所有权冲突」，一条不满足就整体不进（宁少削一条）：
    ///   1. `-fno-omit-frame-pointer`：`%rbp` 已是帧指针（序言 `push`、尾声 `pop`）。
    ///   2. `-fstack-protector*`：基址的存槽 `%r13-8` 就是金丝雀槽。
    ///   3. 函数体里有任何内联汇编：模板文本里怎么写 `%rbp` 不可追溯（名单守不住它）。
    ///   4. 嵌套函数：内层体的发射横跨外层文本，外层的 `%rbp` 约定穿不过去。
    /// 另外 `naked` 函数没有序言/尾声可插，`register asm("rbp")` 已在
    /// [`Self::reserved_reg_reason`] 处无条件拒掉（不然家与基址会同读一个号）。
    ///
    /// **第五条（不变式，不是保守）**：`disptab_base_reg_sym` 为 `Some` 必须与「序言真的
    /// 发了那对存/取」同生同灭。`compile_func` 里 callee-saved 那一段只走
    /// `f.name.name != "main" || self.relocatable` 那条分支，而 `main` 由内置链接器直链
    /// （`!relocatable`）时整个帧建立走另一条分支，那里没有存/取可插。不拒这一格就是：
    /// 分派拿一个从未赋值的 `%rbp` 当表基址，而 CFI 还声称调用者的值存在那个槽里
    /// （实测：`main` 里写 computed goto，`dcc -O2 m3.c -o x.o` 产出的 ET_EXEC 直接 SIGSEGV，
    /// `DCC_DISPTAB_RBP=0` 同输入正确）。
    pub(crate) fn decide_disptab_base_reg(&mut self, f: &FuncDecl) -> Option<String> {
        if !Self::disptab_rbp_on() {
            return None;
        }
        if self.target.frame_pointer || self.func_canary {
            return None;
        }
        if f.is_naked || f.is_nested {
            return None;
        }
        if f.name.name == "main" && !self.relocatable {
            return None;
        }
        let body = f.body.as_ref()?;
        let facts = crate::ast_dispatch::collect(body);
        if facts.any_asm || facts.any_nested_func {
            return None;
        }
        let table = facts.primary_table()?;
        // 必须是本函数的**静态局部**：只有它的地址是 data 段里的一个符号。
        // 栈上的数组（自动存储期）地址每次调用都不同，常驻一个寄存器就是错值。
        // 同名 static 多份时这里**按名字**分不清是哪一张表 (常驻错基址 =
        // 跳到另一张表), 而 `emit_hoisted_jumptable_jump` 侧的判据同样按名字。
        // 两张表同名时最优做法是不削这条 —— 宁可少优化一条。
        if self.static_local_map.count_by_name(&f.name.name, table) != 1 {
            return None;
        }
        let mangled = self
            .static_local_map
            .resolve(&f.name.name, table)?
            .to_string();
        Some(mangled)
    }

    /// 这个家落在的号是不是本端保留号（帧基址 / 帧指针）。
    ///
    /// **只拒这两个，不拒 `%rsp`**：内核与 libm 依赖的
    /// `register unsigned long current_stack_pointer asm("rsp")` 是一个合法且常见的
    /// 「只读别名」（本轮实测：把它拒了直接弄坏 `parallel_codegen_keeps_file_scope_register_asm_binding`）。
    /// 家为 `Reg(4)` 时读侧发的就是 `mov %rsp,%rax`，正是用户要的东西；写侧也是用户自己要求的。
    /// 而 `%r13`/`%rbp` 不一样：前者是本端定位帧的基址，后者要么是帧指针要么已
    /// 归跳表基址（阶段 6 批 4），被当家用都会让那一族的寻址整体失效。
    /// clobber 侧的 `%rsp` 仍被拒（它守无可守），见 `asm/emit.rs::asm_reject_reserved_clobber`。
    fn reserved_reg_reason(&self, loc: &crate::location::Location) -> Option<&'static str> {
        let n = loc.reg()?.n;
        match n {
            13 => Some("%r13 是本后端的帧基址：所有局部量都按 `%r13+disp` 寻址"),
            // 无条件拒 `%rbp`（阶段 6 批 4）：以前只在 `-fno-omit-frame-pointer` 档拒，
            // 而默认档下 `%rbp` 现在是跳表基址的常驻号 —— 它是否在某个函数里被占，
            // 要到发射本函数时才决定，而声明可能在任何地方。留一个「看运气」的窗口
            // 就是静默错值，所以这里不判档位。
            5 => Some(
                "%rbp 要么是帧指针（-fno-omit-frame-pointer），要么是本端跳表基址的常驻号\
                 （阶段 6 批 4）：两者都不许当变量之家",
            ),
            _ => None,
        }
    }

    fn diag_reserved_reg(
        &mut self,
        name: &str,
        text: &str,
        why: &str,
        span: &crate::diagnostics::Span,
    ) {
        use crate::diagnostics::{Diagnostic, Severity};
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: Self::DIAG_RESERVED_REG,
            msg: format!("register '{}': '{}' 是保留寄存器 —— {}", name, text, why),
            span: span.clone(),
            hint: Some(
                "本后端可当变量之家的 callee-saved 号只有 rbx/r12/r14/r15（%r13 是帧基址、\n\
                 %rbp 要么当帧指针要么当跳表基址的常驻号，都不行；%rsp 当只读别名是可以的）"
                    .to_string(),
            ),
            source_line: None,
        });
    }

    /// 声明发射点要不要为本变量登记帧槽（`DCC_FRAME_SKIP_HOMED` 的判据单源）。
    ///
    /// 规划期已经不给「有寄存器家」的标量留槽（`ScopedVarAlloc::alloc_one` +
    /// `plan_late_slot_budget` 的补登记过滤），但三个声明发射点仍无条件
    /// `register_var`；帧规划之后那一步就是**晚分配**，于是刚省下的槽以
    /// `E1133` 的形状回来（实测 `agg_member_store_base`：`rc` 与
    /// `__inl1$rv`/`__inl2$rv`/`__inl4$rv` 四个有家的名字全被捡回，预算只有 1 格）。
    ///
    /// 判据收窄到「这个声明真的需要一格内存」：
    /// * 家必须是 **GP 寄存器**（`mov_to_stack`/`mov_from_stack` 先看家；Fp 家没有
    ///   这条通道，`unplumbed_home` 会报错）；
    /// * VLA 的元数据槽、聚合类型的按地址初始化都离不开内存，照旧登记；
    /// * `_Complex`/向量/宽浮点不进 GP 家，判据本身把它们排除在外。
    ///
    /// 跳过的名字若真被 `var_offset` 问到，会报 `E1134`（fail-closed），不是静默错值。
    pub(crate) fn decl_skips_frame_slot(&self, v: &VarDecl) -> bool {
        if !skip_homed_frame_slots() {
            return false;
        }
        let home = v
            .home
            .or_else(|| self.auto_homes.get(&v.name.name).copied());
        let Some(crate::location::Location::Reg(r)) = home else {
            return false;
        };
        if r.bank != crate::location::RegBank::Gp {
            return false;
        }
        if Self::type_has_vla(&v.ty) || v.ty.size() > 8 {
            return false;
        }
        !matches!(
            v.ty,
            Type::Array(..) | Type::Struct(..) | Type::Union(..) | Type::Vector(..) | Type::Complex(..)
        )
    }

    /// 变量声明处的**家**写入的唯一入口（P12 Phase 4.1c）。
    ///
    /// 为什么要有它：同一件事（「这个声明要住哪个寄存器」）之前在 `compile_func` 与
    /// `gen_block_items` 各有一份拷贝（两处字面同形、改一处漏一处的形状）；而 §7.15/§7.19
    /// 反复实测到的结论是「消费者没读同一张表」就是静默错值的来源。家在这里只有一个
    /// 算式，插入后必然同步 `RegBank` 与钉住集合。
    ///
    /// 两个来源的优先级与今天一致：**用户写的文本名 > IR 位置表给出的分配结果**
    /// （`apply_bindings` 本来就不会覆盖已有的 `register_name`，而自动分配把
    /// `already-register-asm` 当拒因，所以两者不会同时出现在同一个变量上）。
    /// 返回是否真的绑上了。
    pub fn bind_decl_home(&mut self, v: &VarDecl) -> bool {
        // 地址被取走的对象不得绑到物理寄存器：绑定后那个栈槽再没人写过，`&v` 的读者
        // 拿到旧值/垃圾（GCC 遇到逸出同样是忽略绑定，而规划期总是先分好了槽，
        // 退回内存路径无风险）。
        if !self.register_binding_allowed(&v.name.name) {
            return false;
        }
        let home = match &v.register_name {
            // 用户文本名：非法就报错（见 `resolve_asm_reg`），不静默降级成内存变量。
            Some(_) => self.resolve_asm_reg(v),
            // 自动分配的结果：已是 `Location`，不再走名字这一圈。
            // 第三来源是后端自己挑的家（`auto_homes`）：没走 SSA 的函数拿不到
            // `v.home`，只能由 `plan_auto_homes` 在函数入口定好。查本表而不是
            // `homes`，是因为 `homes` 里还有同名的 **file-scope** 绑定 ——
            // 局部声明同名时那不是它的家。
            None => v
                .home
                .or_else(|| self.auto_homes.get(&v.name.name).copied()),
        };
        // 「家从哪来」才是保存通道的判据，而不是「有没有寄存器名」：`apply_bindings` 为了
        // 与旧代码同源，会给自动绑定也补写一个 `register_name` 文本（只在原本为 None 时写），
        // 所以名字那一位区分不出两者。而 `home` 只有一个写入点（分配器），用户显式绑定的
        // `home` 恒为 None —— 于是 `home.is_some()` 就是「这是分配器选的」。把这条记到
        // `explicit` 里，而不是在下一次改动时重新推一遍。
        //
        // 后端自选的家（`auto_homes`）同样不是 `explicit`：它是优化，必须对程序正确，
        // 也就是跨 call 保活（落在 callee-saved 上）；而 `explicit` 的语义是 GNU C
        // 明文的「调用后会毁掉」，两者在跳 call 时的处理完全不同。
        let explicit = v.home.is_none() && v.register_name.is_some();
        match home {
            Some(loc) => {
                self.insert_home(&v.name.name, loc, v.ty.clone(), explicit);
                true
            },
            None => false,
        }
    }

    /// 登记一个家，并同步占用事实。**只给声明处用**：file-scope 表 (`global_homes`) 的
    /// 重填走直接 `insert`，因为那条路径上旧代码不在这里 claim（它在 `reset_reg_tracker`
    /// 里做），而 `naked` 函数根本不会走到 reset —— 改在这里 claim 就是新增行为。
    /// 登记一个家，并同步占用事实。**只给声明处用**：file-scope 表 (`global_homes`) 的
    /// 重填走直接 `insert`，因为那条路径上旧代码不在这里 claim（它在 `reset_reg_tracker`
    /// 里做），而 `naked` 函数根本不会走到 reset —— 改在这里 claim 就是新增行为。
    ///
    /// `explicit` 区分「用户写了 `register asm(...)`」与「分配器选的」（调用方知道，
    /// 它就是从那个字段分流的），因为两者的跳 call 语义不同：前者「会被调用毁掉」是
    /// GNU C 明文语义（实测 gcc/dcc 两侧同值），后者是优化，必须对程序正确。
    pub(super) fn insert_home(
        &mut self,
        name: &str,
        loc: crate::location::Location,
        ty: Type,
        explicit: bool,
    ) {
        self.homes.insert(name.to_string(), (loc, ty.clone()));
        // 存/取窗口是按 8 字节搬的（`store_stack`/`load_stack`），所以宽于 8 字节的对象
        // 不能拿 caller-saved 家：只能存下半就会丢高半。这类家退回到内存，而不是
        // 默默存一半（错值比少一个寄存器难得多，也检不出来）。
        let wide_ext_home = !explicit
            && loc
                .reg()
                .is_some_and(|r| crate::ssa::regalloc::is_auto_ext_reg(r.n))
            && ty.size() > 8;
        let loc = if wide_ext_home {
            crate::location::Location::Stack(self.register_var(name, &ty))
        } else {
            loc
        };
        self.homes.insert(name.to_string(), (loc, ty.clone()));
        if let Some(r) = loc.reg() {
            // 绑定即占用该寄存器：必须保存 + 不得再当临时值发出（见 `RegBank::claim`）
            self.reg_bank.claim(r.n);
            // 住在 caller-saved 号里的**自动**家需要一个帧内保存槽（前置三）。
            if !explicit && crate::ssa::regalloc::is_auto_ext_reg(r.n) {
                let slot_name = format!("<{}#save>", name);
                let slot_off = self.register_var(&slot_name, &ty);
                self.ext_home_saves
                    .push(crate::codegen::x86_64::codegen::ExtHomeSave {
                        name: name.to_string(),
                        reg: r.n,
                        slot_off,
                        size: ty.size(),
                    });
            }
        }
    }

    /// 用户文本名 → 家（后端事实：名字表只有 `parse_reg_name` 这一份）。
    pub fn parse_reg_home(name: &str) -> Option<crate::location::Location> {
        Self::parse_reg_name(name).map(|n| {
            crate::location::Location::Reg(crate::location::RegId {
                bank: crate::location::RegBank::Gp,
                n,
            })
        })
    }

    pub fn ends_with_return(items: &[BlockItem]) -> bool {
        match items.last() {
            Some(BlockItem::Stmt(Stmt::Return { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Break { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Continue { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Block { b, .. })) => Self::ends_with_return(&b.items),
            Some(BlockItem::Stmt(Stmt::If { then_, else_, .. })) => {
                Self::ends_with_return(&[BlockItem::Stmt(*then_.clone())])
                    && else_
                        .as_ref()
                        .is_some_and(|e| Self::ends_with_return(&[BlockItem::Stmt(*e.clone())]))
            },
            _ => false,
        }
    }

    /// Collect all label names defined in a statement tree.
    /// Used for non-local goto detection in nested functions.
    pub fn collect_local_labels_stmt(stmt: &Stmt, out: &mut std::collections::HashSet<String>) {
        match stmt {
            Stmt::Label { name, stmt, .. } => {
                out.insert(name.name.clone());
                Self::collect_local_labels_stmt(stmt, out);
            },
            // C++ 零成本异常: 落地垫标签同样是本函数内定义的标签
            // (`goto` 可以指向它), 必须计入 non-local goto 判定。
            Stmt::EhLandingPad { label, .. } => {
                out.insert(label.name.clone());
            },
            Stmt::Invoke {
                call, landing_pad, ..
            } => {
                // `landing_pad == None` 是 lp=0 的「本帧不接管」区间, 没有标签可登记。
                if let Some(pad) = landing_pad {
                    out.insert(pad.name.clone());
                }
                Self::collect_local_labels_stmt(call, out);
            },
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        Self::collect_local_labels_stmt(s, out);
                    }
                }
            },
            Stmt::If { then_, else_, .. } => {
                Self::collect_local_labels_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_local_labels_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_local_labels_stmt(body, out);
            },
            Stmt::For { body, .. } => Self::collect_local_labels_stmt(body, out),
            Stmt::Switch { cases, prelude, .. } => {
                // `prelude` 里也能带标签（`switch (x) { out: case 1: … }`）：漏了它 E1102
                // 那一族就会以另一种形式回来（标签没被登记 ⇒ 发射期解析不到）。
                for item in prelude {
                    if let BlockItem::Stmt(s) = item {
                        Self::collect_local_labels_stmt(s, out);
                    }
                }
                for c in cases {
                    for item in &c.body.items {
                        if let BlockItem::Stmt(s) = item {
                            Self::collect_local_labels_stmt(s, out);
                        }
                    }
                }
            },
            _ => {},
        }
    }

    // 扫描最大表达式嵌套深度 (binop/call 各+1)
    pub fn max_expr_depth_stmt(items: &[BlockItem]) -> u32 {
        items
            .iter()
            .map(|item| match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    v.init.as_ref().map_or(0, Self::max_expr_depth_expr)
                },
                BlockItem::Stmt(s) => Self::max_expr_depth_stmt_inner(s),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn max_expr_depth_stmt_inner(s: &Stmt) -> u32 {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::max_expr_depth_expr(e)
            },
            Stmt::Return { val: None, .. } => 0,
            Stmt::If {
                cond, then_, else_, ..
            } => {
                let d = Self::max_expr_depth_expr(cond);
                d.max(Self::max_expr_depth_stmt_inner(then_)).max(
                    else_
                        .as_ref()
                        .map_or(0, |e| Self::max_expr_depth_stmt_inner(e)),
                )
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::max_expr_depth_expr(cond).max(Self::max_expr_depth_stmt_inner(body))
            },
            // `init`/`step` 里的表达式同样会占溅出槽（`for (i = f(); …; i += g())`）。
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => init
                .iter()
                .map(|s| Self::max_expr_depth_stmt_inner(s))
                .chain(cond.iter().map(Self::max_expr_depth_expr))
                .chain(step.iter().map(Self::max_expr_depth_expr))
                .max()
                .unwrap_or(0)
                .max(Self::max_expr_depth_stmt_inner(body)),
            Stmt::Block { b, .. } => Self::max_expr_depth_stmt(&b.items),
            // `switch` 的选择子、`prelude` 与每个 case 体都在同一帧里发射。
            // 漏掉 case 体时 `temp_slots` 只剩最小值 2，而 case 内深表达式会用
            // 到更深的溅出槽 —— 槽 d 的偏移在变量区上方重叠，参数槽被常量
            // 覆盖（实测 SQLite `SIX_BYTE_INT`：`buf` 被写成 256，`-O0…-O3`
            // 全部在 `sqlite3VdbeSerialGet` 读 `[0x100]` 段错误）。
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                let mut d = Self::max_expr_depth_expr(expr)
                    .max(Self::max_expr_depth_stmt(prelude));
                for c in cases {
                    if let Some(v) = &c.val {
                        d = d.max(Self::max_expr_depth_expr(v));
                    }
                    if let Some(v) = &c.range_end {
                        d = d.max(Self::max_expr_depth_expr(v));
                    }
                    d = d.max(Self::max_expr_depth_stmt(&c.body.items));
                }
                d
            },
            Stmt::GotoIndirect { expr, .. } => Self::max_expr_depth_expr(expr),
            // `goto` 指到的语句也在本帧里。
            Stmt::Label { stmt, .. } => Self::max_expr_depth_stmt_inner(stmt),
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => outputs
                .iter()
                .chain(inputs.iter())
                .map(Self::max_expr_depth_expr)
                .max()
                .unwrap_or(0),
            Stmt::Invoke { call, .. } => Self::max_expr_depth_stmt_inner(call),
            Stmt::EhLandingPad {
                object, selector, ..
            } => Self::max_expr_depth_expr(object).max(Self::max_expr_depth_expr(selector)),
            Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Fallthrough { .. }
            | Stmt::Goto { .. } => 0,
        }
    }

    fn max_expr_depth_expr(e: &Expr) -> u32 {
        match e {
            Expr::Binop { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Call { callee, args, .. } => {
                1 + Self::max_expr_depth_expr(callee).max(
                    args.iter()
                        .map(Self::max_expr_depth_expr)
                        .max()
                        .unwrap_or(0),
                )
            },
            Expr::Comma { l, r, .. } => {
                Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Assign { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Unary { e, .. }
            | Expr::Cast { e, .. }
            | Expr::AddrOf { e, .. }
            | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. }
            | Expr::Real { e, .. }
            | Expr::Imag { e, .. }
            | Expr::SizeofExpr { e, .. }
            | Expr::AlignofExpr { e, .. } => Self::max_expr_depth_expr(e),
            Expr::Index { arr, idx, .. } => {
                Self::max_expr_depth_expr(arr).max(Self::max_expr_depth_expr(idx))
            },
            Expr::Member { expr, .. } => Self::max_expr_depth_expr(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => Self::max_expr_depth_expr(cond)
                .max(Self::max_expr_depth_expr(then_))
                .max(Self::max_expr_depth_expr(else_)),
            // 语句表达式里的 binop 也吃同一批溅出槽。
            Expr::StmtExpr { block, .. } => Self::max_expr_depth_stmt(&block.items),
            Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
                Self::max_expr_depth_inits(inits)
            },
            Expr::AsmExpr {
                outputs, inputs, ..
            } => outputs
                .iter()
                .chain(inputs.iter())
                .map(Self::max_expr_depth_expr)
                .max()
                .unwrap_or(0),
            Expr::VaArg { ap, .. } => Self::max_expr_depth_expr(ap),
            Expr::Offsetof { indices, .. } => indices
                .iter()
                .map(|e| Self::max_expr_depth_expr(e))
                .max()
                .unwrap_or(0),
            // 叶子：不嵌套表达式，也不占溅出槽。
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::ComplexLit { .. }
            | Expr::ComplexIntLit { .. }
            | Expr::String { .. }
            | Expr::Char { .. }
            | Expr::Ident { .. }
            | Expr::SizeofTy { .. }
            | Expr::AlignofTy { .. }
            | Expr::LabelAddr { .. }
            | Expr::VaArgPack { .. } => 0,
        }
    }

    /// 初始化列表（复合字面量 / `InitList`）里的表达式深度。
    fn max_expr_depth_inits(inits: &[InitItem]) -> u32 {
        inits
            .iter()
            .map(|it| match it {
                InitItem::Expr(e) => Self::max_expr_depth_expr(e),
                InitItem::Designated { value, .. } => Self::max_expr_depth_expr(value),
            })
            .max()
            .unwrap_or(0)
    }

    // ========================================================================
    // sret / 物化槽字节水位静态扫描
    // ------------------------------------------------------------------------
    // 与运行时分配器 alloc_sret_slot 完全同口径: 每个槽按 16 字节对齐,
    // 自 sret_region_base 向下累计; 槽在其所属的“语句/块 item”边界死亡。
    // 扫描返回整函数任意执行路径上的最大同时存活字节水位, 供帧规划把
    // 槽区计入 frame_size (旧 bump 不做扫描, 槽位越过帧底覆盖 call
    // outgoing 区, cld 夹具在此段错误)。
    //
    // 建模规则 (必须与代码生成的 save/restore 挂钩点一一对应):
    //   * 一个 BlockItem 从 incoming 水位开始, 结束时回到 incoming —
    //     gen_block_items / 函数顶层循环在每个 item 边界 save/restore;
    //   * 单个表达式树内部不释放 (兄弟子表达式也不回收), 故表达式的
    //     “本树新增字节”对整棵子树求和 (过近似, 安全);
    //   * 控制流语句的条件用 incoming 求值; 每个体语句从其自己的
    //     incoming 起算 (体在 gen_stmt 外层 save/restore 内, 且 switch
    //     等的体可能复用外层为条件分配的槽);
    //   * switch 表达式分配的 16+8 槽存活至全部 case 体结束 — 取
    //     (24 + 体内最大水位)。
    pub fn max_sret_bytes(items: &[BlockItem]) -> i64 {
        items
            .iter()
            .map(Self::sret_bytes_block_item)
            .max()
            .unwrap_or(0)
    }

    fn sret_bytes_block_item(item: &BlockItem) -> i64 {
        match item {
            BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().map_or(0, Self::sret_bytes_expr),
            BlockItem::Stmt(s) => Self::sret_bytes_stmt(s),
            _ => 0,
        }
    }

    fn sret_bytes_stmt(s: &Stmt) -> i64 {
        match s {
            Stmt::Expr { e, .. } => Self::sret_bytes_expr(e),
            Stmt::Return { val: Some(e), .. } => Self::sret_bytes_expr(e),
            Stmt::Return { val: None, .. } => 0,
            Stmt::If {
                cond, then_, else_, ..
            } => Self::sret_bytes_expr(cond)
                .max(Self::sret_bytes_stmt(then_))
                .max(else_.as_ref().map_or(0, |e| Self::sret_bytes_stmt(e))),
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::sret_bytes_expr(cond).max(Self::sret_bytes_stmt(body))
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => init
                .iter()
                .map(|s| Self::sret_bytes_stmt(s))
                .chain(cond.iter().map(Self::sret_bytes_expr))
                .chain(step.iter().map(Self::sret_bytes_expr))
                .max()
                .unwrap_or(0)
                .max(Self::sret_bytes_stmt(body)),
            Stmt::Block { b, .. } => Self::max_sret_bytes(&b.items),
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                // expr 槽与范围比较低位槽：尺寸走**发射器那把尺**
                // （`Codegen::sret_slot_bytes`，每个槽向上取整到 16B），不在这里抄一个
                // 字面 24：两个 `alloc_sret_slot(16)` / `alloc_sret_slot(8)` 实际步进是
                // 16 + 16 = 32，而本扫描以前写 24 ⇒ 永远少算 8B（以前被每帧那堆
                // 无条件预留吃掉，没人看见）。
                let base = Self::sret_bytes_expr(expr)
                    + Self::sret_slot_bytes(16)
                    + Self::sret_slot_bytes(8);
                // `prelude` 也在同一帧里，它那次结构体返回同样要占槽。
                let body = prelude
                    .iter()
                    .chain(cases.iter().flat_map(|c| c.body.items.iter()))
                    .map(Self::sret_bytes_block_item)
                    .max()
                    .unwrap_or(0);
                // **求和而不是取 max**：switch 那两个槽（选择子 16B + 范围比较低位
                // 8B，`emit/stmt.rs` 里紧邻的两条 `alloc_sret_slot`）在整个 case 体
                // 期间一直活着，所以「体内峰值」是在它们之上再叠的。以前取 max 之所以
                // 看不出来，是因为每帧还平贴着一堆无条件预留（`stack_reserve` 128、
                // complex 带 48、位域 RMW 带 64），体内槽越界后落在那些地上也没人报警；
                // 三条带改成按需之后，同一笔欠账直接以 `E1128` 现形（实测
                // `switch_range_comparison_correctness` 夹具：计划 24B、实际 ≥ 32B）。
                base + body
            },
            Stmt::GotoIndirect { expr, .. } => Self::sret_bytes_expr(expr),
            // `goto` 指到的语句也在本帧里：它里面的 switch / 聚合赋值同样要占槽。
            // 以前这一族全部落到 `_ => 0`，于是「switch 套在标签后面」的函数（解析器
            // 一类是典型：`pr106101.c` 的 `yyparse`）预算算成 0，发射期当场 `E1128`。
            // 这里不写 `_ => 0`：新增 `Stmt` 形态时必须在这里显式表态。
            Stmt::Label { stmt, .. } => Self::sret_bytes_stmt(stmt),
            Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Fallthrough { .. }
            | Stmt::Goto { .. } => 0,
            // C++ 零成本异常：受保护调用的体里可以有聚合赋值；落地垫只接两个寄存器。
            Stmt::Invoke { call, .. } => Self::sret_bytes_stmt(call),
            Stmt::EhLandingPad { object, selector, .. } => {
                Self::sret_bytes_expr(object).max(Self::sret_bytes_expr(selector))
            },
            // asm 语句的操作数也可以含聚合赋值（`(struct S){...}` 等）。
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => outputs
                .iter()
                .chain(inputs.iter())
                .map(Self::sret_bytes_expr)
                .max()
                .unwrap_or(0),
        }
    }

    /// 表达式树在一次求值内“新增的 sret/物化字节”峰值。按当前代码生成,
    /// 表达式求值期间不释放任何槽, 故对各分配站点与其子树求和。
    fn sret_bytes_expr(e: &Expr) -> i64 {
        let own = match e {
            Expr::Call {
                ty, callee, args, ..
            } => {
                let sret = if Self::needs_memory_return(ty) {
                    Self::sret_slot_bytes(ty.size() as i64)
                } else {
                    0
                };
                // _Complex long double 返回 (经 x87, needs_memory_return
                // == false) 仍在调用侧复制到固定 result_off, 不经过
                // alloc_sret_slot, 故不占本区。
                //
                // 但**按值传的 `_Complex long double` 实参**要在调用前各落一个
                // 32B 物化槽 (`gen_complex_ld_materialize`): `complex-7.c` 的
                // `check_long_double(0, ld1…ld5)` 就是五次 32B。旧版本靠每帧
                // 恒 512B 的向量带兜住这笔账，`vec_band_reserve` 改成「无向量
                // 0 预留」后立刻变成 E1128。
                let arg_slots: i64 = args
                    .iter()
                    .map(|a| Self::complex_ld_slot(&a.ty()))
                    .sum();
                sret
                    + arg_slots
                    + std::iter::once(callee.as_ref())
                        .map(Self::sret_bytes_expr)
                        .chain(args.iter().map(Self::sret_bytes_expr))
                        .max()
                        .unwrap_or(0)
            },
            Expr::VaArg { ty, ap, .. } => {
                // 只有真正走 `alloc_sret_slot` 的缓冲区交付才占本区；复数
                // `va_arg` 直接落帧底固定的复数物化带 (与 `gen_expr_va_arg`
                // 同源，见 `va_arg_result_in_cx_band`)。
                let own =
                    if Self::va_arg_result_in_memory(ty) && !Self::va_arg_result_in_cx_band(ty) {
                        Self::sret_slot_bytes(ty.size() as i64)
                    } else {
                        0
                    };
                own + Self::sret_bytes_expr(ap)
            },
            Expr::Binop { l, r, .. } => {
                // _Complex long double 二元: 两个 32B 物化槽同时存活
                // (spill l, spill r), 结果走固定 result_off 不占本区。
                //
                // 比较运算符 (`a1 != ld1`, 结果类型是 int) 不在
                // `complex_ld_sret_binop` 的判据里, 但两个操作数照样各落一槽
                // (complex.rs 的比较分支就是两次 `spill_complex_ld_operand`),
                // 所以这里按**操作数类型**补登记; 与结果侧取 max, 避免同一个
                // 槽被算两遍。
                let operand_slots = Self::complex_ld_slot(&l.ty())
                    + Self::complex_ld_slot(&r.ty());
                // 左右子表达式**求和**，不取 max：发射侧是 bump 分配，表达式树
                // 内部不回收（`||`/`&&` 短路的左支槽也要活到整个条件求值完），
                // 取 max 会让 `a1!=ld1 || a2!=ld2 || …` 只算一个 64B —— 实测
                // `complex-7.c` 的 `check_long_double` 就是五段串联，水位 320B。
                Self::complex_ld_sret_binop(e).max(operand_slots)
                    + Self::sret_bytes_expr(l)
                    + Self::sret_bytes_expr(r)
            },
            Expr::Assign { l, r, .. } => {
                // 聚合赋值走「目标地址先落一个 sret 槽」那一形时，本语句额外占一个
                // 8 字节地址槽（发射侧 `dispatch.rs` 的 `AggDst::Indirect` 分支）。两侧必须用
                // **同一个谓词** `assign_needs_addr_slot`：实测 `block/blk-mq.o` 的
                // `blk_mq_extract_queue_requests`（`*queue_rqs = matched_rqs;`）因为这里只算
                // `l + r` 两个子表达式（都不是 Call）而报 `sret_need=0`，发射侧第一个槽就压穿
                // spill 带顶 ⇒ E1128。
                let lv = crate::ast::peel_instrumented_lvalue(l);
                let addr_slot = if Self::assign_needs_addr_slot(lv, &lv.ty(), r) {
                    // 步长用分配器那把尺（`sret_slot_bytes`），不能登记原始 8 字节：
                    // 分配器按 16B 步进，按 8 登记就是预算永远少一半。
                    Self::sret_slot_bytes(8)
                } else {
                    0
                };
                addr_slot + Self::sret_bytes_expr(l) + Self::sret_bytes_expr(r)
            },
            Expr::StmtExpr { block, ty, .. } => {
                // 聚合 StmtExpr 分配一个类型槽; 内部块另算
                let own = if ty.size() > 0 {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                own + Self::max_sret_bytes(&block.items)
            },
            Expr::Comma { l, r, .. } => Self::sret_bytes_expr(l).max(Self::sret_bytes_expr(r)),
            Expr::Unary { e: inner, .. }
            | Expr::Cast { e: inner, .. }
            | Expr::AddrOf { e: inner, .. }
            | Expr::Deref { e: inner, .. }
            | Expr::PostInc { e: inner, .. }
            | Expr::PostDec { e: inner, .. }
            | Expr::PreInc { e: inner, .. }
            | Expr::PreDec { e: inner, .. } => Self::sret_bytes_expr(inner),
            Expr::Index { arr, idx, .. } => {
                Self::vla_offset_scratch(&arr.ty())
                    + Self::sret_bytes_expr(arr).max(Self::sret_bytes_expr(idx))
            },
            Expr::Member { expr, .. } => {
                // 含 VLA 字段的聚合成员访问需要若干运行时 `sizeof` 槽（`emit/vla.rs`
                // 对每个前置 VLA 字段一次 `alloc_sret_slot(8)`）；以前这一笔没人登记，
                // 靠帧底的无条件预留吸掉（实测 `pr82210.c` 的 `foo`：预算 0B、实际 32B）。
                Self::vla_offset_scratch(&expr.ty()) + Self::sret_bytes_expr(expr)
            },
            Expr::Cond {
                cond,
                then_,
                else_,
                ty,
                ..
            } => {
                // 聚合三元 (`c ? s1 : s2`) 在物化带里占一个类型宽度的临时槽:
                // 两支各自整块拷进该槽, 交付的是槽地址 (`gen_expr_cond`)。
                let own = if Self::value_is_whole_copy(ty)
                    && matches!(ty, Type::Struct(..) | Type::Union(..))
                {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                own + Self::sret_bytes_expr(cond)
                    .max(Self::sret_bytes_expr(then_))
                    .max(Self::sret_bytes_expr(else_))
            },
            Expr::AsmExpr {
                outputs, inputs, ..
            } => {
                // 非 Ident 输出（`"=a"(*eax)` / `"=r"(a[i])`）的写回地址必须在模板
                // 之前算好。驻足池（rbx/r12/r14/r15）被变量的家与本句 asm 的固定操作数
                // 占满时，发射侧退到 `alloc_sret_slot(8)`（步长用 `sret_slot_bytes`，
                // 不统计在这里就是 E1128）。多算无害：发射侧只在驻足失败时才落槽。
                let addr_slots = outputs
                    .iter()
                    .filter(|o| !matches!(o, Expr::Ident { .. }))
                    .count() as i64
                    * Self::sret_slot_bytes(8);
                addr_slots
                    + outputs
                        .iter()
                        .chain(inputs.iter())
                        .map(Self::sret_bytes_expr)
                        .max()
                        .unwrap_or(0)
            },
            _ => 0,
        };
        // 叶子形态的 `_Complex long double` rvalue 在消费端同样落 32B 物化槽
        // (`gen_complex_ld_materialize`): 初始化 (`z = w`)、赋值、cast、按值
        // 实参都以这些形态出现。Call/Binop/Assign/Comma/Cond/StmtExpr/VaArg/
        // AsmExpr 已在各自分支登记 (或由递归覆盖), 这里不重复计入。
        let leaf_slot = match e {
            Expr::Call { .. }
            | Expr::Binop { .. }
            | Expr::Assign { .. }
            | Expr::Comma { .. }
            | Expr::Cond { .. }
            | Expr::StmtExpr { .. }
            | Expr::VaArg { .. }
            | Expr::AsmExpr { .. }
            | Expr::Unary { .. } => 0,
            _ => Self::complex_ld_slot(&e.ty()),
        };
        own + leaf_slot
    }

    /// 含 VLA 字段的聚合类型做一次成员/下标访问要多少帧内偏移暂存。
    ///
    /// 口径：基类型里每个带 VLA 的字段一份，每份走分配器那把尺（`sret_slot_bytes`，
    /// 16B 步进）。数组基址不是记录类型时也给一份（访问同样要算行偏移）。
    /// 宁可多给：少给就是发射期 `E1128`，多给只是这一帧大 16 字节。
    fn vla_offset_scratch(ty: &Type) -> i64 {
        let n: i64 = match ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                fields.iter().filter(|f| f.ty.has_vla()).count() as i64
            },
            other => {
                if other.has_vla() {
                    1
                } else {
                    0
                }
            },
        };
        n.max(0) * Self::sret_slot_bytes(8)
    }

    /// `_Complex long double` 表达式在消费端物化一槽的字节数。
    ///
    /// 这是规划期与发射期 (`emit/expr/complex.rs::gen_complex_ld_materialize`
    /// 的 `alloc_local(32)`) 之间的唯一口径: 宽度写在两边就是又一个 E1128 源。
    fn complex_ld_slot(ty: &Type) -> i64 {
        if matches!(ty, Type::Complex(q) if matches!(q.base, Type::LongDouble)) {
            32
        } else {
            0
        }
    }

    /// 若表达式是 `_Complex long double` 二元 (含一元负/conj 等物化点),
    /// 返回其直接物化槽字节数; 否则 0。
    fn complex_ld_sret_binop(e: &Expr) -> i64 {
        let is_cld = |ty: &Type| {
            matches!(
                ty,
                Type::Complex(q) if matches!(q.base, Type::LongDouble)
            )
        };
        match e {
            Expr::Binop { ty, .. } if is_cld(ty) => 64, // 两个 32B 物化槽
            Expr::Unary { ty, .. } if is_cld(ty) => 32,
            _ => 0,
        }
    }

    /// 单个变量声明占用的 8 字节栈槽数 (分配口径的单一真源)。
    /// VLA 追加 16 字节元信息槽 (基址 + 运行时行大小 + 声明时总大小)。
    /// pre_register_vars 与 register_var 必须共用本函数, 否则两处对
    /// “变量占多大槽”的理解会漂移。
    pub fn var_alloc_slots(ty: &Type) -> i64 {
        let alloc_sz = if Self::type_has_vla(ty) {
            ty.size().saturating_add(16)
        } else {
            ty.size()
        };
        ((alloc_sz.saturating_add(7) / 8).max(1)) as i64
    }

    /// 本函数所有帧内局部变量的最大有效对齐 (`__attribute__((aligned(N)))` /
    /// `alignas` 与类型自然对齐取大)。仅 N>16 才需要序言动态对齐 %r13 (pr16660)。
    /// 复用 `collect_frame_var_decls` 的同一套收集口径, 保证与 pre_register_vars
    /// 看到的是同一批变量。
    pub fn max_local_over_align(
        items: &[BlockItem],
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
    ) -> usize {
        let mut decls: Vec<&VarDecl> = Vec::new();
        Self::collect_frame_var_decls(items, static_local_map, func_name, &mut decls);
        let mut max_align: usize = 0;
        for v in &decls {
            let eff = v.alignment.unwrap_or(0).max(v.ty.align()).max(1);
            if eff > max_align {
                max_align = eff;
            }
        }
        max_align
    }

    /// 扫一个函数体，得到「发射期才会冒出来的帧内对象」的需求清单（`LateNeed`）。
    /// 不下降进嵌套函数体（那是它自己一帧的事，与 `collect_frame_var_decls` 同口径）。
    pub fn late_need(items: &[BlockItem]) -> LateNeed {
        let mut need = LateNeed::default();
        Self::late_need_block(items, &mut need);
        need
    }

    fn late_need_block(items: &[BlockItem], need: &mut LateNeed) {
        // 位域 RMW 链深度按**块**算一次，而不是逐表达式算：`bf_rmw_chain_depth` 自己会
        // 递归整棵子树，挂在 `late_need_expr` 上就变成对每个节点重扫一遍子树（O(n²)）。
        // §5.13 量出来的结论止于此：-O2 的 27% 已经在分配器上，不该再送一层遍历。
        let d = Self::bf_rmw_depth_items(items);
        if d > need.bf_rmw_depth {
            need.bf_rmw_depth = d;
        }
        for item in items {
            match item {
                BlockItem::Decl(d) => Self::late_need_decl(d, need),
                BlockItem::Stmt(s) => Self::late_need_stmt(s, need),
            }
        }
    }

    /// 穷举写：新增 `Decl` 变体会直接编不过，而不是静默少扫一棵子树（§11.23 当年
    /// 就是被「静默少扫」坑的：扫描没数到的那一族到发射期就把预算吃穿了）。
    fn late_need_decl(d: &Decl, need: &mut LateNeed) {
        match d {
            Decl::Var(v) => {
                // 定义+声明一体的局部记录类型也会走一次 `gen_vm_type_bind`
                // (`emit/functions.rs` / `emit/stmt.rs` / 语句表达式那份)
                need.push_vmsize(&v.ty);
                Self::late_need_type(&v.ty, need);
                if let Some(e) = &v.init {
                    Self::late_need_expr(e, need);
                }
            },
            // 块级 struct/union 定义：`gen_vm_struct_bind` 同样按 tag 登一个尺寸槽
            Decl::StructDef(sd) => need.push_vmsize_def(&sd.name.name, &sd.fields, false),
            Decl::UnionDef(sd) => need.push_vmsize_def(&sd.name.name, &sd.fields, true),
            // typedef 不发射运行时代码（`typedef int arr[n];` 的维度也没人求值）；
            // 嵌套函数自己一帧；函数声明不携带站点。
            Decl::Typedef { .. } | Decl::Func(_) | Decl::EnumDef(_) => {},
            // 文件作用域 asm（块级也能写）的操作数是普通表达式，里面有复合字面量与
            // `va_start` 的可能。
            Decl::GlobalAsm {
                outputs, inputs, ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    Self::late_need_expr(e, need);
                }
            },
        }
    }

    /// 类型树里携带的运行时表达式（只有一种形态：VLA 维度 `ArraySize::Var`）也要扫：
    /// 它们是序言/声明点用 `gen_expr` 求值的，里面可以有复合字面量与聚合成员赋值。
    fn late_need_type(ty: &Type, need: &mut LateNeed) {
        match ty {
            // 复数/虚数类型 ⇒ 本函数会走帧底那条物化带（消费点全部由 `Type::Complex` /
            // `Type::Imaginary` 把门，判据单源于 `abi::layout::cx_band_reserve`）。
            Type::Complex(q) | Type::Imaginary(q) => {
                need.needs_cx_band = true;
                Self::late_need_type(&q.base, need)
            },
            Type::Ptr(q) | Type::Atomic(q) => Self::late_need_type(&q.base, need),
            Type::Vector(q, n) => {
                // 向量元素数可以是 0（`vector_size(0)` 非法但解析器不拦），尺寸按
                // `Type::size()` 的口径算，本函数不另建一套乘法。
                let bytes = Type::Vector(q.clone(), *n).size();
                if bytes > need.max_vec_bytes {
                    need.max_vec_bytes = bytes;
                }
                Self::late_need_type(&q.base, need)
            },
            Type::Array(elem, ArraySize::Var(se)) => {
                Self::late_need_expr(se, need);
                Self::late_need_type(&elem.base, need);
            },
            Type::Array(elem, _) => Self::late_need_type(&elem.base, need),
            Type::Func(ret, params) => {
                Self::late_need_type(&ret.base, need);
                for p in params {
                    Self::late_need_type(&p.base, need);
                }
            },
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                if !need.ty_seen.insert(std::sync::Arc::as_ptr(fields) as usize) {
                    return;
                }
                for f in fields.iter() {
                    Self::late_need_type(&f.ty, need);
                }
            },
            _ => {},
        }
    }

    /// 与 `max_sret_bytes` 同骨架的语句遍历（那条轴算 sret 区，本条算变量区晚分配）。
    /// 穷举到每一条臂：`Invoke` 的受保护调用体、`EhLandingPad` 的两个接收者、标号体、
    /// `asm goto` 的操作数里全可以有复合字面量与聚合成员赋值。
    fn late_need_stmt(s: &Stmt, need: &mut LateNeed) {
        match s {
            Stmt::Expr { e, .. } => Self::late_need_expr(e, need),
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    Self::late_need_expr(e, need);
                }
            },
            Stmt::Block { b, .. } => Self::late_need_block(&b.items, need),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::late_need_expr(cond, need);
                Self::late_need_stmt(then_, need);
                if let Some(e) = else_ {
                    Self::late_need_stmt(e, need);
                }
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::late_need_expr(cond, need);
                Self::late_need_stmt(body, need);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::late_need_stmt(i, need);
                }
                if let Some(c) = cond {
                    Self::late_need_expr(c, need);
                }
                if let Some(st) = step {
                    Self::late_need_expr(st, need);
                }
                Self::late_need_stmt(body, need);
            },
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                Self::late_need_expr(expr, need);
                // `switch (x) { int y = 1; case 0: … }` 里那个 `y` 也是本帧的晚期分配对象：
                // 扫描不数它，发射期就会报 `E1133`（实测：静态预算 0 reserved、用了 1 slot）。
                Self::late_need_block(prelude, need);
                for c in cases {
                    Self::late_need_block(&c.body.items, need);
                }
            },
            Stmt::Label { stmt, .. } => Self::late_need_stmt(stmt, need),
            Stmt::GotoIndirect { expr, .. } => Self::late_need_expr(expr, need),
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    Self::late_need_expr(e, need);
                }
            },
            Stmt::Invoke { call, .. } => Self::late_need_stmt(call, need),
            Stmt::EhLandingPad {
                object, selector, ..
            } => {
                Self::late_need_expr(object, need);
                Self::late_need_expr(selector, need);
            },
            // 这四条不携带表达式
            Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. } => {},
        }
    }

    /// `e` 是不是一个**位域**成员访问（`.` / `->` 落在带 `bit_width` 的字段上）。
    ///
    /// 字段解析与发射器用**同一个** `crate::ast::find_field`（支持嵌套匿名 struct/union）：
    /// 自己只扫一层字段表会把 `struct { union { struct { u64 lo:8; }; u64 val; } };`
    /// 这种扁平访问看成普通字段（内核 `arch/x86/events/amd/ibs.c` 与
    /// `arch/x86/kernel/cpu/microcode/amd.c` 就藏在这里，实测直接 `bf_rmw_slot` 断言）。
    ///
    /// **解析不出来时一律保守答「是」**：基类型不是记录、字段名查不到（发射器还有
    /// `resolve_complete_struct_ty` 那条按 tag 补全的路，本静态扫拿不到那张表）、
    /// K&R 形参、`Type::Named(..)`——漏答是静默往带外写，多答只是多 32B。
    fn member_names_bitfield(e: &Expr) -> bool {
        use crate::ast::Expr as E;
        let E::Member { expr: base, field, is_arrow, .. } = e else {
            return false;
        };
        let base_ty = if *is_arrow {
            match base.ty() {
                Type::Ptr(q) => q.base,
                // 形参类型还没解成指针、或走了 typedef 别名：无法证明不是位域。
                _ => return true,
            }
        } else {
            base.ty()
        };
        match &base_ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                match crate::ast::find_field(fields, &field.name) {
                    Some((f, _)) => f.bit_width.is_some(),
                    // 发射器能解而本扫解不出（tag 未在本类型树里展开）⇒ 保守当作位域。
                    None => true,
                }
            },
            // 基址不是记录类型（未解析的具名类型、数组基址等）⇒ 无法证明不是位域。
            _ => true,
        }
    }

    /// 本表达式是不是一个「会被物化到帧底带上的 ≤16B 聚合 rvalue」。
    ///
    /// 为什么不按「所有 struct/union 类型的表达式」算：那会把 `s.a = 1` 这类纯 lvalue
    /// 访问也算进来，而它们只走地址。发射器那几处 spill 都发生在「拿不到地址的
    /// 记录类型右值」上：函数返回值的成员链、复合字面量/初始化列表、聚合赋值的
    /// 源、语句表达式与转换的结果、`? :` 两个分支都是非 lvalue 时的结果、`va_arg` 取出的
    /// 记录（实测 `(r ? p : q).z[baz] = 1;` 就是 `Cond` 那一形，漏它则 `cx_rvalue_off` 当场报）。
    ///
    /// 这也是一份**名单**而不是完备判据，所以下面那条断言是它的兑现：漏一种形态就是
    /// 编译错误（torture 全量与内核全树跑一遍就能把缺的形状抓出来），而不是静默把值
    /// 写到变参保存区/向量带上。
    fn expr_materializes_small_aggregate(e: &Expr) -> bool {
        use crate::ast::Expr as E;
        if !matches!(
            e,
            E::Call { .. }
                | E::CompoundLit { .. }
                | E::InitList { .. }
                | E::StmtExpr { .. }
                | E::Cast { .. }
                | E::Real { .. }
                | E::Imag { .. }
                | E::Assign { .. }
                | E::Cond { .. }
                | E::VaArg { .. }
        ) {
            return false;
        }
        let ty = e.ty();
        if !matches!(ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
            return false;
        }
        let bytes = ty.size();
        bytes > 0 && bytes <= 16
    }

    /// 以 `e` 为根的**位域读-改-写链**最大层数（0 = 本子树里没有位域写）。
    ///
    /// 与发射器同形状：`dispatch.rs` 的位域写在算 RHS 前占一层、`increment.rs::gen_bitfield_inc_dec`
    /// 在本层槽分配后把深度往前推一层，所以链式写法同时占多层（实测
    /// `s.a = (s.b = (s.c = …))` 就是九层）。这里按「自身是 RMW 站点则 1 + 子节点最大值，
    /// 否则取子节点最大值」递归，对发射器是上估（多算不会错，少算会写带外）。
    pub fn bf_rmw_chain_depth(e: &Expr) -> usize {
        use crate::ast::Expr as E;
        let sub: Vec<&Expr> = match e {
            E::Binop { l, r, .. } | E::Comma { l, r, .. } | E::Index { arr: l, idx: r, .. } => {
                vec![l.as_ref(), r.as_ref()]
            },
            E::Assign { l, r, .. } => vec![l.as_ref(), r.as_ref()],
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => vec![x.as_ref()],
            E::Cond { cond, then_, else_, .. } => vec![cond.as_ref(), then_, else_],
            E::Call { callee, args, .. } => {
                let mut v: Vec<&Expr> = vec![callee.as_ref()];
                v.extend(args.iter());
                v
            },
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                use crate::ast::InitItem;
                inits
                    .iter()
                    .filter_map(|it| match it {
                        InitItem::Expr(x) => Some(x),
                        InitItem::Designated { value, .. } => Some(value),
                    })
                    .collect()
            },
            E::StmtExpr { block, .. } => return Self::bf_rmw_depth_block(block),
            E::AsmExpr { outputs, inputs, .. } => {
                return outputs
                    .iter()
                    .chain(inputs.iter())
                    .map(|x| Self::bf_rmw_chain_depth(x))
                    .max()
                    .unwrap_or(0);
            },
            // 叶子与其他形态不带位域写。
            _ => vec![],
        };
        let child = sub
            .iter()
            .map(|x| Self::bf_rmw_chain_depth(x))
            .max()
            .unwrap_or(0);
        // 本节点自己是不是一个位域 RMW 站点。写（含复合赋值）的目标是左值 `l`，
        // 自增自减的目标是 `x`；只读位域不占槽。
        //
        // 复合字面量也是站点：`aggregate.rs::gen_init_list_at` 对位域成员
        // 走 3 槽 RMW（地址/容器/新值），与 `Assign` 一样占一层带。漏了它
        // `bf_rmw_reserve` 就是 0，而发射侧仍会要槽（§5.42 之后直接断言；
        // 更早是写死 -24/-32/-40 落进变量区/保存区）。
        //
        // 裸 `InitList` 不在这里判：解析期它的 `ty` 只是占位 `Type::Int`
        // （`parser.rs` 的「类型稍后由上下文确定」），真类型在声明上——
        // 那份判据在 `bf_rmw_depth_item` 的 `Decl::Var` 臂。
        let is_site = match e {
            E::Assign { l, .. } => Self::member_names_bitfield(l),
            E::PreInc { e: x, .. }
            | E::PostInc { e: x, .. }
            | E::PreDec { e: x, .. }
            | E::PostDec { e: x, .. } => Self::member_names_bitfield(x),
            E::CompoundLit { ty, .. } => Self::type_contains_bitfield(ty),
            _ => false,
        };
        if is_site {
            1 + child
        } else {
            child
        }
    }

    /// 本类型（含嵌套聚合/数组元素）里是否藏着位域。
    ///
    /// 只用于花括号初始化列表的静态计费：`InitList`/`CompoundLit` 的
    /// `gen_init_list_at` 会对每个位域成员发一段 3 槽 RMW，所以只要类型里
    /// **任何一层**有位域，这个初始化列表就要占一层位域带。
    ///
    /// 解不出的具名类型（`Type::Named`）一律答「是」：看不到字段表就无法
    /// 证明没有位域，漏答是写带外，多答只是多 32B。
    fn type_contains_bitfield(ty: &Type) -> bool {
        match ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => fields
                .iter()
                .any(|f| f.bit_width.is_some() || Self::type_contains_bitfield(&f.ty)),
            Type::Array(elem, _) => Self::type_contains_bitfield(&elem.base),
            Type::Named(_) => true,
            _ => false,
        }
    }

    /// 块里的位域 RMW 链深度（语句表达式的体、嵌套块等）。
    fn bf_rmw_depth_block(b: &crate::ast::Block) -> usize {
        Self::bf_rmw_depth_items(&b.items)
    }

    /// 一批 `BlockItem` 里的位域 RMW 链深度。
    fn bf_rmw_depth_items(items: &[BlockItem]) -> usize {
        items.iter().map(Self::bf_rmw_depth_item).max().unwrap_or(0)
    }

    /// 单条 `BlockItem` 的位域 RMW 链深度（switch 的 prelude / case 体与块共用）。
    fn bf_rmw_depth_item(item: &BlockItem) -> usize {
        match item {
            crate::ast::BlockItem::Stmt(s) => Self::bf_rmw_depth_stmt(s),
            crate::ast::BlockItem::Decl(crate::ast::Decl::Var(v)) => {
                let d = v
                    .init
                    .as_ref()
                    .map_or(0, |e| Self::bf_rmw_chain_depth(e));
                // 声明初始化列表：`InitList` 自带的 `ty` 是占位 `Type::Int`，
                // 站点判据只能问声明的类型。`gen_init_list` 对位域成员发 3 槽 RMW，
                // 占一层。
                if matches!(v.init, Some(crate::ast::Expr::InitList { .. }))
                    && Self::type_contains_bitfield(&v.ty)
                {
                    d.max(1)
                } else {
                    d
                }
            },
            // 块级 `asm(...)` 的操作数是普通表达式，也可以含位域写。
            crate::ast::BlockItem::Decl(crate::ast::Decl::GlobalAsm {
                outputs, inputs, ..
            }) => outputs
                .iter()
                .chain(inputs.iter())
                .map(|e| Self::bf_rmw_chain_depth(e))
                .max()
                .unwrap_or(0),
            // 类型/函数/标号/typedef 声明不带位域写（嵌套函数自己一帧）。
            _ => 0,
        }
    }

    /// 语句里的位域 RMW 链深度。**穷举写**：新增 `Stmt` 形态会编不过，而不是静默
    /// 少扫一条子树（`collect_local_decls_stmt` 同一纪律）。
    fn bf_rmw_depth_stmt(s: &crate::ast::Stmt) -> usize {
        use crate::ast::Stmt as S;
        let m = |x: Option<&crate::ast::Expr>| {
            x.map_or(0, |e| Self::bf_rmw_chain_depth(e))
        };
        match s {
            S::Expr { e, .. } => Self::bf_rmw_chain_depth(e),
            S::Return { val, .. } => m(val.as_ref()),
            S::Block { b, .. } => Self::bf_rmw_depth_block(b),
            S::If {
                cond, then_, else_, ..
            } => Self::bf_rmw_chain_depth(cond)
                .max(Self::bf_rmw_depth_stmt(then_))
                .max(else_.as_ref().map_or(0, |e| Self::bf_rmw_depth_stmt(e))),
            S::While { cond, body, .. } | S::Do { cond, body, .. } => {
                Self::bf_rmw_chain_depth(cond).max(Self::bf_rmw_depth_stmt(body))
            },
            S::For {
                init,
                cond,
                step,
                body,
                ..
            } => init
                .iter()
                .map(|i| Self::bf_rmw_depth_stmt(i))
                .chain(std::iter::once(m(cond.as_ref())))
                .chain(std::iter::once(m(step.as_ref())))
                .chain(std::iter::once(Self::bf_rmw_depth_stmt(body)))
                .max()
                .unwrap_or(0),
            S::Switch {
                expr,
                cases,
                prelude,
                ..
            } => std::iter::once(Self::bf_rmw_chain_depth(expr))
                .chain(prelude.iter().map(|i| Self::bf_rmw_depth_item(i)))
                .chain(cases.iter().flat_map(|c| {
                    c.body.items.iter().map(|i| Self::bf_rmw_depth_item(i))
                }))
                .max()
                .unwrap_or(0),
            S::Label { stmt, .. } => Self::bf_rmw_depth_stmt(stmt),
            // 控制流边与不带表达式的形态。
            S::Goto { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Fallthrough { .. } => 0,
            S::GotoIndirect { expr, .. } => Self::bf_rmw_chain_depth(expr),
            S::Invoke { call, .. } => Self::bf_rmw_depth_stmt(call),
            S::EhLandingPad {
                object, selector, ..
            } => {
                Self::bf_rmw_chain_depth(object).max(Self::bf_rmw_chain_depth(selector))
            },
            // asm 的操作数是普通表达式，里面可以有位域写（`(int){s.bf = 1}` 一类）。
            S::AsmGoto {
                outputs, inputs, ..
            } => outputs
                .iter()
                .chain(inputs.iter())
                .map(|e| Self::bf_rmw_chain_depth(e))
                .max()
                .unwrap_or(0),
        }
    }

    /// 表达式树里的晚分配站点：
    /// - `CompoundLit` → 一个名字含发射序的对象，按 `var_alloc_slots(ty)` 预扣（向量形态例外：
    ///   `gen_expr_compound_lit` 对 `Type::Vector` 走 `vector_temp_base`，那不在变量区）；
    /// - `__builtin_va_start(ap, …)` → ap 的 24 字节 `va_list` 槽（按名字复用）；
    /// - 聚合成员赋值的一般 RHS → 全函数共用的 `<agg_member_tmp>`。
    /// 穷举写：新增 `Expr` 变体会直接编不过。
    fn late_need_expr(e: &Expr, need: &mut LateNeed) {
        use crate::ast::Expr as E;
        // 类型侧统一走一遍：结果类型自己就能携带 VLA 维度，逐变体重复一遍必漏。
        Self::late_need_type(&e.ty(), need);
        // §5.12 帧经济：complex / 聚合 rvalue 物化带的尺寸由本扫决定，所以「有没有用到」
        // 的判据必须与发射器同源，而发射器那一侧只读类型/左值形状，不依赖任何运行期
        // 信息：≤16B 的记录类型作为**非 lvalue 右值**出现时才会被存到帧底带上
        // （`lvalue.rs`/`mem/addr.rs` 的聚合 rvalue 暂存、`dispatch.rs` 聚合赋值的源、
        // `stmt.rs` 返回位的复合字面量）；complex/imaginary 在 `late_need_type` 里按类型
        // 登记。位域 RMW 带按**链深度**给（`bf_rmw_chain_depth`），与向量带一样不再恒给；
        // 那一轴在 `late_need_block` 里按块算一次，不在这里逐节点递归。
        if Self::expr_materializes_small_aggregate(e) {
            need.needs_cx_band = true;
        }
        match e {
            E::Ident { .. }
            | E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => Self::late_need_expr(x, need),
            E::Binop { l, r, .. } | E::Comma { l, r, .. } | E::Index { arr: l, idx: r, .. } => {
                Self::late_need_expr(l, need);
                Self::late_need_expr(r, need);
            },
            E::Assign { l, r, .. } => {
                if Self::agg_member_scratch_used(l, r) {
                    need.agg_tmp = true;
                }
                Self::late_need_expr(l, need);
                Self::late_need_expr(r, need);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                Self::late_need_expr(cond, need);
                Self::late_need_expr(then_, need);
                Self::late_need_expr(else_, need);
            },
            E::Offsetof { indices, .. } => {
                for x in indices {
                    Self::late_need_expr(x, need);
                }
            },
            E::StmtExpr { block, .. } => Self::late_need_block(&block.items, need),
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::late_need_expr(x, need);
                }
            },
            E::Call { callee, args, .. } => {
                // `va_start` 的 ap 槽：与 `special/builtins.rs` 同一组判据（callee 是那个
                // 内置名、首个实参是**数组类型的 Ident**；指针型 Ident 走 `emit_va_start_at`，
                // 不占帧槽）。全局 ap 发射期走 rip 相对（`emit_va_start` 里同一个
                // `is_global_not_local`），所以名单先拿着，到了 `reserve` 里再按同一个
                // 判据筛。
                if let (E::Ident { name: cn, .. }, Some(E::Ident { name: an, ty, .. })) =
                    (&**callee, args.first())
                {
                    if cn.name == "__builtin_va_start" && matches!(ty, Type::Array(_, _)) {
                        need.push_ap(an.name.clone());
                    }
                }
                Self::late_need_expr(callee, need);
                for a in args {
                    Self::late_need_expr(a, need);
                }
            },
            E::CompoundLit { ty, inits, .. } => {
                // 向量复合字面量不占变量区（走 `vector_temp_base`），其余一个站点一份
                if !matches!(ty, Type::Vector(_, _)) {
                    need.compound_slots += Self::var_alloc_slots(ty);
                }
                for it in inits {
                    match it {
                        InitItem::Expr(ex) => Self::late_need_expr(ex, need),
                        InitItem::Designated { value, .. } => Self::late_need_expr(value, need),
                    }
                }
            },
            E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        InitItem::Expr(ex) => Self::late_need_expr(ex, need),
                        InitItem::Designated { value, .. } => Self::late_need_expr(value, need),
                    }
                }
            },
        };
    }

    /// 发射点那条「聚合成员赋值 + 一般 RHS」分支在规划期的静态对应（判据见
    /// `emit/expr/dispatch.rs` 里写 `<agg_member_tmp>` 的那一段）。三条必须和发射点一一对齐：
    ///   * LHS 是成员访问，且**字段类型**（`Member` 节点自带的 `ty`）是聚合 —— 就是发射点的
    ///     `field_is_aggregate`；
    ///   * RHS 不在那五族「直接拿地址」的形状里 —— 就是发射点的
    ///     `rhs_is_lvalue || rhs_is_ident || rhs_is_compound`。
    ///
    /// 宁多不少：多计只是多 16 字节，少计就是 `E1133`。旧写法拿 **RHS 自己的类型**
    /// 当「是不是聚合赋值」的代理，而发射点看的是 LHS 字段类型：ASan 把一次读改写成
    /// `(__asan_loadN(&x), x)` 后，逗号节点的 `ty` 不是聚合类型，代理失效 → 实测
    /// `sqlite3ExprListDup` / `sqlite3SrcListDup` 报「2 slots used, 0 reserved」，
    /// `-fsanitize=address` 下整颗 amalgamation 编不外。
    fn agg_member_scratch_used(l: &Expr, r: &Expr) -> bool {
        // LHS 先剥掉 sanitizer 包上的 `Comma{check, lvalue}`：发射点也是先剥再分派，
        // 否则插桩后的 LHS 在这里“不是成员访问”，规划期就会少算一槽（E1133）。
        let l = crate::ast::peel_instrumented_lvalue(l);
        if !matches!(l, Expr::Member { .. }) {
            return false;
        }
        if !matches!(l.ty(), Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _))
        {
            return false;
        }
        // RHS 落在这五族形状时发射点走 `gen_lvalue_addr`/直接取地址，不需要暂存
        !matches!(
            r,
            Expr::Member { .. }
                | Expr::Index { .. }
                | Expr::Deref { .. }
                | Expr::Ident { .. }
                | Expr::CompoundLit { .. }
        )
    }

    /// VM 尺寸元信息槽的**键**（与 `emit/vla.rs::gen_vm_type_bind` 共用一份拼法：两处各写
    /// 一句 `format!` 就是两套事实，少一个前缀扫描就会漏计一槽）。返回 `Some` 只意味着
    /// 「这个类型有键」，不意味着要分配：字段里没 VLA 时发射点是 `remove`。
    pub fn vm_size_key(ty: &Type) -> Option<String> {
        match ty {
            Type::Struct(tag, _, _, false) => Some(format!("struct:{}", tag)),
            Type::Union(tag, _, _, false) => Some(format!("union:{}", tag)),
            _ => None,
        }
    }

    /// 该记录类型是否需要一个尺寸元信息槽（`gen_vm_type_bind` 的第二条判据：任一字段
    /// 带 VLA）。与上面同一个理由：口径只住一处。
    pub fn vm_record_needs_size_slot(ty: &Type) -> bool {
        match ty {
            Type::Struct(_, fields, _, false) | Type::Union(_, fields, _, false) => {
                fields.iter().any(|f| f.ty.has_vla())
            },
            _ => false,
        }
    }

    /// 诊断（`DCC_FRAME_DUMP=<函数名子串>`）：打印一帧的组成，用来回答「这个函数
    /// 的帧到底被哪条带吃掉了」。内核侧 `perf_event_task_tick` 12KB 帧打穿 IRQ 栈
    /// 就是这么定位的。只写 stderr、不进产物；未设置环境变量时零副作用。
    pub fn frame_dump(
        &self,
        name: &str,
        plan: &crate::codegen::x86_64::abi::layout::FramePlan,
    ) {
        let Ok(pat) = std::env::var("DCC_FRAME_DUMP") else {
            return;
        };
        if !pat.is_empty() && !name.contains(&pat) {
            return;
        }
        let layout = LayoutMap::resolve(plan);
        eprintln!(
            "FRAMEPLAN {name} frame=0x{:x} var={} hdr={} late={} sret={} spill={} va={} sc={} cx={} vec={} bf={} tramp={} stack={} temps={}",
            layout.frame_size,
            plan.var_bytes,
            layout.hdr,
            plan.late_slots,
            plan.sret_need,
            plan.spill_slots,
            plan.va_slots,
            plan.static_chain_slot,
            plan.cx_band,
            plan.vec_reserve,
            plan.bf_rmw_reserve,
            plan.tramp_reserve,
            plan.stack_reserve,
            plan.temp_slots
        );
        // 诊断（`DCC_VAR_DUMP=1`，与上面的 pattern 共用）：把变量区里每个名字的偏移
        // 打出来。用来回答「var_bytes 里到底住了什么」—— 反汇编统计 `[r13-0x..]`
        // 能数出有多少槽真的被访问，这里则给出那些槽属于谁。
        if std::env::var_os("DCC_VAR_DUMP").is_none() {
            return;
        }
        let mut entries: Vec<(&String, &(i64, Type))> = self.var_map.iter().collect();
        entries.sort_by_key(|(_, (off, _))| -*off);
        for (n, (off, ty)) in entries {
            eprintln!(
                "VAR {name} <- {n} off={off} slots={}",
                Self::var_alloc_slots(ty)
            );
        }
    }

    /// 帧规划前把「规划期看不见的运行期临时对象」算成晚分配带的尺寸，并把账面交出去。
    /// 两个帧规划分支 (带参数的普通函数 / `main`) 共用本函数, 避免其中一处
    /// 漏扣后又是一次“变量区压进 sret 区”的定向失效。
    ///
    /// 名不叫 `reserve_*`：这里**不预扣变量区水位**（§11.23 步 3 已把那两段删掉），
    /// 真正在做的只是「把晚分配带的槽数算出来，写进 `late_slot_budget`」这一件事，
    /// 再顺手把规划期看得见的自由 ident 登进变量区、把 `frame_planned` 置上作为发射期
    /// 晚分配的闸门。所有槽尺寸（`late_slots` / `spill_slots` / `temp_slots`）都在
    /// `LayoutMap::resolve` 的同一帧里定为输入，本函数只负责晚分配这一条带的输入。
    pub fn plan_late_slot_budget(
        &mut self,
        body: Option<&Block>,
        params: &[crate::ast::Param],
        ret_ty: &Type,
    ) {
        // 向量槽带需求逐函数复位（与 `late_slot_budget` 同口径）：无体声明直接 early
        // return，不能留下上个函数的尺寸。
        self.max_vec_bytes = 0;
        self.needs_cx_band = false;
        self.bf_rmw_max_depth = 0;
        self.reg_homed_names.clear();
        let Some(b) = body else { return };
        // §11.23 步 2：预算 = 发射期那四族的静态需求。每一笔都能对应到一个具体站点，
        // 所以这里不是「宁可多给」而是「算准」（少算了 `E1133` 会响）。
        let mut need = Self::late_need(&b.items);
        let mut slots = need.compound_slots;
        // 每个 VM 尺寸键一份 `Type::Long`（发射点是 `register_var(_, &Type::Long)`）
        slots += (need.vmsize_keys.len() as i64).max(0);
        for ap in &need.ap_names {
            // 全局 ap 在发射点走 rip 相对真全局内存（`emit_va_start` 里同一个
            // `is_global_not_local` 分派），不占本帧槽 ⇒ 不计费。判据只读一处。
            if self.is_global_not_local(ap) {
                continue;
            }
            // 已经登过的 ap（`va_list ap;` 是一条普通局部声明，规划期已按 24B 给了槽）
            // 在发射点只会命中 var_map、不会新占一槽 ⇒ 不得重复计费。
            // 真正需要本项的是「直接把形参当 ap 用」那一类（它不在 var_map 里）。
            if self.var_map.contains_key(ap) {
                continue;
            }
            slots += Self::var_alloc_slots(&Self::va_list_slot_ty());
        }
        if need.agg_tmp {
            slots += Self::var_alloc_slots(&Type::Int128);
        }
        // 形参类型树也喂给同一个扫面：向量形参的尺寸同样是向量带的需求。放在 `slots`
        // 算完之后，所以这里只会多填 `max_vec_bytes`，不会改变晚分配带口径。
        for p in params {
            Self::late_need_type(&p.ty, &mut need);
        }
        // **返回类型也要喂**：`_Complex double int_to_cd(int n){ return n; }` 体内一个
        // complex 都没出现（`return n;` 的表达式类型是 `int`），复数打包是发射器在返回
        // 路径上做的，那里就读帧底那条带。只看体与形参会把它漏成 cx_band=0（实测
        // `complex_component_conv_provider.c` 的 `int_to_cd`/`int_to_cf`）。
        Self::late_need_type(ret_ty, &mut need);
        self.max_vec_bytes = need.max_vec_bytes;
        self.needs_cx_band = need.needs_cx_band;
        self.bf_rmw_max_depth = need.bf_rmw_depth;
        // §11.23 步 3：这份预算不再**预扣进变量区水位**，而是作为晚分配带的尺寸交给
        // 布局解算（`FramePlan::late_slots`）。带里的对象不再挤在局部变量底下，于是
        // `register_var` 里那两段只在生成期推帧坐标的代码（`min_used` 夹逼 +
        // 「晚分配压低 `sret_region_base` 锚点」）一并删除。
        // 延伸池家的保存槽（Phase 5.1 前置三）**也必须算进这份预算**：`insert_home` 会给每个
        // 住在 caller-saved 号里的自动绑定家申请一个 `<名#save>` 槽，预算要是不知道它，
        // 发射期第一次用到就撞 `E1133`（实测：`register_binding_width.c` 开池后报
        // “1 slots used, 0 reserved (last '<ci#save>')”，而 -O0 完全正常 —— 因为只有 -O2 才跑
        // 寄存器绑定 pass，家才会出现）。计数口径与申请处严格一致：`home` 只由分配器写
        // （用户显式绑定的 `home` 恒为 None），且只有 caller-saved 号才建槽。
        if let Some(b) = body {
            let mut ext_decls: Vec<&VarDecl> = Vec::new();
            Self::collect_frame_var_decls(
                &b.items,
                &self.static_local_map,
                &self.current_func_name,
                &mut ext_decls,
            );
            for v in &ext_decls {
                let auto_ext_home = v.home.is_some_and(|l| {
                    l.reg()
                        .is_some_and(|r| crate::ssa::regalloc::is_auto_ext_reg(r.n))
                });
                if auto_ext_home {
                    slots += Self::var_alloc_slots(&v.ty);
                }
            }
            // §5.12：`ScopedVarAlloc` 已经不给「有寄存器家」的名字留槽，但这个函数末尾
            // 还有一道 `collect_free_idents` → `register_var` 的补登记循环；它只看
            // `var_map` 里有没有，于是刚被跳过的那批名字会在这里被原样捡回来 ——
            // `DCC_FRAME_SKIP_HOMED` 因此过去完全无效（实测 `decay_load`：peak=1，
            // 帧里却仍是 8 槽）。这里把「有家」的名单传下去，补登记同样跳过它们。
            if skip_homed_frame_slots() {
                for v in &ext_decls {
                    if matches!(v.home, Some(crate::location::Location::Reg(_))) {
                        self.reg_homed_names.insert(v.name.name.clone());
                    }
                }
            }
        }
        self.late_slot_budget = slots;
        // 体内**被当值引用**、但规划期没登过的名字：以前它们在发射期走
        // `var_offset` 的兜底，拿到一个按 `Type::Int` 猜的栈槽（不是 int 的对象宽度就错，
        // 函数名被当值用时也会白占一个对象槽），而且带容量无法预知。
        // 现在在规划期按**表达式自带的类型**登进变量区：同一帧、同一个可预测位置，
        // 发射期只会命中 var_map，『谁会懒注册』从此不再需要猜。
        // 调用位置（`Call{callee}`）已被收集器跳过，所以函数名不会进入这里；
        // 已在 var_map 里的（形参与规划声明）、已知全局与函数名也排除。
        // 顺序：按首次出现序分配，与 `register_var` 一贯的「首次出现」口径一致。
        let mut idents: Vec<(String, Type)> = Vec::new();
        Self::collect_free_idents(b, &mut idents);
        // 形参类型里的 VLA 维度表达式 (K&R 风格 `int c[1][va_arg(a,int)]`)
        // 也可能引用本帧/外层 upvar 名字; 这些表达式在序言求值, 规划期必须
        // 登记进 var_map, 否则发射期 register_var 触发 E1134/E1133 (pr99324)。
        for p in params {
            Self::collect_free_idents_type(&p.ty, &mut idents);
            if let Some(d) = &p.vla_dim {
                Self::collect_free_idents_expr(d, &mut idents);
            }
        }
        for (name, ty) in idents {
            if self.var_map.contains_key(&name) {
                continue;
            }
            // 有寄存器家的名字不占帧槽（见上面 `reg_homed_names` 的注释）；真被问到
            // 帧偏移时 `var_offset` 会报 E1134，不是静默错值。
            if self.reg_homed_names.contains(&name) {
                continue;
            }
            // 已知全局不占帧内槽（带家的文件作用域 register 变量例外：它本来就要
            // 一个帧内中转槽，见 §11.16）。
            if self.is_global_not_local(&name) && !self.global_needs_frame_slot(&name) {
                continue;
            }
            if matches!(ty, Type::Func(_, _)) {
                // 函数名被当值用：发射期走 `emit_func_addr`，不该占对象槽
                continue;
            }
            // 仍未被规划的名字：块作用域的 extern 声明不再会落到这里 —— 解析器已把它们
            // 上提成文件作用域声明（`parser::hoist_block_extern`），全局表里有名字，上面
            // 那条 `is_global_not_local` 就拦下了。剩下的只是嵌套函数 upvar（靠静态链
            // 访问）与其它未规划局部，归本帧。
            self.register_var(&name, &ty);
        }
        // 规划到此为止：此后的新名字登记就是发射期晚分配，只能吃上面那份预算
        // （`instructions.rs::register_var` 里的 `E1133`）。
        self.frame_planned = true;
    }

    /// 采集本函数的「局部 → 帧内位置」表，交给 DWARF（Phase 6.2）。
    ///
    /// 两个设计决定值得写明：
    ///   * **名字集合用规划期那一份遍历**（`collect_frame_var_decls`），不另起一次 AST
    ///     扫描——「哪些声明是本函数的用户局部」这件事不得有两个答案。合成名
    ///     （`__compound_lit_*`、`<agg_member_tmp>`）与静态局部（实体在 data/bss）因此天然不在表上；
    ///   * 只在 `-g` 下做。不开调试信息时这是纯白的工作，而且多一份可变的每函数状态
    ///     就多一份与产物无关的风险。
    ///
    /// 调用时机必须在函数体生成完之后、嵌套函数之前：那时 `homes` 与 `var_map` 才是本帧
    /// 的最终值，而嵌套函数会 `compile_func` 递归把两者复位。
    ///
    /// `fn_base`/`fn_end` 是本函数在代码 buffer 里的区间。分段位置（loclist）必须发成
    /// **全覆盖**的区间列（`DW_AT_location` 一旦是 loclist，单表达式那个答案就不作数了），
    /// 所以「号里就是真值」的那几段也得写进去。
    pub fn collect_debug_var_facts(&mut self, f: &FuncDecl, fn_base: u64, fn_end: u64) {
        if self.debug_level == 0 {
            return;
        }
        // 区间基准必须与 DWARF 侧 `DW_AT_low_pc` 拿的是同一个读数（`elf/output.rs` 也从
        // `cg.labels` 取）。拿发射侧的 `func_start` 就会差上一段函数前缀（实测差 48 字节，
        // 表现为 loclist 第一条「begin < end」不成立，而调试器对整个区间列表的判断就错了。
        let fn_base = self.labels.get(&f.name.name).copied().unwrap_or(fn_base);
        let Some(b) = &f.body else { return };
        let mut decls: Vec<&VarDecl> = Vec::new();
        Self::collect_frame_var_decls(&b.items, &self.static_local_map, &f.name.name, &mut decls);
        let declared: Vec<(String, Type, u32)> = decls
            .iter()
            .map(|v| (v.name.name.clone(), v.ty.clone(), v.span.line))
            .collect();
        drop(decls);
        let mut out: Vec<crate::codegen::x86_64::DebugVarLoc> = Vec::with_capacity(declared.len());
        for (name, ty, line) in declared {
            if let Some(loc) = self.var_location(&name) {
                let ranges = self.compose_debug_ranges(&name, loc, fn_base, fn_end);
                out.push(crate::codegen::x86_64::DebugVarLoc {
                    name,
                    loc,
                    ranges,
                    ty,
                    decl_line: line,
                });
            }
        }
        self.debug_var_facts.push((f.name.name.clone(), out));
    }

    /// 把某个变量的「区间窗口」与它的默认位置合成全覆盖的区间列。
    ///
    /// 没有窗口、或所有窗口都等于默认位置时返空表 ⇒ DWARF 侧照旧发单 `exprloc`，产物与
    /// 本改动前逐字节相同（5454 组合门关全绿靠的就是这一条）。
    fn compose_debug_ranges(
        &self,
        name: &str,
        base: crate::location::Location,
        fn_base: u64,
        fn_end: u64,
    ) -> Vec<crate::codegen::x86_64::codegen::DebugVarRange> {
        let mut wins: Vec<(u64, u64, crate::location::Location)> = self
            .debug_loc_windows
            .iter()
            .filter(|(n, _, _, _)| n == name)
            .filter_map(|(_, b, e, l)| {
                // 终点没合上的窗口（只有存出没有装载）直接丢：宁发一个单位置，
                // 也不发一段错的开区间。
                if *e == 0 || e <= b {
                    None
                } else {
                    Some((*b, *e, *l))
                }
            })
            .collect();
        if wins.is_empty() {
            return Vec::new();
        }
        wins.sort_by_key(|(b, _, _)| *b);
        let mut out: Vec<crate::codegen::x86_64::codegen::DebugVarRange> =
            Vec::with_capacity(wins.len() * 2);
        let mut cur = fn_base;
        for (b, e, l) in &wins {
            // 重叠窗口（嵌套 call 共用一个号）：跳过去，内层那一轮的终点才是真值边界。
            if b < &cur {
                if e > &cur {
                    cur = *e;
                }
                continue;
            }
            if *b > cur {
                out.push(crate::codegen::x86_64::codegen::DebugVarRange {
                    begin_pc: cur,
                    end_pc: *b,
                    loc: base,
                });
            }
            out.push(crate::codegen::x86_64::codegen::DebugVarRange {
                begin_pc: *b,
                end_pc: *e,
                loc: *l,
            });
            cur = *e;
        }
        if cur < fn_end {
            out.push(crate::codegen::x86_64::codegen::DebugVarRange {
                begin_pc: cur,
                end_pc: fn_end,
                loc: base,
            });
        }
        if out.iter().all(|r| r.loc == base) {
            return Vec::new();
        }
        out
    }

    /// 一个名字在本帧里的位置：有家就是家，没家就是它的变量区栈槽。
    ///
    /// **不能走 `home_for`**：那条路径对没登记的名字会调 `var_offset` 的兜底，而兜底从
    /// §11.23 步 1 起是 `E1134`（后端不变量破坏）。采集调试信息不得自己造出错误。
    /// 优先级与 `home_for` 一致（家 > 变量区），只是没命中时返回 `None` 而不是猜一个槽。
    ///
    /// `Spill` 家在这里就翻译回帧内偏移（走 `LayoutMap::spill_off` —— 全仓唯一那个翻译点），
    /// 存成 `Stack`：调试侧因此不需要知道帧布局，也不会出现第二份算溢出槽坐标的公式。
    /// 采集点在函数体生成完之后，那时 `frame_layout` 正是本帧的解算结果。
    fn var_location(&self, name: &str) -> Option<crate::location::Location> {
        if let Some((loc, _)) = self.homes.get(name) {
            return Some(match *loc {
                crate::location::Location::Spill(slot) => {
                    crate::location::Location::Stack(self.frame_layout.spill_off(slot))
                },
                other => other,
            });
        }
        self.var_map
            .get(name)
            .map(|(off, _)| crate::location::Location::Stack(*off))
    }

    /// 该全局是否**在本帧里**需要一个中转槽。判据就是文件作用域绑定的那一张表
    /// `global_homes`（`compile_module` 在逐声明扫描里登进去，每个函数入口恢复），
    /// 不另开一份「哪些全局有 register 请求」的名单 —— 那又是一套事实。
    fn global_needs_frame_slot(&self, name: &str) -> bool {
        self.global_homes.contains_key(name) || self.homes.contains_key(name)
    }

    /// 收集体内被当值引用的标识符（名字 + 表达式自带的类型）。
    /// 只走 `Expr::Ident`，并跳过调用位置；标号不是 Ident，不会混进来。
    /// 不下降进嵌套函数体（那是它自己一帧的事，与 `collect_frame_var_decls` 同口径）。
    fn collect_free_idents(b: &Block, out: &mut Vec<(String, Type)>) {
        Self::collect_free_idents_items(&b.items, out)
    }

    /// 项列表层面（`switch` 的 `prelude` 不是 `Block`，但它的初始化式同样能带自由引用，
    /// 例如 `switch (x) { int v = arr[n]; case 0: … }` 里的 VLA 长度）。
    fn collect_free_idents_items(items: &[BlockItem], out: &mut Vec<(String, Type)>) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(init) = &v.init {
                        Self::collect_free_idents_expr(init, out);
                    }
                },
                // 嵌套函数自己一帧；类型定义不携带运行时引用
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => Self::collect_free_idents_stmt(s, out),
            }
        }
    }

    /// 类型树里携带的 VLA 维度表达式中的自由标识符。与 `late_need_type`
    /// 同骨架, 但收集 Ident 元组而非 LateNeed 计数 (pr99324)。
    fn collect_free_idents_type(ty: &Type, out: &mut Vec<(String, Type)>) {
        match ty {
            Type::Ptr(q)
            | Type::Complex(q)
            | Type::Imaginary(q)
            | Type::Vector(q, _)
            | Type::Atomic(q) => Self::collect_free_idents_type(&q.base, out),
            Type::Array(elem, ArraySize::Var(se)) => {
                Self::collect_free_idents_expr(se, out);
                Self::collect_free_idents_type(&elem.base, out);
            },
            Type::Array(elem, _) => Self::collect_free_idents_type(&elem.base, out),
            Type::Func(ret, params) => {
                Self::collect_free_idents_type(&ret.base, out);
                for p in params {
                    Self::collect_free_idents_type(&p.base, out);
                }
            },
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                for f in fields.iter() {
                    Self::collect_free_idents_type(&f.ty, out);
                }
            },
            _ => {},
        }
    }

    fn collect_free_idents_stmt(s: &Stmt, out: &mut Vec<(String, Type)>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_free_idents(b, out),
            Stmt::Expr { e, .. } => Self::collect_free_idents_expr(e, out),
            Stmt::Return { val: Some(e), .. } => Self::collect_free_idents_expr(e, out),
            Stmt::GotoIndirect { expr, .. } => Self::collect_free_idents_expr(expr, out),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::collect_free_idents_expr(cond, out);
                Self::collect_free_idents_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_free_idents_stmt(e, out);
                }
            },
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                Self::collect_free_idents_expr(cond, out);
                Self::collect_free_idents_stmt(body, out);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::collect_free_idents_stmt(i, out);
                }
                if let Some(c) = cond {
                    Self::collect_free_idents_expr(c, out);
                }
                if let Some(st) = step {
                    Self::collect_free_idents_expr(st, out);
                }
                Self::collect_free_idents_stmt(body, out);
            },
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                Self::collect_free_idents_expr(expr, out);
                Self::collect_free_idents_items(prelude, out);
                for c in cases {
                    Self::collect_free_idents(&c.body, out);
                }
            },
            Stmt::Label { stmt, .. } | Stmt::Invoke { call: stmt, .. } => {
                Self::collect_free_idents_stmt(stmt, out);
            },
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_free_idents_expr(x, out);
                }
            },
            Stmt::EhLandingPad {
                object, selector, ..
            } => {
                Self::collect_free_idents_expr(object, out);
                Self::collect_free_idents_expr(selector, out);
            },
            Stmt::Return { val: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. } => {},
        }
    }

    /// 表达式树里的 `Expr::Ident` 就是「被当值引用的名字」；调用位置上的 callee 不是
    /// 对象（`printf(...)` 里的 printf 走函数地址），所以 `Call` 只走实参。
    /// 穷举写：新增 `Expr` 变体会直接编不过，而不是静默漏一个子树。
    fn collect_free_idents_expr(e: &crate::ast::Expr, out: &mut Vec<(String, Type)>) {
        use crate::ast::Expr as E;
        match e {
            E::Ident { name, ty, .. } => out.push((name.name.clone(), ty.clone())),
            E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Offsetof { indices, .. } => {
                for x in indices {
                    Self::collect_free_idents_expr(x, out);
                }
            },
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => Self::collect_free_idents_expr(x, out),
            E::Binop { l, r, .. }
            | E::Assign { l, r, .. }
            | E::Comma { l, r, .. }
            | E::Index { arr: l, idx: r, .. } => {
                Self::collect_free_idents_expr(l, out);
                Self::collect_free_idents_expr(r, out);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                Self::collect_free_idents_expr(cond, out);
                Self::collect_free_idents_expr(then_, out);
                Self::collect_free_idents_expr(else_, out);
            },
            E::Call { args, .. } => {
                for x in args {
                    Self::collect_free_idents_expr(x, out);
                }
            },
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        crate::ast::InitItem::Expr(x) => Self::collect_free_idents_expr(x, out),
                        crate::ast::InitItem::Designated { designator, value } => {
                            Self::collect_free_idents_designator(designator, out);
                            Self::collect_free_idents_expr(value, out);
                        },
                    }
                }
            },
            E::StmtExpr { block, .. } => Self::collect_free_idents(block, out),
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_free_idents_expr(x, out);
                }
            },
        }
    }

    fn collect_free_idents_designator(d: &crate::ast::Designator, out: &mut Vec<(String, Type)>) {
        use crate::ast::Designator as D;
        match d {
            D::Field(_) => {},
            D::Index(x) => Self::collect_free_idents_expr(x, out),
            D::Range(a, b) => {
                Self::collect_free_idents_expr(a, out);
                Self::collect_free_idents_expr(b, out);
            },
            D::Chain(items) => {
                for sub in items {
                    Self::collect_free_idents_designator(sub, out);
                }
            },
        }
    }

    /// 本函数需要多少个 spill 槽：从**已经交付到声明上的家**里数（P12 Phase 5.1.b）。
    ///
    /// 为什么不在这里重新算一遍分配：分配器的决定已经写进 `VarDecl::home`，帧只需要回答
    /// 「这些家里出现过的最大槽号是多少」。拿 `max(槽号)+1` 当带尺寸，尺寸与坐标就天生
    /// 来自同一份事实；若另加一个 `FuncDecl::spill_slots` 字段由分配器写，就会出现
    /// 「带尺寸与槽号不一致」这种只能靠自觉维护的双轨（§3 的 H4）。
    ///
    /// 只算**真正占帧内槽**的声明：extern 引用全局符号、已由分析 pass 预注册的 static local
    /// 不占帧内空间（`collect_frame_var_decls` 那两处 `continue` 早于本函数），所以它们
    /// 就算带着 Spill 家也不得拉大带。序号负值按非法忽略（宁可少给一个槽也不能算出
    /// 一个越界的偏移）。
    fn count_spill_slot(v: &crate::ast::VarDecl, spill_slots: &mut i64) {
        if let Some(crate::location::Location::Spill(slot)) = v.home {
            if slot >= 0 && slot.saturating_add(1) > *spill_slots {
                *spill_slots = slot.saturating_add(1);
            }
        }
    }

    /// 函数体里「要占帧内槽」的声明，按首次出现顺序产出**一份**列表。
    ///
    /// 为什么必须是一份：定容（`var_alloc_slots` 的最大值折叠）与预注册以前各走一遍
    /// AST（`collect_var_alloc_maxima*` 与 `pre_register_stmt_max` 两份遍历），两份都必须
    /// 同意「哪些声明是规划期看得见的」。它们当时都漏了 `for` 的 init、标号语句的体与
    /// `({...})` 块里的声明，那批声明于是落到发射期懒注册 —— 实测把晚分配带挤爆
    /// （`E1133` 报 `for (int i = 0; …)` 的 `i` 超出带容量）。收成一份遍历之后，
    /// 「新增一种子树」只有一处可改；跳过规则（extern / 已预注册的 static local）也住在
    /// 这里，因为它们同时决定「占不占帧内槽」与「要不要数 spill 槽」。
    ///
    /// 不下降进嵌套函数体：那是它自己一帧的事（`compile_func` 会递归编译它，
    /// 带的容量与偏移也按那一帧单独解算）。
    fn collect_frame_var_decls<'a>(
        items: &'a [BlockItem],
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.is_extern && v.init.is_none() {
                        // 纯声明：不占帧内槽。名字已在解析器里上提成文件作用域声明
                        // （`parser::hoist_block_extern`），全局表里有它，不靠这里登记。
                        continue;
                    }
                    if v.is_static && static_local_map.is_static_local(func_name, &v.name.name) {
                        continue;
                    }
                    out.push(v);
                    // 初始化式里也能嵌着语句表达式块（`int r = ({ typeof(a) _a = (a); …})`，
                    // 内核 `max()`/`min()` 就是这个形状）：那里的声明属于**同一帧**，
                    // 不递归就会在发射期懒注册。先推自己再走 init，保持首次出现顺序。
                    if let Some(init) = &v.init {
                        Self::collect_frame_var_decls_expr(init, static_local_map, func_name, out);
                    }
                },
                // 类型/枚举/别名/块级 asm 定义不占帧内槽；嵌套函数自己一帧
                BlockItem::Decl(
                    Decl::Func(_)
                    | Decl::StructDef(_)
                    | Decl::UnionDef(_)
                    | Decl::EnumDef(_)
                    | Decl::Typedef { .. }
                    | Decl::GlobalAsm { .. },
                ) => {},
                BlockItem::Stmt(s) => {
                    Self::collect_frame_var_decls_stmt(s, static_local_map, func_name, out)
                },
            }
        }
    }

    /// 语句子树里的声明宿主。**不留 `_`**：新增 `Stmt` 变体会直接编不过，
    /// 而不是静默漏一条通路（漏一条的症状就是发射期懒注册 + 晚分配带溢出）。
    fn collect_frame_var_decls_stmt<'a>(
        s: &'a Stmt,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        match s {
            Stmt::Block { b, .. } => {
                Self::collect_frame_var_decls(&b.items, static_local_map, func_name, out);
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::collect_frame_var_decls_expr(cond, static_local_map, func_name, out);
                Self::collect_frame_var_decls_stmt(then_, static_local_map, func_name, out);
                if let Some(e) = else_ {
                    Self::collect_frame_var_decls_stmt(e, static_local_map, func_name, out);
                }
            },
            // 循环的条件与体里都能带块（`while (MAX(a,b) > c) {…}`、`do {…} while (MIN(i,1));`）：
            // 只走体就会漏到发射期懒注册。
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                Self::collect_frame_var_decls_expr(cond, static_local_map, func_name, out);
                Self::collect_frame_var_decls_stmt(body, static_local_map, func_name, out);
            },
            // `for` 的 init 也是声明的宿主（`for (int i = 0; …)`）
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::collect_frame_var_decls_stmt(i, static_local_map, func_name, out);
                }
                if let Some(c) = cond {
                    Self::collect_frame_var_decls_expr(c, static_local_map, func_name, out);
                }
                if let Some(st) = step {
                    Self::collect_frame_var_decls_expr(st, static_local_map, func_name, out);
                }
                Self::collect_frame_var_decls_stmt(body, static_local_map, func_name, out);
            },
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                Self::collect_frame_var_decls_expr(expr, static_local_map, func_name, out);
                // 同上：prelude 里的声明也必须拿到自己的帧槽，不然发射期会懒注册成另一个槽
                // （或直接撞 `E1133`）。
                Self::collect_frame_var_decls(prelude, static_local_map, func_name, out);
                for case in cases {
                    Self::collect_frame_var_decls(
                        &case.body.items,
                        static_local_map,
                        func_name,
                        out,
                    );
                }
            },
            Stmt::Label { stmt, .. } => {
                Self::collect_frame_var_decls_stmt(stmt, static_local_map, func_name, out);
            },
            Stmt::Invoke { call, .. } => {
                Self::collect_frame_var_decls_stmt(call, static_local_map, func_name, out);
            },
            // 表达式里也可能嵌着块：`({... int x; ...})`、`int v = ({...; x;});`
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::collect_frame_var_decls_expr(e, static_local_map, func_name, out);
            },
            Stmt::GotoIndirect { expr, .. } => {
                Self::collect_frame_var_decls_expr(expr, static_local_map, func_name, out);
            },
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
            Stmt::Return { val: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. }
            | Stmt::EhLandingPad { .. } => {},
        }
    }

    /// 表达式子树里的 `({...})` 块也能带声明。穷举写：新增 `Expr` 变体编不过。
    fn collect_frame_var_decls_expr<'a>(
        e: &'a crate::ast::Expr,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        use crate::ast::Expr as E;
        match e {
            E::StmtExpr { block, .. } => {
                Self::collect_frame_var_decls(&block.items, static_local_map, func_name, out);
            },
            E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::Ident { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => {
                Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
            },
            E::Offsetof { indices, .. } => {
                for x in indices {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
            E::Binop { l, r, .. }
            | E::Assign { l, r, .. }
            | E::Comma { l, r, .. }
            | E::Index { arr: l, idx: r, .. } => {
                Self::collect_frame_var_decls_expr(l, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(r, static_local_map, func_name, out);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                Self::collect_frame_var_decls_expr(cond, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(then_, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(else_, static_local_map, func_name, out);
            },
            E::Call { callee, args, .. } => {
                Self::collect_frame_var_decls_expr(callee, static_local_map, func_name, out);
                for x in args {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        crate::ast::InitItem::Expr(x) => {
                            Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out)
                        },
                        crate::ast::InitItem::Designated { designator, value } => {
                            Self::collect_frame_var_decls_designator(
                                designator,
                                static_local_map,
                                func_name,
                                out,
                            );
                            Self::collect_frame_var_decls_expr(
                                value,
                                static_local_map,
                                func_name,
                                out,
                            );
                        },
                    }
                }
            },
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
        }
    }

    fn collect_frame_var_decls_designator<'a>(
        d: &'a crate::ast::Designator,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        use crate::ast::Designator as D;
        match d {
            D::Field(_) => {},
            D::Index(x) => Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out),
            D::Range(a, b) => {
                Self::collect_frame_var_decls_expr(a, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(b, static_local_map, func_name, out);
            },
            D::Chain(items) => {
                for sub in items {
                    Self::collect_frame_var_decls_designator(sub, static_local_map, func_name, out);
                }
            },
        }
    }

    /// 帧内槽位规划的唯一入口：**一次**遍历产出「谁占槽」，然后
    ///   1) 定容 —— 每个名字取全函数**所有作用域**中的最大分配需求。var_map 按名字
    ///      去重共享栈槽（遮蔽的既有设计，见 `register_var` 的注释），但槽容量必须取
    ///      同名声明的最大值：旧实现按首次声明定容，后续同名更大的结构体声明
    ///      （如先 `struct with_vec s;` 16B 后 `struct vec_int s;` 32B）会溢出到相邻
    ///      局部 —— 根因: flag_clobber_frame.c 中 `s.i = 7` 静默覆盖已执行的 `ok = 0`；
    ///   2) 按首次出现顺序分配偏移，并顺带在同一趟里数 spill band 的尺寸
    ///      （不另开一份遍历，见 `count_spill_slot`）。
    pub fn pre_register_vars(
        var_map: &mut HashMap<String, (i64, Type)>,
        items: &[BlockItem],
        temp_slots: i32,
        cs_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
        spill_slots: &mut i64,
        over_align: usize,
    ) {
        let mut decls: Vec<&VarDecl> = Vec::new();
        Self::collect_frame_var_decls(items, static_local_map, func_name, &mut decls);
        let mut maxima: HashMap<String, (i64, Type)> = HashMap::new();
        // 名字出现次数。>1 的名字（遮蔽声明）必须全函数共用一格，钉在顶层不参与回收：
        // 同名的两块各有自己的作用域，但它们共用的那个偏移不能被第三个变量拿去。
        let mut counts: HashMap<String, u32> = HashMap::new();
        for v in &decls {
            *counts.entry(v.name.name.clone()).or_insert(0) += 1;
            let slots = Self::var_alloc_slots(&v.ty);
            match maxima.get(&v.name.name) {
                Some(&(cur, _)) if cur >= slots => {},
                _ => {
                    maxima.insert(v.name.name.clone(), (slots, v.ty.clone()));
                },
            }
        }
        let pinned: HashSet<String> = counts
            .iter()
            .filter(|(_, &c)| c > 1)
            .map(|(n, _)| n.clone())
            .collect();
        let mut alloc = ScopedVarAlloc {
            var_map,
            maxima: &maxima,
            pinned: &pinned,
            static_local_map,
            func_name,
            temp_slots,
            cs_slots,
            kasan_rz_slots,
            over_align,
            total: *total_slots,
            peak: *total_slots,
            spill: 0,
        };
        // 钉住的同名变量先占位（顺序 = 首次出现顺序），此后任何作用域都共享它。
        for v in &decls {
            if pinned.contains(&v.name.name) {
                alloc.alloc_one(v);
            }
        }
        // 作用域感知分配：块退出即回收水位（§5.12 / 报告 §5.26）。
        // 函数体顶层不回收 —— 形参接下来要落到峰值之下。
        alloc.block(items, false);
        if std::env::var_os("DCC_SLOT_HOME").is_some() {
            eprintln!("SLOT-PEAK {} peak={} spill={}", func_name, alloc.peak, alloc.spill);
        }
        *total_slots = alloc.peak;
        *spill_slots = alloc.spill;
    }

    /// P13 阶段6：本函数里「帧槽读写不得被块内等价事实穿过」的局部变量名 → 槽数。
    ///
    /// 判据与「能不能给它分寄存器家」共用同一条（`functions.rs` 的 `assign_auto_homes`）：
    /// `volatile` / 段限定 / `_Atomic` 的局部，其每一次读写都是可观察事件，既不能把读
    /// 转发成「上次写进去的那个号」，也不能把同值重写当死写削掉。
    ///
    /// 为什么在声明处建表而不是查 `var_map` 的类型：`var_map` 只存 `Type`，限定符住在
    /// `VarDecl::quals` 上，`volatile int x` 的 `Type` 与 `int x` 逐位相同。为什么记名字
    /// 而不记偏移：晚分配（`for` 的 init、`({...})` 块、标号语句里的声明）的偏移要到
    /// 发射期才定，本函数在规划期跑，只能先把「哪些名字要防」定下来。
    pub fn collect_volatile_locals(
        items: &[BlockItem],
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut HashMap<String, i64>,
    ) {
        let mut decls: Vec<&VarDecl> = Vec::new();
        Self::collect_frame_var_decls(items, static_local_map, func_name, &mut decls);
        for v in &decls {
            if !Self::frame_access_is_volatile(v) {
                continue;
            }
            let slots = Self::var_alloc_slots(&v.ty);
            let slot = out.entry(v.name.name.clone()).or_insert(0);
            if slots > *slot {
                *slot = slots;
            }
        }
    }

    /// 一个局部的帧槽读写是否「每一次都是可观察事件」。
    ///
    /// 判据只有一个出处：`crate::ast::var_decl_access_is_observable`。AST 层的
    /// `opt.rs::discarded_statement_is_dead`（「丢弃值的纯读能不能整条不发」）问的是同一
    /// 件事，两边各写一份就是漂移的温床。
    ///
    /// 段限定的局部在本后端不会有段前缀（`[r13+disp]` 是无段的普通帧槽），但它与
    /// volatile 在语义上同类：值可能被外部（per-cpu 基址、信号）改动，所以一并排除，
    /// 宁可少削一条。
    fn frame_access_is_volatile(v: &VarDecl) -> bool {
        crate::ast::var_decl_access_is_observable(v)
    }

    pub fn collect_direct_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Func(f)) if f.body.is_some() => out.push(f.clone()),
                BlockItem::Stmt(s) => Self::collect_direct_nested_funcs_stmt(s, out),
                _ => {},
            }
        }
    }

    fn collect_direct_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_direct_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_direct_nested_funcs_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_direct_nested_funcs_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                Self::collect_direct_nested_funcs_stmt(body, out);
            },
            _ => {},
        }
    }

    pub fn emit_zero_init_frame(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        // 保存 rcx, rdi, rax (它们可能在后续代码中使用)
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rax
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rcx
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    7,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rdi

        // xor eax, eax (rax = 0)
        // P11: 收敛到 AluRm{Xor, B32, rax, rax}.
        {
            let insn = Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::Xor,
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B32,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B32,
                    ),
                },
                &[],
            );
            if !self.emit_insn(&insn) {
                self.emit_insn(&Insn::alu_rm_reg_reg(
                    crate::codegen::x86_64::opcode::AluOp::Xor,
                    crate::codegen::shared::asm_operand::Width::B32,
                    0,
                    0,
                ));
            }
        }
        // mov rcx, frame_size / 8
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                rip_relative: false,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Imm(((frame_size as u64) / 8) as i64),
            },
            &[],
        ));
        // rdi = r13 - frame_size (帧底部)
        self.emit_lea_r13(7, -frame_size);
        // rep stosq: F3 48 AB
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepStosq));

        // 恢复 rdi, rcx, rax
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    7,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rdi
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rcx
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rax
    }

    pub fn emit_stack_clash_probes(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        let page: i64 = 4096;
        let pages = frame_size / page;
        let remaining = frame_size % page;
        if pages > 0 {
            self.mov_imm(11, pages as i128);
            let loop_start = self.pos();
            // sub rsp, 4096: 走 emit_sub_rsp (AluRm{Sub, B64, rsp, imm32} 类型化)
            self.emit_sub_rsp(4096);
            self.emit_insn(&Insn::store_imm_mem(
                crate::codegen::shared::asm_operand::Width::B8,
                4,
                0,
                0,
            )); // mov byte ptr [rsp], 0
                // dec r11: 49 FF CB (REX.W B64, /1 modrm=11 001 011)
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Dec,
                crate::codegen::shared::asm_operand::Width::B64,
                11,
            ));
            let loop_end = self.pos();
            let disp = loop_start as i64 - loop_end as i64 - 2;
            self.emit_insn(&Insn::raw_bytes(&[disp as i8 as u8])); // jnz loop
        }
        self.emit_sub_rsp(remaining);
    }

    pub fn emit_xmm_r13_disp(&mut self, pfx: u8, op: u8, xmm: u8, off: i64) {
        let rex = 0x41 | if xmm >= 8 { 0x04 } else { 0 };
        // P9 方案 C: 按 pfx/op 组合生成一条完整 gas 行. 调用方已把完整字节合成过,
        // 我们这里把前缀+opcode 合并发射成单条 Insn, 便于渲染器直接输出 gas_text.
        use crate::codegen::x86_64::emit::gas::{mem_disp as gas_mem_disp, XMM as GAS_XMM};
        // op=0x11 = mov [r13+off], xmm ; op=0x10 = mov xmm, [r13+off]
        let mnem_base = match op {
            0x10 => "mov",
            0x11 => "mov",
            _ => "mov",
        };
        let pfx_sz = match pfx {
            0xF3 => "ss",  // movss
            0xF2 => "sd",  // movsd
            0x66 => "apd", // movapd
            0x00 => "q",   // movq (no SSE prefix)
            _ => "",
        };
        let mnem = format!("{mnem_base}{pfx_sz}");
        let mem = gas_mem_disp(13, off);
        let xmm_name = GAS_XMM[(xmm & 15) as usize];
        // 用单条发射合并 pfx+rex+0x0f+op+modrm+disp.
        // 之前: pfx/rex/0F/op 由本函数发射, modrm+disp 由 emit_r13_modrm_disp 发射.
        // 为了让渲染器一条 gas 行直接出, 这里我们在内部拼完整字节.
        let mut bytes: Vec<u8> = Vec::new();
        if pfx != 0 {
            bytes.push(pfx);
        }
        bytes.push(rex);
        bytes.push(0x0F);
        bytes.push(op);
        // modrm: mod=01 (disp8) / 10 (disp32), reg=xmm, r/m=13=5
        let reg_field = xmm & 7;
        if Self::fits_disp8(off) {
            bytes.push(0x45 | (reg_field << 3));
            bytes.push(off as i8 as u8);
        } else if Self::fits_disp32(off) {
            bytes.push(0x85 | (reg_field << 3));
            bytes.extend_from_slice(&(off as i32).to_le_bytes());
        } else {
            // fallback: 走原路径
            if pfx != 0 {
                self.emit_insn(&Insn::raw_bytes(&[pfx, rex, 0x0F, op]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, op]));
            }
            self.emit_lea_r13(11, off);
            let rex2 = 0x41 | if xmm >= 8 { 0x04 } else { 0 } | 0x01;
            if pfx != 0 {
                self.emit_insn(&Insn::raw_bytes(&[pfx, rex2, 0x0F, op]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&[rex2, 0x0F, op]));
            }
            self.emit_reg_indirect_modrm(xmm, 11);
            return;
        }
        let gas = if op == 0x11 {
            format!("{mnem} {xmm_name}, {mem}")
        } else {
            format!("{mnem} {mem}, {xmm_name}")
        };
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    pub fn store_xmm32_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0xF3, 0x11, xmm, off);
    }

    /// 发 canary 装载：`mov <seg>:guard, %rax`。
    ///
    /// 有 `-mstack-protector-guard-symbol=`（内核 x86_64 的形态）且当前是 ET_REL 时，
    /// 用 **RIP 相对**寻址读那个符号 —— 内核传的是
    /// `-mstack-protector-guard-symbol=__ref_stack_chk_guard`，链接期解析到 .data 里的
    /// `__stack_chk_guard`。固定偏移 `%gs:0x28` 要求 GS 基址已指向 per-cpu 区，而
    /// 早启动路径（`native_write_cr4()`）在 GS 基址为 0 时就执行了。
    pub(crate) fn emit_canary_load(&mut self) {
        let seg_prefix = match self.target.stack_protector_guard_reg {
            crate::cli::StackGuardReg::Gs => 0x65u8,
            _ => 0x64u8, // Fs is default
        };
        if self.relocatable {
            if let Some(sym) = self.target.stack_protector_guard_symbol.clone() {
                // 65 48 8B 05 <disp32> = mov %seg:sym(%rip), %rax
                let seg = if seg_prefix == 0x65 { "%gs" } else { "%fs" };
                self.emit_insn(
                    &Insn::raw_bytes(&[seg_prefix, 0x48, 0x8b, 0x05, 0, 0, 0, 0])
                        .with_gas(format!("mov {}:{}(%rip), %rax", seg, sym)),
                );
                self.pending_data_rels.push((self.pos() - 4, sym));
                return;
            }
        }
        // 无符号：TLS/per-cpu canary 的固定偏移形态。
        self.emit_insn(&Insn::raw_bytes(&[
            seg_prefix, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00,
        ]));
    }

    /// 发 canary 校验：`xor <seg>:guard, %r11`（随后 `je` 跳过 `__stack_chk_fail`）。
    pub(crate) fn emit_canary_check(&mut self) {
        let seg_prefix = match self.target.stack_protector_guard_reg {
            crate::cli::StackGuardReg::Gs => 0x65u8,
            _ => 0x64u8, // Fs is default
        };
        if self.relocatable {
            if let Some(sym) = self.target.stack_protector_guard_symbol.clone() {
                // 65 4C 33 1D <disp32> = xor %seg:sym(%rip), %r11
                let seg = if seg_prefix == 0x65 { "%gs" } else { "%fs" };
                self.emit_insn(
                    &Insn::raw_bytes(&[seg_prefix, 0x4c, 0x33, 0x1d, 0, 0, 0, 0])
                        .with_gas(format!("xor {}:{}(%rip), %r11", seg, sym)),
                );
                self.pending_data_rels.push((self.pos() - 4, sym));
                return;
            }
        }
        self.emit_insn(&Insn::raw_bytes(&[
            seg_prefix, 0x4c, 0x33, 0x1c, 0x25, 0x28, 0x00, 0x00, 0x00,
        ]));
    }

    /// 函数退出点上的 `__attribute__((cleanup))` —— 内核 `guard()` 的解锁就住在这。
    ///
    /// 存在理由：`end_cleanup_scope` 以前只在 `Stmt::Block` 的出口被调用，而
    /// `include/linux/cleanup.h` 的 `guard(raw_spinlock_irqsave)(&ioapic_lock);` 展开成
    /// 一个**函数体层**的带 `__cleanup__` 声明符 —— 那个作用域没有 `Stmt::Block` 出口，
    /// 于是解锁从未被发射。实测（报告 §5.16）：PVH 启动走到 `Processor #0 (Bootup-CPU)`
    /// 后静默卡死，`ioapic_lock` 的锁字读回 `0x101`（locked=1、pending=1），
    /// 调用栈是 `setup_arch → mpparse_parse_smp_config → smp_read_mpc → mp_register_ioapic
    /// → io_apic_get_redir_entries → _raw_spin_lock_irqsave → queued_spin_lock_slowpath`；
    /// 该对象的 `io_apic_get_redir_entries` 里只有一条 `_raw_spin_lock_irqsave` 调用，
    /// 没有任何解锁字节。同一个 TU 里的块内 `guard()` 正常（块出口本来就发）。
    ///
    /// 为什么只覆盖部分交付类别：清理函数是一次普通 `call`，而返回值此刻已经躺在
    /// ABI 交付位置上。`Integer` 用 `push/pop %rax,%rdx` 能保住，`Memory`（sret）的值
    /// 本体在内存里、只需保地址，所以这两类可以发；`Sse`/`X87`/复数交付的 xmm 与
    /// st(0) 都是 caller-saved，没有对应的廉价保存，那些函数保持旧行为（宁可不执行
    /// cleanup，也不能悄悄把返回值毁掉）。
    fn emit_func_scope_cleanups(&mut self) {
        if self.cleanup_vars.is_empty() {
            return;
        }
        let ret = self.current_func_ret_ty.clone();
        let sret = Self::needs_memory_return(&ret);
        let int_delivery = Self::abi_classes(&ret).iter().all(|c| {
            matches!(
                c,
                crate::codegen::x86_64::AbiClass::Integer
                    | crate::codegen::x86_64::AbiClass::NoClass
            )
        });
        if !matches!(ret, Type::Void) && !(sret || int_delivery) {
            return;
        }
        // `void` 无值可保；其余先把整数交付寄存器压栈，清理调用完了再原样弹出。
        let save_value = !matches!(ret, Type::Void);
        if save_value {
            self.push(0); // rax
            self.push(2); // rdx（Int128 / 两个 eightbyte 的高半）
        }
        // 按 LIFO 发完本函数所有未退出的 cleanup 作用域（包括嵌在块里、由 `return`
        // 提前退出的那些）。用不消费的那一个入口：一个函数有多个 `return` 站点，
        // 每个都要自己发一份（理由见 `Codegen::emit_cleanup_range`）。
        self.emit_cleanup_range(0);
        if save_value {
            self.pop(2);
            self.pop(0);
        }
    }

    pub fn emit_func_epilogue(&mut self) {
        // 取走「体内比帧线低多少」这本账：尾声自己发的恢复指令不该再往里记。
        let ledger = self.rsp_below_frame_line.take();
        // 每一条 `return` 都是一次区出口：区内推迟的存出必须在恢复 callee-saved 号**之前**
        // 写回内存，否则那个值随号一同消失，而帧里留的是旧值（调用方或嵌套路径
        // 再从内存读它就是错值）。函数尾的那一次 flush 只管「顺落下来结束」的路径。
        self.region_flush();
        // 作用域在**函数体**那一层的 `__attribute__((cleanup))` 也从这里退出。必须排在
        // `region_flush()` 之后：清理函数按地址读那个变量，区内推迟的存出得先落回内存。
        self.emit_func_scope_cleanups();
        // Stack canary 检查: 在恢复 rsp 前验证金丝雀值
        if self.func_canary && self.frame_layout.canary_fits() {
            // mov r11, [r13 - 8]  → 加载存储的 canary 到临时寄存器（槽位 = `abi::layout::CANARY_OFF`）
            self.emit_insn(&Insn::mov_rm_mem(
                true,
                crate::codegen::shared::asm_operand::Width::B64,
                11,
                13,
                -8,
            ));
            // xor r11, <guard>  → 与原始 canary 比较（guard 的寻址形态见 `emit_canary_check`）
            self.emit_canary_check();
            // Always call __stack_chk_fail on canary mismatch.
            // Both ET_REL and ET_EXEC use the same path:
            // __stack_chk_fail is emitted as an undefined global
            // symbol (provided by libc / kernel runtime).
            self.emit_insn(&Insn::short_jump_rel8(0x74, 0x05)); // je .+5 (canary OK → skip)
                                                                // noreturn 调用：上面那条 `je` 跳过的就是本条 call，成功路径不会执行它，
                                                                // 失败路径再不会回到本函数 —— 所以这里不需要（也不能）caller-saved 家的
                                                                // 存/取窗口：窗口会多发指令，而 `je` 的位移是按 call 恰 5 字节硬编码的。
                                                                // 实测证据：内核开 `-fstack-protector-strong` + 延伸池后，`lib/list_sort.c`
                                                                // 在旧路上直接命中 E1136 而整 TU 失败。
            self.call_label_noreturn("__stack_chk_fail");
        }
        // 恢复被调用者保存寄存器（报告 §5.42）：只发**计划里**的号，槽号按下发顺序
        // 紧凑编号（与序言同一套 cs_j），偏移 `-(16 + 8*j)`。旧口径无条件发 4 条
        // （32B 槽 + 8B 洞），未用的再由 `patch_callee_saves` 改成 nop —— 帧头却已经
        // 付了 4 个槽的钱。现在帧头尺寸就是计划位图的 popcount，这里必须逐号对齐。
        let mut cs_j: i64 = 0;
        for (bit, reg, name) in [
            (0x01u8, 3u8, "rbx"),
            (0x02, 12, "r12"),
            (0x04, 14, "r14"),
            (0x08, 15, "r15"),
        ] {
            if self.callee_plan & bit == 0 {
                continue;
            }
            let off = -(16 + cs_j * 8) as i32;
            Self::emit_callee_save_site(
                self,
                bit,
                Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    reg,
                    13,
                    off,
                )
                .with_gas(format!("mov {off}(%r13), %{name}")),
            );
            cs_j += 1;
        }
        // 跳表基址常驻 `%rbp` 时把号还给调用者（阶段 6 批 4）。位置必须在四条
        // callee-saved 恢复之后、`mov %r13,%rsp` 之前：前者仍是本帧坐标寻址唯一还能用
        // 的时刻，而多个 return 站点内联展开尾声时每一条都要带上这一步。
        if self.disptab_base_reg_sym.is_some() {
            self.emit_insn(
                &Insn::mov_rm_mem(true, crate::codegen::shared::asm_operand::Width::B64, 5, 13, -8)
                    .with_gas("mov -8(%r13), %rbp".to_string()),
            );
        }
        // 恢复栈指针。两种形状，差别在 objtool 看不看得见：
        //
        // 【为什么不能一律 `mov %r13,%rsp`】objtool 的 x86 解码器（`tools/objtool/arch/x86/
        // decode.c`）是**白名单式**的，只给已知形状发 stack op（push/pop/add·sub rsp,imm/
        // leave）；`mov reg,%rsp` 不在表里 ⇒ 不产生任何 op ⇒ CFA 一直停在「帧已分配」那个
        // 状态。到 ret 时 `has_modified_stack_frame()`（`check.c:2721`）拿 base/offset/
        // stack_size 与函数入口比，不相等就报 `return with modified stack frame`。实测
        // localmodconfig 全树 46 491 条，而 gcc 同旗标同参数 0 条；后果不是少一条提示，而是
        // objtool 就在这套 CFI 状态上生成 `.orc_unwind/_ip` ⇒ 跟不了栈状态的路径上发出去的
        // ORC 条目是错的（`arch_stack_walk` 在 lockdep / oops 转储 / kexec 上给出错回溯）。
        // gcc 在 `-fomit-frame-pointer` 下从来不写 `mov base,%rsp`：它按 `add $帧长,%rsp` 退。
        //
        // 【能换成 add 的判据】`rsp_below_frame_line` 这本账（`codegen.rs` 的字段注释）从
        // 序言分配完帧的那一刻起，只统计「能证明是写 rsp 的指令」：push/+8、pop/-8、
        // `sub $N,%rsp`/+N、`add $N,%rsp`/-N，其余一律弃账。它为 0 就是「rsp 正落在帧线」。
        // 另外两道守卫是「账非 0 时定义标号」与「账非 0 时发跳转」都当场弃账 —— 线性累加
        // 只沿发射顺序成立，跨越窗口的控制流正是它唯一看不见的形状。
        // 三条形状落回旧式：过对齐帧（序言把 %r13 也下调过，帧线不是 `r13 - frame_size`，
        // 得从锚点槽读回）、frame-pointer 模式（序言在 `mov %rsp,%r13` 之后多推了 8 字节，
        // 那 8 不在 `frame_size` 里）、naked（没有本序言的那条 `sub`）。
        let rsp_at_frame_line = matches!(ledger, Some(0))
            && !self.cur_func_do_align
            && !self.target.frame_pointer
            && !self.cur_func_is_naked;
        if std::env::var_os("DCC_EPI_TRACE").is_some() {
            eprintln!(
                "[epi] {} frame={} ledger={:?} do_align={} fp={} naked={}",
                if rsp_at_frame_line { "add-rsp" } else { "mov-r13" },
                self.frame_layout.frame_size,
                ledger,
                self.cur_func_do_align,
                self.target.frame_pointer,
                self.cur_func_is_naked
            );
        }
        if rsp_at_frame_line {
            // `frame_size == 0` 时 `emit_add_rsp` 自己就不发，这时正好：rsp 已经在帧线上，
            // 后面那条 `pop %r13` 读的就是入口时推进去的这一格。
            self.emit_add_rsp(self.frame_layout.frame_size);
        } else if self.cur_func_do_align {
            // 过对齐帧: %r13 已被序言下调, 不能直接 mov r13,rsp (那会漏掉对齐
            // 吃掉的那段)。改从锚点槽读回「对齐前的帧顶」。
            // mov %rsp, anchor_off(%r13) : REX.WB 8B /r (load), reg=rsp(100),
            // r/m=[r13+disp32] → modrm=0xA5, REX=0x49.
            let anchor_off = self.cur_func_anchor_off;
            let d = anchor_off as i32;
            let mut insn = Insn::raw_bytes(&[
                0x49,
                0x8b,
                0xa5,
                (d & 0xff) as u8,
                ((d >> 8) & 0xff) as u8,
                ((d >> 16) & 0xff) as u8,
                ((d >> 24) & 0xff) as u8,
            ]);
            insn.gas_text = Some(format!("mov {}(%r13), %rsp", anchor_off));
            self.emit_insn(&insn);
        } else {
            // P11: 这一条必须保留 0x49 0x8b 0xe5 形式（mov r/m64,r64 中 rsp 是 r/m，需
            // REX.B=1 扩展 r13/r/m）。走 `mov_rm_reg` 的 0x89 形式会发出不同的 REX 字节，
            // 而依赖这份字节形的快照会当场对不上。
            self.emit_insn(&Insn::raw_with_gas(&[0x49, 0x8b, 0xe5], "mov %r13, %rsp"));
        }
        // 上面的三种形状里，只有 add 那一条是「按字节退回」；旧式那两条都是「从帧基读回」，
        // 两者对 `stack_shift` 这本账是同一个终点（帧基 + 弹回 r13 的那一格），所以下面
        // 那句清零与形状无关。
        self.emit_insn(
            &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Pop, 13)
                .with_gas("pop %r13".to_string()),
        );
        if self.target.frame_pointer {
            self.emit_insn(
                &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Pop, 5)
                    .with_gas("pop %rbp".to_string()),
            );
        }
        // noreturn: 在 ret 前插入 trap 指令 (ud2)
        // 防止函数未正常返回时 (如 panic/abort/longjmp)
        // 执行流落入调用者后续代码
        if self.current_is_noreturn {
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Ud2));
        }
        // 尾声已 `mov %r13, %rsp` 复位栈指针到帧基 (16 对齐), 并弹回被调保存
        // 寄存器。`stack_shift` 此刻必须清零: 早期返回 (Stmt::Return 直接调
        // emit_func_epilogue) 是控制流分支, 它后面的落空代码块 (.L3 等) 在
        // 运行期并不会执行这里的 `pop`, 但线性发射计数器已被 `pop` 减过 8,
        // 若不清零, 后续调用点的 frame_bytes 会凭空多算 8 → rsp 在 call 处
        // 8 字节错位 → 调用 libc (snprintf/printf 内部 SSE) 时 SIGSEGV。
        // 序言末尾已 `stack_shift = 0`, 这里与之配平: 序言 push 与尾声 pop
        // 是函数级成对, 都不计入 `stack_shift` (它只计量调用点仍存活的
        // 跨调用 push, 见 codegen.rs 的字段注释)。
        self.stack_shift = 0;
        // 把尾声期间暂停掉的那本账**原样放回**，不是重新起账。
        //
        // 为什么不能重新起账（实测教训）：早期返回的尾声之后的落空代码属于另一条路径，
        // 那条路径上的 rsp 由「本函数迄今的线性账」描述 —— 若中途有过 VLA 那种
        // 动态 `subq %rax, %rsp`（账已经弃了），重新起账会把它假装成「rsp 正落在帧线」，
        // 于是同一函数的**第二个**返回点就敢发 `add $帧长,%rsp` —— 实测
        // `tests/fixtures/c/va_arg/vla_agg_via_va_arg.c` 在 -O0 下 SIGSEGV：
        // `probe` 的第一个尾声还是旧式（弃账），第二个却换成了 add，退栈少退了 VLA 那一截。
        // `stack_shift = 0` 那一招对「mod 16 同余」的判据成立（VLA 的下推是 16 对齐的），
        // 对「绝对等于零」不成立，这里不能照抄。
        self.rsp_below_frame_line = ledger;
        self.emit_return_tail();
    }

    pub fn emit_return_tail(&mut self) {
        if self.target.function_return_thunk_extern {
            self.jmp_external("__x86_return_thunk");
        } else {
            // 步骤 3: 走类型化 Ret{add_imm: None} (C3).
            let mut insn =
                Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Ret { add_imm: None }, &[]);
            insn.gas_text = Some("ret".to_string());
            if !self.emit_insn(&insn) {
                self.emit_insn(&Insn::raw_with_gas(&[0xC3], "ret"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BlockItem, Decl, Stmt};
    use crate::location::Location;
    use crate::static_locals::StaticLocalMap;

    /// 把指定名字的声明的 `home` 改成 `Spill(slot)`。今天分配器还不产出 Spill 家，所以
    /// 本用例只能这样注入 —— 验的是「带的尺寸从已交付的家导出」这条机制本身，
    /// 而不是某个分配结果。
    fn body_with_spill_home(src: &str, fname: &str, name: &str, slot: i64) -> Vec<BlockItem> {
        let mut items = body_of(src, fname);
        let mut hit = false;
        set_home(&mut items, name, slot, &mut hit);
        assert!(hit, "源里没有名为 '{name}' 的声明（用例不得因为找不到名字而空跑）");
        items
    }

    fn body_of(src: &str, fname: &str) -> Vec<BlockItem> {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        m.source()
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == fname => Some(f.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("源里没有函数 {fname}"))
            .body
            .expect("被测函数必须有体")
            .items
    }

    /// 遍历形状与 `pre_register_vars` 那一份一致（只服务于测试，不进产物路径）。
    fn set_home(items: &mut [BlockItem], name: &str, slot: i64, hit: &mut bool) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.name.name == name {
                        v.home = Some(Location::Spill(slot));
                        *hit = true;
                    }
                },
                // 其余声明（函数/类型/typedef/global asm）里没有要改的局部家。
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => set_home_stmt(s, name, slot, hit),
            }
        }
    }

    fn set_home_stmt(s: &mut Stmt, name: &str, slot: i64, hit: &mut bool) {
        match s {
            Stmt::Block { b, .. } => set_home(&mut b.items, name, slot, hit),
            Stmt::If { then_, else_, .. } => {
                set_home_stmt(then_, name, slot, hit);
                if let Some(e) = else_ {
                    set_home_stmt(e, name, slot, hit);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                set_home_stmt(body, name, slot, hit)
            },
            Stmt::Switch { cases, prelude, .. } => {
                // prelude 里的声明也是同一个变量的宿主，漏了它家绑定只改了 case 体那份。
                set_home(prelude, name, slot, hit);
                for c in cases {
                    set_home(&mut c.body.items, name, slot, hit);
                }
            },
            _ => {},
        }
    }

    fn plan_spill_slots(items: &[BlockItem], fname: &str) -> (i64, usize, i64) {
        let mut var_map: HashMap<String, (i64, crate::ast::Type)> = HashMap::new();
        let mut total = 0i64;
        let mut spill = 0i64;
        Codegen::pre_register_vars(
            &mut var_map,
            items,
            2,
            0, // cs_slots: 本用例只数槽位/字节数，与帧头坐标无关
            0, // kasan_rz_slots
            &StaticLocalMap::default(),
            fname,
            &mut total,
            &mut spill,
            0, // over_align: 测试无过量对齐变量
        );
        // 第三个读数就是「遍历真的跑了」的证据：没有它，spill == 0 可能只是根本没进入循环。
        (spill, var_map.len(), total)
    }

    const SRC: &str = "long f(long n){ long a = n; { long deep = n + 1; a += deep; } \
                        extern long g_env; long b = a + n; return b + g_env; }\n";

    /// Phase 5.1.b 的三条：**最大槽号 + 1 就是带的尺寸**；嵌套块里的声明不会漏算；
    /// 而 extern 声明不占帧内空间，就算带着 Spill 家也不得拉大带（拉大了是白吃帧，
    /// 算小了就是越界写）。最后一句钉「没有 Spill 家⇒带为 0」：那才是今天全语料的形状，
    /// 也是「本步产物逐字节不变」的根据。
    #[test]
    fn spill_band_is_sized_from_the_delivered_homes() {
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "a", 0), "f").0, 1);
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "a", 2), "f").0, 3);
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "deep", 5), "f").0, 6);
        // extern 声明不占帧内槽：就算它被塞了 Spill 家也不得拉大带（拉大了就是白吃帧），
        // 而同一个调用里普通局部仍然真的被预注册（否则这条 0 只是空跑）。
        let (for_extern, registered, watermark) =
            plan_spill_slots(&body_with_spill_home(SRC, "f", "g_env", 7), "f");
        assert_eq!(for_extern, 0, "extern 声明不得拉大 spill band");
        assert!(
            registered >= 3 && watermark > 0,
            "同一个调用必须真的预注册了局部（变量区没算起来就是遍历没跑到，上面那个 0 就是假绿）"
        );
        let (none, _, _) = plan_spill_slots(&body_of(SRC, "f"), "f");
        assert_eq!(none, 0, "没有任何 Spill 家时带必须是 0");
    }

    /// §11.23 步 2：晚分配预算的四族各自都能被静态数出来，而且与发射期实际发生的
    /// `register_var` 同口径（同一份语料的名字级对账住在
    /// `.work_tmp/p12_base/late_family_census.sh`：那里逐笔验证「实际晚分配的名字集合」
    /// 就是本用例列的四族，没有第五族）。每一族都带一条反向对照：那个形状必须**不**计费，
    /// 否则本用例只能证明「扫描把所有东西都算上了」。
    #[test]
    fn late_need_charges_the_four_late_families_exactly() {
        // 1. 复合字面量：站点名含发射序 ⇒ 站点之间不复用，16B 一份 = 2 槽
        let n = Codegen::late_need(&body_of(
            "struct S{long a,b;}; long f(int i){ long x = (struct S){i,i}.a; \
             long y = (struct S){1,2}.b; return x + y; }",
            "f",
        ));
        assert_eq!(n.compound_slots, 4, "两个复合字面量站点各 2 槽");
        // 反向对照：向量形态走 `vector_temp_base`，不在变量区 ⇒ 不得计费
        let n = Codegen::late_need(&body_of(
            "typedef int vi __attribute__((vector_size(16))); \
             long f(void){ vi v = (vi){1,2,3,4}; return v[0]; }",
            "f",
        ));
        assert_eq!(n.compound_slots, 0, "向量复合字面量不得预扣变量区槽");

        // 2. VM 尺寸元信息：同一个 tag 的多次声明只对应一槽（发射点按 key 复用）
        let n = Codegen::late_need(&body_of(
            "struct V{int n; int a[n];}; long f(int i){ struct V v; struct V w; \
             v.n=i; return w.n + i; }",
            "f",
        ));
        assert_eq!(n.vmsize_keys, vec!["struct:V".to_string()], "同 tag 去重后的键集");
        // 块级 struct 定义也绑一个尺寸键（`gen_vm_struct_bind`）
        let n = Codegen::late_need(&body_of(
            "long f(int i){ struct W{int m; int b[m];} w; w.m=i; return w.m; }",
            "f",
        ));
        assert_eq!(n.vmsize_keys, vec!["struct:W".to_string()]);
        // 反向对照：字段里没 VLA 的记录类型不绑尺寸槽
        let n = Codegen::late_need(&body_of(
            "struct P{long a,b;}; long f(long i){ struct P p; p.a=i; return p.a; }",
            "f",
        ));
        assert!(n.vmsize_keys.is_empty(), "非 VM 记录类型不得拉大预算");

        // 3. `va_start` 的 ap：按名字去重；指针形态（退化形参）不占帧槽
        let n = Codegen::late_need(&body_of(
            "long f(int i, ...){ __builtin_va_list ap; __builtin_va_start(ap, i); \
             __builtin_va_start(ap, i); return i; }",
            "f",
        ));
        assert_eq!(n.ap_names, vec!["ap".to_string()], "两次 va_start 同一 ap 只一份");
        let n = Codegen::late_need(&body_of(
            "long f(int i, ...){ __builtin_va_list a; __builtin_va_list b; \
             __builtin_va_start(a, i); __builtin_va_start(b, i); return i; }",
            "f",
        ));
        assert_eq!(n.ap_names, vec!["a".to_string(), "b".to_string()]);

        // 4. 聚合成员赋值的一般 RHS 才需要 `<agg_member_tmp>`
        let agg_src = "struct S{long a,b;}; struct T{struct S q; long g;}; \
                       long f(int c, struct S x, struct S y){ struct T t; \
                       t.q = c ? x : y; return t.g; }";
        assert!(Codegen::late_need(&body_of(agg_src, "f")).agg_tmp);
        // 反向对照：Ident / 复合字面量 RHS 走「直接拿地址」那几臂 ⇒ 不需暂存
        let no_tmp_src = "struct S{long a,b;}; struct T{struct S q; long g;}; \
                          long f(struct S x){ struct T t; t.q = x; \
                          t.q = (struct S){1,2}; return t.g; }";
        assert!(
            !Codegen::late_need(&body_of(no_tmp_src, "f")).agg_tmp,
            "排除形状不得置位（否则预算不是算准而是宁可多给）"
        );
        // 判据基准必须是 **LHS 字段类型**，不是 RHS 类型：逗号包裹的 RHS（ASan 读改写
        // 成的 `(__asan_loadN(&x), x)` 就是这个形状）RHS 自己不是聚合类型，但仍走一般臂。
        let comma_src = "struct S{long a,b;}; struct T{struct S q; long g;}; \
                         int probe(void); \
                         long f(struct T t, struct S x){ t.q = (probe(), x); return t.g; }";
        assert!(
            Codegen::late_need(&body_of(comma_src, "f")).agg_tmp,
            "逗号 RHS 的聚合成员赋值必须计费（旧写法看 RHS 类型，在这里算少 ⇒ E1133）"
        );
        // 反向对照：字段是标量时，即使 RHS 是一般表达式也不走聚合臂（否则就是乱多给）
        let scalar_src = "struct S{long a,b;}; struct T{struct S q; long g;}; \
                          int probe(void); \
                          long f(struct T t){ t.g = probe(); return t.g; }";
        assert!(
            !Codegen::late_need(&body_of(scalar_src, "f")).agg_tmp,
            "标量字段不得当聚合成员赋值计费"
        );
    }

    /// E1128 的那条账：**经运行时地址目标的聚合赋值要一个 sret 地址槽**，规划期必须
    /// 按分配器那把尺登记它。
    ///
    /// 实测现场（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.21）：
    /// `block/blk-mq.o` 的 `blk_mq_extract_queue_requests` 里 `*queue_rqs = matched_rqs;` 让
    /// 发射侧调 `alloc_sret_slot(8)`（`dispatch.rs` 的 `AggDst::Indirect` 分支），而旧扫描的
    /// `Expr::Assign` 臂只算 `sret_bytes_expr(l) + sret_bytes_expr(r)` —— 两边都不是 Call ⇒
    /// 登记 0 字节，发射期第一个槽（步长 16）就压穿 spill 带顶，整 TU 以 E1128 硬错收场。
    /// 本用例把两侧口径钉在一起：形状对了就必须留，形状不对时一格都不许多留。
    #[test]
    fn aggregate_assign_through_runtime_address_is_budgeted() {
        // 尺子本身：分配器按 16B 步进，扫描登记必须是同一个式子。
        assert_eq!(Codegen::sret_slot_bytes(0), 0);
        assert_eq!(Codegen::sret_slot_bytes(1), 16);
        assert_eq!(Codegen::sret_slot_bytes(8), 16);
        assert_eq!(Codegen::sret_slot_bytes(16), 16);
        assert_eq!(Codegen::sret_slot_bytes(17), 32);

        let src = "struct S { long a, b; }; struct T { struct S q; }; \
                   long f(struct T *t, struct S s){ *t = (struct T){ s }; return t->q.a; }";
        let items = body_of(src, "f");
        assert!(
            Codegen::max_sret_bytes(&items) >= Codegen::sret_slot_bytes(8),
            "经运行时地址目标的聚合赋值必须为 `AggDst::Indirect` 的地址槽留预算，\
             实测 max_sret_bytes={}（旧扫描在这里登记 0 ⇒ blk-mq.o 的 E1128）",
            Codegen::max_sret_bytes(&items)
        );

        // 反向对照：目标是一个具名局部（`Ident` 左值，直接按帧偏移存）时一格都不许多留。
        let local_src = "struct S { long a, b; }; long f(long k){ struct S s; s.a = k; s.b = k; \
                         struct S d; d = s; return d.a; }";
        assert_eq!(
            Codegen::max_sret_bytes(&body_of(local_src, "f")),
            0,
            "Ident 目标的聚合赋值不走地址槽那一形，不得凭空多留"
        );

        // 标量经指针写入同样不得计费（否则是把每个 `*p = v` 都当聚合）。
        let scalar_src = "long f(long *p, long v){ *p = v; return *p; }";
        assert_eq!(
            Codegen::max_sret_bytes(&body_of(scalar_src, "f")),
            0,
            "标量经指针赋值不得当聚合地址槽计费"
        );
    }
}
