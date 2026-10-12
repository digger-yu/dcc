// This file contains the register planner that handles operand mapping
use crate::codegen::x86_64::abi::regbank::RegBank;
/// for x86_64 inline assembly constructs.
use std::collections::HashSet;

/// Register use classification
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AsmRegUse {
    Output,
    Input,
}

/// Handles early-clobber constraints and prevents conflicts between
/// input and output operands.
///
/// # Design Notes (P12 Phase 6.2 / D6)
/// - GP 占用判据**不**由规划器自维护一份「占用真源」：规划期排除集合
///   = `owned_gp`（构造时快照：函数级 pinned + 溅出池正占的号）
///   ∪ 本条 clobber 名单 ∪ 本条已发出的 GP 号（`used_gp`）。
///   这就是旧 H4「第四套判据」的正确形态：它只是**快照 + 增量**，
///   不与 RegBank 同时活跃，因此不会漂移。
/// - 规划结束后由调用方 `finish(reg_bank)` 一次性把 callee-saved 号 claim
///   进 RegBank（置 allocated+used_by_self）；caller-saved 号不在 RegBank
///   的池内，无需登记（它们本来就是跨 call 不保活的）。
/// - 规划器仍保留：本条 clobber 名单、本条发出去过的号的台账
///   （`chosen_gp` / `input_gp`）、exhausted 标志。这些是 per-asm 的
///   规划状态，不是占用判据。
/// - XMM/MMX 通道今天不在 RegBank 建模范围内，故仍由规划器自维护（Phase 5 的事）。
pub struct AsmRegisterPlanner {
    /// 本条 GP 排除集合：owned_gp 快照 ∪ clobber_gp ∪ 本条已发 GP。
    /// 这是 pick 的「已占用」判据，规划期纯本地，不碰 RegBank。
    used_gp: HashSet<u8>,
    clobber_gp: HashSet<u8>,
    used_xmm: HashSet<u8>,
    used_mmx: HashSet<u8>,
    input_xmm: HashSet<u8>,
    clobber_xmm: HashSet<u8>,
    /// 本条 asm **实际发出去过**的 GP 号（不含 clobber 与已有主人）。
    /// 调用方拿它把 callee-saved 那几个登记回占用真源（`RegBank::claim`）。
    chosen_gp: HashSet<u8>,
    /// 本条 asm 发出去过的 **input** GP 号（early-clobber 重分配判据用）。
    input_gp: HashSet<u8>,
    /// 兜底是否真的发生过：发生过就说明有操作数**复用**了同一个号（值会互相盖）。
    exhausted_gp: bool,
}

impl AsmRegisterPlanner {
    /// Register pool constants for x86_64
    ///
    /// 通用 GP 池：前九个是 caller-saved（rax, rcx, rdx, rsi, rdi, r8-r11），
    /// 后面四个是 callee-saved（rbx, r12, r14, r15）。r13 是帧基址、
    /// rbp/rsp 是帧/栈指针，不进池。
    pub const GP_POOL: [u8; 13] = [0, 1, 2, 6, 7, 8, 9, 10, 11, 3, 12, 14, 15];
    /// Legacy `q`/`Q` 约束专用池（只含可做 32/64 位别名的号）。
    pub const GP_LEGACY_POOL: [u8; 6] = [0, 1, 2, 3, 6, 7];
    /// XMM pool
    pub const XMM_POOL: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    /// MMX pool
    pub const MMX_POOL: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

    /// Create new register planner.
    ///
    /// # Arguments
    /// * `owned_gp` - 本条 asm **之外**已有主人的 GP 号（由 `RegBank::owned_gp()`
    ///   一次性给出：register asm 变量钉住的号 + 外层表达式正握着的溅出槽）。
    ///   这些号进 `used_gp` 但**不进** `clobber_gp` —— 它们不是被本条 asm
    ///   毁掉的，asm 之后原占用者还要继续用它（§7.17 立的那条规矩）。
    /// * `clobber_gp` - 本条 asm 列进 clobber 的 GP 号
    /// * `clobber_xmm` - 本条 asm 列进 clobber 的 XMM 号
    pub fn new(owned_gp: &[u8], clobber_gp: &HashSet<u8>, clobber_xmm: &HashSet<u8>) -> Self {
        let mut used_gp = clobber_gp.clone();
        used_gp.extend(owned_gp.iter().copied());
        Self {
            used_gp,
            clobber_gp: clobber_gp.clone(),
            used_xmm: clobber_xmm.clone(),
            used_mmx: HashSet::new(),
            input_xmm: HashSet::new(),
            clobber_xmm: clobber_xmm.clone(),
            chosen_gp: HashSet::new(),
            input_gp: HashSet::new(),
            exhausted_gp: false,
        }
    }

    pub fn gp_exhausted(&self) -> bool {
        self.exhausted_gp
    }

    pub fn chosen_gp(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.chosen_gp.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// 本条 asm 编完：把规划结果一次性提交到 RegBank。
    ///
    /// 规则（D6 同源）：
    ///   * **callee-saved 池内的号**（rbx/r12/r14/r15）→ `claim`（置 allocated
    ///     + used_by_self）。序言保存位必须跟上，溅出池不得再发同一个号。
    ///   * **caller-saved 号**（rax/rcx/rdx/rsi/rdi/r8-r11）→ 什么都不做：
    ///     它们本来就不在溅出池里，也不需要序言保存；后续 asm 还应能再发它们。
    ///
    /// 本方法取 planner 所有权，意味着「规划期结束」的边界由类型保证——
    /// 规划器存活期间调用方可以自由借用 `&mut self`（如 `prepare_asm_binding`），
    /// 因为 planner 不再持有 `&mut RegBank`。
    pub fn finish(self, reg_bank: &mut RegBank) {
        for reg in self.chosen_gp {
            if RegBank::pool_for_bank(crate::location::RegBank::Gp).contains(&reg) {
                reg_bank.claim(reg);
            }
        }
    }

    /// Allocate a general-purpose register using round-robin strategy
    ///
    /// # Arguments
    /// * `seed` - Starting index in GP_POOL for round-robin selection
    /// * `use_kind` - Whether this is an input or output register
    ///
    /// # 只找空闲号；找不到时按 seed 位兜底
    /// 第一选择永远是「本句 asm 里还没人用过的号」。旧实现在这里没有「空闲」这个约束
    /// （`pick` 的第二遍忽略 `used`，`unwrap_or` 再兜一次），于是同一句 asm 的两个
    /// 操作数共享寄存器：实测六个 `"=q"` 输出修前得 `six=297 o0=66`，修后与 gcc
    /// 一致（`six=231 o0=11`）。
    ///
    /// 下面那条 `unwrap_or_else` 仍是**兜底**：池子（含 callee-saved 四个）全被占或
    /// 全被 clobber 时它还会复用同一个号。那个情形下 GCC 会直接报
    /// `asm operand has impossible constraints`，所以这条兜底是「宁可编下去也不报错」
    /// 的旧选择，它的残留风险由池扩大到 13 个号后已大幅降低；彻底解决要重写操作数
    /// 暂存约定（设计 §11.15 与 §11.16 记的 Phase 6.1 那两条），不能拿兜底当常态。
    pub fn alloc_gp(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let picked = Self::pick(&Self::GP_POOL, &self.used_gp, seed, None);
        if picked.is_none() {
            // 池（13 个号）全被占或全被 clobber：这一步仍会复用同号，但这件事
            // **不得静默**：登记下来交给调用方点名（见 `gp_exhausted`）。
            self.exhausted_gp = true;
        }
        let reg = picked.unwrap_or_else(|| Self::GP_POOL[seed % Self::GP_POOL.len()]);
        self.reserve_gp(reg, use_kind);
        reg
    }

    /// Allocate a specific fixed GP register based on GCC constraint letters
    ///
    /// # Arguments
    /// * `constraint` - GCC asm constraint string (e.g., "a", "b", "=r", "+mem")
    /// * `seed` - Fallback seed for dynamic allocation
    /// * `use_kind` - Whether this is an input or output register
    ///
    /// # Returns
    /// Some((reg_number, is_fixed)) if a register was allocated, None otherwise
    pub fn alloc_gp_for_constraint(
        &mut self,
        constraint: &str,
        seed: usize,
        use_kind: AsmRegUse,
    ) -> Option<(u8, bool)> {
        // Strip modifier characters (=, +, &, %)
        let core = constraint.trim_start_matches(['=', '+', '&', '%']);

        // Skip condition code constraints and brace syntax
        if core.contains('{') || core.starts_with("@cc") {
            return None;
        }

        // Extract alphabetic characters from constraint
        let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();

        // Skip if general register class specified (too flexible)
        if letters.iter().any(|c| matches!(c, 'r' | 'R' | 'g')) {
            return None;
        }

        // Map constraint letters to fixed registers
        let fixed = if letters.contains(&'a') {
            Some(0) // rax
        } else if letters.contains(&'b') {
            Some(3) // rbx
        } else if letters.contains(&'c') {
            Some(1) // rcx
        } else if letters.contains(&'d') {
            Some(2) // rdx
        } else if letters.contains(&'S') {
            Some(6) // rsi
        } else if letters.contains(&'D') {
            Some(7) // rdi
        } else {
            None
        };

        // Use fixed register if specified
        if let Some(reg) = fixed {
            self.reserve_gp(reg, use_kind);
            return Some((reg, true));
        }

        // Handle q/Q constraints (quadword, allows subregister aliasing)
        if letters.iter().any(|c| matches!(c, 'q' | 'Q')) {
            // 只在**空闲**的 legacy 号里选。旧代码在这里 `unwrap_or(pool[seed % len])`，
            // 也就是把一个已经发出去的寄存器再发一次 —— 于是同一句 asm 的两个 q
            // 操作数共享寄存器，或者直接盖掉某个 `register asm` 变量的家
            // （实测 `after=22`，gcc 是 100）。
            if let Some(reg) = Self::pick(&Self::GP_LEGACY_POOL, &self.used_gp, seed, None) {
                self.reserve_gp(reg, use_kind);
                return Some((reg, false));
            }
            // 六个 legacy 号（rax/rbx/rcx/rdx/rsi/rdi；rbp 与 rsp 是帧指针与栈指针，
            // 不能给操作数）都不空闲时返回 None，由调用方按 impossible constraint 报
            // 出去。为什么不放宽成「随便一个 GP 寄存器」：`q` 的意义就是模板里可能用
            // 只认 legacy 编码的指令 (xchg acc,r / in / out / mov m8,al)，换号会编出
            // 语义不同的指令；而实测放宽之后还要额外解决暂存约定（见 `alloc_gp` 的
            // 说明），所以这条留给重写操作数暂存时一起做。
            return None;
        }

        // No register could be allocated
        None
    }

    /// Allocate an XMM (SSE/AVX) register
    pub fn alloc_xmm(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(&Self::XMM_POOL, &self.used_xmm, seed, None)
            .unwrap_or(Self::XMM_POOL[seed % Self::XMM_POOL.len()]);
        self.reserve_xmm(reg, use_kind);
        reg
    }

    /// Allocate an MMX (legacy SIMD) register
    pub fn alloc_mmx(&mut self, seed: usize) -> u8 {
        let reg = Self::pick(&Self::MMX_POOL, &self.used_mmx, seed, None)
            .unwrap_or(Self::MMX_POOL[seed % Self::MMX_POOL.len()]);
        self.used_mmx.insert(reg);
        reg
    }

    /// Reserve a GP register for allocation.
    ///
    /// 纯本地台账：进 `used_gp`（影响后续 pick）、进 `chosen_gp`（供 finish claim）、
    /// input 类进 `input_gp`（early-clobber 判据）。规划期不碰 RegBank。
    pub fn reserve_gp(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_gp.insert(reg);
        self.chosen_gp.insert(reg);
        if matches!(use_kind, AsmRegUse::Input) {
            self.input_gp.insert(reg);
        }
    }

    /// Reserve an XMM register for allocation
    pub fn reserve_xmm(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_xmm.insert(reg);
        if matches!(use_kind, AsmRegUse::Input) {
            self.input_xmm.insert(reg);
        }
    }

    /// Check if a GP register is in the input set
    pub fn input_gp_contains(&self, reg: u8) -> bool {
        self.input_gp.contains(&reg)
    }

    /// Check if an XMM register is in the input set
    pub fn input_xmm_contains(&self, reg: u8) -> bool {
        self.input_xmm.contains(&reg)
    }

    /// Reassign a GP register for early-clobber outputs that conflict with inputs.
    ///
    /// 若旧号不是输入，就撤销它（从 `used_gp` / `chosen_gp` 移除），让它能被重新 pick。
    /// clobber/owned_gp 号不在 chosen_gp 里，这里不需要判断。
    pub fn reassign_early_gp(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_gp.contains(&old) {
            self.used_gp.remove(&old);
            self.chosen_gp.remove(&old);
        }
        let reg = Self::pick(
            &Self::GP_POOL,
            &self.used_gp,
            seed,
            Some(&self.input_gp), // Avoid input registers
        )?;
        self.reserve_gp(reg, AsmRegUse::Output);
        Some(reg)
    }

    /// Reassign an XMM register for early-clobber outputs that conflict with inputs.
    pub fn reassign_early_xmm(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_xmm.contains(&old) && !self.clobber_xmm.contains(&old) {
            self.used_xmm.remove(&old);
        }
        let reg = Self::pick(
            &Self::XMM_POOL,
            &self.used_xmm,
            seed,
            Some(&self.input_xmm), // Avoid input registers
        )?;
        self.used_xmm.insert(reg);
        Some(reg)
    }

    /// Pick an unused register from the pool.
    ///
    /// # Algorithm
    /// 1. **First pass**: Start from `seed` index and iterate forward (circular)
    ///    - Skip registers in `used` set（构造时已并入 clobber 与 owned_gp）
    ///    - Optionally skip registers in `avoid` set (for early-clobber)
    /// 2. 池子里没有一个空闲寄存器时返回 `None` —— 绝不复用已发出去的寄存器。
    fn pick(
        pool: &[u8],
        used: &HashSet<u8>,
        seed: usize,
        avoid: Option<&HashSet<u8>>,
    ) -> Option<u8> {
        // First pass: start from seed index (round-robin)
        for step in 0..pool.len() {
            let reg = pool[(seed + step) % pool.len()];
            if used.contains(&reg) {
                continue;
            }
            if avoid.is_some_and(|set| set.contains(&reg)) {
                continue;
            }
            return Some(reg);
        }

        // 没有第二遍「忽略 used 的兜底」：那条兜底唯一的实际作用就是把**已经发出去**
        // 的寄存器再发一次（第一遍本来就是环形扫完整池，seed 偏好早已覆盖）。于是
        // 同一句 asm 里两个操作数会共享一个寄存器，或 q 类耗尽时直接盖掉变量绑定的
        // 家 —— 实测四个 `"=q"` 输出把值写进了 `register long v asm("rbx")` 里
        // (`after=22`，gcc 是 100)。现在池子真耗尽就是 None，由调用方带着诊断处理。
        None
    }
}
