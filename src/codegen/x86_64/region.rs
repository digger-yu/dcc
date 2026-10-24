// src/codegen/x86_64/region.rs — 区内家（L2 家模型，方案 §3 S4）
//
// 双层家模型的另一半：L1 是「全函数家」（`ssa/regalloc.rs` 的 `POOL`，实测四个号只买回
// 7%），L2 是本文件的「区内家」—— 一个值在**本区内**住进一个号，区内对该槽的读都走那个号。
//
// 它与 `blockopt` 的帧槽转发不是同一件事，容易被混为一谈：
//   * `blockopt` 的转发是**机会主义**的：某条指令刚好把值写进过那个槽，之后同槽的读才可能
//     被换成寄存器搬运。它活在事实表里，一次 `kill_all`、一个标签就没了。
//   * 区内家是**承诺**：家是一个具体的寄存器号，只要那个号没被写过、那个槽没被以本层认不得
//     的方式碰过，家就一直有效 —— 它能活过 `blockopt` 认不得的 raw 指令（Gate 0b 量出的那桶）。
//
// ## 为什么本版**不**推迟存出（方案 §3 S4 原文的「区尾 1 存出」那一半）
//
// 上一版按字面做了推迟存出，两处结构性错误：
//
// 1. **一条指令的字节槽不能同时充当「填家」与「区尾存出」**。把提升那条存出就地换成
//    `mov 源,家`（填家），区尾再把它改回 `mov [槽],家`（回写），就等于那次填家从未发生：
//    家那个号里是调用方留下的垃圾（实测 `percpu_offset.c` 的 SIGSEGV）。
// 2. **回写不能写在比「上一次修改」更早的位置上**。保留区若停留在第一次提升那里，而家
//    在区间内又被第二次转换改过，写回内存的就是错值。
//
// 修好这两条之后（每次转换先回写上一处、再把保留区前移到本条），剩下的结论是硬的：
// **推迟一次存出要么仍然留一次存出（条数与访存都不省，还要多垫 `nop`），要么必须在区出口
// 插一条存出** —— 而「在 `emit_insn` 中间插字节」这条路本轮逐点改到第 12 处仍不收敛
// （`.text` 的补丁台账有十几处形状是「发射前先取 `pos()`、发射完再登记」，插字节会让那些
// 绝对位置落到别的指令中间）。既然推迟存出在零插入下不省任何东西，它就只剩代价，本模块
// 因此**只做读侧服务**。这也是 §5 止损②（「密度 3 轮未降 10% ⇒ 停区域家」）能直接用读数
// 判掉的形状原因。
//
// 由此得到一个对 §4.4 的重要后果：**本层从不让内存落后于家**（存出照常发）。所以带 `-g`
// 时本层不需要 loclist 描述任何区间 —— 内存永远是权威，单 `DW_AT_location` 就是正确答案。
// loclist（`debug.rs` 的 `LocSink`）今天的生产者是延伸池家在 call 序列里的那段窗口
// （`Codegen::debug_loc_windows`），与本层无关；本层任何时候都不会往那张表里写东西。
//
// ## 四条硬条件（缺一即静默错值）
//   1. **家号不能被踩**。区内任何一条指令写了家的号，家必须丢掉 —— 判据就是
//      `blockopt::written_gp`（那个函数存在的首要理由在此）。
//   2. **证明不了不相交的帧访问必须打破家**。能定形的只有 `[r13+disp]` 那一种区间；
//      SIB 下标、`0F`/x87 双字节族、认不得的形态一律按「可能碰到任何槽」丢家。
//   3. **地址逃逸的槽不能当家**。取过址的局部可能被经指针写回，那份写看不见；帧地址一旦
//      被 `lea` 导出，本函数永不再建家（`no_defer`）。非帧基址的内存访问按「碰不到本帧的
//      槽」处理，其前提就是这一条与 `FrameGuard` 的函数级护栏。
//   4. **区的边界必须与 CFG 一致**：`label()`（汇合点）、任何跳转（含 `jcc` 的两条边）、
//      `goto *表[…]`、函数尾声 —— 全部先丢家再动作。丢家不发任何字节，所以边界处不存在
//      「字节插到哪」的问题。
//
// 反例守卫（方案 §3 S4 明文要求）：读多写少、且覆盖很多个区的槽**永不**降级成区内家，
// 否则「1 载入」变成「N 载入 + N 存出」。守卫是 `heat`：一个槽被提升过的区数到达
// `REGION_HEAT_CAP` 就进 `blacklisted`，本函数剩余部分永不再提升它。
//
// 旋钮 `DCC_REGION_HOME`：默认关。开与关的产物字节必须逐字相同是硬要求，由
// `region_home_is_gated_off_and_live_when_on` 与门关的 5454 组合一起守。
// `DCC_REGION_CALLEE=0` 关掉「家号必须 callee-saved」那道门，专用于重现「任意号建家 ⇒
// 服务为零」那个读数（见报告 §6.16.1）。

use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::blockopt::{MemDir, RawFrame, RegWrites, raw_frame, written_gp};
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{MemOperand, Operand as O, X86_64Opcode};
use std::collections::{HashMap, HashSet};

/// 一个住在寄存器里的槽家。
#[derive(Clone, Copy, Debug)]
pub struct RegionHome {
    /// 住进的那个号 —— **它就是建立点那条指令自己的目的号**（存出的源 / 载入的目的地），
    /// 所以建家不占任何字节，也不需要分配器参与。
    pub reg: u8,
    /// 帧内位移（`[r13+slot]` 的 slot）
    pub slot: i64,
    /// 家里只有低 `vlen` 字节与槽一致。载入建立的家只有载入宽度那几字节可信
    /// （`mov -80(%r13),%eax` 之后 `%rax` 的高 32 位是零扩展，不是内存里那 4 字节）。
    pub vlen: u8,
}

impl RegionHome {
    /// 一次宽度为 `w` 的访问能不能由这个家直接服务。
    fn serves(&self, w: u8) -> bool {
        w != 0 && w <= self.vlen && self.vlen <= 8
    }
}

/// 覆盖多少个区之后就永久黑名单一个槽（方案 §3 S4 的「≥70 个区」）。
pub const REGION_HEAT_CAP: u32 = 70;

/// 区内家状态。逐函数重建（`region_begin_func`），逐区推进（`region_at_label`）。
#[derive(Clone, Debug, Default)]
pub struct RegionState {
    /// 当前区的编号（单调递增，只给统计与 by_label 用）
    pub id: u64,
    /// 标签名 → 该标签**之后**所在的区号。出口判定与调试出口都读它。
    pub by_label: HashMap<String, u64>,
    /// 槽位移 → 家。同一区内一个槽最多一个家。
    pub homes: HashMap<i64, RegionHome>,
    /// 槽位移 → 本函数内它被提升过多少个区（反例守卫的计数）。
    pub heat: HashMap<i64, u32>,
    /// 区内还活着的家数（= `homes.len()`，单列出来给统计与仪表用）。
    pub live: u32,
    /// 本函数允许开工（旋钮开 + 函数级硬互斥都不成立）。
    pub allowed: bool,
    /// 被反例守卫拉黑的槽：本函数剩余部分永不再提升。
    pub blacklisted: HashSet<i64>,
    /// 帧地址被 `lea` 导出过 ⇒ 之后可能有一次看不见的写透过指针落到某个槽上，
    /// 本函数此后再不建家（条件 3）。
    pub no_defer: bool,
    pub stats: RegionStats,
}

/// 机制读数（`DCC_REGION_STAT=1` 逐函数打印）。静态条数只作机制检查值，不当墙钟。
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionStats {
    pub promoted: u64,
    pub loads_saved: u64,
    pub drops_clobber: u64,
    pub drops_exit: u64,
    pub drops_opaque: u64,
    pub drops_store: u64,
    pub blacklisted_slots: u64,
    /// 提升被拒的理由分开计：「有但主人查不到」「有但主人取过址」「有但被 volatile /
    /// 地址导出抟住」「那个号是 caller-saved」是四种完全不同的修法的入口。合成一个计数
    /// 就读不出区别（本轮就是靠这几个读数把「机制未命中」与「护栏过宽」分开的）。
    pub refuse_volatile: u64,
    pub refuse_no_owner: u64,
    pub refuse_escaped: u64,
    pub refuse_addr_export: u64,
    pub refuse_caller_saved: u64,
}

/// 见证层对一条指令的处置。
#[derive(Debug)]
pub enum RegionVerdict {
    /// 照原样发。
    Keep,
    /// 整条不发（目的号本身就是家 ⇒ 这次读只是把值抄给自己）。
    Drop(i64),
    /// 换形态后再发（帧读 → 寄存器读）。
    Replace(Insn),
}

/// 帧基址号（与 `blockopt::FRAME` 同一个事实：r13）。
const FRAME: u8 = 13;

/// `[r13+disp]`、无 index、无段前缀时的位移；否则 `None`。
///
/// 与 `blockopt::frame_disp` 同一条判据 —— 两处必须一致，否则「转发看到的」与
/// 「家看到的」不是同一个槽集合。
fn frame_slot(m: &MemOperand) -> Option<i64> {
    if m.base != Some(FRAME) || m.index.is_some() || m.seg.is_some() {
        return None;
    }
    Some(m.disp as i64)
}

/// 两次字节区间访问是否相交。宽度 0（`Width::Bare`）按 1 算：它只会出现在认不得的
/// 形态里，保守一侧。
fn overlaps(a: i64, al: u8, b: i64, bl: u8) -> bool {
    let al = if al == 0 { 1 } else { al };
    let bl = if bl == 0 { 1 } else { bl };
    a < b + bl as i64 && b < a + al as i64
}

/// 一条指令对帧槽的一次可定形访问。
#[derive(Clone, Copy, Debug)]
struct Access {
    slot: i64,
    len: u8,
    dir: MemDir,
}

/// 一条指令对**本帧**的访问集合。
#[derive(Default, Debug)]
struct FrameUse {
    exact: Vec<Access>,
    /// 有一次说不清的帧访问（SIB/`0F`/x87/认不得的族/`lea` 导出帧地址）。
    opaque: bool,
    /// `opaque` 的那一次是不是「把帧地址算进了某个号」（`lea`）—— 它本身不访问内存，
    /// 但那个地址之后可能被用来访问内存，条件 3 从此不再成立。
    addr_escape: bool,
}

/// 宽度 → 字节数（与 `blockopt::width_bytes` 同一条映射；那里是私有函数，这里
/// 不能跨模块读，所以只作为家层自己的访问区间计算）。
fn wb(w: Width) -> u8 {
    match w {
        Width::B8 => 1,
        Width::B16 => 2,
        Width::B32 => 4,
        Width::B64 => 8,
        Width::B128 => 16,
        Width::B256 => 32,
        Width::B512 => 64,
        Width::Bare => 0,
    }
}

/// 类型化变体对帧的访问集合。`Raw` 走 `blockopt::raw_frame`（同一个头解码器）。
///
/// 穷尽 `match`、不留 `_` 臂：新增一个 `X86_64Opcode` 变体不回答「它碰不碰帧」就编不过
/// （与 `written_gp` 同一条编译钉子纪律）。列不出来的变体一律 `opaque`，方向上说「多丢一
/// 次家」而不是「拿着陈值继续」。
fn frame_use(insn: &Insn) -> FrameUse {
    use crate::codegen::x86_64::opcode::{AluOp, StackOp, UnaryOp, X86_64Opcode as Op};
    let mut out = FrameUse::default();
    // 一个操作数位置：内存且基址是帧 ⇒ 记区间；内存但基址不是帧 ⇒ 什么也不记
    // （条件 3：没逃逸的地址进不来本帧）；说不清的形态 ⇒ `opaque`。
    let note = |out: &mut FrameUse, m: &MemOperand, len: u8, dir: MemDir| {
        match frame_slot(m) {
            Some(slot) => out.exact.push(Access { slot, len, dir }),
            // 基址是帧但带 index/段前缀：区间不定 ⇒ 说不清。
            None => {
                if m.base == Some(FRAME) {
                    out.opaque = true;
                }
            },
        }
    };
    match &insn.op {
        Op::MovRm { width, dst, src, rip_relative } => {
            let w = wb(*width);
            match (dst, src) {
                (O::Mem(m), _) if *rip_relative => out.opaque = true,
                (O::Mem(m), _) => note(&mut out, m, w, MemDir::Write),
                (_, O::Mem(m)) => note(&mut out, m, w, MemDir::Read),
                _ => {},
            }
        },
        Op::MovExt { from, to, dst, src, .. } => {
            match (dst, src) {
                (O::Mem(m), _) => note(&mut out, m, wb(*to), MemDir::Write),
                (_, O::Mem(m)) => note(&mut out, m, wb(*from), MemDir::Read),
                _ => {},
            }
        },
        Op::AluRm { op, width, dst, src } => {
            let w = wb(*width);
            // `cmp`/`test` 两个源都是只读；其余是「读改写目的」。
            let pure_read = matches!(op, AluOp::Cmp | AluOp::Test);
            let dst_dir = if pure_read { MemDir::Read } else { MemDir::ReadWrite };
            if let O::Mem(m) = dst {
                note(&mut out, m, w, dst_dir);
            }
            if let O::Mem(m) = src {
                note(&mut out, m, w, MemDir::Read);
            }
        },
        Op::ImulRm { width, dst, src } => {
            let w = wb(*width);
            if let O::Mem(m) = dst {
                note(&mut out, m, w, MemDir::ReadWrite);
            }
            if let O::Mem(m) = src {
                note(&mut out, m, w, MemDir::Read);
            }
        },
        Op::Shift { width, dst, .. } => {
            let w = wb(*width);
            if let O::Mem(m) = dst {
                note(&mut out, m, w, MemDir::ReadWrite);
            }
        },
        Op::UnaryRm { op, width, dst } => {
            let w = wb(*width);
            // `mul`/`imul`/`div`/`idiv` 的内存操作数只读（写的是 rax/rdx）；`not`/`neg`/
            // `inc`/`dec` 是读改写。
            let dir = match op {
                UnaryOp::Mul | UnaryOp::Imul | UnaryOp::Div | UnaryOp::Idiv => MemDir::Read,
                _ => MemDir::ReadWrite,
            };
            if let O::Mem(m) = dst {
                note(&mut out, m, w, dir);
            }
        },
        Op::Setcc { dst, .. } => {
            if let O::Mem(m) = dst {
                note(&mut out, m, 1, MemDir::Write);
            }
        },
        Op::Cmovcc { width, src, .. } | Op::BitScan { width, src, .. } => {
            let w = wb(*width);
            if let O::Mem(m) = src {
                note(&mut out, m, w, MemDir::Read);
            }
        },
        Op::Stack { width, dst, op, .. } => {
            // push/pop 的地址是 `rsp` 栈顶区，与帧槽区不重叠（帧向下生长，栈顶永远在
            // 所有 `[r13+disp]` 之下）；`dst` 是内存时（`pop [槽]`）才与帧有关。
            let w = wb(*width);
            if let O::Mem(m) = dst {
                let dir = if matches!(op, StackOp::Pop) { MemDir::Write } else { MemDir::Read };
                note(&mut out, m, w, dir);
            }
        },
        Op::Lea { mem, .. } => {
            // `lea` 不访问内存，但它把帧地址导出到了一个号里 ⇒ 条件 3 失效。
            if mem.base == Some(FRAME) {
                out.opaque = true;
                out.addr_escape = true;
            }
        },
        Op::JmpRmMem { base, index, .. } => {
            if *base == FRAME || index.map(|(i, _)| i == FRAME).unwrap_or(false) {
                out.opaque = true;
            }
        },
        Op::PushMemDisp8 { base, .. } => {
            if *base == FRAME {
                out.opaque = true;
            }
        },
        // 宽搬运与浮点/原子族：这些变体的内存侧可能是帧槽，而本层不接它们的形态
        // （服务不了、区间也定不准）⇒ 一律说不清。
        Op::SimdMov { .. }
        | Op::SimdArith { .. }
        | Op::SimdShiftImm8 { .. }
        | Op::SimdRound { .. }
        | Op::SimdCmpImm8 { .. }
        | Op::SimdMovRip { .. }
        | Op::ShldShrd { .. }
        | Op::GprXmm { .. }
        | Op::Cvt2Sd { .. }
        | Op::X87 { .. }
        | Op::LockCmpxchgRm { .. }
        | Op::LockXchgRm { .. }
        | Op::LockXaddRm { .. }
        | Op::XchgRm { .. }
        | Op::Pmulld { .. } => out.opaque = true,
        // 裸字节：交给与 `raw_writes` 共用的那个头解码器。
        Op::Raw { bytes, .. } => match raw_frame(bytes) {
            RawFrame::NoMem | RawFrame::Foreign => {},
            RawFrame::Opaque => out.opaque = true,
            RawFrame::Slot { disp, len, dir } => {
                out.exact.push(Access { slot: disp as i64, len, dir });
            },
        },
        // 控制流与无操作数形：它们不碰帧内存。（区的边界另有专门入口处理。）
        Op::Jcc { .. }
        | Op::Call { .. }
        | Op::CallRel32
        | Op::Jmp { .. }
        | Op::JmpRel32
        | Op::Ret { .. }
        | Op::NoArg { .. } => {},
    }
    out
}

/// 一次「可定形的 8 字节存出」：`mov [r13+slot], 源号`。它的作用是**把家建在那个源号上**
/// （存出照发 ⇒ 号与内存同时拿到值），不是推迟它。
fn promotable_store(insn: &Insn) -> Option<(i64, u8)> {
    match &insn.op {
        X86_64Opcode::MovRm {
            width, dst: O::Mem(m), src: O::Reg(s, _), rip_relative,
        } if !*rip_relative && matches!(width, Width::B64) => {
            frame_slot(m).map(|slot| (slot, *s))
        },
        _ => None,
    }
}

/// 一次「可被家服务的读」。同时给出这次读的**内存侧宽度**（决定家要可信到几字节）。
enum ReadForm {
    /// `mov [槽],dst`
    MovRm { dst: u8, width: Width, len: u8 },
    /// `movsxd/movzx [槽],dst`
    MovExt { dst: u8, sign: bool, from: Width, to: Width, len: u8 },
    /// `cmp/test [槽],src`：只读，换成家号语义完全相同，但它不产生新的家（没有目的地号）。
    AluRm { op: crate::codegen::x86_64::opcode::AluOp, width: Width, dst: u8, len: u8 },
}

impl ReadForm {
    /// 这次读需不需要家里那几字节。
    fn needs(&self) -> u8 {
        match self {
            ReadForm::MovRm { len, .. }
            | ReadForm::MovExt { len, .. }
            | ReadForm::AluRm { len, .. } => *len,
        }
    }
    /// 这次读会不会建立一个新家（有目的地号才会）。
    fn binder(&self) -> Option<u8> {
        match self {
            ReadForm::MovRm { dst, .. } | ReadForm::MovExt { dst, .. } => Some(*dst),
            ReadForm::AluRm { .. } => None,
        }
    }
}

fn readable_load(insn: &Insn) -> Option<(ReadForm, i64)> {
    use crate::codegen::x86_64::opcode::AluOp;
    match &insn.op {
        X86_64Opcode::MovRm { width, dst: O::Reg(d, _), src: O::Mem(m), rip_relative }
            if !*rip_relative && (1..=8).contains(&wb(*width)) =>
        {
            let len = wb(*width);
            frame_slot(m).map(|slot| (ReadForm::MovRm { dst: *d, width: *width, len }, slot))
        },
        X86_64Opcode::MovExt {
            sign, from, to, dst: O::Reg(d, _), src: O::Mem(m), rip_relative
        } if !*rip_relative && (1..=8).contains(&wb(*from)) => {
            let len = wb(*from);
            frame_slot(m).map(|slot| {
                (ReadForm::MovExt { dst: *d, sign: *sign, from: *from, to: *to, len }, slot)
            })
        },
        X86_64Opcode::AluRm {
            op, width, dst: O::Reg(d, _), src: O::Mem(m)
        } if (1..=8).contains(&wb(*width)) && matches!(op, AluOp::Cmp | AluOp::Test) => {
            let len = wb(*width);
            frame_slot(m).map(|slot| {
                (ReadForm::AluRm { op: *op, width: *width, dst: *d, len }, slot)
            })
        },
        _ => None,
    }
}

/// 一槽的主人情况（提升护栏的三个不同答案）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotOwner {
    /// 某个局部/形参占有它，而且那个局部的地址从未被取过。
    Owned,
    /// 查不到主人（合成栈槽、晚分配临时区、复合字面量）⇒ 保守不推。
    NoOwner,
    /// 主人取过址 ⇒ callee/指针可能看得见那份内存，不能拿号当它的权威副本。
    AddressEscaped,
}

impl crate::codegen::x86_64::codegen::Codegen {
    /// 逐函数开工。`allowed` 由调用方从**同一个**护栏算出（方案 §4.7 的同源要求），
    /// 本模块不写第二份控制流形状判据。
    pub fn region_begin_func(&mut self, allowed: bool, fname: &str) {
        self.region = RegionState::default();
        self.region.allowed = allowed && region_knob_on();
        if std::env::var_os("DCC_REGION_STAT").is_some() {
            let knob = region_knob_on();
            let ok = self.region.allowed;
            eprintln!("regionbegin fn={fname} 护栏={allowed} 旋钮={knob} ⇒ allowed={ok}");
        }
    }

    pub fn region_allowed(&self) -> bool {
        self.region.allowed
    }

    /// 中途彻底关掉本函数的家（「枚举不完一个巨大逃逸对象」那一类保守退路）。
    ///
    /// 本版没有需要回写的状态（存出从未被推迟），但接口保留「先清家再关闸门」的顺序，
    /// 这样以后有人往本层加回写义务时不会忘记这一步。
    pub fn region_disable(&mut self) {
        self.region_flush();
        self.region.homes.clear();
        self.region.live = 0;
        self.region.allowed = false;
    }

    /// 区划分 + 出口：`label()` 处先丢家再登记新边界。
    pub fn region_at_label(&mut self, name: &str) {
        if !self.region.allowed {
            return;
        }
        self.region_flush();
        self.region.id = self.region.id.wrapping_add(1);
        self.region.by_label.insert(name.to_string(), self.region.id);
    }

    /// 区出口：把所有活着的家丢掉。四个边界（`label()`、任何跳转、`goto *表[…]`、
    /// 函数尾声）都走这一个入口。
    pub fn region_flush(&mut self) {
        if !self.region.allowed || self.region.homes.is_empty() {
            return;
        }
        self.region.stats.drops_exit += 1;
        self.region.homes.clear();
        self.region.live = 0;
    }

    /// 见证一条即将发射的指令，给出家层的处置。
    ///
    /// 顺序：先把「本条会踩到某个家号」与「本条是一次证明不了不相交的帧访问」处理掉
    /// （丢家，不发任何字节），再处理本条自身对某个槽的访问。
    pub fn region_witness(&mut self, insn: &Insn) -> RegionVerdict {
        if !self.region.allowed {
            return RegionVerdict::Keep;
        }
        let use_ = frame_use(insn);
        // 本条自己那一次「将由下面某个分支直接处置」的访问：它不算义务（否则会把自己
        // 的家拆掉再重新建一个，白丢一次服务）。
        let self_store = promotable_store(insn);
        let self_read = readable_load(insn);
        if self.region.live > 0 {
            // 条件 1：家号被这条踩到 ⇒ 丢家。
            let rw = written_gp(insn);
            let clobbered: Vec<i64> =
                self.region.homes.values().filter(|h| !rw.keeps(h.reg)).map(|h| h.slot).collect();
            if !clobbered.is_empty() {
                self.region.stats.drops_clobber += 1;
            }
            for slot in clobbered {
                self.drop_home(slot);
            }
            // 条件 2：说不清的帧访问 ⇒ 全部丢家（一次都服务不了）。
            if use_.opaque {
                self.region.stats.drops_opaque += 1;
                self.region_flush_homes_quiet();
            }
            // 条件 2 的另一半：能定形、落在某个家的区间里、而本层又不当它服务的访问。
            for a in &use_.exact {
                if let Some((s, _)) = self_store {
                    if a.slot == s {
                        continue;
                    }
                }
                if let Some((_, s)) = &self_read {
                    if a.slot == *s && !a.dir.writes() {
                        continue;
                    }
                }
                let hit: Vec<i64> = self
                    .region
                    .homes
                    .values()
                    .filter(|h| overlaps(a.slot, a.len, h.slot, 8))
                    .map(|h| h.slot)
                    .collect();
                for slot in hit {
                    self.drop_home(slot);
                }
            }
        }
        if use_.addr_escape {
            // 条件 3：帧地址被导出 ⇒ 本函数此后再不建家。
            self.region.no_defer = true;
        }

        // ---- 存出侧：存出照发，家就建在这条的源号上 ----
        if let Some((slot, src)) = self_store {
            if let Some(why) = self.reject_reason(slot, src) {
                self.note_reject(why);
                return RegionVerdict::Keep;
            }
            // 已有同一个槽的家：把家改绑到本条的源号上（内存刚刚被本条写过，改绑后两者
            // 仍然等价）。没有就新建。
            if self.region_heat_ok(slot) {
                self.region.homes.insert(slot, RegionHome { reg: src, slot, vlen: 8 });
                self.region.live = self.region.homes.len() as u32;
                self.region.stats.promoted += 1;
            }
            return RegionVerdict::Keep;
        }

        // ---- 读侧 ----
        if let Some((form, slot)) = self_read {
            if let Some(h) = self.region.homes.get(&slot).copied() {
                if !h.serves(form.needs()) {
                    // 家里只有那几字节可信（例如它是由一次 4 字节载入建立的），而本条要
                    // 读更多 ⇒ 这次只能真去读内存。家仍然有效：内存与家等价。
                    return RegionVerdict::Keep;
                }
                self.region.stats.loads_saved += 1;
                // 换形后的文本不手写：类型化变体的 `op.render()` 是 `-S` 的第一级真源
                // （见 `elf/output.rs` 的 P10 闭环），自己拼一份就是第二份真源。
                let n = match form {
                    ReadForm::MovRm { dst, width, .. } => {
                        if dst == h.reg {
                            // 目的号就是家本身 ⇒ 这次读无事可做。
                            return RegionVerdict::Drop(slot);
                        }
                        Insn::mov_rm_reg(width, dst, h.reg)
                    },
                    ReadForm::MovExt { dst, sign, from, to, .. } => {
                        Insn::mov_ext(sign, from, to, dst, h.reg)
                    },
                    ReadForm::AluRm { op, width, dst, .. } => Insn::new(
                        X86_64Opcode::AluRm {
                            op,
                            width,
                            dst: O::Reg(dst, width),
                            src: O::Reg(h.reg, width),
                        },
                        &[],
                    ),
                };
                return RegionVerdict::Replace(n);
            }
            // 首次读这个槽：把本条的目的地号收作家（载入照发 ⇒ 号与内存同时拿到值）。
            let Some(dst) = form.binder() else {
                return RegionVerdict::Keep;
            };
            if let Some(why) = self.reject_reason(slot, dst) {
                self.note_reject(why);
                return RegionVerdict::Keep;
            }
            if !self.region_heat_ok(slot) || self.region.homes.len() as u32 >= region_max_homes() {
                return RegionVerdict::Keep;
            }
            self.region.homes.insert(
                slot,
                RegionHome { reg: dst, slot, vlen: form.needs() },
            );
            self.region.live = self.region.homes.len() as u32;
            self.region.stats.promoted += 1;
            return RegionVerdict::Keep;
        }
        RegionVerdict::Keep
    }

    /// 这一号能不能成为某个槽的家。返回 `Some(理由)` = 拒。
    ///
    /// 家号必须是 callee-saved 这道门是本轮读数逼出来的：`luaV_execute` 上以任意号建家
    /// 606 次，其中 521 次下一条求值就把那个号踩了（`Codegen` 把求值结果恒落 `%rax`），
    /// 一次服务都拿不到 ⇒ 建家只剩簿记成本。门关掉后可以重现那个读数（`DCC_REGION_CALLEE=0`）。
    fn reject_reason(&self, slot: i64, reg: u8) -> Option<&'static str> {
        if callee_gate_on() && !RegWrites::callee_saved(reg) {
            return Some("caller_saved");
        }
        if self.volatile_frames.contains(&slot) {
            return Some("volatile");
        }
        if self.region.no_defer {
            return Some("addr_export");
        }
        match self.slot_owner(slot) {
            SlotOwner::Owned => None,
            SlotOwner::NoOwner => Some("no_owner"),
            SlotOwner::AddressEscaped => Some("escaped"),
        }
    }

    /// 把某次拒绝记到对应的桶里（读数分桶，理由同 `reject_reason` 的注释）。
    fn note_reject(&mut self, why: &str) {
        let s = &mut self.region.stats;
        match why {
            "caller_saved" => s.refuse_caller_saved += 1,
            "volatile" => s.refuse_volatile += 1,
            "addr_export" => s.refuse_addr_export += 1,
            "no_owner" => s.refuse_no_owner += 1,
            _ => s.refuse_escaped += 1,
        }
    }

    /// `heat` 到上限就拉黑（反例守卫）。返回 `false` = 本槽已不可再提升。
    fn region_heat_ok(&mut self, slot: i64) -> bool {
        if self.region.blacklisted.contains(&slot) {
            self.note_reject("escaped");
            return false;
        }
        if self.region.heat.get(&slot).copied().unwrap_or(0) >= REGION_HEAT_CAP {
            self.region.blacklisted.insert(slot);
            self.region.stats.blacklisted_slots += 1;
            return false;
        }
        *self.region.heat.entry(slot).or_insert(0) += 1;
        true
    }

    /// 丢掉一个家。本版的家不占用分配器（号就是建立点那条指令的目的号），所以这里没有
    /// 「归还」那一步，也不需要回写任何内存。
    fn drop_home(&mut self, slot: i64) {
        self.region.homes.remove(&slot);
        self.region.live = self.region.homes.len() as u32;
    }

    fn region_flush_homes_quiet(&mut self) {
        self.region.homes.clear();
        self.region.live = 0;
    }

    /// 一槽的主人情况 —— **按变量名查**，而不是查一张预先登记的偏移集。
    ///
    /// 为什么不预先登记偏移：帧槽的登记点不是一处。`register_var` 是一条，参数与预注册
    /// 局部是另外八处 `var_map.insert`，`collect_frame_var_decls` 又是一处 —— 预先登记就
    /// 得把每一条都接上，而漏一条就是静默错值（实测：`taken_underscore()` 的 `_acc` 正是
    /// 被这么漏掉的）。改在提升点现查 `var_map` 之后，答案只有一个真源：名字在不在
    /// `frame_guard.escaped_names` 里。
    ///
    /// 查不到主人（合成栈槽、晚分配临时区、复合字面量）也算**不可提升**：那里的
    /// 访问模式不在这层的视野里，保守方向不推。
    fn slot_owner(&self, slot: i64) -> SlotOwner {
        let mut owned = false;
        for (name, &(off, ref ty)) in self.var_map.iter() {
            let slots = Self::var_alloc_slots(ty).max(1);
            // 帧向下生长：声明里 `off` 是最高那个字节所属的槽，对象占
            // `[off - 8*(slots-1), off]`。拿 `off` 当 lower bound 会把数组主体漏掉。
            let hi = off;
            let lo = off - 8 * (slots - 1);
            if slot >= lo && slot <= hi {
                owned = true;
                if self.frame_guard.escaped_names.contains(name.as_str()) {
                    return SlotOwner::AddressEscaped;
                }
            }
        }
        if owned {
            SlotOwner::Owned
        } else {
            SlotOwner::NoOwner
        }
    }

    /// 逐函数的机制读数（`DCC_REGION_STAT=1`，在函数发射收尾时打）。
    ///
    /// 为什么必须逐函数打而不是全进程累加：本层的每一张表都是逐函数的，一个「全局平均」
    /// 会被 `luaV_execute` 这种大函数掩盖掉真正拦住它的那一个函数（上面那张服务读数就
    /// 是逐函数打的：报告 §6.16.1，归因在方案 §12.6）。
    pub fn region_dump_stats(&mut self, fname: &str) {
        if std::env::var_os("DCC_REGION_STAT").is_none() {
            return;
        }
        let s = self.region.stats;
        eprintln!(
            "regionstat fn={fname} promoted={} loads_saved={} drop_clobber={} drop_exit={} \
             drop_opaque={} black={} \
             refuse(vol={},no_owner={},escaped={},addr_export={},caller_saved={})",
            s.promoted,
            s.loads_saved,
            s.drops_clobber,
            s.drops_exit,
            s.drops_opaque,
            s.blacklisted_slots,
            s.refuse_volatile,
            s.refuse_no_owner,
            s.refuse_escaped,
            s.refuse_addr_export,
            s.refuse_caller_saved,
        );
    }
}

/// `DCC_REGION_HOME` 旋钮（默认关）。
pub fn region_knob_on() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("DCC_REGION_HOME").map(|s| s != "0").unwrap_or(false))
}

/// 「家号必须是 callee-saved」那道门（`DCC_REGION_CALLEE=0` 关掉）。
///
/// 留着它不是怕那个结论错，而是为了让它能被**重测**：「任意号建家 ⇒ 服务为零」只能从
/// `luaV_execute` 的读数读出来（promoted 606 / loads_saved 0 / drop_clobber 521），
/// 关掉那道门就是那个读数的重现。
pub fn callee_gate_on() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("DCC_REGION_CALLEE").map(|s| s != "0").unwrap_or(true)
    })
}

/// 区内家最多几个（`DCC_REGION_MAX`，默认 4）。
///
/// 家不占分配器（号就是建立点那条指令的目的号），所以这里只管**状态规模**：家越多，
/// 每条指令要做的区间比对越多，且服务机会被稀释。
pub fn region_max_homes() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("DCC_REGION_MAX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|v| *v > 0 && *v <= 8)
            .unwrap_or(4)
    })
}
