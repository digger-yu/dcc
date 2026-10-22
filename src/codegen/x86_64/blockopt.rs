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
            zbits: [0; 16],
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
pub(crate) const R_SEXT: u8 = 1 << 0;
pub(crate) const R_COPY: u8 = 1 << 1;
pub(crate) const R_SLOT: u8 = 1 << 2;
/// 批2 的三项在此接入；位号先占好，免得接入时再改动已有判据。
/// R_ZBITS：`zbits` 值域事实（掩码后 / `movzx` 后的 sext 恒等）。
/// R_DEADW：**只写一次就被覆盖的死写**。位号先占着，但它在这条通道上**结构上做不到**
/// （批 2 实测后登记）：`decide` 只能处置**当前**这条，而「上一条 `mov $imm, r` 后来被覆盖」
/// 要删的是**上一条** —— 那需要回删已发字节，已经在发射层做了（`instructions.rs` 的
/// `dead_pc` 那条：同块相邻/跨若干条不碰此号的覆盖都能 `code.truncate` 回删，现产物里
/// 那 281 条死 0 写就是它吃的）。剩下的非相邻形态要的是真 DCE（块内后向扫）而不是窥孔，
/// 那是 `decide` 这一层的结构之外。故本位永远不开，只占号不写臂 —— 而不是留一个
/// 「开了也什也不会发生」的假开关。
/// R_RAW：`Raw` 里「只写目的号一个」的形态按 `kill_reg` 而不是 `kill_all`。
pub(crate) const R_ZBITS: u8 = 1 << 3;
pub(crate) const R_DEADW: u8 = 1 << 4;
pub(crate) const R_RAW: u8 = 1 << 5;
/// R_CPY32：`mov r64,r64` 在**源号高 32 位已知为零**时改用 32 位形态（批 2 C-lite 第二条）。
/// 只在两条长度相同时换（见 `raw_mov_rr` 那条臂里的注释）。
pub(crate) const R_CPY32: u8 = 1 << 6;

/// `level` 的三档语义映射到本位图：这一函数是「旧开关不漂字节」的全部依据。
pub(crate) fn rules_for(level: u8) -> u8 {
    match level {
        0 => 0,
        1 => R_SEXT,
        2 => R_SEXT | R_COPY,
        _ => R_SEXT | R_COPY | R_SLOT,
    }
}

/// 解析 `DCC_BLOCKOPT_RULES`：十进制或 `0x` 十六进制；超出一个字节（或有符号、
/// 带空白以外的杂质）就视为无效 ⇒ 按未设置处理。
pub(crate) fn parse_rules(s: &str) -> Option<u8> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u8::from_str_radix(hex, 16).ok();
    }
    t.parse::<u16>().ok().and_then(|v| u8::try_from(v).ok())
}

/// 本进程生效的规则位图。与 `level()` 一样只算一次（`OnceLock`）：位图在发射途中
/// 变化会让同一函数的两块按不同规则发射，那种产物没人能复现。
pub(crate) fn rules() -> u8 {
    static R: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *R.get_or_init(|| {
        std::env::var("DCC_BLOCKOPT_RULES")
            .ok()
            .and_then(|v| parse_rules(&v))
            .unwrap_or_else(|| rules_for(level()))
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

    /// 「本块里这个号的值连 bit31 都是 0」⇒ `movsxd d, d` 是恒等。
    ///
    /// 为什么上限是 31 而不是 32：32 位写只能保证「高 32 位为零」（`zext32`），而
    /// `mov eax, -1; movsxd rax, eax` 删掉扩展会得到 4294967295 而不是 -1——这条反例
    /// 必须继续保留扩展。
    ///
    /// 为什么收 `rules` 参数而不自己读全局：这样判据能单独测（进程内的位图只初始化
    /// 一次，拿全局的话「开 ZBITS」那条正路径永远测不到）。
    fn zbits_within_sext(&self, d: u8, rules: u8) -> bool {
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

    /// 只看不写：给出本条的处置。第三个参数是 [`R_SEXT`]..[`R_RAW`] 的位图（不是旧的
    /// 单调档位），默认值由 [`rules_for`] 从 `DCC_BLOCKOPT` 的档位推得。
    pub(crate) fn decide(&self, insn: &Insn, volatiles: &HashSet<i64>, rules: u8) -> Verdict {
        if rules == 0 {
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
            // ---- `mov r64, r64` → 32 位同形替换（批 2 C-lite 第二条）----
            // 源号的高 32 位已知为零（`zext32[s]`）时，`mov s64 -> d64` 与
            // `mov s32 -> d32` 得到同一个值：32 位写自动把高的 32 位清成零。
            //
            // **只接受长度完全相同的替换**（`d >= 8 || s >= 8` 时两条都带 REX，都 3 字节）。
            // 低 8 号那一族 3B→2B 会缩短，那会挪动整块之后每条指令的 PC，不是本通道该做
            // 的事（方案 §13.5 的补丁点漂移就是同一类错误的代价）。
            X86_64Opcode::Raw { bytes, .. } if rules & R_CPY32 != 0 => {
                match Self::raw_mov_rr(bytes) {
                    Some((d, s)) if self.zext32[s as usize] && (d >= 8 || s >= 8) => {
                        use crate::codegen::shared::asm_operand::Width;
                        let insn = Insn::mov_rm_reg(Width::B32, d, s);
                        let (src_name, dst_name) = (
                            crate::codegen::x86_64::emit::gas::REG32[(s & 15) as usize],
                            crate::codegen::x86_64::emit::gas::REG32[(d & 15) as usize],
                        );
                        return Verdict::Replace(Box::new(insn.with_gas(format!("mov {src_name}, {dst_name}"))));
                    },
                    _ => Verdict::Keep,
                }
            },
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
            // `Raw` 里的「只写目的号一个」形态（批2 的 F 项）。`R_RAW` 关掉时回到
            // 下面的 catch-all `kill_all()`，所以 `DCC_BLOCKOPT=0..3` 的产物与本改动前
            // 逐字相同（默认位图不含 R_RAW，见 `rules_for`）。
            //
            // 为什么这条重要：发射层那批立即数形态（`add/sub/and/or/xor/cmp r64,imm`、
            // `mov r64,imm32`、移位 `C1/D3`、`imul r,r,imm`）全走 `Opcode::Raw`，
            // 以前一律当全杀——一条只写一号的指令擦掉整张 copy/sext/slot 事实表。
            // 实测 luaV_execute 里这类形态占 21.6%（2451/11366），挡住 274 对
            // 「存了立刻同槽重载」纯浪费里的 247 对（批2 开工前的实测归因）。
            X86_64Opcode::Raw { bytes, .. } if rules() & R_RAW != 0 => match raw_writes(bytes) {
                RawWrite::Nothing => {},
                RawWrite::Reg(r) => self.kill_reg(r),
                RawWrite::Masked { dst, bits } => {
                    self.kill_reg(dst);
                    if rules() & R_ZBITS != 0 {
                        self.zbits[dst as usize] = bits;
                    }
                },
                RawWrite::Unknown => self.kill_all(),
            },
            // 认不得的一律全杀。
            _ => self.kill_all(),
        }
    }
}

/// `Raw` 字节流的写入集判据。
///
/// 为什么解码字节而不是「拿 mnemonic 字符串比」：字节是唯一真源，而且 modrm 拆完
/// 就能定出到底写 reg 字段还是 r/m 字段（`0x81/0x69` 写 reg，`0xC7/0xC1` 写 r/m），
/// 而助记符在部分构造子上根本没有。认不得的形状一律 `Unknown` ⇒  caller 全杀，
/// 宁少削一条也不能静默错值。
#[derive(Debug)]
pub(crate) enum RawWrite {
    /// 不写任何 GP 号（`cmp`/`test`），事实全部存活。
    Nothing,
    /// 只写这一个个号。
    Reg(u8),
    /// `and r, imm`（imm 非负且 < 2^31）：结果既只写这个号，又得到一个**上界已知**的值。
    /// `bits` 就是 imm 的位宽（值 < 2^bits）。
    Masked { dst: u8, bits: u8 },
    /// 认不得：可能是内存写、可能同时写 rdx 之类。
    Unknown,
}

pub(crate) fn raw_writes(bytes: &[u8]) -> RawWrite {
    let mut i = 0usize;
    let rex = if matches!(bytes.first(), Some(b) if (0x40..=0x4f).contains(b)) {
        i = 1;
        bytes[0]
    } else {
        0u8
    };
    // 32 位形态下 0x67/0x66 前缀不在本批评范围内：宁可 Unknown 全杀。
    let (op, modrm) = match (bytes.get(i), bytes.get(i + 1)) {
        (Some(o), Some(m)) => (*o, *m),
        _ => return RawWrite::Unknown,
    };
    // 只接受寄存器-寄存器形（mod=11）；带内存操作数的同 opcode 可能写内存，不在此列。
    if modrm >> 6 != 3 {
        return RawWrite::Unknown;
    }
    let reg = ((modrm >> 3) & 7) | if rex & 0x04 != 0 { 8 } else { 0 };
    let rm = (modrm & 7) | if rex & 0x01 != 0 { 8 } else { 0 };
    match op {
        // ALU r/m64, imm32(/imm8)：**r/m 字段是目的**，reg 字段是 ALU digit（/7 是 CMP，不写号）。
        // 这一点本函数第一版写反了（当成 reg），是拿 `insn.rs` 那六个构造子的真实
        // 字节写测才抱出来的（见 `raw_writes_matches_the_imm_constructors`）。
        0x81 | 0x83 => {
            if reg == 7 {
                RawWrite::Nothing
            } else if reg == 4 {
                // AND：拿到立即数才能说上界。取不到立即数（字节不够）或立即数为负/超过
                // 2^31-1 时退回普通写——宁可少一条事实，不可多一条错事实。
                match raw_immediate(bytes, i, op) {
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
        0xc7 | 0xc6 => RawWrite::Reg(rm),
        // 移位 r/m64, imm8(/CL)：r/m 字段是目的（/0-/3 都只写 r/m；/7 是 SAR）。
        0xc1 | 0xd3 | 0xd1 => RawWrite::Reg(rm),
        _ => RawWrite::Unknown,
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
        assert_eq!(rules_for(3), R_SEXT | R_COPY | R_SLOT);
        // `level()` 把 >3 夹到 3，位图也必须落在同一处。
        assert_eq!(rules_for(4), rules_for(3));
        assert_eq!(rules_for(255), rules_for(3));
        // 新臂的位号不得被旧档位顺带打开。
        let extra = R_ZBITS | R_DEADW | R_RAW | R_CPY32;
        assert_eq!(rules_for(3) & extra, 0);
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
        assert_eq!(parse_rules("256"), None);
        assert_eq!(parse_rules("0x100"), None);
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
        let w = |ins: &Insn| match raw_writes(&ins.encode_bytes().unwrap_or_default()) {
            RawWrite::Nothing => "nothing".to_string(),
            RawWrite::Reg(r) => format!("r{r}"),
            RawWrite::Masked { dst, bits } => format!("r{dst}<2^{bits}"),
            RawWrite::Unknown => "?".to_string(),
        };
        // ALU：r/m 字段是目的；/7(CMP) 不写号。r13 = 13（REX.B 那位止于 r/m）。
        assert_eq!(w(&Insn::alu_rm_reg_imm32_b64(0, 13, 8)), "r13", "add r13, imm");
        assert_eq!(w(&Insn::alu_rm_reg_imm8_b64(5, 12, 1)), "r12", "sub r12, imm8");
        assert_eq!(w(&Insn::alu_rm_reg_imm32_b64(7, 13, 4)), "nothing", "cmp 不写任何号");
        // and 是非破坏写且上界已知：同一个 dst 要报成 Masked 而不是裸 Reg。
        assert_eq!(w(&Insn::alu_rm_reg_imm32_b64(4, 8, 0xff)), "r8<2^8");
        // MOV r/m64, imm32：r/m 字段是目的。
        assert_eq!(w(&Insn::mov_reg_imm32_b64(12, 5)), "r12");
        // 移位：r/m 字段是目的。
        assert_eq!(w(&Insn::shift_reg_imm8_c1_b64(4, 3, 2)), "r3");
        // IMUL：自反形，reg 字段是目的。
        assert_eq!(w(&Insn::imul_rm_reg_imm32_b64(9, 16)), "r9");
    }

    /// 反面判据：不能把「写了别的号或写了内存」的形态当成只写一号。
    #[test]
    fn raw_writes_stays_unknown_for_memory_and_multi_write_forms() {
        // `add QWORD PTR [rbx], imm32`：mod=00 且 r/m=3 ⇒ 写内存，必须全杀。
        assert!(matches!(raw_writes(&[0x48, 0x81, 0x00 + 3, 1, 0, 0, 0]), RawWrite::Unknown));
        // `mul r/m64`（F7 /4）：同时写 rdx:rax，不在本批评范围内。
        assert!(matches!(raw_writes(&[0x48, 0xF7, 0xE0 + 3]), RawWrite::Unknown));
        // `shift` 但 mod=10（带 disp 的内存操作数）。
        assert!(matches!(raw_writes(&[0x48, 0xC1, 0x40 | 3, 2]), RawWrite::Unknown));
        // 空字节流不能猜测。
        assert!(matches!(raw_writes(&[]), RawWrite::Unknown));
    }

    /// 默认位图下 `Raw` 仍走 `kill_all`：上面那些能力不得在 `DCC_BLOCKOPT=0..3` 下
    /// 静默生效（否则旧开关的产物字节就不是零漂移了）。
    #[test]
    fn raw_bytes_still_clear_everything_without_the_raw_bit() {
        let mut st = BlockState::default();
        st.observe(&insn_mov(mem(-64), O::Reg(0, Width::B64), Width::B64), &nv());
        // 一条 `add rcx, 1` 的 Raw 形态：默认（R_RAW 未开）应当擦掉帧槽事实。
        let add = crate::codegen::x86_64::insn::Insn::alu_rm_reg_imm32_b64(0, 1, 1);
        st.observe(&add, &nv());
        let load = insn_mov(O::Reg(2, Width::B64), mem(-64), Width::B64);
        assert!(
            matches!(st.decide(&load, &nv(), rules_for(3)), Verdict::Keep),
            "默认位图不含 R_RAW，槽事实应已被清；能转发就说明新臂偷开了"
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
}
