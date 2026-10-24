// src/codegen/x86_64/blockopt.rs — 块内机器级优化（P13 阶段6 的地基）
//
// 为什么在这里而不是 SSA 层：`luaV_execute` / `sqlite3VdbeExec` 这类 computed-goto 函数走
// analysis-only 通道，**一条 SSA pass 都不跑**（重建 AST 需要把 phi 拷贝放到 computed goto 的
// 每一条入边上，而那些边按定义是不可知的）。于是它们的全部代码质量只来自「发射层窥孔 + 寄存器家」。
// 发射层已有的三条窥孔规则都只看**相邻两条**，跨指令的事实（这个号的值就是那个槽、这两个号
// 相等）一概看不见。本模块把「上一条」换成「本块内的状态」：
//
//   * 屏障与失效：`label()`（控制流汇合点）、任何裸字节 `emit()`、函数边界都整体清空 ——
//     与既有 `peep_last` 同一条纪律（`jcc` 例外：它的落空边继续往下走，事实在落空边上仍成立，
//     而任何跳转的**入口**必然是 `label()`，那里已经清了）。
//   * 读写集只从**类型化** `X86_64Opcode` 取；`Raw`（内联 asm、字符串指令、序言裸字节）与
//     一切「本模块认不得的变体」一律当作**全杀**（保守方向：少削一条 vs 静默错值）。
//   * 帧槽事实只对 `[r13+disp]`、无 index、无段前缀的访存建立。`push/pop` 只碰 `[rsp-8]`
//     以下的栈顶区，与 `[r13+disp]` 的帧槽区不重叠，所以它们**不清**帧槽事实（这是解释器
//     热路径里事实能活过一条指令的直接原因）；代价是 rsp 自己的事实一律清掉。
//   * 帧槽事实**不跨 volatile 对象**：volatile / 段限定的局部偏移由
//     `func_setup::note_volatile_frame_slots` 逐函数登记，凡命中就既不建事实、也不转发。
//
// 分级开关 `DCC_BLOCKOPT`（0 关 / 1 只削恒等扩展 / 2 加寄存器相等 / 3 再加帧槽转发与去重，
// 默认 3）：一旦某条规则削错，症状是别处读垃圾，能二分定位是哪一条。

use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{
    AluOp, MemOperand, Operand as O, StackOp, UnaryOp, X86_64Opcode,
};
use std::collections::HashSet;

/// 帧基址寄存器号（r13）。只有以它为基址的访存才算「帧槽」，才允许被转发/去重。
const FRAME: u8 = 13;
/// `copy` 表的空标。寄存器号只到 15，`0xFF` 不会与真号撞车。
const NO_REG: u8 = 0xFF;
/// 块内最多跟踪多少个帧槽事实。超出就整体放弃（容量上限比哈希表便宜，且行为可预测）。
///
/// `pub(crate)` 的理由：Gate 0a 的影子表（`shadow`）必须与它同容量，否则
/// `hit_capacity` 永远读不到数，A3（容量淘汰）要买的那部分机会就被量尺本身遮蔽了。
const MAX_SLOTS: usize = 16;

/// `DCC_KILL_ALL_STAT=1` 的归因仪表（方案 §3 的 P0 前置）。
///
/// 为什么要记「丢了几条」而不只记「杀了几次」：一次 `kill_all` 若本块里一条事实都没有，
/// 它一分钱损失也没有；损失等于被清掉的活事实数。不看丢失量就排不出「先修哪个站点」。
/// 计数器是进程级原子量，由发射器在函数边界 zero/dump 配对使用，因此**多 TU 并行编译时
/// 每行输出只保证与同一函数的边界配对**（同一进程内两个函数不会交叉，发射是单线程按序的）。
pub(crate) mod stat {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 站点表。编号是产物对账用的契约：**只能追加在末尾，不得重排或删除**。
    pub const SITES: &[&str] = &[
        "reset_label",          // 控制流汇合点（label 入口）
        "reset_bare_emit",      // 任何裸字节发射
        "reset_fn_boundary",    // 函数/调用边界
        "mov_rm_other",         // observe: MovRm 的未建模形态
        "movext_other",         // observe: MovExt 的未建模形态
        "alu_dst_other",        // observe: AluRm 目的非寄存器/内存
        "imul_dst_other",
        "shift_dst_other",
        "unary_muldiv",         // mul/imul/div/idiv 的隐式 rax/rdx
        "unary_dst_other",
        "lea_dst_other",
        "setcc_dst_other",
        "gprxmm_other",
        "cvt2sd_other",
        "noarg",
        "jmp_ret_call",
        "raw_unknown",          // Raw 字节解不出写集
        "catchall",             // 整个 opcode 变体没列进 observe
        "slots_capacity",       // note_slot 满 16 条 ⇒ 全弃
        "slots_alias",          // 未知基址的写 ⇒ 当作可别名
        "slots_width0",         // 宽度未知的写
        // 以下不是「杀」而是「计数」站点（hit 的两个 lost 传 0）：用来区分
        // 「事实被杀了」与「事实根本没建起来」——两者的修法完全不同。
        "cnt_slot_created",     // note_slot 成功登记一条帧槽事实
        "cnt_store_no_src",     // 写槽的源不是寄存器（算出来的值）⇒ 建不了事实
        "cnt_store_volatile",   // volatile 槽：故意不建事实
        "cnt_load_fact_hit",    // 帧槽读在 decide 里查到了事实
        "cnt_load_fact_miss",   // 帧槽读查不到事实（被杀 or 从未建立）
        "cnt_slot_by_kill_reg", // 因 `kill_reg` 连带丢弃的帧槽事实（该号不再等于那个槽）
        "cnt_slot_by_invalidate", // 因另一条写到重叠位移的写而丢弃的帧槽事实
        "cnt_copy_by_kill_reg", // 因 `kill_reg` 丢掉的「两号相等」事实
        // `raw_writes` 的四类结果。它们才能定 P0 的真实工作量：如果 Raw 大多解成
        // Unknown，那么开关 R_RAW 本身不会带来任何变化（实测确实如此），要做的是
        // 把解码器补全到能认出那些形态。
        "raw_nothing", "raw_reg", "raw_masked", "raw_unknown_decoded",
        // 处置点的权威计数：前面的 hit/miss 只说明「看到了事实」，这两行才说明
        // 「事实真的换成了不发 / 换了形态」。实测证明两者可以差很多。
        "veto_drop", "veto_replace",
        // R_CPYBASE（地址中转改写）真的换成了新基址的次数。
        "cnt_cpybase_hit",
        // S1 补全后的六个新写集形态（方案 §8.2：「S1 的价值取决于补全后有多少热路径
        // raw 真解码出写集」—— 没这几个计数器那句话无法回答）。
        "raw_regs", "raw_call", "raw_terminate", "raw_memory", "raw_slotstore", "raw_strs",
        // A2（标签汇合）真的「不整体清」了几次：不计数就无法把「A2 实现了」与
        // 「A2 在任何函数上都碰不到」分开。
        "a2_inherit",
        // S4 区域家的两个处置计数（同样只读，不进默认路径的判定）。
        "region_drop", "region_replace",
        // A2（多前驱取交）真的「按交汇保留」了几次。与 `a2_inherit`（单前驱落空继承）
        // 分开计：两者是两个不同的判据，合在一起就读不出「取交」那一臂到底有没有跑。
        "a2_intersect",
        // S3c「立即数 mov 复用」（R_DEADW）：当前这条常量写因为同一个号已经是同一个值
        // 而整条不发的次数。没有它就无法把「机制没接」与「机制没机会」分开。
        "deadw_drop",
        // S3c/S3a：`mov r64, imm32` 解出「已经是符号扩展形态」的次数（上一条恒等 `movsxd` 能削
        // 多少次全靠它）。
        "raw_immsext",
    ];
    const N: usize = 48;

    static CNT: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static SLOT_LOST: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static COPY_LOST: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    static ON: AtomicU64 = AtomicU64::new(u64::MAX);

    fn on() -> bool {
        let v = ON.load(Ordering::Relaxed);
        if v == u64::MAX {
            let e = std::env::var("DCC_KILL_ALL_STAT").map(|s| s != "0").unwrap_or(false);
            ON.store(e as u64, Ordering::Relaxed);
            return e;
        }
        v != 0
    }

    pub fn hit(site: usize, slots_lost: u64, copy_lost: u64) {
        if !on() || site >= N {
            return;
        }
        CNT[site].fetch_add(1, Ordering::Relaxed);
        SLOT_LOST[site].fetch_add(slots_lost, Ordering::Relaxed);
        COPY_LOST[site].fetch_add(copy_lost, Ordering::Relaxed);
    }

    pub fn enabled() -> bool {
        on()
    }

    /// 按站点名取编号：站点表只写一份，调用方一律用名字，避免手写数字飘移。
    pub fn site(name: &str) -> usize {
        SITES.iter().position(|s| *s == name).unwrap_or(N - 1)
    }

    pub fn zero() {
        for c in CNT.iter() {
            c.store(0, Ordering::Relaxed)
        }
        for c in SLOT_LOST.iter() {
            c.store(0, Ordering::Relaxed)
        }
        for c in COPY_LOST.iter() {
            c.store(0, Ordering::Relaxed)
        }
    }

    /// 返回 (站点名, 次数, 丢掉的帧槽事实, 丢掉的寄存器相等事实)，只留非零项。
    pub fn dump() -> Vec<(&'static str, u64, u64, u64)> {
        (0..N)
            .map(|i| (SITES[i], CNT[i].load(Ordering::Relaxed), SLOT_LOST[i].load(Ordering::Relaxed), COPY_LOST[i].load(Ordering::Relaxed)))
            .filter(|r| r.1 != 0)
            .collect()
    }

    // 变体明细：只给 `catchall` 用。知道“杀了 5895 次”不能下手，必须知道
    // 是哪几个 opcode 变体没被 observe 认识到。
    static VARIANTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, (u64, u64, u64)>>,
    > = std::sync::OnceLock::new();

    pub fn hit_variant(name: &str, slots_lost: u64, copy_lost: u64) {
        if !on() {
            return;
        }
        let m = VARIANTS.get_or_init(Default::default);
        if let Ok(mut g) = m.lock() {
            let e = g.entry(name.to_string()).or_insert((0, 0, 0));
            e.0 += 1;
            e.1 += slots_lost;
            e.2 += copy_lost;
        }
    }

    pub fn dump_variants() -> Vec<(String, u64, u64, u64)> {
        match VARIANTS.get() {
            None => Vec::new(),
            Some(m) => m
                .lock()
                .map(|g| g.iter().map(|(k, v)| (k.clone(), v.0, v.1, v.2)).collect())
                .unwrap_or_default(),
        }
    }

    pub fn zero_variants() {
        if let Some(m) = VARIANTS.get() {
            if let Ok(mut g) = m.lock() {
                g.clear();
            }
        }
    }
}

/// `DCC_FACTS_DUMP=1` 的逐条导出（仅给 `scripts/check_facts_vs_objdump.sh` 对账用）。
///
/// 只写 stderr、只在旋钮开着时跑，且不进任何判据路径 ⇒ 产物逐字节不变。
mod facts_dump {
    use super::raw_writes;

    fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var("DCC_FACTS_DUMP").map(|s| s != "0").unwrap_or(false))
    }

    pub fn note(bytes: &[u8]) {
        if !enabled() || bytes.is_empty() {
            return;
        }
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let verdict = match raw_writes(bytes) {
            super::RawWrite::Nothing => "nothing".to_string(),
            super::RawWrite::Reg(r) => format!("reg{r}"),
            super::RawWrite::Regs(m) => format!("regs{m}"),
            super::RawWrite::Masked { dst, bits } => format!("mask{dst}:{bits}"),
            super::RawWrite::ImmSExt { dst, imm } => format!("sext{dst}:{imm}"),
            super::RawWrite::SExt { dst } => format!("sext{dst}:"),
            super::RawWrite::ZExt { dst, bits } => format!("zext{dst}:{bits}"),
            super::RawWrite::Call => "call".to_string(),
            super::RawWrite::Terminate => "term".to_string(),
            super::RawWrite::Strs => "strs".to_string(),
            super::RawWrite::SlotStore { disp, len, src } => match src {
                Some(s) => format!("slot:{disp}:{len}:reg{s}"),
                None => format!("slot:{disp}:{len}:"),
            },
            super::RawWrite::Memory => "mem".to_string(),
            super::RawWrite::Unknown => "unknown".to_string(),
        };
        eprintln!("factdump {hex} {verdict}");
    }
}

// ---------------------------------------------------------------------------
// Gate 0a：影子槽表（`DCC_SLOT_SHADOW=1`）
// ---------------------------------------------------------------------------
//
// 它回答一个「机制写完之后再问就问不出来」的问题：本产物里那些重复帧载入，究竟
// 被哪种屏障挡住（call / label / 容量 / 事实被杀）？只有把三种屏障**都拿掉**再数
// 一遍「本可转发」，才知道 S2 的上限在哪。所以这一层：
//
//   * 影子表**不在** call / label / 容量溢出时整体清 —— 只被真实存槽写清（方案 §2 0a 的定义）；
//   * 每次真实表查不到事实、于是本应发一条帧载入时，问影子表「本可转发吗」；
//   * 命中就按「存槽之后最后发生的那一种屏障」归到桶里。
//
// 三个设计约束，缺一即读数不可信：
//   1. **只读**：影子层绝不改变处置（`Verdict`），也不参与发射 ⇒ 产物逐字节不变。这是
//      方案 §2「两个探针都不改产物，可同一次构建完成」的实现条件。
//   2. **不能挂住 `BlockState`**：它是 `Copy` 的内联数组，加一个 `RefCell` 字段会直接破
//      掉 `Copy`。所以影子状态放 `thread_local`：并行 codegen 下每个 worker 一条，
//      逐函数 zero/dump 配对（与 `stat` 同一纪律）。
//   3. 影子表容量与真实表**同为 16**，溢出时淘汰最久未用的一条并记下容量事件。若把
//      影子表开大，`hit_capacity` 永远为 0，而 A3（容量淘汰）要买的就是那部分机会。
pub(crate) mod shadow {
    use std::cell::RefCell;

    /// 与真实表同容量（理由见上面约束 3）。
    const CAP: usize = super::MAX_SLOTS;
    /// 空标。寄存器号只到 15，`0xFF` 不会与真号撞车。
    const FREE: u8 = 0xFF;

    #[derive(Clone, Copy)]
    struct Entry {
        disp: i32,
        len: u8,
        reg: u8,
        /// 建立这条事实时的序列号（分类屏障用）。
        seq: u64,
        /// 最后一次被读到的序列号（LRU 淘汰依据）。
        used: u64,
    }

    const EMPTY: Entry = Entry { disp: 0, len: 0, reg: FREE, seq: 0, used: 0 };

    #[derive(Default, Debug, Clone, Copy)]
    pub struct Count {
        /// 影子表本可转发、而真实表没转发的总次数（= S2 的总上限）。
        pub total: u64,
        pub hit_call: u64,
        pub hit_label: u64,
        /// 没有任何屏障，事实被 `kill_all` 一类动作打断（方案 §1 的 L1/L3 那一桶）。
        pub hit_pressure: u64,
        pub hit_capacity: u64,
        /// 影子表自己也查不到 ⇒ 这条重复载入不是屏障问题（从未建过事实）。
        pub miss_never: u64,
        /// Gate 0b：两次载入之间夹着一条 raw-Unknown（= L1 的派生，不是真压力）。
        pub hit_raw: u64,
    }

    struct St {
        entries: [Entry; CAP],
        seq: u64,
        last_call: u64,
        last_label: u64,
        last_capacity: u64,
        last_raw_unknown: u64,
        cnt: Count,
    }

    impl Default for St {
        fn default() -> Self {
            Self {
                entries: [EMPTY; CAP],
                seq: 0,
                last_call: 0,
                last_label: 0,
                last_capacity: 0,
                last_raw_unknown: 0,
                cnt: Count::default(),
            }
        }
    }

    thread_local! {
        static ST: RefCell<St> = RefCell::new(St::default());
    }

    pub fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var("DCC_SLOT_SHADOW").map(|s| s != "0").unwrap_or(false))
    }

    /// 真实存槽写：影子表与真实表一样按字节区间失效，然后记下新事实。
    /// `src = None` 是「写进去的不是寄存器」：仍失效旧事实，但不建新事实。
    pub fn note_store(disp: i32, len: u8, src: Option<u8>) {
        if !enabled() {
            return;
        }
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            s.seq += 1;
            let hi = disp as i64 + len as i64;
            for e in s.entries.iter_mut() {
                if e.reg == FREE {
                    continue;
                }
                let ehi = e.disp as i64 + e.len as i64;
                if (disp as i64) < ehi && (e.disp as i64) < hi {
                    *e = EMPTY;
                }
            }
            let Some(r) = src else { return };
            if len == 0 {
                return;
            }
            if let Some(slot) = s.entries.iter_mut().find(|e| e.reg == FREE) {
                *slot = Entry { disp, len, reg: r, seq: s.seq, used: s.seq };
                return;
            }
            // 溢出：淘汰最久未用的一条（= A3 要做的事），并记下容量屏障。
            let mut victim = 0usize;
            let mut oldest = u64::MAX;
            for (i, e) in s.entries.iter().enumerate() {
                if e.used < oldest {
                    oldest = e.used;
                    victim = i;
                }
            }
            s.entries[victim] = Entry { disp, len, reg: r, seq: s.seq, used: s.seq };
            s.last_capacity = s.seq;
        });
    }

    /// 未知基址的写：与真实表同一口径当作可别名，整表作废（那是真实写，不是屏障）。
    pub fn note_aliasing_store() {
        if !enabled() {
            return;
        }
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            for e in s.entries.iter_mut() {
                *e = EMPTY;
            }
        });
    }

    pub fn note_call() {
        if !enabled() {
            return;
        }
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            s.seq += 1;
            s.last_call = s.seq;
        });
    }

    pub fn note_label() {
        if !enabled() {
            return;
        }
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            s.seq += 1;
            s.last_label = s.seq;
        });
    }

    /// Gate 0b：一条由 raw 构造子发出、而 `raw_writes` 解不出写集的指令。
    ///
    /// 为什么这是一个**单独的**桶而不是归进 `hit_pressure`：方案 §1 的 L3（48.1% 的
    /// 「既无 call 也无 label」那一批）究竟是「寄存器真不够」还是「L1 的派生」，
    /// 只能靠这个计数分开：夹着一条 raw-Unknown 的那些是 L1 的衍生物，补完 facts
    /// 就会自己消；剩下的才是真压力。所以下面把它当作一种**屏障**参与「取最晚者」。
    pub fn note_raw_unknown() {
        if !enabled() {
            return;
        }
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            s.seq += 1;
            s.last_raw_unknown = s.seq;
        });
    }

    /// 真实表查不到、于是要发一条帧载入：问影子表「本可转发吗」并按屏障归桶。
    pub fn note_load(disp: i32, len: u8) {
        if !enabled() || len == 0 {
            return;
        }
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            let hit = s
                .entries
                .iter_mut()
                .find(|e| e.reg != FREE && e.len != 0 && e.len >= len && e.disp == disp);
            let Some(e) = hit else {
                s.cnt.miss_never += 1;
                return;
            };
            e.used = s.seq;
            s.cnt.total += 1;
            // 取「存槽之后发生得最晚的那一种屏障」为准：屏障越靠后就越先是它把事实清掉的。
            let mut kind = 0u8;
            let mut barrier = e.seq;
            if s.last_call > barrier {
                barrier = s.last_call;
                kind = 1;
            }
            if s.last_label > barrier {
                barrier = s.last_label;
                kind = 2;
            }
            if s.last_capacity > barrier {
                barrier = s.last_capacity;
                kind = 3;
            }
            if s.last_raw_unknown > barrier {
                kind = 4;
            }
            match kind {
                1 => s.cnt.hit_call += 1,
                2 => s.cnt.hit_label += 1,
                3 => s.cnt.hit_capacity += 1,
                4 => {
                    // 同时计入两个名字：`hit_raw` 是 0b 的直接读数，而它在旧的四桶
                    // 判据式里就是 `hit_pressure` 的那一部分。
                    s.cnt.hit_raw += 1;
                    s.cnt.hit_pressure += 1;
                },
                // 存槽之后没有任何屏障，事实却不在真实表里 ⇒ 被 `kill_all` 一类
                // 「既非 call 也非 label」的动作打断（方案 §1 的 L1/L3 那一桶）。
                _ => s.cnt.hit_pressure += 1,
            }
        });
    }

    pub fn dump() -> Count {
        ST.with(|c| c.borrow().cnt)
    }

    pub fn zero() {
        ST.with(|c| {
            let s = &mut *c.borrow_mut();
            *s = St::default();
        });
    }
}

/// 一条指令对 GP 号与内存的写入集（方案 §3 S4 要求的 `written_gp`，也是 S1 那条
/// 「穷尽 `match` 编译钉子」的实体）。
///
/// 与 [`BlockState::observe`] 的分工不是「两套真源」：`observe` 回答「事实表怎么变」，
/// 本函数只回答「写了什么」。区域家（S4）需要的是后者 —— 它得知道「这个号现在能不能
/// 拿来当家」，而不是「该不该清某条事实」。
///
/// 为什么单独抽一个穷尽 `match` 而不是复用 `observe`：`observe` 末尾有 `other => 全杀`
/// 的 catch-all，新增一个 `X86_64Opcode` 变体会**默默**落进去（方向安全但永远没人知道）。
/// 本函数不留 catch-all：新增变体不回答「写了什么」就编不过（方案 §3 S1 的编译钉子）。
/// 两者对同一批变体必须给同一个保守度，那条不变式由用例
/// `written_gp_agrees_with_observe_on_the_kill_all_set` 钉住。
pub(crate) fn written_gp(insn: &Insn) -> RegWrites {
    use X86_64Opcode as Op;
    match &insn.op {
        // 目的在寄存器：只写那一个号。目的在内存：只写内存。
        Op::MovRm { dst, .. } | Op::MovExt { dst, .. } | Op::Lea { dst, .. } | Op::ImulRm { dst, .. }
        | Op::Shift { dst, .. } | Op::Setcc { dst, .. } => match dst {
            O::Reg(r, _) => RegWrites::Regs(regbit(*r)),
            O::Mem(_) => RegWrites::Mem,
            _ => RegWrites::All,
        },
        Op::AluRm { op, dst, .. } => {
            if matches!(op, AluOp::Cmp | AluOp::Test) {
                RegWrites::None
            } else {
                match dst {
                    O::Reg(r, _) => RegWrites::Regs(regbit(*r)),
                    O::Mem(_) => RegWrites::Mem,
                    _ => RegWrites::All,
                }
            }
        },
        // mul/imul/div/idiv 的隐式目的号是 rax(±rdx)，而 rm 只读。
        Op::UnaryRm { op, dst, .. } => {
            use crate::codegen::x86_64::opcode::UnaryOp;
            match op {
                UnaryOp::Mul | UnaryOp::Imul => RegWrites::Regs(regbit(0)),
                UnaryOp::Div | UnaryOp::Idiv => RegWrites::Regs(regbit(0) | regbit(2)),
                _ => match dst {
                    O::Reg(r, _) => RegWrites::Regs(regbit(*r)),
                    O::Mem(_) => RegWrites::Mem,
                    _ => RegWrites::All,
                },
            }
        },
        // push 写内存（栈顶）并改 rsp；pop 写一个号与 rsp。
        Op::Stack { op, dst, .. } => {
            let rsp = regbit(4);
            match op {
                crate::codegen::x86_64::opcode::StackOp::Pop => match dst {
                    O::Reg(r, _) => RegWrites::Regs(rsp | regbit(*r)),
                    _ => RegWrites::All,
                },
                _ => RegWrites::Regs(rsp).and_mem(),
            }
        },
        // SIMD 算术/搬运/取整/移位/比较：变体本身就只会写 xmm（目的为寄存器时一定不是
        // GP），内存目的则是一次宽写。与 `observe` 的 `SimdArith` 臂同一条保守度。
        Op::SimdMov { dst, .. }
        | Op::SimdArith { dst, .. }
        | Op::SimdShiftImm8 { dst, .. }
        | Op::SimdRound { dst, .. }
        | Op::SimdCmpImm8 { dst, .. } => match dst {
            O::Mem(_) => RegWrites::Mem,
            O::Reg(_, _) => RegWrites::None,
            _ => RegWrites::All,
        },
        // shld/shrd：目的是 GP（dst），并且读 cl。
        Op::ShldShrd { dst, .. } => match dst {
            O::Reg(r, _) => RegWrites::Regs(regbit(*r)),
            O::Mem(_) => RegWrites::Mem,
            _ => RegWrites::All,
        },
        // cmovcc / bsf/bsr/tzcnt 类：目的恒在 GP 寄存器。
        Op::Cmovcc { dst, .. } | Op::BitScan { dst, .. } => match dst {
            O::Reg(r, _) => RegWrites::Regs(regbit(*r)),
            _ => RegWrites::All,
        },
        // lock 前缀的 cmpxchg/xadd 同时写 `rax` 与目的（可能还是内存），xchg 写两个号：
        // 与 `observe` 里那几个变体同样保守，不拆细。
        Op::LockCmpxchgRm { .. } | Op::LockXchgRm { .. } | Op::LockXaddRm { .. } | Op::XchgRm { .. }
        | Op::Pmulld { .. } => RegWrites::All,
        // x87：状态栈与 ST(0) 不在 GP 号体系里，而带 mem 的形还会读写那块内存。
        Op::X87 { mem, .. } => {
            if mem.is_some() {
                RegWrites::Mem
            } else {
                RegWrites::All
            }
        },
        // rip 相对的 SIMD 搬运：写的是那个数据对象的内存，不是任何 GP 号。
        // 不能当「不写内存」处理：帧数据可能被它盖到（本仓的 `.rodata`/数据节写入）。
        Op::SimdMovRip { .. } => RegWrites::Mem,
        Op::GprXmm { to_xmm, dst, .. } | Op::Cvt2Sd { to_xmm, dst, .. } => {
            if *to_xmm {
                RegWrites::None
            } else {
                match dst {
                    O::Reg(r, _) => RegWrites::Regs(regbit(*r)),
                    O::Mem(_) => RegWrites::Mem,
                    _ => RegWrites::All,
                }
            }
        },
        // 只读 flags 的条件跳转：落空边继续往下走，GP 号一个不动。
        Op::Jcc { .. } => RegWrites::None,
        // 跳转/返回/调用与分派尾部：call 破坏 caller-saved，jmp/ret 结束本块。
        Op::Call { .. } | Op::CallRel32 => RegWrites::Regs(SYSV_CALLER_SAVED).and_mem(),
        Op::Jmp { .. } | Op::Ret { .. } | Op::JmpRmMem { .. } | Op::JmpRel32 => RegWrites::All,
        // `push [base+disp8]` 是一次栈写并改 rsp（与 `Stack` 的 push 同一条处置）；
        // 另一条同族的无操作数变体（下面那个 `_ =>` 永远走不到的兜底）保守到 `All`。
        Op::PushMemDisp8 { .. } => RegWrites::Regs(regbit(4)).and_mem(),
        Op::NoArg { op } => {
            use crate::codegen::x86_64::opcode::NoArgOp;
            match op {
                NoArgOp::Cld => RegWrites::None,
                _ => RegWrites::All,
            }
        },
        // raw 字节流：写集由 `raw_writes` 回答，两者不得各说一套。
        Op::Raw { bytes, .. } => match raw_writes(bytes) {
            RawWrite::Nothing | RawWrite::Terminate => RegWrites::None,
            RawWrite::Reg(r) | RawWrite::Masked { dst: r, .. } => RegWrites::Regs(regbit(r)),
            RawWrite::Regs(m) => RegWrites::Regs(m),
            RawWrite::Call => RegWrites::Regs(SYSV_CALLER_SAVED).and_mem(),
            RawWrite::Memory => RegWrites::Mem,
            RawWrite::SlotStore { .. } | RawWrite::Strs | RawWrite::Memory => RegWrites::Mem,
            RawWrite::ImmSExt { dst, .. } => RegWrites::Regs(regbit(dst)),
            RawWrite::SExt { dst } => RegWrites::Regs(regbit(dst)),
            RawWrite::ZExt { dst, .. } => RegWrites::Regs(regbit(dst)),
            RawWrite::Unknown => RegWrites::All,
        },
        // 到这里就没有「默认全杀」了：上面必须逐变体回答。新加一个 `X86_64Opcode`
        // 不填本函数就编不过，这就是 S1 要的编译钉子。
    }
}

/// [`written_gp`] 的值域。`All` 是「未知且可能很大」（保守到整张表作废）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegWrites {
    /// 不写任何 GP 号也不写内存。
    None,
    /// 只写这些 GP 号。
    Regs(u16),
    /// 只写内存。
    Mem,
    /// 既写号又写内存。
    RegsMem(u16),
    /// 未知/可能碰任何东西。
    All,
}

impl RegWrites {
    /// 这个号是不是 callee-saved（SysV）：只有 callee-saved 的号才可能被区域家当作
    /// 一个「能活过下一条求值」的家号（S4 的读数：把家绑在 `%rax` 上 606 次里 521 次
    /// 立刻被下一次求值踩掉，一次服务都拿不到）。
    pub(crate) const fn callee_saved(r: u8) -> bool {
        SYSV_CALLER_SAVED & regbit(r) == 0
    }

    fn and_mem(self) -> RegWrites {
        match self {
            RegWrites::Regs(m) => RegWrites::RegsMem(m),
            other => other,
        }
    }

    /// 这个写集能不能让一个**已选的号**继续当家。
    pub(crate) fn keeps(self, r: u8) -> bool {
        match self {
            RegWrites::None | RegWrites::Mem => true,
            RegWrites::Regs(m) | RegWrites::RegsMem(m) => m & regbit(r) == 0,
            RegWrites::All => false,
        }
    }
}

/// 一次内存访问的方向。区域家（S4）必须把「读了这个槽」与「写了这个槽」分开：
/// 读可以让家继续存在（内存与家一致时读谁都行），写则决定家里那个号还作不作数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemDir {
    Read,
    Write,
    ReadWrite,
}

impl MemDir {
    pub(crate) fn reads(self) -> bool {
        matches!(self, MemDir::Read | MemDir::ReadWrite)
    }
    pub(crate) fn writes(self) -> bool {
        matches!(self, MemDir::Write | MemDir::ReadWrite)
    }
}

/// 一条 raw 字节流里的内存操作数与**本帧**的关系。
///
/// 为什么要有这个函数而不去复用 [`raw_writes`]：那边回答「写了哪些号/写没写内存」，
/// 而区域家要的是「这条指令**读**了哪个槽、区间多大」。两者必须共用同一个头解码器
/// （[`raw_head`] / [`raw_modrm`] / [`raw_op_len`]），否则「写集说这条不碰内存、区间说它
/// 碰了那个槽」这种分裂就是静默错值 —— 所以它长在本模块里，而不是 `region.rs` 里再写
/// 一份 modrm/SIB 解释。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawFrame {
    /// 没有内存操作数：纯寄存器形、立即数形、相对跳转、无操作数形。
    NoMem,
    /// 恰好访问 `[r13+disp]` 上一个固定区间（无 index、无段前缀依赖）。
    Slot { disp: i32, len: u8, dir: MemDir },
    /// 有一次内存访问，但基址**确定**不是 r13（堆、`rsp` 栈顶区、rip 相对数据）。
    Foreign,
    /// 说不清：SIB 带 index 且基址/变址里有 r13、`0F` 双字节表、认不得的编码族。
    /// 调用方必须按「可能碰到任何槽」处置。
    Opaque,
}

/// 解 raw 字节的内存操作数落在哪里。`allow_lea_addr` 为 `true` 时 `lea` 的内存字段
/// 按「只是取地址、不访问」算（区域家要用它判断「帧地址是否被导出」）。
///
/// 方向表只列本后端真会发出的那些族；列不出来的族一律 `Opaque`（多回写一次家只是丢
/// 优化，漏一次就是错值）。
pub(crate) fn raw_frame(bytes: &[u8]) -> RawFrame {
    let Some(h) = raw_head(bytes) else {
        return RawFrame::Opaque;
    };
    let Head { prefix, rex, op_at, op0 } = h;
    if op_at == usize::MAX {
        // 只剩前缀字节：它自己不访问内存。
        return RawFrame::NoMem;
    }
    // 双字节表（SSE/`movzx`/`cmovcc`/`cpuid`…）这一族本函数不解：它们的搬运宽度与
    // 读写方向各不相同，说清的成本高于收益 ⇒ 一律 Opaque。
    if op0 == 0x0F || matches!(op0, 0xD8..=0xDF) {
        // x87 的访存形同理。
        return RawFrame::Opaque;
    }
    // 不带 modrm 的族：要么没有内存操作数，要么它的地址藏在 rsp/ri 里（与帧无关）。
    match op0 {
        // 相对跳转/调用/返回、`mov r,imm`、累加器立即数形、`pushf/popf`、`leave`、
        // 串指令（rsi/rdi/rcx 的地址）、in/out、`int3`、`nop` 等：都不是 `[r13+disp]`。
        // 串指令写的是 `(%rdi)`，基址确定不是 r13 ⇒ Foreign。
        0x04 | 0x05 | 0x0C | 0x0D | 0x14 | 0x15 | 0x1C | 0x1D | 0x24 | 0x25 | 0x2C | 0x2D
        | 0x34 | 0x35 | 0x3C | 0x3D | 0x50..=0x5F | 0x68 | 0x6A | 0x70..=0x7F | 0x90..=0x99
        | 0x9B | 0x9C | 0x9D | 0xA4 | 0xA5 | 0xA6 | 0xA7 | 0xAA | 0xAB | 0xAC | 0xAD | 0xAE
        | 0xAF | 0xB0..=0xBF | 0xC2 | 0xC3 | 0xC9 | 0xCC | 0xCD | 0xE0..=0xEF
        | 0xF4 | 0xF5 | 0xF8..=0xFD => {
            // `0xFE/0xFF`（inc/dec/push/jmp 那一族）带 modrm，必须往下走；它们不在
            // 这一段里，不是遗漏而是有意为之。
            return RawFrame::NoMem;
        }
        _ => {}
    }
    let Some(md) = raw_modrm(bytes, op_at + 1, rex) else {
        return RawFrame::Opaque;
    };
    let byte_form = matches!(
        op0,
        0x00 | 0x02 | 0x08 | 0x0A | 0x10 | 0x12 | 0x18 | 0x1A | 0x20 | 0x22 | 0x28 | 0x2A
            | 0x30 | 0x32 | 0x38 | 0x3A | 0x80 | 0x88 | 0x8A | 0xC0 | 0xC6 | 0xD0 | 0xD2
            | 0xF6
    );
    let len = raw_op_len(prefix, rex, byte_form);
    if md.is_reg {
        // mod=11：目的与源都在寄存器里，一次内存访问都没有。`Codegen::mov` 那条
        // `48 8B /r` 的搬运族正落在这一臂（它不该被当成「可能碰帧」而赶走家）。
        return RawFrame::NoMem;
    }
    // SIB：`raw_modrm` 只说「是内存但不是固定位移」。基址或变址里有 r13 就说不清区间
    // ⇒ Opaque；两者都不是 r13 ⇒ 与帧无关。
    if bytes[op_at + 1] & 7 == 4 {
        let frame_sib = match bytes.get(op_at + 2) {
            Some(sib) => {
                let base = (sib & 7) | if rex & 0x01 != 0 { 8 } else { 0 };
                let idx = ((sib >> 3) & 7) | if rex & 0x02 != 0 { 8 } else { 0 };
                base == FRAME || (idx != 4 && idx == FRAME) || (sib & 7 == 5 && base == FRAME)
            },
            // SIB 字节都不完整：字节流被截断，只能按说不清处理。
            None => true,
        };
        return if frame_sib { RawFrame::Opaque } else { RawFrame::Foreign };
    }
    // `lea` 只是把地址算进目的号，不访问那块内存 —— 但它导出了帧地址，调用方要按
    // 「这个对象的地址可能被留着以后用」处置，所以这里仍报出区间而不是 `NoMem`。
    let dir = mem_dir(op0, md.reg);
    if op0 == 0x8D {
        return match md.frame_slot {
            Some(disp) => RawFrame::Slot { disp, len, dir: MemDir::ReadWrite },
            None => RawFrame::Foreign,
        };
    }
    let dir = match dir {
        Some(d) => d,
        None => return RawFrame::Opaque,
    };
    match md.frame_slot {
        Some(disp) => RawFrame::Slot { disp, len, dir },
        None => RawFrame::Foreign,
    }
}

/// `reg` 字段是 modrm.reg（ALU 立即数形里它装着 `/digit`）。`None` = 这一族没列进
/// 方向表 ⇒ 调用方必须按 Opaque 处理。
fn mem_dir(op0: u8, reg: u8) -> Option<MemDir> {
    Some(match op0 {
        // `mov r/m, r`（0x88/0x89）与 `mov r/m, imm`（0xC6/0xC7）：内存是目的。
        0x88 | 0x89 | 0xC6 | 0xC7 => MemDir::Write,
        // `mov r, r/m`（0x8A/0x8B）、`movsxd`（0x63）、`imul r,r/m,i`（0x69/0x6B）与
        // 奇数（reg 是目的）ALU 形：内存只读。
        0x8A | 0x8B | 0x63 | 0x69 | 0x6B | 0x02 | 0x03 | 0x0A | 0x0B | 0x12 | 0x13 | 0x1A
        | 0x1B | 0x22 | 0x23 | 0x2A | 0x2B => MemDir::Read,
        // `cmp`/`test` 两种 mod 都只读。
        0x38 | 0x39 | 0x3A | 0x3B | 0x84 | 0x85 | 0xA8 | 0xA9 => MemDir::Read,
        // 偶数形（rm 是目的）ALU：读改写。
        0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29
        | 0x30 | 0x31 => MemDir::ReadWrite,
        // 移位与半移位：rm 是目的，读改写（`shl [槽],1` 在解释器的 pc 更新里真会出现）。
        0xC0 | 0xC1 | 0xD0 | 0xD1 | 0xD2 | 0xD3 | 0x8F => MemDir::ReadWrite,
        // ALU 立即数形：`/7`（cmp）只读，其余读改写。
        0x80 | 0x81 | 0x83 => {
            if reg == 7 {
                MemDir::Read
            } else {
                MemDir::ReadWrite
            }
        },
        // `f6/f7`：`/0``/1` 是 test（只读），`/2``/3` 是 not/neg（读改写），
        // `/4`–`/7` 是 mul/imul/div/idiv（内存只读，写的是 rax/rdx）。
        0xF6 | 0xF7 => match reg {
            0 | 1 => MemDir::Read,
            2 | 3 => MemDir::ReadWrite,
            4..=7 => MemDir::Read,
            _ => return None,
        },
        // `fe/ff`：`/0``/1` inc/dec（读改写）、`/6` push（只读那个槽）、`/2`–`/4`
        // call/jmp（读的是代码地址，按只读算）；`/7`（jmp far）本后端不发 ⇒ Opaque。
        0xFE | 0xFF => match reg {
            0 | 1 => MemDir::ReadWrite,
            2..=4 | 6 => MemDir::Read,
            _ => return None,
        },
        _ => return None,
    })
}

/// 一条帧槽等价事实：`[r13+disp]` 的低 `len` 字节 == 号 `reg` 的低 `len` 字节。
///
/// `stamp` 只给 A3 的淘汰序用（最久未用 = `stamp` 最小），默认档位下不参与任何判定。
#[derive(Clone, Copy)]
struct SlotFact {
    disp: i32,
    len: u8,
    reg: u8,
    stamp: u32,
}

const EMPTY_SLOT: SlotFact = SlotFact {
    disp: 0,
    len: 0,
    reg: NO_REG,
    stamp: 0,
};

/// 本块内的等价状态。字段都是小容量定长数组：寄存器只有 16 个，块内跟踪不必用哈希。
#[derive(Clone, Copy)]
pub(crate) struct BlockState {
    /// `copy[r] = s`：号 `r` 此刻的 64 位值与号 `s` 整条相同。
    copy: [u8; 16],
    /// `sext32[r]`：号 `r` 已经是「低 32 位符号扩展」的规范形态 ⇒ `movsxd r,r` 是恒等。
    sext32: [bool; 16],
    /// `zext32[r]`：号 `r` 的高 32 位是零（32 位写/载入的自动结果）⇒ `mov r32,r32` 是恒等。
    zext32: [bool; 16],
    /// `kval[r] = Some(v)`：号 `r` 此刻的**整字值就是 `v`**（只能由“写进一个已知常量”与
    /// “从另一个已知号 64 位抄过来”两件事建立，其余一律抹掉）。
    ///
    /// 它存下来的是 `R_DEADW` 那一臂的依据：`mov rD, K` 而 `rD` 已经是 `K` 时，**当前**
    /// 这一条就是死写，`decide` 可以整条不发。与“上一条死写要回删”方向不同 ——
    /// 那个在这条通道上结构上做不到（只能删尾），已由 `instructions.rs` 的 `dead_pc` 吃掉了
    /// 相邻/跨若干条不碰此号的形状。
    kval: [Option<i64>; 16],
    slots: [SlotFact; MAX_SLOTS],
    /// 本块的单调时计（只给 A3 的 LRU 序用）。每次 `observe` 前进一格；读命中一条
    /// 槽事实也刷一次。默认档位下它只累加不参与判定 ⇒ 不改变任何字节。
    clock: u32,
    /// 本次处置用的规则位图（由 `observe_at_call` 在进函数体时抄进来）。
    ///
    /// 为什么拄在状态里而不是读全局 `rules()`：`rules()` 是 `OnceLock`，进不了单测的
    /// 「开一个位、关一个位的」两条臂。A1/A3 都需要能在同一个测试进程里切位图。
    rules: u16,
    /// 值域事实：`zbits[r] = n` 表示「号 r 当前的值 < 2^n」（0 = 未知）。
    ///
    /// 为什么不用 `zext32` 就够：`and r8, 0xff` 之后 `movsxd r8, r8d` 是恒等，因为值连
    /// bit31 都是 0；而 `zext32` 只说「高 32 位为零」，推不出这条。发射层里这类掩码
    /// 全走 `Opcode::Raw`（立即数形态），所以生产点在 `raw_writes` 的解码里。
    zbits: [u8; 16],
}

impl Default for BlockState {
    fn default() -> Self {
        Self {
            copy: [NO_REG; 16],
            sext32: [false; 16],
            zext32: [false; 16],
            kval: [None; 16],
            zbits: [0; 16],
            slots: [EMPTY_SLOT; MAX_SLOTS],
            clock: 0,
            rules: 0,
        }
    }
}

/// 一条指令的处置。
pub(crate) enum Verdict {
    /// 照原样发。
    Keep,
    /// 整条不发（它是恒等/重复读/死写）。
    Drop,
    /// 换形态后再发（帧槽读 → 寄存器读）。
    Replace(Box<Insn>),
}

/// 宽度 → 字节数。向量/未知宽度一律 0（调用方据此放弃优化）。
fn width_bytes(w: Width) -> u8 {
    match w {
        Width::B8 => 1,
        Width::B16 => 2,
        Width::B32 => 4,
        Width::B64 => 8,
        Width::B128 => 16,
        Width::B256 => 32,
        Width::B512 => 64,
        // 宽度由上下文决定：本模块不知道这条写了几字节，所以 0 = 「未知」
        // （调用方一律放弃建事实，写操作则整体清空槽事实）。
        Width::Bare => 0,
    }
}

/// 帧槽的偏移；不是「`[r13+disp]` 且无 index/无段」就返回 `None`。
fn frame_disp(m: &MemOperand) -> Option<i32> {
    if m.base != Some(FRAME) || m.index.is_some() || m.seg.is_some() {
        return None;
    }
    Some(m.disp)
}

fn is_reg(o: &O) -> Option<u8> {
    match o {
        O::Reg(r, _) => Some(*r),
        _ => None,
    }
}

pub(crate) fn level() -> u8 {
    static L: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *L.get_or_init(|| {
        std::env::var("DCC_BLOCKOPT")
            .ok()
            .and_then(|v| v.trim().parse::<u8>().ok())
            .unwrap_or(3)
            .min(3)
    })
}

// ---------------------------------------------------------------------------
// 规则位图（P13 阶段6 批0c）
// ---------------------------------------------------------------------------
//
// 为什么不是「再加一级」：`level` 是单调累积的（1 恒等扩展 / 2 加寄存器相等 /
// 3 再加帧槽转发与去重），于是「只关其中一条」在今天的开关面上做不到。批2 要跑的
// 是六臂格阵（base / all / −F / −D / −C / −(D+C)），每一项都必须能单独关：一条规则
// 单独变慢而整批净收益为正时，正确的处置是关掉那一条而不是回退整批。
// `level` 继续存在并且语义不变：它是位图的**默认来源**，因此
// `DCC_BLOCKOPT=0..3` 的产物字节与本改动之前逐字相同（判据见
// `scripts/check_reproducible_output.sh` 与本文件的 `rules_for_maps_levels_without_drift`）。
// `DCC_BLOCKOPT_RULES` 一旦给出就是**完整位图**（不与 level 叠加），二分定位时
// 「全开」与「只关某条」都是可写的表达式；解析失败按未设置处理（宁可得不到新臂，
// 也不要静默落到一个没人预期的位图上）。
pub(crate) const R_SEXT: u16 = 1 << 0;
pub(crate) const R_COPY: u16 = 1 << 1;
pub(crate) const R_SLOT: u16 = 1 << 2;
/// 批2 的三项在此接入；位号先占好，免得接入时再改动已有判据。
/// R_ZBITS：`zbits` 值域事实（掩码后 / `movzx` 后的 sext 恒等）。
/// R_RAW：`Raw` 里「只写目的号一个」的形态按 `kill_reg` 而不是 `kill_all`。
pub(crate) const R_ZBITS: u16 = 1 << 3;
/// R_DEADW：**当前这一条常量写是死写**（同一个号已经是这个值）⇒ 整条不发。
///
/// 与旧登记不矛盾：“上一条 `mov $imm, r` 后来被覆盖”要删的是**上一条**，那需要回删
/// 已发字节，发射层的 `dead_pc` 已经在吃相邻与“跨若干条不碰此号”的形状；而“同一个
/// 常量被重复写进同一个号”是**当前这条**冗余，`decide` 一层就能处置 —— 旧注释说
/// “结构上做不到”只对前半个方向成立。
/// 方案 §3 S3c 说的「立即数 `mov` 复用」就是这一条：`luaV_execute` 里同值重复写保守估计
/// 有 83 处（只算 `mov r,imm` 与 `mov r,r` 传播），现有任何一条臂都抽不到它们。
pub(crate) const R_DEADW: u16 = 1 << 4;
pub(crate) const R_RAW: u16 = 1 << 5;
/// R_CPY32：`mov r64,r64` 在**源号高 32 位已知为零**时改用 32 位形态（批 2 C-lite 第二条）。
/// 只在两条长度相同时换（见 `raw_mov_rr` 那条臂里的注释）。
pub(crate) const R_CPY32: u16 = 1 << 6;
/// R_CPYBASE：内存访问的**基址只是另一个号的副本**时，把基址换成那个号。
///
/// 这一位要吃的就是方案 §2 根因 2 的 (c) 项「地址本身被存取 1,157 条 = 30.2%」：
/// 发射器算地址时习惯先把基址复制进 `%rax` 再走 `(%rax)`，于是每条访存白多一条
/// `mov rax, <号>`。已有的 `R_COPY` 只把「两号相等」当**删掉搬运**的判据（搬运本身
/// 还在），本位把它用成**换基址**。
pub(crate) const R_CPYBASE: u16 = 1 << 7;
/// R_CALLSLOT（S2 A1）：`call` 只清寄存器事实，帧槽事实按护栏保留。
///
/// 声部只占一个位而不是复用 `R_RAW`：A1 的正确性前提是「callee 写不到本帧的槽」，
/// 那是一个控制流护栏结论，与「raw 字节能不能解出写集」是两件事，必须能单独关。
pub(crate) const R_CALLSLOT: u16 = 1 << 8;
/// R_TYPED：类型化变体里「旧版落在 catch-all 全杀、其实只写 xmm」的那几条（`SimdArith`）。
/// 与 `R_RAW` 一样是可单独关的一档，默认不开 ⇒ `DCC_BLOCKOPT=0..3` 逐字节不变。
pub(crate) const R_TYPED: u16 = 1 << 9;
/// R_LRU（S2 A3）：`MAX_SLOTS` 满时淘汰一条最久未用的事实，而不是 `clear_slots()` 全弃。
///
/// 实测门限制本身在 Lua 上不绑（Gate 0a 的 `hit_capacity=0`），但「不绑」与「不存在」
/// 是两件事：一个函数只要同时持有 >16 个活跃帧槽，旧版就会把它的全部槽事实清零，
/// 那类函数的形状与 Lua 不同。保留可单独关的一档，并由用例
/// `slot_capacity_evicts_one_not_all` 钉住两臂的差异。
pub(crate) const R_LRU: u16 = 1 << 10;
/// R_LABELJOIN（S2 A2 的可单遍实现那一臂）：没有任何跳转指向它、也不是后端自造名
/// （`.L`）的用户标签，唯一前驱就是落空 —— 那种标签不整体清事实。
///
/// 为什么不是方案 §8.3-2 描述的「发射后 pass、多前驱取交」：交需要所有前驱的事实快照，
/// 而单遍直写在 `label(L)` 时永远看不到以后才发出的回边。要拿到它就得把本函数
/// 重发一遗（或者先建块图）——那是架构改动，不是这一位能涵盖的。本臂只做到
/// 「前驱集在发射前就能定完」的那一类（唯一前驱 = 落空），剩下的多前驱与循环头
/// 仍照旧整体清。那一部分的真实上限已由 Gate 0a 量出（见报告）。
pub(crate) const R_LABELJOIN: u16 = 1 << 11;
/// R_IMMSEXT（S1/S3a 第十四轮补的那批事实）：从 raw 字节里解出「整字写，而写进去的值
/// 已经是低 32 位的符号/零扩展形态」（`mov r64,imm32`、`movsx r64,r/m8|16|32`、
/// `movzx r64,r/m8|16`），从而削掉紧随其后的恒等 `movsxd d,d`。
///
/// 为什么单独一个位而不并进 `R_SEXT`/`R_ZBITS`：那两个位本来就在默认档里跑着旧来源的
/// 事实，拿它们做量尺测不出「本轮补的这批事实本身赚不赚」（实测就是证据：`R_SEXT`
/// 整位关掉时 Lua 慢 3.6%、SQLite 快 5.5%，那个数字属于旧形状，不属于本轮改动）。
/// 翻默认的依据写在 [`rules_for`] 的注释里（同轮配对：SQLite 保留、Lua 不可判）。
pub(crate) const R_IMMSEXT: u16 = 1 << 12;

/// `level` 的各档语义映射到本位图。
///
/// **档 3（默认）在 S1 这一轮含入了 `R_RAW`**：方案 §8.3-1 的原文判据是「只补 facts
/// 不改默认（或不让 catch-all 走 `raw_writes`），默认产物一条不省」——那批 facts
/// 不翻默认就是死代码。实测翻后的读数：`luaV_execute` 11210 → **10986**（−224 条，
/// 旧稿估计的 −22 条是十倍低估），r13 引用 2657 → 2437。
/// 同时含入 `R_ZBITS`（S3a 的 movsxd 消除：685 → **424**）。
/// 仍然不默认开的三项都有**双负载**实测根据：`R_CALLSLOT`（A1）在 Lua 上买 0 条（Gate 0a 的
/// `hit_call=0`），在 SQLite 上也一字不变（`sqlite3VdbeExec` 默认位图 vs +256：insn 19634 /
/// movrr 3162 / r13 4134 / movsxd 1020 全同，整个 `sqlite3.o` 只差 208 字节）—— 因为 Vdbe 里
/// `pCtx`/data 指针被取过地址，`addr_escaped` 护栏说「callee 写得着本帧的槽」，A1 按 §4.7
/// 必须不清：这是护栏在挡一条不安全的事实保留，不是量尺失灵。`R_LRU`（A3）同样买 0 条
/// （`hit_capacity=0`，在 `lvm.o` 上打开后产出逐字节相同的对象），`R_TYPED` 买 0 条。
///
/// 旧的「`DCC_BLOCKOPT=0..3` 逐字不变」不变量在本轮**故意不再成立**（它成立在一批
/// 永不生效的代码上）；`0..2` 三档依旧逐字不变，判据见
/// `rules_for_maps_levels_without_drift`。
///
/// 默认档位图里的 `R_IMMSEXT` 自第十四轮起在内，根据是两负载的同轮配对：
/// SQLite `median(r)=0.9482`、95%CI=[0.9334,0.9588]、**25/25 同向** ⇒ 保留；
/// Lua `median(r)=0.9847`、CI=[0.9812,0.9947]、31/41 同向 ⇒ 不可判（不记它的收益，但也不回退）。
/// 本轮之前还有一个量尺错：先拿整位 `R_SEXT` 去测，量到的是「旧形状 + 新事实」的混合
/// （SQLite 当时读成 1.0553 回退），拆出单独一个位之后才看清本轮那批事实自己是赚的。
pub(crate) fn rules_for(level: u8) -> u16 {
    match level {
        0 => 0,
        1 => R_SEXT,
        2 => R_SEXT | R_COPY,
        _ => R_SEXT | R_COPY | R_SLOT | R_RAW | R_ZBITS | R_IMMSEXT,
    }
}

/// 解析 `DCC_BLOCKOPT_RULES`：十进制或 `0x` 十六进制；超出一个字节（或有符号、
/// 带空白以外的杂质）就视为无效 ⇒ 按未设置处理。
pub(crate) fn parse_rules(s: &str) -> Option<u16> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u16::from_str_radix(hex, 16).ok();
    }
    t.parse::<u16>().ok()
}

/// 本进程生效的规则位图。与 `level()` 一样只算一次（`OnceLock`）：位图在发射途中
/// 变化会让同一函数的两块按不同规则发射，那种产物没人能复现。
pub(crate) fn rules() -> u16 {
    static R: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        std::env::var("DCC_BLOCKOPT_RULES")
            .ok()
            .and_then(|v| parse_rules(&v))
            .unwrap_or_else(|| rules_for(level()))
    })
}

/// 一条 `call` 对本块帧槽事实的影响范围（S2 A1 的输入）。
///
/// 为什么它是一个**传进来的结论**而不是在本模块里算的判据：四个清槽条件
/// （returns_twice / addr_escaped / VLA / 内联 asm）的真源在 `crate::frame_guard`，
/// 寄存器家绑定那边也读它。在本模块再拼一份就是两套可以飘动的判据（方案 §4.7）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallPolicy<'a> {
    /// 护栏说本帧的槽事实原则上可以活过一次调用（四条硬互斥都不成立）。
    pub slots_survive: bool,
    /// 例外的那批槽偏移：地址被取走过的局部（`escaped_frames`）。护栏只能回答「整个函数」，
    /// 而 callee 能写的只是那一个逃逸对象，所以这里按**槽**剔。拿掉这一列就是拿粗粒度
    /// 判据当细粒度用：`luaV_execute` 一个家都提升不了，A1 也永远不生效。
    pub must_clear: &'a HashSet<i64>,
}

impl<'a> CallPolicy<'a> {
    /// 「不参A1」的那一档：与今天逐字相同（整表全杀），给两参 `observe` 与单测用。
    ///
    /// 为什么不走 `Default`：`must_clear` 是一个引用，而默认值必须指向一张**活得比
    /// 本次调用久**的表; 现成的答案就是下面那个函数级的 `empty_marks()`。
    pub fn no_relax() -> CallPolicy<'a> {
        CallPolicy {
            slots_survive: false,
            must_clear: empty_marks(),
        }
    }
}

/// 一张进程级的空集：`slots_survive = false` 时根本不会读它，而引用需要一个稳定的落点。
pub(crate) fn empty_marks() -> &'static HashSet<i64> {
    static EMPTY: std::sync::OnceLock<HashSet<i64>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(HashSet::new)
}

impl BlockState {
    /// 当前活着的帧槽事实数（= 一次整体清空会丢掉的转发机会上界）。
    fn live_slots(&self) -> u64 {
        self.slots.iter().filter(|s| s.reg != NO_REG && s.len != 0).count() as u64
    }

    /// 当前活着的「寄存器相等」事实数。
    fn live_copies(&self) -> u64 {
        self.copy.iter().filter(|c| **c != NO_REG).count() as u64
    }

    pub(crate) fn reset(&mut self) {
        stat::hit(Self::site("reset_label"), self.live_slots(), self.live_copies());
        shadow::note_label();
        *self = BlockState::default();
    }

    /// 多前驱取交（S2 A2 的另一臂）：只留下**两条边上都成立**的事实。
    ///
    /// 为什么能取交而不是必须整体清：标签是一个汇合点，从它往下走的代码在任何一条入边
    /// 到达时都得成立。一条事实只在一条边上成立，它就是另一条边上的陈值（那是错值，不是
    /// 丢优化）。取交是**保守且sound** 的那一侧：交集里的每条事实都在每个已知前驱上成立。
    ///
    /// 未知前驱（反向边、`goto *表[…]`）不得走这条路：那个状态本 pass 拿不到，所以调用方
    /// 必须退回 `reset`。这就是循环头仍然要重载的原因（也是反面用例钉住的那一条）。
    ///
    /// `report` 只在「真的推进了指令流的状态」时给：多条前驱边之间的**暂存**取交
    /// （`a2_contribute` 在每条边上做一次）不是屏障，报上去会把 0a 的读数掺水。
    pub(crate) fn intersect_with(&mut self, other: &BlockState, report: bool) {
        let before_slots = self.live_slots();
        let before_copies = self.live_copies();
        for r in 0..16 {
            if self.copy[r] != NO_REG && self.copy[r] != other.copy[r] {
                self.copy[r] = NO_REG;
            }
            self.sext32[r] &= other.sext32[r];
            self.zext32[r] &= other.zext32[r];
            // 值域取两边的较小值：一个上界只有两边都保证时才成立。
            self.zbits[r] = self.zbits[r].min(other.zbits[r]);
            // 精确常量只在两边是同一个值时保留。
            if self.kval[r] != other.kval[r] {
                self.kval[r] = None;
            }
        }
        for i in 0..self.slots.len() {
            let f = self.slots[i];
            if f.reg == NO_REG || f.len == 0 {
                continue;
            }
            // 同位移、同目的号才算同一条事实；可信宽度取窄的那一边。
            let keep = other
                .slots
                .iter()
                .find(|o| o.reg != NO_REG && o.disp == f.disp && o.reg == f.reg)
                .map(|o| if o.len < f.len { o.len } else { f.len });
            match keep {
                Some(len) => self.slots[i].len = len,
                None => self.slots[i] = EMPTY_SLOT,
            }
        }
        if !report {
            return;
        }
        stat::hit(
            Self::site("a2_intersect"),
            before_slots - self.live_slots(),
            before_copies - self.live_copies(),
        );
        // 0a 影子表的屏障归桶里，取交与整体清是同一个桶（标签汇合点）：两者丢掉的都是
        // 「跨一条边不成立」的事实，区分它们的是处置方式而不是屏障类型。
        shadow::note_label();
    }

    /// 与 [`reset`](Self::reset) 同义，但分开报站点：`label()` 的汇合点是发射器里
    /// 唯一「按控制流语义必须清」的位置，其余调用点（函数边界、裸字节、调用）各自归类。
    pub(crate) fn reset_at(&mut self, site: usize) {
        stat::hit(site, self.live_slots(), self.live_copies());
        // 影子表只关心 label 汇合点：函数边界与裸字节 emit 不是「跳进来」，前者本来
        // 就要重开一帧，后者在本仓只剩序言那几处，把它们计入 label 会把 0a 的读数掺水。
        if site == Self::site("reset_label") {
            shadow::note_label();
        }
        *self = BlockState::default();
    }

    fn slot_of(&self, disp: i32, len: u8) -> Option<u8> {
        if len == 0 {
            return None;
        }
        self.slots
            .iter()
            .find(|s| s.reg != NO_REG && s.len != 0 && s.disp == disp && s.len >= len)
            .map(|s| s.reg)
    }

    fn note_slot(&mut self, disp: i32, len: u8, reg: u8) {
        // 先干掉同偏移的旧事实（新写的字节已经变了）。
        self.invalidate(disp, len);
        self.clock = self.clock.wrapping_add(1);
        for s in self.slots.iter_mut() {
            if s.reg == NO_REG || s.len == 0 {
                *s = SlotFact { disp, len, reg, stamp: self.clock };
                stat::hit(Self::site("cnt_slot_created"), 0, 0);
                return;
            }
        }
        // A3：满时只淘汰一最久未用的事实（`stamp` 最小的那一条），其余保留。
        // 旧版在这里 `clear_slots()`，把 16 条一起丢掉——丢得多不不错值，但一个
        // 同时持有 >16 个活跃帧槽的函数从此不再有任何槽事实。
        if self.rules & R_LRU != 0 {
            let mut victim = 0usize;
            let mut oldest = u32::MAX;
            for (i, s) in self.slots.iter().enumerate() {
                if s.stamp < oldest {
                    oldest = s.stamp;
                    victim = i;
                }
            }
            stat::hit(Self::site("slots_capacity"), 1, 0);
            self.slots[victim] = SlotFact { disp, len, reg, stamp: self.clock };
            return;
        }
        // 容量满：整体放弃帧槽跟踪（已建的事实也一并清掉，避免「只记得一半」）。
        stat::hit(Self::site("slots_capacity"), self.live_slots(), 0);
        // 不往影子表重复记一笔：那条事实已由 `note_store` 存过，影子表自己在溢出时就把
        // `last_capacity` 推前了（那是 A3 要量到的同一个事件）。
        self.clear_slots();
    }

    /// 读命中一条槽事实：刷 LRU 时计（A3）。默认档位下 `stamp` 不参与任何判定。
    fn touch_slot(&mut self, disp: i32, len: u8) {
        if self.rules & R_LRU == 0 {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        for s in self.slots.iter_mut() {
            if s.reg != NO_REG && s.len != 0 && s.disp == disp && s.len >= len {
                s.stamp = self.clock;
            }
        }
    }

    /// 清掉与 `[disp, disp+len)` 有交集的全部事实。部分覆盖必须清：
    /// 写低 4 字节会让「高 8 字节 == 某号」这条事实失效。
    fn invalidate(&mut self, disp: i32, len: u8) {
        let hi = disp as i64 + len as i64;
        for s in self.slots.iter_mut() {
            if s.reg == NO_REG || s.len == 0 {
                continue;
            }
            let shi = s.disp as i64 + s.len as i64;
            if (disp as i64) < shi && (s.disp as i64) < hi {
                stat::hit(Self::site("cnt_slot_by_invalidate"), 0, 0);
                *s = EMPTY_SLOT;
            }
        }
    }

    fn clear_slots(&mut self) {
        self.slots = [EMPTY_SLOT; MAX_SLOTS];
    }

    /// 只丢掉关于某一位移的那条槽事实。
    ///
    /// 存在的理由是区域家（S4）：存出被推迟到区尾之后，**内存里那个槽不再是任何号的副本**。
    /// 如果把 `mov [槽], rax` 照常 `observe`，本表就会记下「槽 == rax」，紧随其后的读会被
    /// `decide` 直接换成 `mov dst, rax` —— 而那次换形发生在见证层**之前**，家层就再也看不到
    /// 一次帧访问，于是拿不到家里的新值。实测 `acc_underscore()` 在 -O1 就错在这里。
    pub(crate) fn invalidate_slot(&mut self, disp: i32) {
        self.invalidate(disp, 8);
    }

    /// 只丢掉偏移落在 `marks` 里的那些槽事实（A1 的逃逸槽那一列）。
    ///
    /// 为什么不是「整表清掉再补」：同一个 call 上两者差的是「剩下的 15 个槽还要不要
    /// 重新载入」。一个名字没逃的标量，它的槽在 call 之后仍然等于同一个 callee-saved 号。
    fn clear_marked_slots(&mut self, marks: &HashSet<i64>) {
        if marks.is_empty() {
            return;
        }
        for s in self.slots.iter_mut() {
            if s.reg == NO_REG || s.len == 0 {
                continue;
            }
            // 一个对象占多个槽：任何与它相交的槽都算逃逸（登记侧逐槽插，这里也逐槽比）。
            if marks.contains(&(s.disp as i64)) {
                *s = EMPTY_SLOT;
            }
        }
    }

    /// 「本块里这个号的值连 bit31 都是 0」⇒ `movsxd d, d` 是恒等。
    ///
    /// 为什么上限是 31 而不是 32：32 位写只能保证「高 32 位为零」（`zext32`），而
    /// `mov eax, -1; movsxd rax, eax` 删掉扩展会得到 4294967295 而不是 -1——这条反例
    /// 必须继续保留扩展。
    ///
    /// 为什么收 `rules` 参数而不自己读全局：这样判据能单独测（进程内的位图只初始化
    /// 一次，拿全局的话「开 ZBITS」那条正路径永远测不到）。
    fn zbits_within_sext(&self, d: u8, rules: u16) -> bool {
        rules & R_ZBITS != 0 && (d as usize) < 16 && (1..=31).contains(&self.zbits[d as usize])
    }

    fn kill_reg(&mut self, r: u8) {
        if r >= 16 {
            return;
        }
        let ri = r as usize;
        self.copy[ri] = NO_REG;
        self.sext32[ri] = false;
        self.zext32[ri] = false;
        self.zbits[ri] = 0;
        // 整字值也不再可知：写过的号不可能还「一定是那个常量」。
        self.kval[ri] = None;
        for s in self.slots.iter_mut() {
            if s.reg == r {
                stat::hit(Self::site("cnt_slot_by_kill_reg"), 0, 0);
                *s = EMPTY_SLOT;
            }
        }
        // 「别的号等于 r」的关系也断了（r 变了，那些号还是旧值）。
        for c in self.copy.iter_mut() {
            if *c == r {
                stat::hit(Self::site("cnt_copy_by_kill_reg"), 0, 0);
                *c = NO_REG;
            }
        }
    }

    fn kill_all(&mut self, site: usize) {
        stat::hit(site, self.live_slots(), self.live_copies());
        // `Call` 是 0a 的四类屏障之一（`jmp_ret_call` 那条臂）。`Jmp`/`Ret` 也走同一个
        // 站点：它们结束本块，紧随其后的代码必然由一个 `label()` 落入，那条会再记一笔
        // label 屏障并靠「取最晚者」归到 label —— 所以把 jmp/ret 当 call 不会串桶。
        if site == Self::site("jmp_ret_call") {
            shadow::note_call();
        }
        *self = BlockState::default();
    }

    /// 按站点名取编号（站点表只写一份，避免数字飘移）。
    fn site(name: &str) -> usize {
        stat::site(name)
    }

    /// 一次写内存对本块状态的影响。
    ///
    /// `src` 是「写进去的寄存器号」（`None` = 写的是算出来的值，无法建事实）。
    /// 未知基址的写一律当作可别名，清掉全部帧槽事实。
    fn note_store(&mut self, m: &MemOperand, len: u8, src: Option<u8>, volatiles: &HashSet<i64>) {
        let Some(disp) = frame_disp(m) else {
            stat::hit(Self::site("slots_alias"), self.live_slots(), 0);
            shadow::note_aliasing_store();
            self.clear_slots();
            return;
        };
        if len == 0 {
            // 宽度未知的写：它可能盖住任何一个被跟踪的槽。
            stat::hit(Self::site("slots_width0"), self.live_slots(), 0);
            shadow::note_aliasing_store();
            self.clear_slots();
            return;
        }
        self.invalidate(disp, len);
        let Some(s) = src else {
            // 影子表仍要失效旧事实（那是一次真写），但不能建新事实。
            shadow::note_store(disp, len, None);
            stat::hit(Self::site("cnt_store_no_src"), 0, 0);
            return;
        };
        if volatiles.contains(&(disp as i64)) {
            stat::hit(Self::site("cnt_store_volatile"), 0, 0);
            // volatile 槽的读写每一次都是可观察事件：它永不可被转发，所以下面不建事实，
            // 影子表也不能把它当「本可转发」—— 那一笔会直接掺高 0a 的总量。
            shadow::note_store(disp, len, None);
            return;
        }
        shadow::note_store(disp, len, Some(s));
        if matches!(len, 1 | 2 | 4 | 8) {
            self.note_slot(disp, len, s);
        }
    }

    /// 「本条的内存基址能不能换成另一个号」（R_CPYBASE）。返 `Some(s)` = 换成号 `s`。
    ///
    /// 为什么这一步是等价的：`copy[b] == s` 说的是「两个 64 位值此刻逐位相同」，
    /// 所以 `[b+disp]` 与 `[s+disp]` 是**同一个地址**；本条只换基址，不动宽度、位移、
    /// 目的与源，也不改变访存对象。写集与读集因此不变。
    ///
    /// 三条形制全是「字节长度不许变」的化身（同一个理由见 `R_CPY32` 那条臂：长度一变
    /// 就挪动整块后续指令的 PC，那不是窥孔通道该做的事）：
    ///   * `index`/`seg` 必须为空：SIB 与段前缀的有无由基址决定；
    ///   * 两边都不许是 rsp(4)/r12(12)：这两个号做基址必须带 SIB；
    ///   * 两边都不许是 rbp(5)/r13(13)：`disp==0` 时 mod=00 与 mod=01+disp8 长度不同；
    ///     而且帧基址 r13 是上面整套「帧槽事实 + `frame_disp`」的口径，换掉它等于把
    ///     那些事实从此看不见（那不会错值，但会把 R_SLOT 的命中全断掉）。
    fn cpybase_base(&self, m: &MemOperand) -> Option<u8> {
        if m.index.is_some() || m.seg.is_some() {
            return None;
        }
        let b = m.base?;
        let keep_frame_base = |r: u8| !matches!(r, 4 | 5 | 12 | 13);
        if !keep_frame_base(b) {
            return None;
        }
        let s = *self.copy.get(b as usize)?;
        if s == NO_REG || s == b || !keep_frame_base(s) {
            return None;
        }
        Some(s)
    }

    /// 把 `cpybase_base` 的结果落到一条具体的指令上（只覆盖「移动/扩展载入」三类，
    /// 它们的内存侧都是 `MemOperand`）。返 `None` = 本条没有可改写的内存基址。
    fn cpybase_rewrite(&self, insn: &Insn) -> Option<Insn> {
        let m = match &insn.op {
            X86_64Opcode::MovRm { dst: O::Mem(m), .. } | X86_64Opcode::MovRm { src: O::Mem(m), .. }
            | X86_64Opcode::MovExt { src: O::Mem(m), .. } => m,
            _ => return None,
        };
        let nb = self.cpybase_base(m)?;
        let mut n = insn.clone();
        match &mut n.op {
            X86_64Opcode::MovRm { dst: O::Mem(mm), .. } | X86_64Opcode::MovRm { src: O::Mem(mm), .. }
            | X86_64Opcode::MovExt { src: O::Mem(mm), .. } => {
                mm.base = Some(nb);
            },
            _ => unreachable!(),
        }
        // 文本必须与字节同源：基址换了，调用点挂的 gas 就过期了。清成 `None` 让
        // `emit_insn` 用 `op.render()` 兜底重生成（P9 的那条 fallback），而不是留着
        // 一行写旧基址的文本。
        n.gas_text = None;
        stat::hit(Self::site("cnt_cpybase_hit"), 0, 0);
        Some(n)
    }

    /// 只看不写：给出本条的处置。第三个参数是 [`R_SEXT`]..[`R_CPYBASE`] 的位图（不是旧的
    /// 单调档位），默认值由 [`rules_for`] 从 `DCC_BLOCKOPT` 的档位推得。
    pub(crate) fn decide(&self, insn: &Insn, volatiles: &HashSet<i64>, rules: u16) -> Verdict {
        if rules == 0 {
            return Verdict::Keep;
        }
        // ---- 地址中转：基址只是另一个号的副本（R_CPYBASE）----
        // 放在 match **之前**而不是各条臂内部：下面每条帧槽臂都在「基址不是 r13」时
        // 直接 `return Verdict::Keep`，把本规则塞进任何一条臂里都会被前面那条 `return`
        // 吃掉（实测这种「先写判据再被早退截胡」是本函数最容易犯的错）。
        if rules & R_CPYBASE != 0 {
            if let Some(n) = self.cpybase_rewrite(insn) {
                return Verdict::Replace(Box::new(n));
            }
        }
        match &insn.op {
            // ---- 帧槽读 ----
            X86_64Opcode::MovRm {
                width,
                dst,
                src: O::Mem(m),
                ..
            } => {
                let Some(d) = is_reg(dst) else {
                    return Verdict::Keep;
                };
                let Some(disp) = frame_disp(m) else {
                    return Verdict::Keep;
                };
                if volatiles.contains(&(disp as i64)) {
                    return Verdict::Keep;
                }
                let w = width_bytes(*width);
                let Some(r) = self.slot_of(disp, w) else {
                    stat::hit(Self::site("cnt_load_fact_miss"), 0, 0);
                    // Gate 0a：本条就是一个「本应发帧载入」的站点，问影子表归桶。
                    shadow::note_load(disp, w);
                    return Verdict::Keep;
                };
                stat::hit(Self::site("cnt_load_fact_hit"), 0, 0);
                if r == d {
                    // 同号读：64 位载入是恒等；窄载入还会把目的号的高位清掉（32 位）
                    // 或只写一部分（8/16 位），只有在目的号的高位已是规范形态时才恒等。
                    let identity = w == 8 || (w == 4 && self.zext32[d as usize]);
                    if identity && rules & R_COPY != 0 {
                        return Verdict::Drop;
                    }
                    return Verdict::Keep;
                }
                if rules & R_SLOT != 0 && w != 0 {
                    return Verdict::Replace(Box::new(Insn::mov_rm_reg(*width, d, r)));
                }
                Verdict::Keep
            },
            // ---- movsxd/movzx 的帧槽读 ----
            X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst,
                src: O::Mem(m),
                ..
            } => {
                let Some(d) = is_reg(dst) else {
                    return Verdict::Keep;
                };
                let Some(disp) = frame_disp(m) else {
                    return Verdict::Keep;
                };
                if volatiles.contains(&(disp as i64)) {
                    return Verdict::Keep;
                }
                let src_len = width_bytes(*from);
                let Some(r) = self.slot_of(disp, src_len) else {
                    shadow::note_load(disp, src_len);
                    return Verdict::Keep;
                };
                if r == d {
                    let identity = if *sign {
                        self.sext32[d as usize]
                    } else {
                        self.zext32[d as usize]
                    };
                    if identity && rules & R_COPY != 0 {
                        return Verdict::Drop;
                    }
                    return Verdict::Keep;
                }
                // 只有目的号是 32/64 位时，寄存器形态才与内存形态逐位等价。
                let dst_w = width_bytes(*to);
                if rules & R_SLOT != 0 && dst_w >= 4 && src_len != 0 {
                    return Verdict::Replace(Box::new(Insn::mov_ext(*sign, *from, *to, d, r)));
                }
                Verdict::Keep
            },
            // ---- 寄存器搬运：自反 / 已经相等 ----
            X86_64Opcode::MovRm {
                width: _,
                dst: O::Reg(d, _),
                src: O::Imm(v),
                ..
            } => {
                // **同一个号已经是同一个常量** ⇒ 这一条写就是死写，整条不发。
                // `mov` 不碰标志也不碰内存，所以删掉它对可观察行为没有任何影响。
                if rules & R_DEADW != 0 && self.kval[*d as usize] == Some(*v) {
                    stat::hit(Self::site("deadw_drop"), 0, 0);
                    Verdict::Drop
                } else {
                    Verdict::Keep
                }
            }
            X86_64Opcode::Raw { bytes, .. } if rules & (R_DEADW | R_CPY32) != 0 => {
                // 上面那个 typed 臂接不到实际产物：本后端的立即数搬运走的是 raw 字节
                // （`mov_reg_imm32_b64`），所以同样的判据得在字节侧再走一遍。只认
                // `raw_writes` 能报出精确值的形态（`ImmSExt`），不猜。
                //
                // 注意它与下面 `R_CPY32` 那条 Raw 臂是**互斥但同族**的：两个臂不能分写两处，
                // 因为 match 只看第一条匹上的臂 —— 写两条的话，开着 R_DEADW 就会把 R_CPY32
                // 那一整臂默默遮掉（那正是「一个位打开让另一个位的读数变成零」的假阴性形状）。
                // 所以两条判据共用一个臂，先判死写再判换形。
                match raw_writes(bytes) {
                    RawWrite::ImmSExt { dst, imm }
                        if rules & R_DEADW != 0 && self.kval[dst as usize] == Some(imm as i64) =>
                    {
                        stat::hit(Self::site("deadw_drop"), 0, 0);
                        Verdict::Drop
                    },
                    _ if rules & R_CPY32 != 0 => match Self::raw_mov_rr(bytes) {
                        Some((d, s)) if self.zext32[s as usize] && (d >= 8 || s >= 8) => {
                            use crate::codegen::shared::asm_operand::Width;
                            let insn = Insn::mov_rm_reg(Width::B32, d, s);
                            let (src_name, dst_name) = (
                                crate::codegen::x86_64::emit::gas::REG32[(s & 15) as usize],
                                crate::codegen::x86_64::emit::gas::REG32[(d & 15) as usize],
                            );
                            Verdict::Replace(Box::new(
                                insn.with_gas(format!("mov {src_name}, {dst_name}"))
                            ))
                        },
                        _ => Verdict::Keep,
                    },
                    _ => Verdict::Keep,
                }
            }
            X86_64Opcode::MovRm {
                width,
                dst,
                src,
                ..
            } => {
                let (Some(d), Some(s)) = (is_reg(dst), is_reg(src)) else {
                    return Verdict::Keep;
                };
                if rules & R_COPY == 0 {
                    return Verdict::Keep;
                }
                let w = width_bytes(*width);
                // 64 位自反 mov 恒等；32 位自反只在目的号高位已为零时恒等（否则它承担零扩展）。
                if d == s && (w == 8 || (w == 4 && self.zext32[d as usize])) {
                    return Verdict::Drop;
                }
                if w == 8 && self.copy[d as usize] == s {
                    return Verdict::Drop;
                }
                Verdict::Keep
            },
            // ---- 帧槽写：槽里已经是这个号的值 ----
            X86_64Opcode::MovRm {
                width,
                dst: O::Mem(m),
                src,
                ..
            } => {
                let (Some(s), Some(disp)) = (is_reg(src), frame_disp(m)) else {
                    return Verdict::Keep;
                };
                if rules & R_SLOT == 0 || volatiles.contains(&(disp as i64)) {
                    return Verdict::Keep;
                }
                let w = width_bytes(*width);
                if self.slot_of(disp, w) == Some(s) {
                    return Verdict::Drop;
                }
                Verdict::Keep
            },
            // ---- 恒等符号扩展（`movsxd r,r` 在 r 已是规范形态时）----
            X86_64Opcode::MovExt {
                sign: true,
                from: Width::B32,
                to: Width::B64,
                dst,
                src,
                ..
            } => {
                let (Some(d), Some(s)) = (is_reg(dst), is_reg(src)) else {
                    return Verdict::Keep;
                };
                if d == s
                    && (self.sext32[d as usize] || self.zbits_within_sext(d, rules))
                    && rules & R_SEXT != 0
                {
                    return Verdict::Drop;
                }
                Verdict::Keep
            },
            // （`R_CPY32` 与 `R_DEADW` 的 `Raw` 判据共用上面那一个臂：两条判据若分写成两个
            //   `Raw` 臂，match 只会走到第一个，第二个会被默默遮掉。）
            _ => Verdict::Keep,
        }
    }

    /// `Codegen::mov(dst, src)` 的唯一形态：`[REX.W|REX.B|REX.R] 8B mod=11`，
    /// reg 字段是目的、r/m 字段是源。返 `None` = 不是这一族（保守，不是错值）。
    fn raw_mov_rr(bytes: &[u8]) -> Option<(u8, u8)> {
        let (rex, op, modrm) = match bytes.len() {
            3 if (bytes[0] & 0xF0) == 0x40 => (bytes[0], bytes[1], bytes[2]),
            _ => return None,
        };
        // 必须是 REX.W（64 位）且 mod=11、opcode 0x8B（mov r ← r/m）。
        if rex & 0x08 == 0 || op != 0x8B || modrm >> 6 != 3 {
            return None;
        }
        let dst = ((modrm >> 3) & 7) | if rex & 0x04 != 0 { 8 } else { 0 };
        let src = (modrm & 7) | if rex & 0x01 != 0 { 8 } else { 0 };
        Some((dst, src))
    }

    /// 记下本条（可能是替换后的指令）对块内状态的影响。
    ///
    /// 两参形状 = `CallPolicy::no_relax()`，即「槽事实不跨 call 存活」（今天的行为）。
    /// 存在的理由不是方便：A1 的默认关必须有一个**写不出错**的调用点，否则任何一个
    /// 忘了传结论的站点都会静默地把 A1 打开。生产调用点一律走 `observe_at_call`。
    pub(crate) fn observe(&mut self, insn: &Insn, volatiles: &HashSet<i64>) {
        self.observe_at_call(insn, volatiles, CallPolicy::no_relax(), rules())
    }

    /// 记下本条（可能是替换后的指令）对块内状态的影响。
    ///
    /// `call_policy` 是「本帧的槽能不能活过一次 call」的结论，由调用方从
    /// `crate::frame_guard::FrameGuard` 算出（方案 §4.7：四个清槽条件不许在本模块
    /// 再拼一份）。默认全关时它完全不参与判定，所以 `DCC_BLOCKOPT=0..3` 的产物逐字不变。
    pub(crate) fn observe_at_call(
        &mut self,
        insn: &Insn,
        volatiles: &HashSet<i64>,
        call_policy: CallPolicy<'_>,
        rules: u16,
    ) {
        // 先拄位图：下面 `note_slot` / `touch_slot` 都要按它分流（A1/A3），而 `rules()`
        // 的 `OnceLock` 缓存让单测无法切臂。
        self.rules = rules;
        // `DCC_FACTS_DUMP=1`：逐条打出 raw 字节与解码器给的答案。存在的唯一理由是把
        // `raw_writes` 交给**外部 oracle** 对账（`scripts/check_facts_vs_objdump.sh` 拿
        // objdump 对同一串字节反解目的号）——本模块自己的单测只能证明「我写的与我想的
        // 一致」，证明不了「我与真 x86 一致」（方案 §3 S1：不以自证为真）。
        if let X86_64Opcode::Raw { bytes, .. } = &insn.op {
            facts_dump::note(bytes);
        }
        match &insn.op {
            X86_64Opcode::MovRm {
                width, dst, src, ..
            } => match (dst, src) {
                (O::Reg(d, _), O::Reg(s, _)) => {
                    let w = width_bytes(*width);
                    self.kill_reg(*d);
                    if w == 8 {
                        self.copy[*d as usize] = *s;
                        self.sext32[*d as usize] = self.sext32[*s as usize];
                        self.zext32[*d as usize] = self.zext32[*s as usize];
                        // 64 位整字抄过来的，目标与源同值：常量知识跟着传。
                        self.kval[*d as usize] = self.kval[*s as usize];
                    } else if w == 4 {
                        // 32 位写自动零扩展高 32 位。
                        self.zext32[*d as usize] = true;
                    }
                },
                (O::Reg(d, _), O::Imm(v)) => {
                    let w = width_bytes(*width);
                    let raw = *v;
                    self.kill_reg(*d);
                    if w == 4 {
                        self.zext32[*d as usize] = true;
                    }
                    if rules & R_DEADW != 0 {
                        // 整字被写成一个已知值：w==4 时高 32 位自动为零，所以精确值是零扩展形。
                        self.kval[*d as usize] = match w {
                            8 => Some(raw),
                            4 => Some(raw & 0xFFFF_FFFF),
                            _ => None,
                        };
                    }
                },
                (O::Reg(d, _), O::Mem(m)) => {
                    let w = width_bytes(*width);
                    // 读命中一条帧槽事实：刷 A3 的 LRU 时计（不参与任何处置判定）。
                    if let Some(disp) = frame_disp(m) {
                        self.touch_slot(disp, w);
                    }
                    self.kill_reg(*d);
                    if w == 4 {
                        self.zext32[*d as usize] = true;
                    }
                },
                (O::Mem(m), O::Reg(s, _)) => {
                    let w = width_bytes(*width);
                    self.note_store(m, w, Some(*s), volatiles);
                },
                _ => self.kill_all(Self::site("mov_rm_other")),
            },
            X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst,
                src,
                ..
            } => match (dst, src) {
                (O::Reg(d, _), _) => {
                    // 扩展载入同样是一次帧槽读：刷 LRU 时计。
                    if let O::Mem(m) = src {
                        if let Some(disp) = frame_disp(m) {
                            self.touch_slot(disp, width_bytes(*from));
                        }
                    }
                    self.kill_reg(*d);
                    // **`to` 的宽度决定高位是不是规范形态，两条事实不能共用一个判据**：
                    //  * `to` 是 64 位：整字被写，`sext32` / `zext32` 都成立。
                    //  * `to` 是 32 位（`movswl %ax,%eax` 这一族，complex.rs 与原子辅助真的在发）：
                    //    32 位写**清高 32 位**，所以只有零扩展成立；此时
                    //    `rax == movsxd(eax)` 是假的（eax 的 bit31 为 1 时差一个符号），
                    //    把 `sext32` 记上去会让消费点把必要的 `movsxd %rax,%rax` 当恒等削掉。
                    //  * `to` 是 8/16 位：高位保留旧值，两条都不记。
                    let w_to = width_bytes(*to);
                    if w_to == 8 {
                        if *sign {
                            self.sext32[*d as usize] = true;
                        } else {
                            self.zext32[*d as usize] = true;
                        }
                    } else if w_to == 4 && !*sign {
                        self.zext32[*d as usize] = true;
                    }
                    let _ = from;
                },
                (O::Mem(m), _) => self.note_store(m, width_bytes(*to), None, volatiles),
                _ => self.kill_all(Self::site("movext_other")),
            },
            X86_64Opcode::AluRm {
                op, width, dst, ..
            } => {
                // cmp/test 只读不写。
                if matches!(op, AluOp::Cmp | AluOp::Test) {
                    return;
                }
                match dst {
                    O::Reg(d, _) => self.kill_reg(*d),
                    O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                    _ => self.kill_all(Self::site("alu_dst_other")),
                }
            },
            X86_64Opcode::ImulRm { width, dst, .. } => match dst {
                O::Reg(d, _) => self.kill_reg(*d),
                O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                _ => self.kill_all(Self::site("imul_dst_other")),
            },
            X86_64Opcode::Shift { width, dst, .. } => match dst {
                O::Reg(d, _) => self.kill_reg(*d),
                O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                _ => self.kill_all(Self::site("shift_dst_other")),
            },
            X86_64Opcode::UnaryRm { op, width, dst } => {
                // mul/imul/div/idiv 的隐式目的号是 rax/rdx。
                if matches!(op, UnaryOp::Mul | UnaryOp::Imul | UnaryOp::Div | UnaryOp::Idiv) {
                    self.kill_all(Self::site("unary_muldiv"));
                    return;
                }
                match dst {
                    O::Reg(d, _) => self.kill_reg(*d),
                    O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                    _ => self.kill_all(Self::site("unary_dst_other")),
                }
            },
            X86_64Opcode::Lea { dst, .. } => match dst {
                O::Reg(d, _) => self.kill_reg(*d),
                _ => self.kill_all(Self::site("lea_dst_other")),
            },
            X86_64Opcode::Setcc { dst, .. } => match dst {
                O::Reg(r, _) => self.kill_reg(*r),
                O::Mem(m) => self.note_store(m, 1, None, volatiles),
                _ => self.kill_all(Self::site("setcc_dst_other")),
            },
            X86_64Opcode::SimdMov { dst, .. } => {
                if let O::Mem(m) = dst {
                    let len = mem_width_bytes(&insn.op);
                    self.note_store(m, len, None, volatiles);
                }
            },
            X86_64Opcode::GprXmm { to_xmm, dst, .. } => {
                // 只有 xmm → GPR 那一向写通用号。
                if !*to_xmm {
                    match dst {
                        O::Reg(d, _) => self.kill_reg(*d),
                        _ => self.kill_all(Self::site("gprxmm_other")),
                    }
                }
            },
            X86_64Opcode::Cvt2Sd { to_xmm, dst, .. } => {
                if !*to_xmm {
                    match dst {
                        O::Reg(d, _) => self.kill_reg(*d),
                        _ => self.kill_all(Self::site("cvt2sd_other")),
                    }
                }
            },
            X86_64Opcode::Stack { op, dst, .. } => {
                // push/pop 只动栈顶区（`[rsp-8]` 以下），与 `[r13+disp]` 帧槽不重叠，
                // 所以帧槽事实保留；rsp 自己的事实清掉。
                self.kill_reg(4);
                if matches!(op, StackOp::Pop) {
                    if let Some(r) = is_reg(dst) {
                        self.kill_reg(r);
                    }
                }
            },
            X86_64Opcode::Jcc { .. } => {
                // 落空边继续往下走，事实在落空边上仍成立；任何跳转的入口必然是
                // `label()`，那里已经清空。
            },
            X86_64Opcode::NoArg { op } => {
                use crate::codegen::x86_64::opcode::NoArgOp;
                if !matches!(op, NoArgOp::Cld) {
                    self.kill_all(Self::site("noarg"));
                }
            },
            // 类型化的 `Jmp`/`Ret` 与分派尾部的 `jmp *(%rbp,%rcx,8)`：本块到此为止。
            // 它们继续报 `jmp_ret_call` 那个站点（站点表的编号是产物对账的契约，
            // 不能因为换了个写法就把旧读数变成不可比）。旧版那 80 次 `JmpRmMem` 落在
            // catch-all 里，读出来是「变体没被认识到」而不是「它结束了本块」。
            X86_64Opcode::Jmp { .. } | X86_64Opcode::Ret { .. } | X86_64Opcode::JmpRmMem { .. } => {
                self.kill_all(Self::site("jmp_ret_call"))
            },
            // 一次调用。默认位图下与旧版逐字相同（整表 `kill_all`）；A1 打开才走分流：
            // caller-saved 逐号杀（连带只与那些号相等的槽事实），callee-saved 号上的
            // 事实与帧槽事实按护栏保留。callee 按 ABI 必须还原 callee-saved 号，
            // 而「callee 写不到本帧的槽」就是护栏那四条的结论。
            X86_64Opcode::Call { .. } => {
                if rules & R_CALLSLOT == 0 {
                    self.kill_all(Self::site("jmp_ret_call"));
                } else {
                    stat::hit(Self::site("raw_call"), 0, 0);
                    for r in 0..16u8 {
                        if SYSV_CALLER_SAVED & (1u16 << r) != 0 {
                            self.kill_reg(r);
                        }
                    }
                    if !call_policy.slots_survive {
                        stat::hit(Self::site("jmp_ret_call"), self.live_slots(), 0);
                        self.clear_slots();
                    } else {
                        // 护栏允许跨 call 存活，但逃逸槽仍然必须丢：callee 拿着逃出去的
                        // 指针可以写它们，而那份写不在本表的视野里。
                        self.clear_marked_slots(call_policy.must_clear);
                    }
                }
            },
            // SIMD 算术：目的是 xmm，不碰任何 GP 号也不碰内存。旧版整条 `SimdArith`
            // 落在 catch-all 全杀（实测 `luaV_execute` 62 次、`floatforloop` 9 次）。
            // 本臂由 `R_TYPED` 门控：默认位图不开就逐字节与旧产物相同。
            X86_64Opcode::SimdArith { dst, .. } if rules & R_TYPED != 0 => match dst {
                O::Mem(m) => {
                    let len = mem_width_bytes(&insn.op);
                    self.note_store(m, len, None, volatiles)
                },
                O::Reg(_, _) => {},
                _ => self.kill_all(Self::site("catchall")),
            },
            // `Raw` 里的「只写目的号一个」形态（批2 的 F 项）。`R_RAW` 关掉时回到
            // 下面的 catch-all `kill_all()`，所以 `DCC_BLOCKOPT=0..3` 的产物与本改动前
            // 逐字相同（默认位图不含 R_RAW，见 `rules_for`）。
            //
            // 为什么这条重要：发射层那批立即数形态（`add/sub/and/or/xor/cmp r64,imm`、
            // `mov r64,imm32`、移位 `C1/D3`、`imul r,r,imm`）全走 `Opcode::Raw`，
            // 以前一律当全杀——一条只写一号的指令擦掉整张 copy/sext/slot 事实表。
            // 实测 luaV_execute 里这类形态占 21.6%（2451/11366），挡住 274 对
            // 「存了立刻同槽重载」纯浪费里的 247 对（批2 开工前的实测归因）。
            X86_64Opcode::Raw { bytes, .. } if rules & R_RAW != 0 => match raw_writes(bytes) {
                RawWrite::Nothing => {
                    stat::hit(Self::site("raw_nothing"), 0, 0);
                },
                RawWrite::Reg(r) => {
                    stat::hit(Self::site("raw_reg"), 0, 0);
                    self.kill_reg(r);
                    // 本后端「`mov r64,r64`」的唯一形态就是 `REX.W 8B /r`（`Codegen::mov`）。
                    // 目的与源都拿得到时，常量知识跟着传（否则 kill_reg 已经把目的抹零）。
                    if rules & R_DEADW != 0 {
                        if let Some((d, s)) = Self::raw_mov_rr(bytes) {
                            self.kval[d as usize] = self.kval[s as usize];
                        }
                    }
                },
                RawWrite::Masked { dst, bits } => {
                    stat::hit(Self::site("raw_masked"), 0, 0);
                    self.kill_reg(dst);
                    if rules & R_ZBITS != 0 {
                        self.zbits[dst as usize] = bits;
                    }
                },
                // `movzx r64, r/m8|16`：上界已知，而且高 48/56 位确定为零。本轮新补，
                // 受 `R_IMMSEXT` 管；关掉时退回旧答话（只杀那个号，不建任何事实）。
                RawWrite::ZExt { dst, bits } => {
                    stat::hit(Self::site("raw_masked"), 0, 0);
                    self.kill_reg(dst);
                    if rules & R_IMMSEXT != 0 && rules & R_ZBITS != 0 {
                        self.zbits[dst as usize] = bits;
                    }
                    if rules & R_IMMSEXT != 0 {
                        // 零扩展到一个小于 32 的上界：高 32 位必然为零。
                        self.zext32[dst as usize] = bits <= 32;
                    }
                },
                // `mov r64, imm32`：整字写且写的就是符号扩展形态。`sext32` 记上去之后，
                // 下一条同源的 `movsxd dst,dst` 在 `decide` 里就是恒等形 ⇒ 整条不发。
                // 本轮新补，受 `R_IMMSEXT` 管；关掉时退回旧答话（只杀那个号）。
                RawWrite::ImmSExt { dst, imm } => {
                    stat::hit(Self::site("raw_immsext"), 0, 0);
                    self.kill_reg(dst);
                    if rules & R_DEADW != 0 {
                        // `mov r64, imm32` 写进去的就是符号扩展后的整字值。
                        self.kval[dst as usize] = Some(imm as i64);
                    }
                    if rules & R_IMMSEXT != 0 {
                        self.sext32[dst as usize] = true;
                        // 非负且不到 2^31 时，零扩展形态也成立（两条事实可以同时在）。
                        self.zext32[dst as usize] = imm >= 0 && (imm as u32) < 0x8000_0000;
                        if rules & R_ZBITS != 0 {
                            // 负值的上界就是 64 位（推不出任何更小的），非负值按最高置位算。
                            let bits: u8 = if imm < 0 {
                                64
                            } else {
                                (64 - (imm as u32).leading_zeros()) as u8
                            };
                            self.zbits[dst as usize] = bits;
                        }
                    }
                },
                // `movsx/movsxd`：整字写且结果就是符号扩展形态 ⇒ 下一条同源的 `movsxd d,d`
                // 在 `decide` 里是恒等形（`R_SEXT` 那条判据）。本轮新补，受 `R_IMMSEXT` 管。
                RawWrite::SExt { dst } => {
                    stat::hit(Self::site("raw_immsext"), 0, 0);
                    self.kill_reg(dst);
                    if rules & R_IMMSEXT != 0 {
                        self.sext32[dst as usize] = true;
                    }
                },
                // 只写几个号、不碰内存：逐号杀，帧槽事实只在与那些号相等时连带失效。
                RawWrite::Regs(mask) => {
                    stat::hit(Self::site("raw_regs"), 0, 0);
                    for r in 0..16u8 {
                        if mask & (1u16 << r) != 0 {
                            self.kill_reg(r);
                        }
                    }
                },
                // 一次调用。寄存器侧：caller-saved 全集必杀（ callee 可以自由破坏它们）。
                // 帧槽侧：默认照旧全弃；A1（`R_CALLSLOT`）打开且护栏说「本帧的槽不可能被
                // callee 写到」时才保留 —— 护栏就是方案 §4.7 要求同源的那四条。
                // 一次调用：寄存器侧必杀 caller-saved，帧槽侧与类型化 `Call` 那条约用
                // 同一个 `R_CALLSLOT` 闸门（否则同一个语义在 raw 与类型化两条形制下
                // 会给出两种保守度）。
                RawWrite::Call => {
                    if rules & R_CALLSLOT == 0 {
                        self.kill_all(Self::site("jmp_ret_call"));
                    } else {
                        stat::hit(Self::site("raw_call"), 0, 0);
                        for r in 0..16u8 {
                            if SYSV_CALLER_SAVED & (1u16 << r) != 0 {
                                self.kill_reg(r);
                            }
                        }
                        if !call_policy.slots_survive {
                            stat::hit(Self::site("jmp_ret_call"), self.live_slots(), 0);
                            self.clear_slots();
                        } else {
                            self.clear_marked_slots(call_policy.must_clear);
                        }
                    }
                },
                // 控制流终结（jmp/ret）：与类型化那两条臂同一个站点。
                RawWrite::Terminate => {
                    self.kill_all(Self::site("jmp_ret_call"))
                },
                // 串指令：写 rsi/rdi/rcx 并且碰内存。
                RawWrite::Strs => {
                    stat::hit(Self::site("raw_strs"), 0, 0);
                    for r in [1u8, 2, 5, 6] {
                        self.kill_reg(r);
                    }
                    self.clear_slots();
                },
                // 内存写但不是本帧固定槽：槽事实全弃，**寄存器事实保留**。
                RawWrite::Memory => {
                    stat::hit(Self::site("raw_memory"), self.live_slots(), 0);
                    shadow::note_aliasing_store();
                    self.clear_slots();
                },
                // 一次真存槽：失效范围只那一个字节区间，并且（源是 GP 号时）直接建事实。
                RawWrite::SlotStore { disp, len, src } => {
                    stat::hit(Self::site("raw_slotstore"), 0, 0);
                    self.invalidate(disp, len);
                    shadow::note_store(disp, len, src);
                    if let Some(s) = src {
                        if matches!(len, 1 | 2 | 4 | 8) {
                            self.note_slot(disp, len, s);
                        }
                    }
                },
                RawWrite::Unknown => {
                    stat::hit(Self::site("raw_unknown_decoded"), self.live_slots(), self.live_copies());
                    // 把认不得的形态按「首字节+modrm」归类记账：光知道数量不能下手，
                    // 必须知道是哪些 opcode 组没进到解码器。
                    stat::hit_variant(&raw_tag(bytes), self.live_slots(), self.live_copies());
                    shadow::note_raw_unknown();
                    *self = BlockState::default();
                },
            },
            // 认不得的一律全杀。同时按变体名记账：光知道「杀了多少次」无法下手，
            // 必须知道是哪几个变体没进到上面的分支（见 stat::hit_variant）。
            other => {
                let n = variant_name(other);
                stat::hit(Self::site("catchall"), self.live_slots(), self.live_copies());
                stat::hit_variant(&n, self.live_slots(), self.live_copies());
                // `R_RAW` 关着时**所有** Raw 落在这条 catch-all 上（方案 §8.3-1），所以
                // 0b 的 raw-Unknown 读数必须在这里也记一笔，否则默认档下那一桶永远是 0。
                if n == "Raw" {
                    shadow::note_raw_unknown();
                }
                *self = BlockState::default();
            },
        }
    }
}

/// 给认不得的 `Raw` 形态一个可对账的短标签：跳过 legacy 前缀与 REX 取主 opcode，两字节
/// opcode（`0F xx`）带上第二字节，模态组（`/digit` 选具体操作的那几族）再带上 modrm 的
/// reg 字段。仅归因仪表用，不在默认路径跑。
///
/// 第二字节这一笔不是修饰：`raw:0f` 这一桶在旧口径下把 movzx/cpuid/jcc/cmov 全混在
/// 一起，读数无法指到具体的臂（实测补上 op1 后才知道 `luaV_execute` 那批是哪几条）。
fn raw_tag(bytes: &[u8]) -> String {
    let mut i = 0usize;
    while i < bytes.len() && matches!(bytes[i], 0x66 | 0xF2 | 0xF3 | 0x2E | 0x36 | 0x3E) {
        i += 1;
    }
    if i < bytes.len() && (bytes[i] & 0xF0) == 0x40 {
        i += 1;
    }
    let op = *bytes.get(i).unwrap_or(&0);
    if op == 0x0F {
        let op1 = *bytes.get(i + 1).unwrap_or(&0);
        let modrm = *bytes.get(i + 2).unwrap_or(&0);
        if matches!(op1, 0x01 | 0x1F | 0xAE | 0xB0 | 0xB1 | 0xBA | 0xA3) {
            return format!("raw:0f{op1:02x}/{}", (modrm >> 3) & 7);
        }
        return format!("raw:0f{op1:02x}");
    }
    let group = matches!(op, 0x80 | 0x81 | 0x82 | 0x83 | 0xC0 | 0xC1 | 0xC6 | 0xC7 | 0xD0 | 0xD1
        | 0xD2 | 0xD3 | 0xF6 | 0xF7 | 0xFE | 0xFF);
    if group {
        let modrm = *bytes.get(i + 1).unwrap_or(&0);
        format!("raw:{op:02x}/{}", (modrm >> 3) & 7)
    } else {
        format!("raw:{op:02x}")
    }
}

/// 取 `Debug` 输出的变体名（截到第一个空白/大括号/圆括号），例如
/// `SimdAlu { op: AddPacked, .. }` → `SimdAlu`。仅给归因仪表用，不在热路径跑。
fn variant_name(op: &X86_64Opcode) -> String {
    let s = format!("{op:?}");
    let end = s
        .find([' ', '{', '('])
        .unwrap_or(s.len());
    s[..end].to_string()
}

/// `Raw` 字节流的写入集判据（方案 §3 S1 的指令事实层）。
///
/// 为什么解码字节而不是「拿 mnemonic 字符串比」：字节是唯一真源，而且 modrm 拆完
/// 就能定出到底写 reg 字段还是 r/m 字段（`0x81/0x69` 写 reg，`0xC7/0xC1` 写 r/m），
/// 而助记符在部分构造子上根本没有。认不得的形状一律 `Unknown` ⇒ caller 全杀，
/// 宁少削一条也不能静默错值。
///
/// 从「四种」到「八种」的每一个新变体都对应一类**真实存在**的形态，而不是一般化：
/// 它们各 自解决一个「旧版只能全杀」的形状（`Regs` 只写两个号却擦了 16 条槽事实；
/// `Memory` 不碰任何号却擦了寄存器等价事实；`SlotStore` 只盖一个槽却擦了全部）。
/// 逐条根据在下面 `raw_writes_matches_the_imm_constructors` 与
/// `raw_writes_decodes_every_raw_constructor` 两条用例里。
#[derive(Debug)]
pub(crate) enum RawWrite {
    /// 不写任何 GP 号（`cmp`/`test`/`jcc` 落空边/SIMD 算术），事实全部存活。
    Nothing,
    /// 只写这一个个号。
    Reg(u8),
    /// 写多个号（位图）：`div`/`idiv` 写 rax+rdx、`mul`/`imul` 写 rax(±rdx)、
    /// `cpuid` 写四个号、`xchg` 同时写两个、`syscall` 写 rcx/r11。
    /// 它们共同的关键是：**不碰内存也不碰别的号**，所以没理由全杀。
    Regs(u16),
    /// `and r, imm`（imm 非负且 < 2^31）：结果既只写这个号，又得到一个**上界已知**的值。
    /// `bits` 就是 imm 的位宽（值 < 2^bits）。
    Masked { dst: u8, bits: u8 },
    /// 一次调用：写 caller-saved 全集并且可能写内存。单独一个变体而不是并进 `Regs`，
    /// 因为 A1（调用分流）要把「寄存器事实」与「帧槽事实」分开处置。
    Call,
    /// 控制流终结（`jmp`/`ret`）：本块到此为止。它与「解不出写集」必须分开记账，
    /// 否则 0b 会把**结构上不可修**的那批也算进 S1 的靶子。
    Terminate,
    /// 串指令（`rep movs*`/`stos*`/`scas*`/`cmps*`）：写 rsi/rdi/rcx **并且**写内存。
    /// 单独一条而不是并进 `Regs`，因为内存那一侧要清槽事实。
    Strs,
    /// 一条**帧槽写**：`mov [r13+disp], r` 或 SIMD 的 `movsd [r13+disp], xmm`。
    /// `src=None` 是「写进去的是 xmm/立即数」：建不了寄存器等价事实，但失效范围
    /// 只是那一个位移，而不是把全部槽事实清掉。
    SlotStore { disp: i32, len: u8, src: Option<u8> },
    /// `mov r64, imm32`（REX.W + C7 /0 id）：目的号整字被写，而且那个值**就是**
    /// imm32 的符号扩展 ⇒ `sext32[dst]` 成立，而且 `zext32[dst]` 在 `0 <= imm < 2^31`
    /// 时也成立。单独一个变体而不是并进 `Reg`/`Masked`：那两者都说不出「低 32 位
    /// 往高符号扩展就是当前形态」，而削掉紧随其后的 `movsxd dst,dst` 需要的正是这句话。
    ImmSExt { dst: u8, imm: i32 },
    /// 一次「整字写，而且写进去的就是低 32 位的符号扩展」：`movsx r64,r/m8|16|32`
    /// 与 `movsxd r64,r/m32`。与 `Masked`（零扩展上界）分开，因为负值没有零扩展上界
    /// 却仍然满足 `sext32`；与 `ImmSExt` 分开，因为这里没有一个立即数可报。
    SExt { dst: u8 },
    /// 一条「整字写 + 结果已是零扩展形态」：`movzx r64,r/m8|16`。与 `Masked`（来
    /// 自 `and r,imm` 那一路）分成两个形，不是因为形状相近不重要，而是因为**开关不同**：
    /// `Masked` 那一路早就在默认档里，`movzx` 这一路是本轮新补的，必须能单独关掉去量
    /// （见 `R_IMMSEXT`）。两者合成一个形就只能连带关掉旧事实，量尺就不干净了。
    ZExt { dst: u8, bits: u8 },
    /// 一次内存写，但不是本帧的固定槽：槽事实必须全弃，**寄存器事实保留**
    /// （内存写不改任何号的值）。与 `Unknown` 全杀的差别就是这一条。
    Memory,
    /// 认不得：可能是内存写、可能同时写 rdx 之类。
    Unknown,
}

/// GP 号位图（`Regs` 用）。`const fn` 的理由：`SYSV_CALLER_SAVED` 是一个常量位图，
/// 而它必须由同一个加法元素拼出来，不能在运行期再算（否则新增一个被破坏号时
/// 需要改动默认路径的一次遍历）。
pub(crate) const fn regbit(r: u8) -> u16 {
    1u16 << (r & 15)
}

/// SysV 调用约定里被 callee 破坏的号：`rax,rcx,rdx,rsi,rdi,r8..r11`。
///
/// 与 `abi::regbank` 的 caller-saved 集合不是同一份：那边排除了拿来做暂存的号，
/// 本常量的用途是「事实表要杀哪些号」，必须是 ABI 事实本身。两处同存由用例
/// `call_write_set_is_the_sysv_clobber_set` 钉住内容。
///
/// **rbx(3)、rbp(5)、r12–r15 不在里面**：它们是 callee-saved，被调函数必须还原。
/// 把 3 误当 caller-saved（本常量第一版就是这么错的，拿 `rbx == r12` 那条事实直接
/// 被 call 擦掉）会把 A1 的收益归零；把 7(rdi) 漏在里面则是静默错值。
pub(crate) const SYSV_CALLER_SAVED: u16 = regbit(0)
    | regbit(1)
    | regbit(2)
    | regbit(6)
    | regbit(7)
    | regbit(8)
    | regbit(9)
    | regbit(10)
    | regbit(11);

pub(crate) fn raw_writes(bytes: &[u8]) -> RawWrite {
    let Some(h) = raw_head(bytes) else {
        return RawWrite::Unknown;
    };
    let Head { prefix, rex, op_at, op0 } = h;
    if op_at == usize::MAX {
        // 只剩段/操作尺寸前缀（见 `raw_head`）。
        return RawWrite::Nothing;
    }
    // 【为什么旧版在这里一律 `modrm>>6 != 3 ⇒ Unknown》：那时只考虑了「r/m 字段是目的」那一族
    // （立即数 ALU、`mov r/m,imm`、移位），对它们确实必须挡住内存目的。但「目的恒在
    // reg 字段」那一族（`mov r,r/m`、`lea`、`movsxd`、`movzx/movsx`、`add/or/adc/sbb/and/sub/xor r,r/m`）
    // **无论 mod 是不是 11 都只写那一个寄存器**（内存只读），旧闸门把它们错杀了：实测
    // `luaV_execute` 里光 `0x8B` 就有 1,647 条，杀掉了 340 条帧槽事实。所以闸门下放到具体的臂上。
    if op0 == 0x0F {
        return raw_writes_0f(bytes, prefix, rex, op_at);
    }
    // ---- 不带 modrm 的那几族先走 ----
    // 它们的号编在 opcode 里，拿 modrm 去读会读到完全不相干的号（本仓第一版就是这么错的）。
    match op0 {
        // `mov r, imm8`（B0+r）/`mov r, imm32`（B8+r）/`mov r64, imm32`（REX.W B8+r）。
        // `mov r, imm8`（B0+r）：无 REX 时 4..7 是 ah/ch/dh/bh（见 `byte_reg`）。
        0xB0..=0xB7 => {
            return RawWrite::Reg(byte_reg(
                (op0 & 7) | if rex & 0x01 != 0 { 8 } else { 0 },
                rex,
            ))
        },
        0xB8..=0xBF => {
            let dst = (op0 & 7) | if rex & 0x01 != 0 { 8 } else { 0 };
            // 只有**真正的 32 位形**（既无 REX.W 也无 0x66）才能说「高 32 位被清成零」。
            // `66 B8 iw`（`mov ax, imm16`）只写低 16 位，高 48 位保留旧值：拿 32 位那一套
            // 去推会得到一条错事实。
            if rex & 0x08 != 0 || prefix == 0x66 {
                return RawWrite::Reg(dst);
            }
            // 32 位 reg-immediate 形：整字写，而且写进去的就是那个 imm32。**非负**时
            // 零扩展与符号扩展同值 ⇒ 可以报 `ImmSExt`，把 `sext32` 一起带上。这一条不
            // 是锦上添花：`mov_imm` 从 S3c 体量交换起就用这一形发非负常量，只报 `Masked`
            // 会让紧随其后的 `movsxd dst,dst` 从可削变成必发（反向多指令）。
            // 负立即数不能报：那一形的 64 位值是零扩展，不等于符号扩展。
            return match raw_immediate32_id(bytes, op_at) {
                Some(imm) if imm >= 0 => RawWrite::ImmSExt { dst, imm },
                _ => RawWrite::Masked { dst, bits: 32 },
            };
        },
        // 累加器专用形（`add/or/adc/sbb/and/sub/xor al|eAX, imm`）：**没有 modrm**，
        // 目的恒为 AL/EAX（号 0）。旧版把它们全杀（实测 `raw:05` 在 luaV_execute 83 次）。
        0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 => return RawWrite::Reg(0),
        0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 => {
            // 同 `B8+r`：`66 05 iw` 只写 AX，不能推「值 < 2^32」。
            return if rex & 0x08 != 0 || prefix == 0x66 {
                RawWrite::Reg(0)
            } else {
                RawWrite::Masked { dst: 0, bits: 32 }
            }
        },
        // `cmp al/eax, imm`：只读不写。
        0x3C | 0x3D => return RawWrite::Nothing,
        // `xchg rax, r`（90+r）同时写两个号；不带 REX 的 0x90 是 nop。
        0x90..=0x97 => {
            let r = (op0 & 7) | if rex & 0x01 != 0 { 8 } else { 0 };
            return if r == 0 && rex == 0 && prefix == 0 {
                RawWrite::Nothing
            } else {
                RawWrite::Regs(regbit(0) | regbit(r))
            };
        },
        // `cbw/cwde/cdqe`（0x98）只写 rax；`cwd/cd/cqo`（0x99）只写 rdx（方案 §3 S1 点名的 cqto）。
        // 两者在 0x66/无 REX.W 下写的是 AX/DX（高位保留），但 `kill_reg` 对「只写低半」
        // 同样是正确的保守处置，所以不区分。
        0x98 => return RawWrite::Reg(0),
        0x99 => return RawWrite::Reg(2),
        // `fwait`（0x9B）不写 GP 号；`int3`（0xCC）只产生陷阱。
        0x9B | 0xCC => return RawWrite::Nothing,
        // `ret`/`ret imm16`：本块到此为止。
        0xC2 | 0xC3 => return RawWrite::Terminate,
        // `jmp rel8/16/32`：本块到此为止（实测 `raw:e9` 在 luaV_execute 391 次，旧版全杀）。
        0xE9 | 0xEB | 0xEA => return RawWrite::Terminate,
        // `call rel32`（E8）：A1 的分流点（实测 238 次）。
        0xE8 => return RawWrite::Call,
        // `loop`/`loope/loopne`（E0-E3）既写 rcx 又可能跳转：两件事拆不开 ⇒ 保守。
        0xE0..=0xE3 => return RawWrite::Unknown,
        // 串指令：`movs/stos/scas/cmps`（含 `rep` 形）读写 rsi/rdi/rcx 并碰内存。
        0xA4 | 0xA5 | 0xAA | 0xAB | 0xA6 | 0xA7 | 0xAE | 0xAF => return RawWrite::Strs,
        _ => {},
    }
    let Some(md) = raw_modrm(bytes, op_at + 1, rex) else {
        return RawWrite::Unknown;
    };
    let (reg, rm, rm_is_reg) = (md.reg, md.rm, md.is_reg);
    // 8 位形的字段号要按 ah/ch/dh/bh 解释（`byte_reg`，由外部 oracle 抖出来的一条真错）：
    // 无 REX 时 4..7 不是 rsp/rbp/rsi/rdi，而是 rax/rcx/rdx/rbx 的高字节。报错了号就是
    // 「带着已被改过的号继续用」，那是静默错值而不是丢优化。
    let byte_form = matches!(
        op0,
        0x00 | 0x02 | 0x08 | 0x0A | 0x10 | 0x12 | 0x18 | 0x1A | 0x20 | 0x22 | 0x28 | 0x2A
            | 0x30 | 0x32 | 0x38 | 0x3A | 0x80 | 0x88 | 0x8A | 0xC0 | 0xC6 | 0xD0 | 0xD2
            | 0xF6
    );
    let (reg, rm) = if byte_form {
        (byte_reg(reg, rex), byte_reg(rm, rex))
    } else {
        (reg, rm)
    };
    match op0 {
        // ---- 目的恒在 modrm.reg：mod 是不是 11 都只写那一个号 ----
        // mov r8/r16/r32/r64, r/m（0x8A/0x8B）与偶数形 ALU 的 reg-目的形（02/0A/12/1A/22/2A/32
        // 是 8 位、03/0B/13/1B/23/2B/33 是宽形），lea（0x8D，mod=11 是非法编码）、
        // movsxd（0x63，本后端只发 REX.W 形）。
        0x02 | 0x03 | 0x0A | 0x0B | 0x12 | 0x13 | 0x1A | 0x1B | 0x22 | 0x23 | 0x2A | 0x2B | 0x32
        | 0x33 | 0x8A | 0x8B | 0x8D => RawWrite::Reg(reg),
        // movsxd 只在 REX.W 下存在（无 REX.W 的 0x63 是未定义别名），宁可 Unknown。
        // 报 `SExt` 而不是 `Reg`：`movsxd rax,[mem]` 之后紧接的 `movsxd rax,eax` 是恒等
        // （实测 lvm.o 里 48 次）—— 写进去的本来就已经是低 32 位的符号扩展。
        0x63 if rex & 0x08 != 0 => RawWrite::SExt { dst: reg },
        // ---- 只读不写：cmp / test（两种 mod 都是）----
        0x38 | 0x39 | 0x3A | 0x3B | 0x84 | 0x85 | 0xA8 | 0xA9 => RawWrite::Nothing,
        // 短跳转（jcc rel8）：落空边继续往下走，事实仍成立 —— 与类型化 `Jcc` 那条臂
        // 同一条纪律（任何跳转的入口必然是 `label()`，那里已经清）。本后端的
        // `short_jump_rel8` 构造子就走这里（旧版全杀）。
        0x70..=0x7F => RawWrite::Nothing,
        // ---- 目的是 modrm.rm：寄存器目的 / 内存目的分开处置 ----
        0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29 | 0x30
        | 0x31 | 0x88 | 0x89 => {
            if rm_is_reg {
                RawWrite::Reg(rm)
            } else if op0 == 0x88 || op0 == 0x89 {
                // 一次真存槽：能定出位移就只失效那一个槽，并且（来自 GP 寄存器时）
                // 直接建一条新事实。旧版把这种形态归到 `Unknown` 全杀，而解释器把
                // TValue 存回 `[r13+disp]` 走的正是它。
                let len = if op0 == 0x88 { 1 } else { raw_op_len(prefix, rex, false) };
                match md.frame_slot {
                    Some(disp) => RawWrite::SlotStore { disp, len, src: Some(reg) },
                    None => RawWrite::Memory,
                }
            } else {
                RawWrite::Memory
            }
        },
        // ALU r/m64, imm32(/imm8)：**r/m 字段是目的**，reg 字段是 ALU digit（/7 是 CMP，不写号）。
        // 这一点本函数第一版写反了（当成 reg），是拿 `insn.rs` 那六个构造子的真实
        // 字节写测才抱出来的（见 `raw_writes_matches_the_imm_constructors`）。
        0x80 | 0x81 | 0x83 => {
            if !rm_is_reg {
                // 内存目的：/7 的 cmp 不写内存，其余都是一次内存写（寄存器事实保留）。
                if reg == 7 {
                    RawWrite::Nothing
                } else {
                    RawWrite::Memory
                }
            } else if reg == 7 {
                RawWrite::Nothing
            } else if reg == 4 {
                // AND：拿到立即数才能说上界。取不到立即数（字节不够）或立即数为负/超过
                // 2^31-1 时退回普通写——宁可少一条事实，不可多一条错事实。
                match raw_immediate(bytes, op_at, op0) {
                    Some(imm) if imm >= 0 && imm < (1i64 << 31) => {
                        let bits = 64 - (imm as u64).leading_zeros();
                        if bits <= 31 {
                            RawWrite::Masked { dst: rm, bits: bits as u8 }
                        } else {
                            RawWrite::Reg(rm)
                        }
                    },
                    _ => RawWrite::Reg(rm),
                }
            } else {
                RawWrite::Reg(rm)
            }
        },
        // IMUL r64, r/m64, imm：reg 字段是目的（本后端只发 reg==rm 的自反形）。
        0x69 | 0x6b => RawWrite::Reg(reg),
        // MOV r/m64, imm32(/8)：r/m 字段是目的。
        0xc6 | 0xc7 => {
            if rm_is_reg {
                // `mov r64, imm32`（REX.W + C7 /0 id）：整字被写，而且写的就是 imm32 的
                // **符号扩展**形态。这一条事实直接把紧随其后的 `movsxd dst,dst` 变成恒等：
                // 实测 lvm.o 里 348 条 `mov rax,0x0` 与 277 条 `movsxd rax,eax` 就是一对一对
                // 出现的，旧版报 `Reg(rm)` 只说“写了这个号”，拿不到那半信息 ⇒ 一条也没削。
                // 32 位形（无 REX.W）不报符号扩展：32 位写自动**零**扩展高 32 位。
                if op0 == 0xc7 && rex & 0x08 != 0 {
                    match raw_immediate32(bytes, op_at) {
                        Some(imm) => RawWrite::ImmSExt { dst: rm, imm },
                        // 字节流不完整：宁可只报“写了一个号”。
                        None => RawWrite::Reg(rm),
                    }
                } else {
                    // 32 位形（无 REX.W）与 0x66 的 16 位形：本轮不动它们的答话（旧版就报
                    // `Reg(rm)`）。把它们改成 `ZExt` 会混进 `R_IMMSEXT` 量尺以外的那一笔。
                    RawWrite::Reg(rm)
                }
            } else {
                // `mov qword [r13+d], imm32`：一次真存槽，但写进去的不是寄存器。
                match md.frame_slot {
                    Some(disp) => RawWrite::SlotStore {
                        disp,
                        len: raw_op_len(prefix, rex, op0 == 0xc6),
                        src: None,
                    },
                    None => RawWrite::Memory,
                }
            }
        },
        // 移位 r/m64, imm8(/CL/1)：r/m 字段是目的（/0-/3 都只写 r/m；/7 是 SAR）。
        // `%cl` 那一族（D2/D3）的**源**是 cl，读不影响任何事实，所以目的照旧写 rm。
        0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
            if rm_is_reg {
                RawWrite::Reg(rm)
            } else {
                RawWrite::Memory
            }
        },
        // F6/F7 组：/0 test imm 不写；/2 not /3 neg 写 rm；/4 mul /5 imul 写 rax(±rdx)，
        // /6 div /7 idiv 写 rax+rdx。旧版把整组当 Unknown 全杀 —— 而 `mul`/`div` 这一族
        // **只读 rm、不写内存**，一条只写两个号的指令没理由把 16 条无关帧槽事实擦掉。
        0xf6 | 0xf7 => match reg {
            0 => RawWrite::Nothing,
            2 | 3 => {
                if rm_is_reg {
                    RawWrite::Reg(rm)
                } else {
                    RawWrite::Memory
                }
            },
            4 | 5 => RawWrite::Regs(regbit(0)),
            6 | 7 => RawWrite::Regs(regbit(0) | regbit(2)),
            _ => RawWrite::Unknown,
        },
        // FE/FF 组：/0 /1 是 inc/dec，/2 call far，/6 push，/7 not。
        // push 改 rsp 又写内存、call far 改控制流 —— 那两形保守 Unknown。
        0xfe | 0xff => {
            if reg >= 2 && reg != 3 && reg != 4 && reg != 5 {
                RawWrite::Unknown
            } else if rm_is_reg {
                RawWrite::Reg(rm)
            } else {
                RawWrite::Memory
            }
        },
        // 其余形态维持保守：宁可 Unknown 全杀。
        _ => RawWrite::Unknown,
    }
}

/// 两字节 opcode（`0F xx`）那一族。前缀由调用方带进来：`66`/`F2`/`F3` 只区分 SIMD 的
/// 宽度与标量形，**不改「写哪个字段」这一件事**，所以多数臂能共用。
fn raw_writes_0f(bytes: &[u8], prefix: u8, rex: u8, op_at: usize) -> RawWrite {
    let op1 = match bytes.get(op_at + 1) {
        Some(b) => *b,
        None => return RawWrite::Unknown,
    };
    // 无 modrm 的那几条必须先返回：拿 modrm 去读会把紧随的字节当成号。
    match op1 {
        // `syscall`：写 rcx/r11/rax，并且什么都能干 ⇒ 与 call 同处置。
        0x05 => return RawWrite::Call,
        // `rdtsc`：写 eax/edx。
        0x31 => return RawWrite::Regs(regbit(0) | regbit(2)),
        // `cpuid`：写 eax/ebx/ecx/edx 四个号（方案 §3 S1 点名的一条）。
        0xA2 => {
            return RawWrite::Regs(regbit(0) | regbit(1) | regbit(2) | regbit(3))
        },
        // `bswap r{16,32,64}`：号编在 opcode 低 3 位 + REX.B，**没有 modrm**。
        // 这一条必须在拿 modrm 之前返回，否则会把后面的字节当成号来读。
        0xC8..=0xCF => {
            return RawWrite::Reg((op1 & 7) | if rex & 0x01 != 0 { 8 } else { 0 })
        },
        // 系统指令族（rdmsr/wrmsr/hlt/lgdt/sgdt/sidt/invalidate 等）：权限级别与写集都不
        // 在本模块的知识里，一律 Unknown。
        0x06 | 0x07 | 0x08 | 0x09 | 0x0B | 0x20 | 0x21 | 0x22 | 0x30 | 0x32 | 0x34 | 0x37
        | 0x38 | 0x39 => return RawWrite::Unknown,
        _ => {},
    }
    let Some(md) = raw_modrm(bytes, op_at + 2, rex) else {
        return RawWrite::Unknown;
    };
    let (reg, rm) = (md.reg, md.rm);
    match op1 {
        // movzx：目的恒在 reg 字段，而且结果是非负的小值 ⇒ 上界已知（`Masked`）。
        // 报 `Reg` 只说“写了这个号”，丢的是后半句：`movzx rax,al` 之后 `movsxd rax,eax`
        // 是恒等（值连 bit31 都是 0），实测 lvm.o 里这一对出现 49 次。
        0xB6 | 0xB7 => RawWrite::ZExt { dst: reg, bits: if op1 == 0xB6 { 8 } else { 16 } },
        // movsx（字节/字/双字 → 64 位）：整字写，而且结果就是低 32 位的符号扩展形态。
        0xBE | 0xBF => RawWrite::SExt { dst: reg },
        // imul/popcnt/bsf/bsr/cmovcc：目的恒在 reg 字段，但推不出扩展形态。
        0xAF | 0xB8 | 0xB9 | 0xBC | 0xBD | 0x40..=0x4F => RawWrite::Reg(reg),
        // test r/m, r：只读不写。
        0x84 | 0x85 => RawWrite::Nothing,
        // jcc rel32：落空边与 `jcc rel8` 同一条纪律。
        0x80..=0x8F => RawWrite::Nothing,
        // setcc：目的是 rm（1 字节写）。
        0x90..=0x9F => {
            if md.is_reg {
                RawWrite::Reg(rm)
            } else {
                match md.frame_slot {
                    Some(disp) => RawWrite::SlotStore { disp, len: 1, src: None },
                    None => RawWrite::Memory,
                }
            }
        },
        // 多字节 nop（0F 1F /0）与 prefetch（0F 0D）：不写 GP 号。
        0x1F | 0x0D => RawWrite::Nothing,
        // endbr64 是 `0F 1E FA`（modrm 固定）；同组其余形态保守 Unknown。
        0x1E => match bytes.get(op_at + 2) {
            Some(0xFA) => RawWrite::Nothing,
            _ => RawWrite::Unknown,
        },
        // `0F 01` 子组：`D0`=xgetbv 写 rax/rdx，`E8/F0/F8`=lfence/sfence/mfence 不写号。
        0x01 => match bytes.get(op_at + 2) {
            Some(0xD0) => RawWrite::Regs(regbit(0) | regbit(2)),
            Some(0xE8) | Some(0xF0) | Some(0xF8) => RawWrite::Nothing,
            _ => RawWrite::Unknown,
        },
        0xAE => match bytes.get(op_at + 2) {
            Some(0xE8) | Some(0xF0) | Some(0xF8) => RawWrite::Nothing,
            _ => RawWrite::Unknown,
        },
        // shld/shrd：目的是 rm。
        0xA4 | 0xA5 | 0xAC => {
            if md.is_reg {
                RawWrite::Reg(rm)
            } else {
                RawWrite::Memory
            }
        },
        // bt/bts/btr/btc 与 cmpxchg/xadd：rm 既可能是目的又可能写内存，号集也不止一个。
        0xA3 | 0xB0 | 0xB1 | 0xBA => RawWrite::Unknown,
        // ---- SIMD ----
        // 载入/算术形：目的是 xmm（modrm.reg），GP 号一个不动。实测 `raw:f2` 在
        // `luaV_execute` 里 251 次全杀，而那批就是 `movsd/addsd/subsd/mulsd/divsd/
        // sqrtsd/comisd/cmpsd/xorps/movaps` 这一族。
        // 例外：`2C`/`2D`（cvttsd2si/si2sd 到 GP）与 `50`（movmskps 到 GP）目的在 GP。
        0x10 | 0x12 | 0x14 | 0x16 | 0x28 | 0x2E | 0x2F | 0x51..=0x59 | 0x5B..=0x5F | 0xC2
        | 0xC4 | 0xC5 | 0xC6 | 0xD0..=0xDF | 0xE0..=0xEF | 0xF0..=0xFE => RawWrite::Nothing,
        0x2C | 0x2D | 0x50 => RawWrite::Reg(reg),
        // `0F 5A`/`0F 2A`/`0F 2B`：cvtsi2sd（目的 xmm，rm 只读）、cvtsi2ss（同）、
        // movntps（**写内存**，非临时性，但仍是写）。
        0x5A | 0x2A => RawWrite::Nothing,
        0x2B => {
            if md.is_reg {
                RawWrite::Reg(rm)
            } else {
                RawWrite::Memory
            }
        },
        // `66 0F 6E`：movd GPR→xmm（rm 只读）；`F3 0F 7E`：movq/movd xmm←r/m（目的 reg）；
        // `66 0F 7E`：movd r/m←xmm（目的 rm）。三形不能共用一个臂。
        0x6E => RawWrite::Nothing,
        0x7E => {
            if prefix == 0xF3 {
                RawWrite::Reg(reg)
            } else if md.is_reg {
                RawWrite::Reg(rm)
            } else {
                match md.frame_slot {
                    Some(disp) => RawWrite::SlotStore { disp, len: 4, src: None },
                    None => RawWrite::Memory,
                }
            }
        },
        // 存形（r/m 是目的）：能定出帧位移就只失效那一个槽。解释器热路径把 TValue
        // 存回 `[r13+disp]` 走的就是 `F2 0F 11`（movsd m←xmm），旧版把 16 条槽事实全擦。
        0x11 | 0x13 | 0x17 | 0x21 | 0x22 | 0x23 | 0x29 | 0x7F => {
            if md.is_reg {
                // `movd xmm, r/m32` 的寄存器形在 66 前缀下写 rm（movd/movq 到 GP 的形）。
                RawWrite::Reg(rm)
            } else {
                let len = if matches!(prefix, 0xF2 | 0xF3) { 8 } else { 16 };
                match md.frame_slot {
                    Some(disp) => RawWrite::SlotStore { disp, len, src: None },
                    None => RawWrite::Memory,
                }
            }
        },
        _ => RawWrite::Unknown,
    }
}

/// 8 位形里 modrm/opcode 字段号 → 实际被写的 64 位号。
///
/// 为什么必须有它（`check_facts_vs_objdump.sh` 抖出来的真错）：无 REX 前缀时，8 位寄存器
/// 编号 4..7 指的是 **ah/ch/dh/bh**，它们分属 rax/rcx/rdx/**rbx**；而旧版直接把 4..7 当成
/// rsp/rbp/rsi/rdi。拿 `mov dh, imm`（B6）来说：报成「只写 rsi」就是漏写 rbx —— 一条
/// 「某槽 == rbx」的事实会带着已经被改过的 rbx 继续存活，那是静默错值，不是丢优化。
/// 加了任意 REX 前缀后编号又回到 spl/bpl/sil/di（低 8 位），所以判据是「rex 缺席」。
pub(crate) fn byte_reg(field: u8, rex: u8) -> u8 {
    if rex != 0 || field < 4 {
        return field;
    }
    // ah/ch/dh/bh → rax/rcx/rdx/rbx
    field - 4
}

/// 一条 raw 字节流的头部：legacy 前缀、REX、主 opcode 的下标与值。
struct Head {
    prefix: u8,
    rex: u8,
    op_at: usize,
    op0: u8,
}

/// 拆完前缀与 REX 拿主 opcode。`0x67`（地址尺寸前缀）一律不往下解：那时 modrm 里的
/// 号与位移宽度都是 32 位模型，拿 64 位的解法去读会得到完全不相干的号。
/// 段前缀（2E/36/3E/26/64/65）不影响写集，但要带着走，否则「带段前缀的访存」会被
/// 当成一条不认识的 opcode。
fn raw_head(bytes: &[u8]) -> Option<Head> {
    let mut i = 0usize;
    let mut prefix = 0u8;
    while let Some(&b) = bytes.get(i) {
        match b {
            0x66 | 0xF2 | 0xF3 | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 => {
                if b != 0x66 && !matches!(b, 0xF2 | 0xF3) {
                    // 段前缀只影响段，不影响操作数形状：带着走但不记成 prefix。
                    i += 1;
                    continue;
                }
                prefix = b;
                i += 1;
            },
            _ => break,
        }
    }
    let mut rex = 0u8;
    if let Some(&b) = bytes.get(i) {
        if (0x40..=0x4F).contains(&b) {
            rex = b;
            i += 1;
        }
    }
    // 只剩一个前缀字节（`Insn::seg_prefix` 那一族）：它自己不改任何号，真正的指令由
    // 紧随其后的那条提供、它自己会被 `observe` 处置。报 `Nothing` 而不是 `Unknown`：
    // 前缀不是「认不得的写集」，把它计成 Unknown 会让 0b 的残余表多出一个无法行动项。
    // **空字节流不是前缀**，它是「什么都没有」：必须 `None` ⇒ `Unknown`（否则
    // `Insn::raw_bytes(&[])` 这种上游错误会被当成不写号而默默放过）。
    if i > 0 && i >= bytes.len() {
        return Some(Head { prefix, rex, op_at: usize::MAX, op0: 0 });
    }
    let op0 = *bytes.get(i)?;
    Some(Head { prefix, rex, op_at: i, op0 })
}

/// 解 modrm 的结果。
struct Md {
    reg: u8,
    rm: u8,
    is_reg: bool,
    /// 基址是帧寄存器 r13、无 index、非 rip 相对时的位移；否则 `None`。
    frame_slot: Option<i32>,
}

/// 解 modrm。位移宽度由 `mod` 定（`00` 除 `rm=5` 外无位移；`rm=5&&mod=00` 是 rip 相对；
/// `01`=disp8；`10`=disp32）。SIB（`rm=4`）只说「是内存但不是固定位移」：带 index 的
/// 下标序列访问的不是一个槽，登记不了事实，但可以只清寄存器那一侧。
fn raw_modrm(bytes: &[u8], at: usize, rex: u8) -> Option<Md> {
    let m = *bytes.get(at)?;
    let md = m >> 6;
    let reg = ((m >> 3) & 7) | if rex & 0x04 != 0 { 8 } else { 0 };
    let rm3 = m & 7;
    let rm = rm3 | if rex & 0x01 != 0 { 8 } else { 0 };
    if md == 3 {
        return Some(Md { reg, rm, is_reg: true, frame_slot: None });
    }
    if rm3 == 4 {
        return Some(Md { reg, rm, is_reg: false, frame_slot: None });
    }
    if rm3 == 5 && md == 0 {
        return Some(Md { reg, rm, is_reg: false, frame_slot: None });
    }
    let disp = match md {
        0 => 0i32,
        1 => *bytes.get(at + 1)? as i8 as i32,
        _ => {
            let b: [u8; 4] = (*bytes.get(at + 1..at + 5)?).try_into().ok()?;
            i32::from_le_bytes(b)
        },
    };
    // 只有基址是帧寄存器 r13 才算帧槽（与 `frame_disp` 同一个口径）。
    let frame_slot = if rm == 13 { Some(disp) } else { None };
    Some(Md { reg, rm, is_reg: false, frame_slot })
}

/// 从 `0xC7 /0`（`mov r/m64, imm32`）且 mod=11 的字节流里取 modrm 后面那个 imm32。
///
/// 与 [`raw_immediate`] 分开而不是复用：那边的偏移是从 opcode 下标算的，假定 modrm 后
/// 就是立即数；`C7` 只在 mod=11 时才是这个形状（内存形还夹着 disp），而调用这一条的
/// 臂已经掌住了 `rm_is_reg`。拿不到（截断的字节流）就 `None`，调用方退化成「只写一号」。
fn raw_immediate32(bytes: &[u8], op_at: usize) -> Option<i32> {
    let at = op_at + 2;
    let b: [u8; 4] = (*bytes.get(at..at + 4)?).try_into().ok()?;
    Some(i32::from_le_bytes(b))
}

/// 从**不带 ModRM** 的 id 形（`B8+r imm32`、`05 id` 那一族）取 opcode 后面那 4 字节。
/// 与 [`raw_immediate32`] 分开而不是复用：那条假定 opcode 下标之后还有一个 ModRM，
/// 而 id 形没有 —— 共用就会读到立即数的低三字节当 imm。
fn raw_immediate32_id(bytes: &[u8], op_at: usize) -> Option<i32> {
    let at = op_at + 1;
    let b: [u8; 4] = (*bytes.get(at..at + 4)?).try_into().ok()?;
    Some(i32::from_le_bytes(b))
}

/// 一条宽操作的内存写宽度：REX.W⇒8，无 REX.W 且 `0x66`⇒2，其余⇒4。`byte_form` ⇒1。
fn raw_op_len(prefix: u8, rex: u8, byte_form: bool) -> u8 {
    if byte_form {
        1
    } else if rex & 0x08 != 0 {
        8
    } else if prefix == 0x66 {
        2
    } else {
        4
    }
}

/// 从 `0x81`（imm32）/`0x83`（imm8）字节流里取 modrm 后面那个立即数。
/// 取不到（截断的字节流）就返回 `None`，调用方会退化成「只写一号」。
fn raw_immediate(bytes: &[u8], op_index: usize, op: u8) -> Option<i64> {
    let at = op_index + 2;
    if op == 0x83 {
        let v = *bytes.get(at)? as i8;
        Some(v as i64)
    } else {
        let b: [u8; 4] = (*bytes.get(at..at + 4)?).try_into().ok()?;
        Some(i32::from_le_bytes(b) as i64)
    }
}

/// 向量搬运用到的字节数：`SimdMov` 的宽度藏在变体里，单独取。
fn mem_width_bytes(op: &X86_64Opcode) -> u8 {
    match op {
        X86_64Opcode::SimdMov { width, .. } => match width {
            crate::codegen::x86_64::opcode::SimdWidth::SS
            | crate::codegen::x86_64::opcode::SimdWidth::SD
            | crate::codegen::x86_64::opcode::SimdWidth::DQ => 8,
            crate::codegen::x86_64::opcode::SimdWidth::O
            | crate::codegen::x86_64::opcode::SimdWidth::PS
            | crate::codegen::x86_64::opcode::SimdWidth::PD
            | crate::codegen::x86_64::opcode::SimdWidth::B
            | crate::codegen::x86_64::opcode::SimdWidth::W
            | crate::codegen::x86_64::opcode::SimdWidth::D
            | crate::codegen::x86_64::opcode::SimdWidth::Q => 16,
        },
        _ => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn_mov(dst: O, src: O, w: Width) -> Insn {
        Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: false,
                width: w,
                dst,
                src,
            },
            &[],
        )
    }

    fn mem(disp: i32) -> O {
        O::Mem(MemOperand {
            disp,
            base: Some(FRAME),
            index: None,
            seg: None,
        })
    }

    fn nv() -> HashSet<i64> {
        HashSet::new()
    }

    #[test]
    fn slot_read_is_forwarded_to_the_register_that_holds_it() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let load = insn_mov(O::Reg(1, Width::B64), mem(-64), Width::B64);
        match st.decide(&load, &nv(), rules_for(3)) {
            Verdict::Replace(n) => match n.op {
                X86_64Opcode::MovRm {
                    dst: O::Reg(1, _),
                    src: O::Reg(0, _),
                    ..
                } => {},
                other => panic!("期望转发成 mov rcx,rax，实际 {:?}", other),
            },
            _ => panic!("槽里就是 rax 的值，读应该被转发"),
        }
    }

    #[test]
    fn volatile_slot_is_neither_forwarded_nor_deduplicated() {
        let mut st = BlockState::default();
        let mut vol = HashSet::new();
        vol.insert(-64i64);
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &vol);
        let load = insn_mov(O::Reg(1, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load, &vol, rules_for(3)), Verdict::Keep));
        // 同一号二次写也不许当死写削掉。
        let again = insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64);
        assert!(matches!(st.decide(&again, &vol, rules_for(3)), Verdict::Keep));
    }

    /// `movswl %ax,%eax`（`MovExt{sign, from:B16, to:B32}`）**不**建立 `sext32` 事实。
    ///
    /// 32 位目的写清掉高 32 位，所以 `%rax` 里是 `0x00000000FFFF8000`，而
    /// `movsxd %rax,%rax` 要的是 `0xFFFFFFFFFFFF8000` —— 不是恒等。以前按
    /// `width_bytes(to) >= 4` 一并记成符号扩展形态，消费点就把这条必需的 `movsxd`
    /// 当恒等 Drop 掉（复杂数的分量搬运与原子辅助真在发这一族形态）。
    #[test]
    fn movswl_to_32bit_dest_does_not_authorize_the_identity_movsxd_drop() {
        let widen = Insn::mov_ext(true, Width::B32, Width::B64, 1, 1);
        // 正例：32 位目的的符号扩展不建立事实 ⇒ 那条 movsxd 必须照发
        let mut st = BlockState::default();
        st.observe(&Insn::mov_ext(true, Width::B16, Width::B32, 1, 0), &nv());
        assert!(
            matches!(st.decide(&widen, &nv(), rules_for(3)), Verdict::Keep),
            "movswl 之后 movsxd %rax,%rax 被当恒等削掉了（少一次符号扩展 = 静默错值）"
        );
        // 反向对照：64 位目的的符号扩展确实建立规范形态，那一族的 Drop 不能跟着关掉
        let mut st = BlockState::default();
        st.observe(&Insn::mov_ext(true, Width::B16, Width::B64, 1, 0), &nv());
        assert!(
            matches!(st.decide(&widen, &nv(), rules_for(3)), Verdict::Drop),
            "movsxw 到 64 位目的之后，movsxd 才是恒等 —— 这条得照旧削"
        );
    }

    /// R_CPY32（批 2 C-lite 第二条）：`mov r64,r64` 只在「源号高 32 位已知为零」且
    /// **两条长度相同**时才换 32 位形态。
    ///
    /// 三个反面各钉一条：没事实不能换；低 8 号那一族 3B→2B 会缩短整块，不能换；
    /// 位图没给 R_CPY32 时一律照发（默认档位不漂字节的根据）。
    #[test]
    fn mov_rr_is_narrowed_only_with_a_zero_extended_source_and_same_length() {
        use crate::codegen::shared::asm_operand::Width;
        // `mov %r12, %rcx`：`Codegen::mov` 的唯一形态就是 REX + 8B + mod=11，
        // reg 字段是目的(rcx)、r/m 字段是源(r12 ⇒ REX.B)。REX.WB = 0x49，
        // ModRM = mod=11 reg=001 rm=100 = 0xCC。（字节按 `classify_peep` 同一对读法。）
        let copy = Insn::raw_bytes(&[0x49u8, 0x8B, 0xCC]);
        // 1) 无事实 ⇒ 不换
        let st = BlockState::default();
        assert!(matches!(st.decide(&copy, &nv(), rules_for(3) | R_CPY32), Verdict::Keep));
        // 2) 源号高 32 位已知为零 ⇒ 换，且长度仍是 3 字节
        let mut st = BlockState::default();
        st.observe(
            &insn_mov(O::Reg(12, Width::B32), O::Imm(0i64), Width::B32),
            &nv(),
        );
        match st.decide(&copy, &nv(), rules_for(3) | R_CPY32) {
            Verdict::Replace(n) => {
                let bytes = n.encode_bytes().expect("32 位形态要能编码");
                assert_eq!(bytes.len(), 3, "替换必须严格同长，实得 {bytes:02x?}");
                assert_eq!(n.gas_text.as_deref(), Some("mov %r12d, %ecx"));
            },
            other => panic!("zext32[r12] 已知时应换 32 位形态，实得 {:?}", match other {
                Verdict::Keep => "keep",
                Verdict::Drop => "drop",
                Verdict::Replace(_) => "replace",
            }),
        }
        // 默认位图（不带 R_CPY32）必须完全不换 —— 「门关零漂移」的根据
        assert!(matches!(st.decide(&copy, &nv(), rules_for(3)), Verdict::Keep));
        // 3) 低 8 号对（`mov %rcx, %rax`）：32 位形态不带 REX，会缩到 2 字节 ⇒ 不得换
        let low = Insn::raw_bytes(&[0x48u8, 0x8B, 0xC1]);
        let mut st2 = BlockState::default();
        st2.observe(
            &insn_mov(O::Reg(1, Width::B32), O::Imm(0i64), Width::B32),
            &nv(),
        );
        assert!(
            matches!(st2.decide(&low, &nv(), rules_for(3) | R_CPY32), Verdict::Keep),
            "低 8 号对的 32 位形态会缩短整块，不得换"
        );
    }

    #[test]
    fn narrow_load_into_the_same_register_is_not_an_identity() {
        // `mov eax,[slot]` 会把 rax 的高 32 位清掉，所以「槽里就是 rax」并不等于恒等，
        // 除非 rax 的高位已经是零。少了这一条就是静默错值。
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let load32 = insn_mov(O::Reg(0, Width::B32), mem(-64), Width::B32);
        assert!(
            matches!(st.decide(&load32, &nv(), rules_for(3)), Verdict::Keep),
            "高位未清零时窄载入不是恒等，不得删"
        );
        st.zext32[0] = true;
        assert!(matches!(st.decide(&load32, &nv(), rules_for(3)), Verdict::Drop));
    }

    #[test]
    fn call_and_raw_bytes_clear_the_block_state() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        st.observe(
            &Insn::new(
                X86_64Opcode::Call {
                    target: O::Sym("f".into()),
                    pending_rel: false,
                },
                &[],
            ),
            &nv(),
        );
        let load = insn_mov(O::Reg(1, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load, &nv(), rules_for(3)), Verdict::Keep));
    }

    #[test]
    fn unknown_base_store_invalidates_frame_slots() {
        // 通过指针的写可能别名任何帧槽，必须整体清掉。
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let ptr_store = insn_mov(
            O::Mem(MemOperand {
                disp: 0,
                base: Some(5),
                index: None,
                seg: None,
            }),
            O::Reg(1, Width::B64),
            Width::B64,
        );
        st.observe(&ptr_store, &nv());
        let load = insn_mov(O::Reg(2, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load, &nv(), rules_for(3)), Verdict::Keep));
    }

    #[test]
    fn partial_store_kills_the_wider_fact() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        st.observe(&insn_mov(mem(-64), O::Reg(1, Width::B32), Width::B32), &nv());
        let load64 = insn_mov(O::Reg(2, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load64, &nv(), rules_for(3)), Verdict::Keep));
        // 低 32 位的事实仍在：按 4 字节读可以转发给 rcx。
        let load32 = insn_mov(O::Reg(2, Width::B32), mem(-64), Width::B32);
        match st.decide(&load32, &nv(), rules_for(3)) {
            Verdict::Replace(n) => assert!(matches!(
                n.op,
                X86_64Opcode::MovRm {
                    src: O::Reg(1, _),
                    ..
                }
            )),
            _ => panic!("低 32 位的事实应当仍然有效"),
        }
    }

    #[test]
    fn push_and_pop_keep_frame_slot_facts() {
        // push/pop 只碰 [rsp-8] 以下的栈顶区；解释器热路径全靠这条精度。
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        st.observe(
            &Insn::new(
                X86_64Opcode::Stack {
                    op: StackOp::Push,
                    width: Width::B64,
                    dst: O::Reg(1, Width::B64),
                },
                &[],
            ),
            &nv(),
        );
        st.observe(
            &Insn::new(
                X86_64Opcode::Stack {
                    op: StackOp::Pop,
                    width: Width::B64,
                    dst: O::Reg(1, Width::B64),
                },
                &[],
            ),
            &nv(),
        );
        let load = insn_mov(O::Reg(2, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load, &nv(), rules_for(3)), Verdict::Replace(_)));
    }

    #[test]
    fn redundant_copy_is_dropped_only_after_a_real_equalizing_move() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(O::Reg(1, Width::B64), O::Reg(0, Width::B64), Width::B64), &nv());
        let again = insn_mov(O::Reg(1, Width::B64), O::Reg(1, Width::B64), Width::B64);
        assert!(matches!(st.decide(&again, &nv(), rules_for(2)), Verdict::Drop));
        let same = insn_mov(O::Reg(1, Width::B64), O::Reg(0, Width::B64), Width::B64);
        assert!(matches!(st.decide(&same, &nv(), rules_for(2)), Verdict::Drop));
        // rax 被写坏之后，rcx==rax 这条事实必须消失。
        st.observe(&insn_mov(O::Reg(0, Width::B64), O::Imm(7), Width::B64), &nv());
        assert!(matches!(st.decide(&same, &nv(), rules_for(2)), Verdict::Keep));
    }

    #[test]
    fn level_zero_never_decides_anything() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let load = insn_mov(O::Reg(0, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load, &nv(), rules_for(0)), Verdict::Keep));
    }

    /// 批0c 的「旧开关不漂字节」依据：三档 `DCC_BLOCKOPT` 映射到的位图必须与旧档位
    /// 的单调累积语义逐位相同。位图里三个已知位之外的任何位都不许被旧档位顺带打开
    /// （否则 `level=1` 会捎上批2 的新臂，那就不再是同一个编译器了）。
    #[test]
    fn rules_for_maps_levels_without_drift() {
        assert_eq!(rules_for(0), 0);
        assert_eq!(rules_for(1), R_SEXT);
        assert_eq!(rules_for(2), R_SEXT | R_COPY);
        // 档 3 是默认档，S1/S3a 翻入了 `R_RAW` 与 `R_ZBITS`（理由与实测读数写在函数
        // 注释里）。0..2 三档继续逐字不变，那是「单条规则能单独关」的归因基础。
        assert_eq!(rules_for(3), R_SEXT | R_COPY | R_SLOT | R_RAW | R_ZBITS | R_IMMSEXT);
        // `level()` 把 >3 夹到 3，位图也必须落在同一处。
        assert_eq!(rules_for(4), rules_for(3));
        assert_eq!(rules_for(255), rules_for(3));
        // 三条实测买 0 条的臂必须仍在默认位图之外：「翻默认」只能翻有读数的。
        let not_flipped = R_DEADW | R_CPY32 | R_CPYBASE | R_CALLSLOT | R_TYPED | R_LRU;
        assert_eq!(rules_for(3) & not_flipped, 0, "未实证的臂不得顺带打开");
        // 本轮新补的那批事实（`R_IMMSEXT`）已过了两负载的同轮配对才进默认档：
        // SQLite median 0.9482 / CI 上界 0.9588 / 25⁄25 同向（保留），Lua median 0.9847 /
        // CI 上界 0.9947（不可判 ⇒ 不记它的收益）。读数与量尺拆分过程写在函数注释里。
        assert_ne!(rules_for(3) & R_IMMSEXT, 0, "R_IMMSEXT 已过配对，应当在默认档里");
        assert_eq!(not_flipped & R_IMMSEXT, 0, "上面那份「未实证」名单不得混进已实证的位");
    }

    /// `written_gp` 与 `observe` 对同一批变体必须给同一个保守度。
    ///
    /// 为什么需要这条而不只依赖穷尽 `match`：编译钉子保证「新变体被回答了」，但不保证
    /// 「回答与事实层一致」。两者不一致时，区域家会拿一个「事实层认为已被破坏」的号当家，
    /// 而 `written_gp` 那边却显示它安全 —— 那就是静默错值。
    #[test]
    fn written_gp_agrees_with_observe_on_the_kill_all_set() {
        // 1) 只写一个号的形态：observe 必须只杀那一个号，written_gp 也必须只报那一个号。
        let add = Insn::alu_rm_reg_imm32_b64(0, 9, 1); // add r9, imm（Raw 形态）
        assert_eq!(written_gp(&add), RegWrites::Regs(regbit(9)));
        // 2) 不写号的形态（cmp）：两边都必须「一个号也不杀」。
        let cmp = Insn::alu_rm_reg_imm32_b64(7, 9, 1); // cmp r9, imm
        assert_eq!(written_gp(&cmp), RegWrites::None);
        // 3) 一次调用：caller-saved 全集，而 callee-saved 不在里面。
        let call = Insn::new(
            X86_64Opcode::Call { target: O::Imm(0), pending_rel: false },
            &[],
        );
        assert_eq!(written_gp(&call), RegWrites::RegsMem(SYSV_CALLER_SAVED));
        for r in [3u8, 5, 12, 13, 14, 15] {
            assert!(written_gp(&call).keeps(r), "callee-saved 号 {r} 必须可以活过 call");
        }
        for r in [0u8, 1, 2, 6, 7, 8, 9, 10, 11] {
            assert!(!written_gp(&call).keeps(r), "caller-saved 号 {r} 不得活过 call");
        }
        // 4) 与 `observe` 的一致性：同一条指令后，一个 callee-saved 上的槽事实两边都应当能活。
        let store = insn_mov(mem(-64), O::Reg(12, Width::B64), Width::B64);
        let mut st = BlockState::default();
        st.observe(&store, &nv());
        assert_eq!(st.slot_of(-64, 8), Some(12));
        st.observe(&cmp, &nv());
        assert!(written_gp(&cmp).keeps(12));
        assert_eq!(st.slot_of(-64, 8), Some(12), "cmp 之后：两边都说 r12 安全，事实必须在");
        st.observe(&call, &nv());
        assert!(!written_gp(&call).keeps(0), "rax 被 call 破坏，written_gp 必须说杀");
        assert_eq!(st.copy[3], NO_REG, "observe 与 written_gp 不得对 rax 给相反答案");
        // 5) 认不得的 raw 字节：两边都必须是 `All`/全杀。
        let bogus = Insn::raw_bytes(&[0x0f, 0x01, 0x00]);
        assert_eq!(written_gp(&bogus), RegWrites::All, "认不得的 raw 只能报 All");
    }

    /// A3 的两臂差异：同一个溢出形状，关时全弃、开时只淘一条。
    ///
    /// 为什么用「造 17 个不同位移」而不是跑一个真函数：容量溢出在 Lua 上不绑
    /// （Gate 0a 读数 `hit_capacity=0`），拿真函数做判据就永远碰不到这一臂，
    /// 于是「A3 实现了」会变成一句无法证伪的话。
    #[test]
    fn slot_capacity_evicts_one_not_all() {
        use crate::codegen::shared::asm_operand::Width as W;
        let base = R_SEXT | R_COPY | R_SLOT;
        // 先满 16 条：-256, -248, … 都是不同位移（注意不能推到 -8，否则第 17 条
        // 用的就是表里已有的那一条，碰不到溢出臂）。
        let fill = |st: &mut BlockState, rules: u16| {
            for i in 0..MAX_SLOTS {
                let d = -256 + (i as i32) * 8;
                st.observe_at_call(&insn_mov(mem(d), O::Reg(3, W::B64), W::B64), &nv(), CallPolicy::no_relax(), rules);
            }
        };
        // 关（默认位图不含 R_LRU）：第 17 条把全部 16 条一起丢掉。
        let mut off = BlockState::default();
        fill(&mut off, base);
        assert_eq!(off.live_slots(), MAX_SLOTS as u64, "填满是本用例的前提");
        off.observe_at_call(
            &insn_mov(mem(-8), O::Reg(3, W::B64), W::B64),
            &nv(),
            CallPolicy::no_relax(),
            base,
        );
        assert_eq!(off.live_slots(), 0, "R_LRU 关时溢出必须整体作废（今天的行为）");
        // 开：只丢掉最久未用那一条，其余 15 条 + 新的 1 条仍在。
        let mut on = BlockState::default();
        fill(&mut on, base | R_LRU);
        // 刷一次最久未用那条以外的所有槽的时计，让淘汰目标可预期：读命中会推前 stamp。
        for i in 1..MAX_SLOTS {
            let d = -256 + (i as i32) * 8;
            on.rules = base | R_LRU;
            on.touch_slot(d, 8);
        }
        on.observe_at_call(
            &insn_mov(mem(-8), O::Reg(3, W::B64), W::B64),
            &nv(),
            CallPolicy::no_relax(),
            base | R_LRU,
        );
        assert_eq!(on.live_slots(), MAX_SLOTS as u64, "R_LRU 开时必须只淘一条");
        assert_eq!(on.slot_of(-8, 8), Some(3), "新事实必须已登记");
        assert_eq!(on.slot_of(-256, 8), None, "最久未用那一条才是被淘汰的");
        assert_eq!(on.slot_of(-248, 8), Some(3), "刚被读到过的槽不得被淘汰");
    }

    /// 位图的真正用处：能只关其中一条。这里验「开 SLOT、关 COPY」与
    /// 「开 COPY、关 SLOT」各自只能看到自己那一条的处置 —— 六臂格阵的归因
    /// 全靠这条，否则「某一项为负」根本无法表达。
    #[test]
    fn rules_bits_isolate_each_verdict() {
        let mut st = BlockState::default();
        // 事实：[rbp-64] 的低 8 字节就是 rax。
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let load_from_slot = insn_mov(O::Reg(1, Width::B64), mem(-64), Width::B64);
        // 只关 COPY（SLOT 开着）：帧槽读仍转发成寄存器搬运。
        assert!(matches!(
            st.decide(&load_from_slot, &nv(), R_SLOT),
            Verdict::Replace(_)
        ));
        // 只开 COPY（SLOT 关着）：不得再碰帧槽事实。
        assert!(matches!(
            st.decide(&load_from_slot, &nv(), R_COPY),
            Verdict::Keep
        ));
        // 寄存器相等那一条只看 COPY：同号自反 mov 在 COPY 位上恒等可删。
        st.observe(&insn_mov(O::Reg(1, Width::B64), O::Reg(0, Width::B64), Width::B64), &nv());
        let again = insn_mov(O::Reg(1, Width::B64), O::Reg(1, Width::B64), Width::B64);
        assert!(matches!(
            st.decide(&again, &nv(), R_COPY),
            Verdict::Drop
        ));
        assert!(matches!(
            st.decide(&again, &nv(), R_SLOT),
            Verdict::Keep
        ));
    }

    /// `DCC_BLOCKOPT_RULES` 的解析面：十进制/十六进制可用，越界与非数一律当作
    /// 未设置（宁可得不到新臂，也不要静默落到一个没人预期的位图上）。
    #[test]
    fn parse_rules_accepts_dec_and_hex_and_rejects_garbage() {
        assert_eq!(parse_rules("7"), Some(R_SEXT | R_COPY | R_SLOT));
        assert_eq!(parse_rules(" 0x28 "), Some(R_ZBITS | R_RAW));
        assert_eq!(parse_rules("255"), Some(0xFF));
        // 位图从 u8 扩到 u16 的理由写在 `R_CALLSLOT` 那里：A1/A2/A3 三个屏障重划项
        // 需要 `1 << 8` 以上的位号。上界也跟着抬，旧读法（16#100 视为无效）必须继续无效。
        assert_eq!(parse_rules("256"), Some(256));
        assert_eq!(parse_rules("0x1FF"), Some(0x1FF));
        assert_eq!(parse_rules("65536"), None);
        assert_eq!(parse_rules("0x10000"), None);
        assert_eq!(parse_rules(""), None);
        assert_eq!(parse_rules("-1"), None);
        assert_eq!(parse_rules("all"), None);
    }

    /// 把 `raw_writes` 钉在 `insn.rs` 那些构造子的**真实字节**上。
    ///
    /// 为什么不手写码字数组当输入：手写会把「构造子到底把号放哪个字段」这件事藏起来——
    /// `0x81 /digit, r/m64, imm32` 的目的在 r/m，`0x69 /r, r/m64, imm32` 的目的在 reg，
    /// 本函数第一版把前者当成了 reg，只看字节序列是看不出来的。拿构造子取字节，
    /// 以对方改了编码就立即变红。
    #[test]
    fn raw_writes_matches_the_imm_constructors() {
        use crate::codegen::x86_64::insn::Insn;
        // 这个 `match` 保持**穷尽**（无 `_` 臂）就是方案 §3 S1 要求的编译钉子：
        // 以后给 `RawWrite` 加一个形态而不在此处说明它「应报成什么」就编不过。
        let w = |ins: &Insn| match raw_writes(&ins.encode_bytes().unwrap_or_default()) {
            RawWrite::Nothing => "nothing".to_string(),
            RawWrite::Reg(r) => format!("r{r}"),
            RawWrite::Regs(m) => format!("regs{m:04x}"),
            RawWrite::Masked { dst, bits } => format!("r{dst}<2^{bits}"),
            RawWrite::Call => "call".to_string(),
            RawWrite::Terminate => "terminate".to_string(),
            RawWrite::Strs => "strs".to_string(),
            RawWrite::SlotStore { disp, len, src } => {
                format!("slot[{disp};{len};{}]", src.map_or('-'.to_string(), |s| format!("r{s}")))
            },
            // `mov r64, imm32` 与 `movsx/movsxd`：两者都答「整字写 + 结果已是低 32 位的
            // 符号扩展」，区别只在有无一个立即数可报。
            RawWrite::ImmSExt { dst, imm } => format!("r{dst}=sext32({imm})"),
            RawWrite::SExt { dst } => format!("r{dst}=sext32"),
            // `movzx` 单独一个形（而不是共用 `Masked`）的理由就是它能单独关：
            // 见 `R_IMMSEXT`。不在此处说它「该报成什么」就编不过。
            RawWrite::ZExt { dst, bits } => format!("r{dst}=zext({bits})"),
            RawWrite::Memory => "mem".to_string(),
            RawWrite::Unknown => "?".to_string(),
        };
        // ALU：r/m 字段是目的；/7(CMP) 不写号。r13 = 13（REX.B 那位止于 r/m）。
        assert_eq!(w(&Insn::alu_rm_reg_imm32_b64(0, 13, 8)), "r13", "add r13, imm");
        assert_eq!(w(&Insn::alu_rm_reg_imm8_b64(5, 12, 1)), "r12", "sub r12, imm8");
        assert_eq!(w(&Insn::alu_rm_reg_imm32_b64(7, 13, 4)), "nothing", "cmp 不写任何号");
        // and 是非破坏写且上界已知：同一个 dst 要报成 Masked 而不是裸 Reg。
        assert_eq!(w(&Insn::alu_rm_reg_imm32_b64(4, 8, 0xff)), "r8<2^8");
        // MOV r/m64, imm32：r/m 字段是目的，而且写进去的就是 imm32 的符号扩展
        // （本条事实就是「紧随其后的 `movsxd dst,dst` 可删」的全部依据）。
        assert_eq!(w(&Insn::mov_reg_imm32_b64(12, 5)), "r12=sext32(5)");
        // 同一构造子的负值：仍然答 sext32（imm32 符号扩展后高位全是 1）。
        assert_eq!(w(&Insn::mov_reg_imm32_b64(0, -7)), "r0=sext32(-7)");
        // movsxd 的内存形与寄存器形：目的在 reg 字段，答 sext32。
        assert_eq!(w(&Insn::mov_ext(true, Width::B32, Width::B64, 1, 5)), "r1=sext32");
        // movzx 字节/字形：答零扩展上界（单独一个形，开关与 `and` 那一路的 `Masked` 不同）。
        assert_eq!(w(&Insn::raw_bytes(&[0x0F, 0xB6, 0x03])), "r0=zext(8)");
        assert_eq!(w(&Insn::raw_bytes(&[0x48, 0x0F, 0xB7, 0xC1])), "r0=zext(16)");
        // 移位：r/m 字段是目的。
        assert_eq!(w(&Insn::shift_reg_imm8_c1_b64(4, 3, 2)), "r3");
        // IMUL：自反形，reg 字段是目的。
        assert_eq!(w(&Insn::imul_rm_reg_imm32_b64(9, 16)), "r9");
    }

    /// caller-saved 集的内容就是 ABI 事实本身：少一个号是静默错值，多一个号是把
    /// A1 的收益归零。拿号表直接钉住，而不是靠注释。
    #[test]
    fn call_write_set_is_the_sysv_clobber_set() {
        // 必被破坏：rax,rcx,rdx,rsi,rdi,r8,r9,r10,r11
        for r in [0u8, 1, 2, 6, 7, 8, 9, 10, 11] {
            assert!(SYSV_CALLER_SAVED & regbit(r) != 0, "号 {r} 是 caller-saved，必须在表里");
        }
        // 必被保留：rbx(3), rbp(5), r12–r15；rsp(4) 由调用约定自己管
        for r in [3u8, 4, 5, 12, 13, 14, 15] {
            assert!(SYSV_CALLER_SAVED & regbit(r) == 0, "号 {r} 是 callee-saved，不得进表");
        }
    }

    /// 反面判据：不能把「写了别的号或写了内存」的形态当成**只写一个号**。S1 补全后
    /// 这些形态不再只能报 `Unknown`，但不变量不变：目的在内存时绝不能报成某个寄存器的
    /// 写（那等于拿一条错事实去削后续指令）。
    #[test]
    fn raw_writes_never_reports_a_memory_write_as_a_single_reg() {
        // `add QWORD PTR [rbx], imm32`：mod=00 且 r/m=3 ⇒ 写内存。
        // 必须是 `Memory`（保留寄存器事实、弃槽事实），不能是 `Reg(3)`。
        assert!(matches!(raw_writes(&[0x48, 0x81, 0x00 + 3, 1, 0, 0, 0]), RawWrite::Memory));
        // `mul r/m64`（F7 /4）：同时写 rax 与 rdx（rdx 只在高位非零时被改），不碰内存。
        // 报成只写一号就是漏一个号 ⇒ 必须带 rdx。
        assert!(matches!(
            raw_writes(&[0x48, 0xF7, 0xE0 + 3]),
            RawWrite::Regs(m) if m & 1 != 0
        ));
        // `shift` 但 mod=10（带 disp 的内存操作数）。
        assert!(matches!(raw_writes(&[0x48, 0xC1, 0x40 | 3, 2]), RawWrite::Memory));
        // `mov [r13-64], rdx` 是一次真存槽：报出的位移与宽度必须与字节一致，
        // 否则失效范围会算错（多失效是丢优化，少失效是错值）。
        // REX.B 那一位是帧基址 r13 的一部分，不带它就是 rbp。
        assert!(matches!(
            raw_writes(&[0x49, 0x89, 0x55, 0xC0]),
            RawWrite::SlotStore { disp: -64, len: 8, src: Some(2) }
        ));
        // 同一字节序列不带 REX.B 时基址是 rbp，不是帧槽。
        assert!(matches!(raw_writes(&[0x48, 0x89, 0x55, 0xC0]), RawWrite::Memory));
        // `[rbx]` 基址的存槽不是帧槽 ⇒ 只能报 `Memory`，不能报 `SlotStore`。
        assert!(matches!(raw_writes(&[0x48, 0x89, 0x13]), RawWrite::Memory));
        // rip 相对访存（mod=00, rm=101）不是任何号的副本，也不是帧槽。
        assert!(matches!(raw_writes(&[0x48, 0x89, 0x1D, 0, 0, 0, 0]), RawWrite::Memory));
        // SIB（rm=4）：基址要再读一字节，带 index 的下标访问不是一个固定槽。
        assert!(matches!(raw_writes(&[0x48, 0x89, 0x14, 0xCD]), RawWrite::Memory));
    }

    /// 反面判据的另一半：S1 **不能把不确定形态报成确定写集**。下面每一条形制都
    /// 必须维持 `Unknown`（报成任何具体写集都会拿一条错事实削指令）。
    #[test]
    fn raw_writes_keeps_conservative_forms_unknown() {
        // `xchg r/m, r` 的内存形（48 87 03）：同时写内存与一个号，而且号的写入值
        // 是「内存旧值」—— 那不是一条能从字节流定出来的寄存器写。
        assert!(matches!(raw_writes(&[0x48, 0x87, 0x03]), RawWrite::Unknown));
        // `0x63` 无 REX.W 是未定义别名：宁可 Unknown，不能当 movsxd。
        assert!(matches!(raw_writes(&[0x63, 0x03]), RawWrite::Unknown));
        // `push`（FF /6）改 rsp 又写内存：不在事实表的知识里。
        assert!(matches!(raw_writes(&[0xFF, 0x70 | 3]), RawWrite::Unknown));
        // `bt r/m, r`（0F A3）：内存形可能写 r/m（bts/btr/btc 更是直接写）。
        assert!(matches!(raw_writes(&[0x0F, 0xA3, 0x03]), RawWrite::Unknown));
        // `0F 01` 除 xgetbv/序列化那一族外的子操作（如 `sgdt`）：权限级别与写集都不知道。
        assert!(matches!(raw_writes(&[0x0F, 0x01, 0x00]), RawWrite::Unknown));
        // 只剩一个 0x66 的孤立前缀不是「不写号」，而是「下面那条指令还没读到」：
        // 段/操作尺寸前缀报 Nothing，但 `0x66` 带着它只能与真正的指令一起读。
        // 本后端不会单独发 `66` 字节，所以本项只钉住「不会把它误当成某个号的写」。
        assert!(!matches!(raw_writes(&[0x66]), RawWrite::Reg(_)));
    }

    /// 正面判据：「目的恒在 modrm.reg、内存只读」那一族无论 mod 是不是 11 都只写一个号。
    /// 这一族在旧版里被 `modrm>>6 != 3 ⇒ Unknown` 一律错杀（实测 `luaV_execute` 光 0x8B
    /// 就 1,647 条），本测同《闸门下放》一起作为回归锁。
    #[test]
    fn raw_writes_reg_destination_family_survives_memory_operand() {
        let dst = |b: &[u8]| match raw_writes(b) {
            RawWrite::Reg(r)
            | RawWrite::Masked { dst: r, .. }
            | RawWrite::SExt { dst: r }
            | RawWrite::ZExt { dst: r, .. }
            | RawWrite::ImmSExt { dst: r, .. } => Some(r),
            _ => None,
        };
        // `mov rax, QWORD PTR [rbx]`：写 rax。
        assert_eq!(dst(&[0x48, 0x8B, 0x03]), Some(0));
        // `mov r12, QWORD PTR [r13-0x30]`：REX.R 把 reg 字段抬到 r12。
        assert_eq!(dst(&[0x4C, 0x8B, 0x65, 0xD0]), Some(12));
        // `lea rax, [rip+0x100]`：写 rax，不写内存。
        assert_eq!(dst(&[0x48, 0x8D, 0x05, 0x00, 0x01, 0x00, 0x00]), Some(0));
        // `movzx eax, BYTE PTR [rbx]` 与 `movsx rax, BYTE PTR [rbx]`。
        assert_eq!(dst(&[0x0F, 0xB6, 0x03]), Some(0));
        assert_eq!(dst(&[0x48, 0x0F, 0xBE, 0x03]), Some(0));
        // `movsxd rax, DWORD PTR [rbx]`（REX.W 形）。目的在 reg 字段，而且多带一条
        // 「结果已是 sext32」的事实（下面两条 movzx/movsx 同一条理由）。
        assert_eq!(dst(&[0x48, 0x63, 0x03]), Some(0));
        assert!(
            matches!(raw_writes(&[0x48, 0x63, 0x03]), RawWrite::SExt { dst: 0 }),
            "movsxd 必须把「整字已是符号扩展」报上去，否则紧随的 `movsxd rax,eax` 削不掉"
        );
        assert!(
            matches!(raw_writes(&[0x0F, 0xB6, 0x03]), RawWrite::ZExt { dst: 0, bits: 8 }),
            "movzx 字节形必须报上界 8（单独一个 `ZExt` 形，好拿 `R_IMMSEXT` 单独关）：\n\
             那是 `movzx rax,al; movsxd rax,eax` 这一对可削的依据"
        );
        // `add rax, QWORD PTR [rbx]` / `sub r13, QWORD PTR [r12]`：reg 是目的。
        assert_eq!(dst(&[0x48, 0x03, 0x03]), Some(0));
        assert_eq!(dst(&[0x4C, 0x2B, 0x2C]), Some(13));
        // `mov r, imm`：目的在 rm 字段。S1 补全后 32 位形还能多给一条值域事实
        // （`mov r8d, imm32` 自动把高 32 位清零），所以这一条不再是 `Reg`。
        // S3c 体量交换后再精确一档：**非负** imm32 的零扩展与符号扩展同值，所以
        // 连 `sext32` 也能报（`ImmSExt`）—— 发射器从本轮起用这一形发非负常量，只报
        // `Masked` 会让紧随其后的 `movsxd dst,dst` 从可削变成必发。
        assert!(matches!(
            raw_writes(&[0x41, 0xB8, 0x05, 0x00, 0x00, 0x00]),
            RawWrite::ImmSExt { dst: 8, imm: 5 }
        ));
        // 负立即数不能报符号扩展：32 位形写进去的是零扩展值，两者不同。
        assert!(matches!(
            raw_writes(&[0xB8, 0xFF, 0xFF, 0xFF, 0xFF]),
            RawWrite::Masked { dst: 0, bits: 32 }
        ), "bit31 置高的 `mov r32,imm32` 只能推零扩展上界");
        // 截断的字节流退化为保守答话，不能猜一个立即数。
        assert!(matches!(
            raw_writes(&[0xB8, 0x05, 0x00]),
            RawWrite::Masked { dst: 0, bits: 32 }
        ));
        assert_eq!(dst(&[0x49, 0xB8, 0x05, 0x00, 0x00, 0x00]), Some(8), "REX.W 形写满 64 位");
        // `cmp`/`test` 两种 mod 都不写号。
        assert!(matches!(raw_writes(&[0x48, 0x3B, 0x03]), RawWrite::Nothing));
        assert!(matches!(raw_writes(&[0x48, 0x85, 0xC9]), RawWrite::Nothing));
        // `test rax, imm`（F7 /0）不写号；`idiv`（F7 /7）写 rax+rdx 但不碰内存。
        assert!(matches!(raw_writes(&[0x48, 0xF7, 0xC0]), RawWrite::Nothing));
        assert!(matches!(
            raw_writes(&[0x48, 0xF7, 0xF8]),
            RawWrite::Regs(m) if m == regbit(0) | regbit(2)
        ));
        // `endbr64`（0F 1E FA）与多字节 nop（0F 1F 00）不写号。
        assert!(matches!(raw_writes(&[0x0F, 0x1E, 0xFA]), RawWrite::Nothing));
        assert!(matches!(raw_writes(&[0x0F, 0x1F, 0x00]), RawWrite::Nothing));
        // 空字节流不能猜测。
        assert!(matches!(raw_writes(&[]), RawWrite::Unknown));
    }

    /// `R_RAW` 关掉那一臂时 `Raw` 仍走 `kill_all`：事实层的能力必须能单独收回。
    ///
    /// 旧版本这条测的是「默认位图不含 R_RAW」。S1 翻默认后默认已经含入了它（理由
    /// 与实测读数写在 `rules_for` 上），所以臂改钉在「不含那一位」这个形状上：
    /// 只要有人拿 `DCC_BLOCKOPT_RULES` 关掉它，行为就必须退回旧样。
    #[test]
    fn raw_bytes_still_clear_everything_without_the_raw_bit() {
        let no_raw = R_SEXT | R_COPY | R_SLOT;
        let with_raw = no_raw | R_RAW;
        let store = insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64);
        // 一条 `add rcx, 1` 的 Raw 形态：只写 rcx。
        let add = crate::codegen::x86_64::insn::Insn::alu_rm_reg_imm32_b64(0, 1, 1);
        let load = insn_mov(O::Reg(2, Width::B64), mem(-64), Width::B64);
        // 关：整表全杀 ⇒ 帧读不能转发。
        let mut off = BlockState::default();
        off.observe_at_call(&store, &nv(), CallPolicy::no_relax(), no_raw);
        off.observe_at_call(&add, &nv(), CallPolicy::no_relax(), no_raw);
        assert!(
            matches!(off.decide(&load, &nv(), no_raw), Verdict::Keep),
            "R_RAW 关掉时，一条只写 rcx 的指令仍必须擦掉 rax 的槽事实"
        );
        // 开：只写 rcx ⇒ rax 的槽事实仍在，帧读可以转发。
        let mut on = BlockState::default();
        on.observe_at_call(&store, &nv(), CallPolicy::no_relax(), with_raw);
        on.observe_at_call(&add, &nv(), CallPolicy::no_relax(), with_raw);
        assert!(
            matches!(on.decide(&load, &nv(), with_raw), Verdict::Replace(_)),
            "R_RAW 开着时，一条只写 rcx 的指令不该再擦 rax 的槽事实"
        );
    }

    /// `and` 立即数产生上界事实；负立即数与取不到立即数都不得产生。
    #[test]
    fn raw_and_produces_a_bound_only_for_non_negative_immediates() {
        // `and r12d, 0xff`（REX.W 81 /4，imm32）
        let a = Insn::alu_rm_reg_imm32_b64(4, 12, 0xff);
        match raw_writes(&a.encode_bytes().unwrap_or_default()) {
            RawWrite::Masked { dst, bits } => {
                assert_eq!(dst, 12);
                assert_eq!(bits, 8, "0xff 是 8 位");
            },
            other => panic!("and r12, 0xff 应当产生上界事实，实际 {other:?}"),
        }
        // imm8 形：`and r9d, 7`
        let b = Insn::alu_rm_reg_imm8_b64(4, 9, 7);
        assert!(matches!(
            raw_writes(&b.encode_bytes().unwrap_or_default()),
            RawWrite::Masked { dst: 9, bits: 3 }
        ));
        // 负立即数（`and r, -1`）与超过 2^31 的不算上界。
        let c = Insn::alu_rm_reg_imm32_b64(4, 9, -1);
        assert!(matches!(
            raw_writes(&c.encode_bytes().unwrap_or_default()),
            RawWrite::Reg(9)
        ));
    }

    /// 值域事实的消费：开了 `R_ZBITS` 时「掩码后的 sext」可删；没开不得删；
    /// 而 32 位写给出的 `zbits = 32` **永远不得**拿来做这条判断（反例：
    /// `int i = -1; long l = i;` 的 `movsxd` 必须保留）。
    #[test]
    fn zbits_enables_the_sext_identity_only_below_32_bits() {
        let movsxd = |d: u8| {
            Insn::mov_ext(
                true,
                crate::codegen::shared::asm_operand::Width::B32,
                crate::codegen::shared::asm_operand::Width::B64,
                d,
                d,
            )
        };
        let mut st = BlockState::default();
        st.zbits[3] = 8;
        let r = R_SEXT | R_COPY | R_SLOT;
        assert!(
            matches!(st.decide(&movsxd(3), &nv(), r | R_ZBITS), Verdict::Drop),
            "掩码后值 < 2^8，符号扩展是恒等，开了 R_ZBITS 应当能删"
        );
        assert!(
            matches!(st.decide(&movsxd(3), &nv(), r), Verdict::Keep),
            "R_ZBITS 未开时不得拿 zbits 做判断（旧开关必须零漂移）"
        );
        st.zbits[4] = 32;
        assert!(
            matches!(st.decide(&movsxd(4), &nv(), r | R_ZBITS), Verdict::Keep),
            "zbits=32 只保证高 32 位为零，bit31 可能为 1：这条 sext 不能删"
        );
    }

    // ---------------------------------------------------------------------
    // S1：指令事实层（方案 §3 S1 / §8.2 的「39 个构造子零 Unknown」判据）
    // ---------------------------------------------------------------------

    /// 拿构造子的**真实字节**问解码器，而不是问手写字节串。
    ///
    /// 为什么必须是构造子：事实层要服务的是发射器实际发出去的东西。手写
    /// `&[0x48, 0x83, 0xEC, …]` 只能证明解码器认得理想形态，而构造子会自己决定
    /// 带不带 REX、把 disp 编成 mod=01 还是 mod=10、要不要 0x66 前缀——本仓
    /// 已经有两次「解码器与构造子的形状不一致」的记录（立即数 ALU 的目的字段拿反、
    /// `B8+r` 的号编在 opcode 而不是 modrm）。
    fn decoded_ctor(name: &str, insn: &Insn) -> (String, bool) {
        let bytes = insn
            .clone()
            .encode_bytes()
            .unwrap_or_else(|| panic!("{name} 构造子编不出字节，本用例无法代理它"));
        let ok = !matches!(raw_writes(&bytes), RawWrite::Unknown);
        (hex(&bytes), ok)
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
    }

    /// 每一个产 `Raw` 的公开构造子都必须被解码器认识。
    ///
    /// 两个**故意保留 `Unknown`** 的形态写在下面的 `still_unknown` 里，各自都有理由：
    /// 把它们报成「认得」就是拿一条错事实去削指令，那不是补全而是造假。
    #[test]
    fn raw_writes_decodes_every_raw_constructor() {
        use crate::codegen::shared::asm_operand::Width as W;
        // (名字, 指令)。参数取「本后端真会发的形态」：号 0/3/12/13 覆盖低号、callee-saved
        // 与帧基址 r13；位移 -64 是 disp8、512 是 disp32，两形都要过。
        let ctors: Vec<(&str, Insn)> = vec![
            ("seg_prefix", Insn::seg_prefix(0x64)),
            ("sub_rsp_imm", Insn::sub_rsp_imm(-128)),
            ("add_rsp_imm", Insn::add_rsp_imm(120)),
            ("add_rsp_imm_gas", Insn::add_rsp_imm_gas(64, "add $64, %rsp")),
            ("sub_rsp_imm_gas", Insn::sub_rsp_imm_gas(-32, "sub $32, %rsp")),
            ("alu_rm_reg_imm8", Insn::alu_rm_reg_imm8(0, 0, 8)),
            ("alu_rm_reg_imm8_b64", Insn::alu_rm_reg_imm8_b64(5, 3, -8)),
            ("alu_rm_reg_imm32_b64", Insn::alu_rm_reg_imm32_b64(0, 12, 70000)),
            ("cmp_eax_imm32", Insn::cmp_eax_imm32(-5)),
            ("mov_eax_imm32", Insn::mov_eax_imm32(0xE6)),
            ("mov_ecx_imm32", Insn::mov_ecx_imm32(4)),
            ("and_eax_imm32", Insn::and_eax_imm32(0xFFF)),
            ("alu_mem_imm8_b32", Insn::alu_mem_imm8_b32(0, 13, -64, 1)),
            ("alu_mem_imm8_b32/disp32", Insn::alu_mem_imm8_b32(4, 3, 512, 1)),
            ("alu_mem_imm32_b64", Insn::alu_mem_imm32_b64(0, 13, -64, 70000)),
            ("store_imm32_mem", Insn::store_imm32_mem(13, -64, 7)),
            ("store_imm32_mem/disp32", Insn::store_imm32_mem(3, 512, 7)),
            ("store_imm_mem/B8", Insn::store_imm_mem(W::B8, 13, -8, 1)),
            ("store_imm_mem/B16", Insn::store_imm_mem(W::B16, 13, -8, 1)),
            ("store_imm_mem/B32", Insn::store_imm_mem(W::B32, 13, -8, 1)),
            ("store_imm_mem/B64", Insn::store_imm_mem(W::B64, 3, 512, -1)),
            ("shift_reg_imm8_c1_b64", Insn::shift_reg_imm8_c1_b64(4, 0, 3)),
            ("shift_reg_cl_b64", Insn::shift_reg_cl_b64(4, 9)),
            ("shift_reg_imm8_c1_b32", Insn::shift_reg_imm8_c1_b32(7, 1, 5)),
            ("shift_reg_imm8_c1_b16", Insn::shift_reg_imm8_c1_b16(0, 2, 8)),
            ("test_rm_reg_imm32_b64", Insn::test_rm_reg_imm32_b64(0, 0xFF)),
            ("movabs_reg_imm64", Insn::movabs_reg_imm64(13, 0x1_2345_6789)),
            ("short_jump_rel8", Insn::short_jump_rel8(0x74, 12)),
            ("popcnt_reg_reg_b64", Insn::popcnt_reg_reg_b64(0)),
            ("movmskps_gpr_xmm", Insn::movmskps_gpr_xmm(3, 1)),
            ("bswap_reg/B64", Insn::bswap_reg(W::B64, 0)),
            ("bswap_reg/B32", Insn::bswap_reg(W::B32, 9)),
            ("rol_reg_imm8_b16", Insn::rol_reg_imm8_b16(2, 8)),
            ("mov_reg_imm32_b64", Insn::mov_reg_imm32_b64(12, 512)),
            // 体量档那个选形构造子：两臂都要在枚举里 —— 它可能发 5 字节 `B8+r id`，也可能
            // 退回 7 字节 `C7 /0 id`，而两条臂的写集答话不同（非负两者都报 `ImmSExt`，
            // 负数只有 C7 那一形报得出）。
            ("mov_reg_imm_compact/非负", Insn::mov_reg_imm_compact(12, 512)),
            ("mov_reg_imm_compact/非负低号", Insn::mov_reg_imm_compact(0, 0)),
            ("mov_reg_imm_compact/负", Insn::mov_reg_imm_compact(3, -7)),
            ("alu_acc_imm32_b64_acc", Insn::alu_acc_imm32_b64_acc(0x05, 70000)),
            ("alu_acc_imm32_b32_acc", Insn::alu_acc_imm32_b32_acc(0x25, 0xFFF)),
            ("alu_acc_imm16_b16_acc", Insn::alu_acc_imm16_b16_acc(0x0D, 5)),
            (
                "alu_acc_imm32_b64_acc_gas",
                Insn::alu_acc_imm32_b64_acc_gas(0x33, 1, "xor $1, %eax"),
            ),
            ("cmovcc_reg_reg_b64", Insn::cmovcc_reg_reg_b64(0x44, 0, 3)),
            ("alu_rm_reg_imm32_b64_full", Insn::alu_rm_reg_imm32_b64_full(0, 0, 70000)),
            ("sign_extend_rax_to_rdx/B64", Insn::sign_extend_rax_to_rdx(W::B64)),
            ("sign_extend_rax_to_rdx/B32", Insn::sign_extend_rax_to_rdx(W::B32)),
            ("sign_extend_rax_to_rdx/B16", Insn::sign_extend_rax_to_rdx(W::B16)),
            ("imul_rm_reg_imm32_b64", Insn::imul_rm_reg_imm32_b64(1, 8)),
            ("idiv_rm_reg_b64", Insn::idiv_rm_reg_b64(3)),
            ("div_rm_reg_b64", Insn::div_rm_reg_b64(1)),
        ];
        let mut bad: Vec<String> = Vec::new();
        for (name, insn) in &ctors {
            let (bytes, ok) = decoded_ctor(name, insn);
            if !ok {
                bad.push(format!("{name} [{bytes}]"));
            }
        }
        assert!(bad.is_empty(), "有 {} 个 raw 构造子的写集解不出来：\n{}", bad.len(), bad.join("\n"));
    }

    /// 两个必须维持 `Unknown` 的形状，写成断言而不是靠注释：它们一旦变成「认得」，
    /// 就是在拿一条看不见的写集削指令。
    #[test]
    fn prefix_fragments_stay_unknown_or_mean_nothing() {
        // `mov_ext_prefix_no_modrm` 只发 `REX 0F B6` 三字节，modrm 与访存形式在**下一条**
        // 指令的字节里。这一条自己无法定出目的号 ⇒ 必须 `Unknown`。
        let frag = Insn::mov_ext_prefix_no_modrm(0x48, 0xB6);
        let b = frag.encode_bytes().expect("前缀碎片要能编出来");
        assert!(
            matches!(raw_writes(&b), RawWrite::Unknown),
            "modrm 在下一条字节里的前缀碎片不能报成认得：[{}]", hex(&b)
        );
        // 单独的段前缀不写任何号（真正那条指令自己会被 observe）⇒ 是 `Nothing` 不是 `Unknown`。
        let seg = Insn::seg_prefix(0x65);
        let sb = seg.encode_bytes().expect("段前缀要能编出来");
        assert!(
            matches!(raw_writes(&sb), RawWrite::Nothing),
            "段前缀不是「认不得的写集」：[{}]", hex(&sb)
        );
    }

    /// 每一条新臂都要能举出它**不会**多claim 的反例：宽度不到 32 位的形不能推值域。
    #[test]
    fn narrow_forms_do_not_claim_zero_extension() {
        // `66 B8 iw`：`mov ax, imm16` 只写低 16 位，高 48 位保留。
        let mov_ax = Insn::alu_acc_imm16_b16_acc(0x0D, 5);
        let b = mov_ax.encode_bytes().unwrap();
        assert!(
            matches!(raw_writes(&b), RawWrite::Reg(0)),
            "16 位累加器形只能报「写 rax」，不能报值域：[{}]", hex(&b)
        );
        // `48 B8+r id`（movabs 的 imm32 形）写满 64 位，同样没有值域可claim。
        let movabs = Insn::movabs_reg_imm64(1, 0x7FFF_FFFF);
        let mb = movabs.encode_bytes().unwrap();
        assert!(matches!(raw_writes(&mb), RawWrite::Reg(1)), "REX.W 的 B8+r 应当只报写号");
        // 真正的 32 位形才有值域。S3c 体量交换后，**非负** imm32 还能多报一档：
        // 零扩展与符号扩展同值 ⇒ `ImmSExt`（携带那个值）。负 imm32 仍只能报
        // 零扩展上界，因为它的 64 位值不等于符号扩展。
        let eax = Insn::mov_eax_imm32(0x1234);
        let eb = eax.encode_bytes().unwrap();
        assert!(
            matches!(raw_writes(&eb), RawWrite::ImmSExt { dst: 0, imm: 0x1234 }),
            "非负 imm32 的 32 位形应报 ImmSExt：[{}]",
            hex(&eb)
        );
        let eax_neg = Insn::mov_eax_imm32(0x8000_0000);
        let nb = eax_neg.encode_bytes().unwrap();
        assert!(
            matches!(raw_writes(&nb), RawWrite::Masked { dst: 0, bits: 32 }),
            "bit31 置高时不能报符号扩展（那会把紧随其后的 movsxd 削成错值）：[{}]",
            hex(&nb)
        );
    }

    /// 方案 §3 S1 点名要补的那几条具体形态（rep movs / %cl / cpuid / syscall /
    /// xgetbv / div / cqto / 下标序列），逐条钉住写集。
    #[test]
    fn named_forms_from_the_plan_are_decoded() {
        // `rep movsb`（F3 A4）：写 rsi/rdi/rcx 并且碰内存。
        assert!(matches!(raw_writes(&[0xF3, 0xA4]), RawWrite::Strs));
        // `cpuid`（0F A2）：四个号。
        assert!(matches!(raw_writes(&[0x0F, 0xA2]), RawWrite::Regs(m) if m == (1 | 2 | 4 | 8)));
        // `syscall`（0F 05）：按 call 处置。
        assert!(matches!(raw_writes(&[0x0F, 0x05]), RawWrite::Call));
        // `xgetbv`（0F 01 D0）：rax/rdx。
        assert!(matches!(raw_writes(&[0x0F, 0x01, 0xD0]), RawWrite::Regs(m) if m == (1 | 4)));
        // `cqto`（48 99）：只写 rdx。
        assert!(matches!(raw_writes(&[0x48, 0x99]), RawWrite::Reg(2)));
        // `idiv r/m64`（48 F7 /7）：rax+rdx，而且不碰内存。
        assert!(matches!(raw_writes(&[0x48, 0xF7, 0xF8 | 3]), RawWrite::Regs(m) if m == (1 | 4)));
        // 下标序列 `imul rcx, rdx`（48 0F AF C9）：目的在 reg 字段 = rcx。
        assert!(matches!(raw_writes(&[0x48, 0x0F, 0xAF, 0xC1 | (1 << 3)]), RawWrite::Reg(1)));
        // `mov rdx, [r13-64]`（48 8B 55 C0）：目的在 reg 字段，读帧存不建事实。
        assert!(matches!(raw_writes(&[0x48, 0x8B, 0x55, 0xC0]), RawWrite::Reg(2)));
        // `mov [r13-64], rdx`（49 89 55 C0）：一次真存槽，宽 8 字节，源号 2。
        // REX.B 那一位不能省：rm=5 带 REX.B 才是 r13，不带就是 rbp。
        assert!(matches!(
            raw_writes(&[0x49, 0x89, 0x55, 0xC0]),
            RawWrite::SlotStore { disp: -64, len: 8, src: Some(2) }
        ));
        // `movsd xmm0, [rbp-64]`（F2 0F 10 45 C0）：不写任何 GP 号。
        assert!(matches!(raw_writes(&[0xF2, 0x0F, 0x10, 0x45, 0xC0]), RawWrite::Nothing));
        // `movsd [r13-64], xmm0`（F2 41 0F 11 45 C0）：只失效那一个槽（宽 8）。
        assert!(matches!(
            raw_writes(&[0xF2, 0x41, 0x0F, 0x11, 0x45, 0xC0]),
            RawWrite::SlotStore { disp: -64, len: 8, src: None }
        ));
        // `jmp rel32`（E9）/ `ret`（C3）：终结而不是 Unknown。
        assert!(matches!(raw_writes(&[0xE9, 0x00, 0x00, 0x00, 0x00]), RawWrite::Terminate));
        assert!(matches!(raw_writes(&[0xC3]), RawWrite::Terminate));
        // `call rel32`（E8）：分流而不是全杀。
        assert!(matches!(raw_writes(&[0xE8, 0x00, 0x00, 0x00, 0x00]), RawWrite::Call));
        // `jcc rel8`（74）与 `jcc rel32`（0F 84）：落空边保留事实。
        assert!(matches!(raw_writes(&[0x74, 0x08]), RawWrite::Nothing));
        assert!(matches!(raw_writes(&[0x0F, 0x84, 0x00, 0x00, 0x00, 0x00]), RawWrite::Nothing));
        // `add eax, imm32`（05 id）：只写 rax。
        assert!(matches!(raw_writes(&[0x05, 0x01, 0x00, 0x00, 0x00]), RawWrite::Masked { dst: 0, bits: 32 }));
        assert!(matches!(raw_writes(&[0x48, 0x05, 0x01, 0x00, 0x00, 0x00]), RawWrite::Reg(0)));
    }

    /// A1 的门控必须做到「门关逐字节全等」：同一个 `Call` 在 `R_CALLSLOT` 关时
    /// 与今天完全一致（整表全杀），开时才只杀 caller-saved。
    #[test]
    fn a1_call_split_is_gated_by_its_own_bit() {
        let call = Insn::new(
            X86_64Opcode::Call {
                target: O::Imm(0),
                pending_rel: false,
            },
            &[],
        );
        let mut st = BlockState::default();
        st.copy[3] = 12; // rbx == r12（callee-saved 对 callee-saved）
        st.note_slot(-64, 8, 3);
        // 门关：与今天相同 ⇒ 整表全杀，rbx 的搬运事实与槽事实都走。
        let mut off = st;
        off.observe(&call, &nv());
        assert_eq!(off.copy[3], NO_REG, "默认位图下 Call 必须照旧全杀");
        assert_eq!(off.slot_of(-64, 8), None, "默认位图下槽事实必须照旧全弃");
        // 门开（但护栏说不允许跳 call 保留）：槽仍要弃，callee-saved 的搬运事实可以留。
        let a1 = R_SEXT | R_COPY | R_SLOT | R_RAW | R_CALLSLOT;
        let mut on = st;
        on.observe_at_call(&call, &nv(), CallPolicy { slots_survive: false, must_clear: empty_marks() }, a1);
        assert_eq!(on.copy[3], 12, "rbx/r12 都是 callee-saved，call 不改它们的值");
        assert_eq!(on.slot_of(-64, 8), None, "护栏不允许时槽事实必须弃");
        // 门开 + 护栏允许：两边都留。
        let mut ok = st;
        ok.observe_at_call(&call, &nv(), CallPolicy { slots_survive: true, must_clear: empty_marks() }, a1);
        assert_eq!(ok.slot_of(-64, 8), Some(3), "护栏允许时槽事实可以活过 call");
    }

    /// caller-saved 号上的事实必须被 call 杀掉，不管护栏怎么说不允许。
    /// 否则 A1 会把「rax 等于某个槽」这条已经作废的事实留在表里。
    #[test]
    fn a1_still_kills_caller_saved_facts() {
        let call = Insn::new(
            X86_64Opcode::Call {
                target: O::Imm(0),
                pending_rel: false,
            },
            &[],
        );
        let a1 = R_SEXT | R_COPY | R_SLOT | R_RAW | R_CALLSLOT;
        let mut st = BlockState::default();
        st.note_slot(-64, 8, 0); // [r13-64] == rax
        st.copy[3] = 0; // rbx == rax
        st.observe_at_call(&call, &nv(), CallPolicy { slots_survive: true, must_clear: empty_marks() }, a1);
        assert_eq!(st.slot_of(-64, 8), None, "rax 是 caller-saved，它代表的槽事实必须死");
        assert_eq!(st.copy[3], NO_REG, "「rbx == rax」在 call 后不再成立");
        assert_eq!(st.copy[0], NO_REG, "rax 自己的相等事实也必须死");
    }

    /// A2 取交那一臂的单位判据：两条前驱边上都成立的事实才留，只成立一条的必须丢。
    ///
    /// 红法：把 `intersect_with` 改成「直接拿 `other`」或「不交」⇒ 下面两个断言之一变红
    /// （前者会把只在一条边上成立的事实当全局事实用，后者会让取交那一臂永远是整体清）。
    #[test]
    fn intersect_keeps_only_facts_true_on_both_edges() {
        use crate::codegen::shared::asm_operand::Width as W;
        let rules = R_SEXT | R_COPY | R_SLOT | R_RAW;
        // 边 1：-64 槽 == r12，且 rbx == r12。
        let mut e1 = BlockState::default();
        e1.observe_at_call(&insn_mov(mem(-64), O::Reg(12, W::B64), W::B64), &nv(), CallPolicy::no_relax(), rules);
        e1.observe_at_call(&insn_mov(O::Reg(3, W::B64), O::Reg(12, W::B64), W::B64), &nv(), CallPolicy::no_relax(), rules);
        // 边 2：-64 槽 == r12（同一条），-72 槽 == r14（只在边 2 上），没有 rbx 的相等事实。
        let mut e2 = BlockState::default();
        e2.observe_at_call(&insn_mov(mem(-64), O::Reg(12, W::B64), W::B64), &nv(), CallPolicy::no_relax(), rules);
        e2.observe_at_call(&insn_mov(mem(-72), O::Reg(14, W::B64), W::B64), &nv(), CallPolicy::no_relax(), rules);

        let mut j = e1;
        j.intersect_with(&e2, false);
        assert_eq!(j.slot_of(-64, 8), Some(12), "两条边上都有的槽事实必须保留（这就是取交买的东）");
        assert_eq!(j.slot_of(-72, 8), None, "只在一条边上成立的事实必须丢");
        assert_eq!(j.copy[3], NO_REG, "「rbx == r12」在另一条边上不成立 ⇒ 必须丢");
        assert_eq!(j.slot_of(-64, 4), Some(12), "同一条事实的窄读仍然可用");
    }

    /// 取交必须把可信宽度收到窄的一边：两条边上一条写 8 字节、一条写 4 字节时，
    /// 交汇点后只能按 4 字节服务。不收到窄边就是拿高 4 位的陈值当有效位（错值）。
    #[test]
    fn intersect_narrows_the_slot_to_the_smaller_side() {
        use crate::codegen::shared::asm_operand::Width as W;
        let rules = R_SEXT | R_COPY | R_SLOT | R_RAW;
        let mut wide = BlockState::default();
        wide.observe_at_call(&insn_mov(mem(-64), O::Reg(12, W::B64), W::B64), &nv(), CallPolicy::no_relax(), rules);
        let mut narrow = BlockState::default();
        narrow.observe_at_call(&insn_mov(mem(-64), O::Reg(12, W::B32), W::B32), &nv(), CallPolicy::no_relax(), rules);
        wide.intersect_with(&narrow, false);
        assert_eq!(wide.slot_of(-64, 8), None, "8 字节读在「只写了 4 字节」那条边上是陈值");
        assert_eq!(wide.slot_of(-64, 4), Some(12), "4 字节以内两边都成立");
    }

    /// S3c「立即数 `mov` 复用」（`R_DEADW`）：同一个号被同一个常量重复写时，第二条是死写。
    ///
    /// 为什么与 `dead_pc` 那条窥孔不重叠：那个删的是**上一条**（要求它就在字节尾），
    /// 本条删的是**当前这条** —— 所它在 `decide` 一层就能做，不需回删任何已发字节。
    #[test]
    fn same_constant_rewritten_to_same_register_is_dropped() {
        use crate::codegen::shared::asm_operand::Width as W;
        let deadw = R_SEXT | R_COPY | R_SLOT | R_RAW | R_ZBITS | R_IMMSEXT | R_DEADW;
        let w0 = insn_mov(O::Reg(0, W::B64), O::Imm(0), W::B64);
        let w7 = insn_mov(O::Reg(0, W::B64), O::Imm(7), W::B64);
        let r0 = insn_mov(O::Reg(3, W::B64), O::Imm(0), W::B64);

        let mut st = BlockState::default();
        st.observe_at_call(&w0, &nv(), CallPolicy::no_relax(), deadw);
        // 同一个号再写同一个值 ⇒ 整条不发。
        assert!(matches!(st.decide(&w0, &nv(), deadw), Verdict::Drop));
        // 换值写就不是死写（删了就丢那次赋值）。
        assert!(matches!(st.decide(&w7, &nv(), deadw), Verdict::Keep));
        // 同一个值写到**另一个号**上也不是（两条事实不能混为一谈）。
        assert!(matches!(st.decide(&r0, &nv(), deadw), Verdict::Keep));
        // 开着位但值已被普通算术打掌 ⇒ 不能当已知常量。
        let mut st2 = BlockState::default();
        st2.observe_at_call(&w0, &nv(), CallPolicy::no_relax(), deadw);
        st2.observe_at_call(
            &insn_mov(O::Reg(0, W::B64), O::Reg(3, W::B64), W::B64),
            &nv(),
            CallPolicy::no_relax(),
            deadw,
        );
        assert!(matches!(st2.decide(&w0, &nv(), deadw), Verdict::Keep));
        // 反向对照：关掉那一位，重复写也不得被削（否则默认档会静默吃到本臂的行为）。
        let mut st3 = BlockState::default();
        st3.observe_at_call(&w0, &nv(), CallPolicy::no_relax(), deadw);
        assert!(
            matches!(st3.decide(&w0, &nv(), deadw & !R_DEADW), Verdict::Keep),
            "R_DEADW 关着却仍在削常量重写 ⇒ 那道门没作用到判据上"
        );
    }

    /// 常量知识必须能**跟着 `mov r,r` 传**，也必须被 call 打掉（caller-saved 那几个）。
    #[test]
    fn const_knowledge_propagates_through_moves_and_dies_on_calls() {
        use crate::codegen::shared::asm_operand::Width as W;
        let rules = R_SEXT | R_COPY | R_SLOT | R_RAW | R_ZBITS | R_IMMSEXT | R_DEADW;
        let w0 = insn_mov(O::Reg(0, W::B64), O::Imm(0), W::B64);
        let cp = insn_mov(O::Reg(12, W::B64), O::Reg(0, W::B64), W::B64);
        let w12 = insn_mov(O::Reg(12, W::B64), O::Imm(0), W::B64);
        let call = Insn::new(
            X86_64Opcode::Call {
                target: O::Imm(0),
                pending_rel: false,
            },
            &[],
        );

        // `mov $0,%rax; mov %rax,%r12; mov $0,%r12` ⇒ 第三条是死写。
        let mut st = BlockState::default();
        st.observe_at_call(&w0, &nv(), CallPolicy::no_relax(), rules);
        st.observe_at_call(&cp, &nv(), CallPolicy::no_relax(), rules);
        assert!(
            matches!(st.decide(&w12, &nv(), rules), Verdict::Drop),
            "常量知识没能跟着 64 位抄手传过去"
        );
        // call 在两条通道上的处置不同，而本臂的可行域就跟着它走：
        //   * `R_CALLSLOT` 关（默认）⇒ call 走 `kill_all`，**全部**事实一起没，r12 的
        //     常量也保不住 ⇒ 重复写不得被削；
        //   * `R_CALLSLOT` 开 ⇒ 只杀 caller-saved 那几个，callee-saved 的 r12 保留 ⇒ 能削。
        let mut st2 = BlockState::default();
        st2.observe_at_call(&w12, &nv(), CallPolicy::no_relax(), rules);
        st2.observe_at_call(&call, &nv(), CallPolicy::no_relax(), rules);
        assert!(
            matches!(st2.decide(&w12, &nv(), rules), Verdict::Keep),
            "默认通道上 call 仍是一律全杀（R_CALLSLOT 没开），此时不应把 callee-saved 的常量当活着"
        );
        let mut st4 = BlockState::default();
        let with_a1 = rules | R_CALLSLOT;
        st4.observe_at_call(&w12, &nv(), CallPolicy::no_relax(), with_a1);
        st4.observe_at_call(&call, &nv(), CallPolicy::no_relax(), with_a1);
        assert!(
            matches!(st4.decide(&w12, &nv(), with_a1), Verdict::Drop),
            "开了 R_CALLSLOT 后 r12 是 callee-saved，它的已知常量应该活过 call"
        );
        // 同理但放在 caller-saved 的 rax 上：call 之后值不再是那个常量。
        let mut st3 = BlockState::default();
        st3.observe_at_call(&w0, &nv(), CallPolicy::no_relax(), rules);
        st3.observe_at_call(&call, &nv(), CallPolicy::no_relax(), rules);
        assert!(
            matches!(st3.decide(&w0, &nv(), rules), Verdict::Keep),
            "rax 上的常量知识活不过 call ⇒ 不得把重复写削掉"
        );
    }

    /// `raw_frame` 的三条主干：寄存器形不碰内存；帧存出报出位移与方向；非帧基址
    /// 与「说不清」必须分开。区域家（S4）的读写集合完全建立在这三条上。
    #[test]
    fn raw_frame_separates_reg_mov_frame_slot_and_foreign_base() {
        // `mov %rbx, %rax`（48 8B C3）：mod=11，一个内存操作数也没有。
        assert_eq!(raw_frame(&[0x48, 0x8B, 0xC3]), RawFrame::NoMem, "寄存器搬运不得被当成「可能碰帧」");
        // `mov %rbx, -64(%r13)`（49 89 5D C0：rm=5 必须带 REX.B 才是 r13）：一次 8 字节的帧存出。
        match raw_frame(&[0x49, 0x89, 0x5D, 0xC0]) {
            RawFrame::Slot { disp, len, dir } => {
                assert_eq!((disp, len), (-64, 8), "位移与宽度必须解对：错了就是战为别人家");
                assert!(dir.writes() && !dir.reads(), "mov r→m 是纯写");
            },
            other => panic!("帧存出必须解成 Slot，实际 {other:?}"),
        }
        // `mov -64(%r13), %rax`（49 8B 45 C0）：同位移的一次读。
        match raw_frame(&[0x49, 0x8B, 0x45, 0xC0]) {
            RawFrame::Slot { disp, len, dir } => {
                assert_eq!((disp, len), (-64, 8));
                assert!(dir.reads() && !dir.writes(), "mov m→r 是纯读");
            },
            other => panic!("帧载入必须解成 Slot，实际 {other:?}"),
        }
        // `cmp %rax, (%rdi)`（48 39 07）：基址不是 r13 ⇒ 与本帧无关。
        assert_eq!(raw_frame(&[0x48, 0x39, 0x07]), RawFrame::Foreign, "非帧基址不得当成「可能碰帧」而拆家");
        // `mov (%r13,%rax,8), %rdx`（49 8B 54 C5 00）：SIB 带变址且基址是 r13 ⇒ 说不清。
        assert_eq!(raw_frame(&[0x49, 0x8B, 0x54, 0xC5, 0x00]), RawFrame::Opaque, "下标序列访问的是整个对象，不能只算一个槽");
        // `0F B6 05 …`（movzx 双字节表）：本函数不解 ⇒ 必须报说不清而不是报 Foreign。
        assert_eq!(
            raw_frame(&[0x0F, 0xB6, 0x05, 0x00, 0x00, 0x00, 0x00]),
            RawFrame::Opaque,
            "认不得的族只能 Opaque：报 NoMem/Foreign 会把陈值当新值用"
        );
    }

    /// `raw_frame` 的方向表：ALU 立即数形的 `/7`（cmp）只读，其余读改写；
    /// `f6/f7` 的 test 只读、not/neg 读改写、mul/div 只读。方向错一处就是
    /// 「本层以为那个槽没被写」而拿着陈值继续服务。
    #[test]
    fn raw_frame_direction_table_matches_the_alu_digits() {
        let digits = |op0: u8, reg: u8| -> MemDir {
            // REX.W+REX.B + op + modrm(mod=01, rm=5⇒r13, reg=ALU digit 或目的号) + disp8。
            let modrm = (reg << 3) | 0x45;
            let bytes = [0x49, op0, modrm, 0xF8u8];
            match raw_frame(&bytes) {
                RawFrame::Slot { dir, disp, len } => {
                    assert_eq!(disp, -8, "位移解错：{bytes:02x?} len={len}");
                    dir
                },
                other => panic!("这一族必须解成帧槽：{other:?} bytes={bytes:02x?}"),
            }
        };
        assert_eq!(digits(0x81, 7), MemDir::Read, "add/or/adc/sbb/and/sub/xor/cmp 的 /7 是 cmp，只读");
        assert_eq!(digits(0x81, 0), MemDir::ReadWrite, "add [槽],imm 是读改写");
        assert_eq!(digits(0x83, 5), MemDir::ReadWrite, "sub [槽],imm8 是读改写");
        assert_eq!(digits(0xF7, 0), MemDir::Read, "test [槽],imm 只读");
        assert_eq!(digits(0xF7, 3), MemDir::ReadWrite, "neg [槽] 读改写");
        assert_eq!(digits(0xF7, 6), MemDir::Read, "div [槽] 只读那块内存（商写 rax）");
        assert_eq!(digits(0xC7, 0), MemDir::Write, "mov q [槽],imm32 是纯写");
        assert_eq!(digits(0xC1, 0), MemDir::ReadWrite, "shl [槽],imm8 是读改写");
    }
}
