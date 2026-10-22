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
const MAX_SLOTS: usize = 16;

/// 一条帧槽等价事实：`[r13+disp]` 的低 `len` 字节 == 号 `reg` 的低 `len` 字节。
#[derive(Clone, Copy)]
struct SlotFact {
    disp: i32,
    len: u8,
    reg: u8,
}

const EMPTY_SLOT: SlotFact = SlotFact {
    disp: 0,
    len: 0,
    reg: NO_REG,
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
    slots: [SlotFact; MAX_SLOTS],
}

impl Default for BlockState {
    fn default() -> Self {
        Self {
            copy: [NO_REG; 16],
            sext32: [false; 16],
            zext32: [false; 16],
            slots: [EMPTY_SLOT; MAX_SLOTS],
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

impl BlockState {
    pub(crate) fn reset(&mut self) {
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
        for s in self.slots.iter_mut() {
            if s.reg == NO_REG || s.len == 0 {
                *s = SlotFact { disp, len, reg };
                return;
            }
        }
        // 容量满：整体放弃帧槽跟踪（已建的事实也一并清掉，避免「只记得一半」）。
        self.clear_slots();
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
                *s = EMPTY_SLOT;
            }
        }
    }

    fn clear_slots(&mut self) {
        self.slots = [EMPTY_SLOT; MAX_SLOTS];
    }

    fn kill_reg(&mut self, r: u8) {
        if r >= 16 {
            return;
        }
        let ri = r as usize;
        self.copy[ri] = NO_REG;
        self.sext32[ri] = false;
        self.zext32[ri] = false;
        for s in self.slots.iter_mut() {
            if s.reg == r {
                *s = EMPTY_SLOT;
            }
        }
        // 「别的号等于 r」的关系也断了（r 变了，那些号还是旧值）。
        for c in self.copy.iter_mut() {
            if *c == r {
                *c = NO_REG;
            }
        }
    }

    fn kill_all(&mut self) {
        *self = BlockState::default();
    }

    /// 一次写内存对本块状态的影响。
    ///
    /// `src` 是「写进去的寄存器号」（`None` = 写的是算出来的值，无法建事实）。
    /// 未知基址的写一律当作可别名，清掉全部帧槽事实。
    fn note_store(&mut self, m: &MemOperand, len: u8, src: Option<u8>, volatiles: &HashSet<i64>) {
        let Some(disp) = frame_disp(m) else {
            self.clear_slots();
            return;
        };
        if len == 0 {
            // 宽度未知的写：它可能盖住任何一个被跟踪的槽。
            self.clear_slots();
            return;
        }
        self.invalidate(disp, len);
        let Some(s) = src else { return };
        if volatiles.contains(&(disp as i64)) {
            return;
        }
        if matches!(len, 1 | 2 | 4 | 8) {
            self.note_slot(disp, len, s);
        }
    }

    /// 只看不写：给出本条的处置。
    pub(crate) fn decide(&self, insn: &Insn, volatiles: &HashSet<i64>, level: u8) -> Verdict {
        if level == 0 {
            return Verdict::Keep;
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
                    return Verdict::Keep;
                };
                if r == d {
                    // 同号读：64 位载入是恒等；窄载入还会把目的号的高位清掉（32 位）
                    // 或只写一部分（8/16 位），只有在目的号的高位已是规范形态时才恒等。
                    let identity = w == 8 || (w == 4 && self.zext32[d as usize]);
                    if identity && level >= 2 {
                        return Verdict::Drop;
                    }
                    return Verdict::Keep;
                }
                if level >= 3 && w != 0 {
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
                    return Verdict::Keep;
                };
                if r == d {
                    let identity = if *sign {
                        self.sext32[d as usize]
                    } else {
                        self.zext32[d as usize]
                    };
                    if identity && level >= 2 {
                        return Verdict::Drop;
                    }
                    return Verdict::Keep;
                }
                // 只有目的号是 32/64 位时，寄存器形态才与内存形态逐位等价。
                let dst_w = width_bytes(*to);
                if level >= 3 && dst_w >= 4 && src_len != 0 {
                    return Verdict::Replace(Box::new(Insn::mov_ext(*sign, *from, *to, d, r)));
                }
                Verdict::Keep
            },
            // ---- 寄存器搬运：自反 / 已经相等 ----
            X86_64Opcode::MovRm {
                width,
                dst,
                src,
                ..
            } => {
                let (Some(d), Some(s)) = (is_reg(dst), is_reg(src)) else {
                    return Verdict::Keep;
                };
                if level < 2 {
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
                if level < 3 || volatiles.contains(&(disp as i64)) {
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
                if d == s && self.sext32[d as usize] && level >= 1 {
                    return Verdict::Drop;
                }
                Verdict::Keep
            },
            _ => Verdict::Keep,
        }
    }

    /// 记下本条（可能是替换后的指令）对块内状态的影响。
    pub(crate) fn observe(&mut self, insn: &Insn, volatiles: &HashSet<i64>) {
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
                    } else if w == 4 {
                        // 32 位写自动零扩展高 32 位。
                        self.zext32[*d as usize] = true;
                    }
                },
                (O::Reg(d, _), O::Imm(_)) => {
                    let w = width_bytes(*width);
                    self.kill_reg(*d);
                    if w == 4 {
                        self.zext32[*d as usize] = true;
                    }
                },
                (O::Reg(d, _), O::Mem(_)) => {
                    let w = width_bytes(*width);
                    self.kill_reg(*d);
                    if w == 4 {
                        self.zext32[*d as usize] = true;
                    }
                },
                (O::Mem(m), O::Reg(s, _)) => {
                    let w = width_bytes(*width);
                    self.note_store(m, w, Some(*s), volatiles);
                },
                _ => self.kill_all(),
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
                    self.kill_reg(*d);
                    // `to` 为 32/64 位时，结果的高位是规范形态。
                    if width_bytes(*to) >= 4 {
                        if *sign {
                            self.sext32[*d as usize] = true;
                        } else {
                            self.zext32[*d as usize] = true;
                        }
                    }
                    let _ = from;
                },
                (O::Mem(m), _) => self.note_store(m, width_bytes(*to), None, volatiles),
                _ => self.kill_all(),
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
                    _ => self.kill_all(),
                }
            },
            X86_64Opcode::ImulRm { width, dst, .. } => match dst {
                O::Reg(d, _) => self.kill_reg(*d),
                O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                _ => self.kill_all(),
            },
            X86_64Opcode::Shift { width, dst, .. } => match dst {
                O::Reg(d, _) => self.kill_reg(*d),
                O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                _ => self.kill_all(),
            },
            X86_64Opcode::UnaryRm { op, width, dst } => {
                // mul/imul/div/idiv 的隐式目的号是 rax/rdx。
                if matches!(op, UnaryOp::Mul | UnaryOp::Imul | UnaryOp::Div | UnaryOp::Idiv) {
                    self.kill_all();
                    return;
                }
                match dst {
                    O::Reg(d, _) => self.kill_reg(*d),
                    O::Mem(m) => self.note_store(m, width_bytes(*width), None, volatiles),
                    _ => self.kill_all(),
                }
            },
            X86_64Opcode::Lea { dst, .. } => match dst {
                O::Reg(d, _) => self.kill_reg(*d),
                _ => self.kill_all(),
            },
            X86_64Opcode::Setcc { dst, .. } => match dst {
                O::Reg(r, _) => self.kill_reg(*r),
                O::Mem(m) => self.note_store(m, 1, None, volatiles),
                _ => self.kill_all(),
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
                        _ => self.kill_all(),
                    }
                }
            },
            X86_64Opcode::Cvt2Sd { to_xmm, dst, .. } => {
                if !*to_xmm {
                    match dst {
                        O::Reg(d, _) => self.kill_reg(*d),
                        _ => self.kill_all(),
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
                    self.kill_all();
                }
            },
            X86_64Opcode::Jmp { .. } | X86_64Opcode::Ret { .. } | X86_64Opcode::Call { .. } => {
                self.kill_all()
            },
            // 认不得的一律全杀。
            _ => self.kill_all(),
        }
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
        match st.decide(&load, &nv(), 3) {
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
        assert!(matches!(st.decide(&load, &vol, 3), Verdict::Keep));
        // 同一号二次写也不许当死写削掉。
        let again = insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64);
        assert!(matches!(st.decide(&again, &vol, 3), Verdict::Keep));
    }

    #[test]
    fn narrow_load_into_the_same_register_is_not_an_identity() {
        // `mov eax,[slot]` 会把 rax 的高 32 位清掉，所以「槽里就是 rax」并不等于恒等，
        // 除非 rax 的高位已经是零。少了这一条就是静默错值。
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let load32 = insn_mov(O::Reg(0, Width::B32), mem(-64), Width::B32);
        assert!(
            matches!(st.decide(&load32, &nv(), 3), Verdict::Keep),
            "高位未清零时窄载入不是恒等，不得删"
        );
        st.zext32[0] = true;
        assert!(matches!(st.decide(&load32, &nv(), 3), Verdict::Drop));
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
        assert!(matches!(st.decide(&load, &nv(), 3), Verdict::Keep));
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
        assert!(matches!(st.decide(&load, &nv(), 3), Verdict::Keep));
    }

    #[test]
    fn partial_store_kills_the_wider_fact() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        st.observe(&insn_mov(mem(-64), O::Reg(1, Width::B32), Width::B32), &nv());
        let load64 = insn_mov(O::Reg(2, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load64, &nv(), 3), Verdict::Keep));
        // 低 32 位的事实仍在：按 4 字节读可以转发给 rcx。
        let load32 = insn_mov(O::Reg(2, Width::B32), mem(-64), Width::B32);
        match st.decide(&load32, &nv(), 3) {
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
        assert!(matches!(st.decide(&load, &nv(), 3), Verdict::Replace(_)));
    }

    #[test]
    fn redundant_copy_is_dropped_only_after_a_real_equalizing_move() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(O::Reg(1, Width::B64), O::Reg(0, Width::B64), Width::B64), &nv());
        let again = insn_mov(O::Reg(1, Width::B64), O::Reg(1, Width::B64), Width::B64);
        assert!(matches!(st.decide(&again, &nv(), 2), Verdict::Drop));
        let same = insn_mov(O::Reg(1, Width::B64), O::Reg(0, Width::B64), Width::B64);
        assert!(matches!(st.decide(&same, &nv(), 2), Verdict::Drop));
        // rax 被写坏之后，rcx==rax 这条事实必须消失。
        st.observe(&insn_mov(O::Reg(0, Width::B64), O::Imm(7), Width::B64), &nv());
        assert!(matches!(st.decide(&same, &nv(), 2), Verdict::Keep));
    }

    #[test]
    fn level_zero_never_decides_anything() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        let load = insn_mov(O::Reg(0, Width::B64), mem(-64), Width::B64);
        assert!(matches!(st.decide(&load, &nv(), 0), Verdict::Keep));
    }
}
