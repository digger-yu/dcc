// src/codegen/x86_64/asm/planner/orchestration.rs
/// Register allocation and planning for inline assembly
///
/// Extracted from src/codegen/x86_64/mod.rs (Turn 33-37 ASM module migration)
/// This file contains the register planner that handles operand mapping
/// for x86_64 inline assembly constructs.
use std::collections::HashSet;

/// Register use classification
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AsmRegUse {
    /// Input operands
    Input,
    /// Output operands
    Output,
}

/// Register planner for x86_64 inline assembly operand mapping
///
/// Allocates GP, XMM, MMX registers while respecting clobber constraints.
/// Handles early-clobber constraints and prevents conflicts between
/// input and output operands.
///
/// # Design Notes
/// - Uses round-robin register selection with seed-based starting point
/// - Tracks used registers to prevent double-allocation  
/// - Maintains separate input/output sets for early-clobber analysis
/// - Supports fixed register constraints (a/b/c/d/S/D/q/Q)
pub struct AsmRegisterPlanner {
    used_gp: HashSet<u8>,     // All allocated GP registers
    used_xmm: HashSet<u8>,    // All allocated XMM registers
    used_mmx: HashSet<u8>,    // Allocated MMX registers
    input_gp: HashSet<u8>,    // GP registers used by inputs (for early-clobber check)
    input_xmm: HashSet<u8>,   // XMM registers used by inputs (for early-clobber check)
    clobber_gp: HashSet<u8>,  // GP registers marked as clobbered (excluded from alloc)
    clobber_xmm: HashSet<u8>, // XMM registers marked as clobbered (excluded from alloc)
}

impl AsmRegisterPlanner {
    /// Register pool constants for x86_64
    ///
    /// 通用 GP 池：rax, rcx, rdx, rsi, rdi, r8-r11 —— 全部是 caller-saved，
    /// 因此不含 callee-saved 的 rbx/r12-r15（那四个由表达式的溅出池与变量绑定独占）。
    const GP_POOL: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];

    /// Legacy GP pool for q/Q constraints (allows quadword subregister usage).
    ///
    /// GCC 的 `q` 类是「不需要 REX 前缀的前 8 个寄存器」(rax rbx rcx rdx rsi rdi rbp
    /// rsp)。这里去掉 rbp 与 rsp：它们分别是帧指针与栈指针，发给 asm 操作数就直接毁
    /// 掉帧。旧表只有 [rax, rbx, rcx, rdx] 四个，于是「%rbx 已被某个 register asm
    /// 变量占用」这件完全正常的事会让 q 类立刻耗尽，然后掉进那条复用已占寄存器的
    /// 回退里（实测 `after=22`，gcc 是 100）。放宽到六个之后，排除占用者仍然够用。
    const GP_LEGACY_POOL: [u8; 6] = [0, 3, 1, 2, 6, 7];

    /// XMM register pool: xmm0-xmm15 (SSE/AVX vector registers)
    const XMM_POOL: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    /// MMX register pool: mm0-mmx7 (legacy SIMD registers)
    const MMX_POOL: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

    /// Create new register planner initialized with clobbered registers
    ///
    /// # Arguments
    /// * `clobber_gp` - Set of general-purpose registers that will be clobbered
    /// * `clobber_xmm` - Set of XMM registers that will be clobbered
    ///
    /// These registers are pre-marked as used so they won't be allocated.
    pub fn new(clobber_gp: &HashSet<u8>, clobber_xmm: &HashSet<u8>) -> Self {
        Self {
            used_gp: clobber_gp.clone(),
            used_xmm: clobber_xmm.clone(),
            used_mmx: HashSet::new(),
            input_gp: HashSet::new(),
            input_xmm: HashSet::new(),
            clobber_gp: clobber_gp.clone(),
            clobber_xmm: clobber_xmm.clone(),
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
    /// （`pick` 的第二遍忽略 `used`，`unwrap_or` 再兜一次），于是同一句 asm 的两个操作数
    /// 共享寄存器：实测六个 `"=q"` 输出修前得 `six=297 o0=66`，修后与 gcc 一致
    /// （`six=231 o0=11`）。
    ///
    /// 为什么不把池子扩到 callee-saved（rbx/r12/r14/r15）：试过，**实测否决**。
    ///   * 只扩池：10 个 `"=r"` 输出的结果仍是错的（`s=64`，gcc 是 `s=55`）；
    ///   * 再补上「asm 期间独占这些号」的登记（`reg_allocated` + `callee_saved_used`）：
    ///     结果更糟（`s=10`；七个 `"=q"` 输出变成 `77 0 0 0 0 0 0`）。
    ///
    /// 原因是还没查清的暂存约定：操作数求值、写回与表达式求值之间对哪些寄存器可用的
    /// 假设有重叠，扩池会打破它（病灶未定位，因此这条不是「再改两行」就能收的）。
    /// 结论：操作数多于 9 个 GP 号时仍会复用同一个号，这是**已知的残余错值**，
    /// 连同实测记录在 docs/ssa-pass-upgrade-plan.md；要真解决得先重写 asm 操作数的
    /// 暂存约定。
    pub fn alloc_gp(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(&Self::GP_POOL, &self.used_gp, &self.clobber_gp, seed, None)
            .unwrap_or_else(|| Self::GP_POOL[seed % Self::GP_POOL.len()]);
        self.reserve_gp(reg, use_kind);
        reg
    }

    /// 把「本条 asm 之外已经属于别人的寄存器」标为不可分配。
    ///
    /// 只进 `used_gp`，不进 `clobber_gp`：这些寄存器不是被本条 asm 毁掉的，占用者
    /// （`register asm` 变量的家、或外层表达式正握着的 callee-saved 溅出槽）在 asm
    /// 之后还要继续用它，两个集合的后续处理完全不同，混起来会让 clobber 名单变假。
    /// 寄存器所有权的唯一真源是 `Codegen::reg_bindings` 与 `Codegen::reg_allocated`；
    /// 本方法只是把那份事实转达给规划器。
    pub fn mark_unavailable_gp<I: IntoIterator<Item = u8>>(&mut self, regs: I) {
        for r in regs {
            self.used_gp.insert(r);
        }
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
    ///
    /// # Constraint Mapping
    /// - `a/b/c/d/S/D` → Fixed registers rax/rbx/rcx/rbx/rsi/rdi
    /// - `q/Q` → Any register from legacy pool (allows 32/64-bit aliasing)
    /// - `=r/+r/&r` → Dynamic allocation via `alloc_gp()` (returns None here)
    /// - `{cc}`, `@cc*` → Condition codes (returns None)
    /// - `g/r` → General register class too flexible (returns None)
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
            if let Some(reg) =
                Self::pick(&Self::GP_LEGACY_POOL, &self.used_gp, &self.clobber_gp, seed, None)
            {
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
    ///
    /// # Arguments
    /// * `seed` - Starting index in XMM_POOL for round-robin selection
    /// * `use_kind` - Whether this is an input or output register
    ///
    /// # Returns
    /// Register number (0-15 mapping to xmm0-xmm15)
    pub fn alloc_xmm(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(&Self::XMM_POOL, &self.used_xmm, &self.clobber_xmm, seed, None)
            .unwrap_or(Self::XMM_POOL[seed % Self::XMM_POOL.len()]);
        self.reserve_xmm(reg, use_kind);
        reg
    }

    /// Allocate an MMX (legacy SIMD) register
    ///
    /// # Arguments
    /// * `seed` - Starting index in MMX_POOL for round-robin selection
    ///
    /// # Returns
    /// Register number (0-7 mapping to mm0-mm7)
    ///
    /// # Note
    /// MMX registers share state with XMM registers, so special care needed
    pub fn alloc_mmx(&mut self, seed: usize) -> u8 {
        let reg = Self::pick(&Self::MMX_POOL, &self.used_mmx, &HashSet::new(), seed, None)
            .unwrap_or(Self::MMX_POOL[seed % Self::MMX_POOL.len()]);
        self.used_mmx.insert(reg);
        reg
    }

    /// Reserve a GP register for allocation
    ///
    /// Marks register as used and adds to input set if it's an input operand.
    /// This prevents future allocations from reusing the same register.
    pub fn reserve_gp(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_gp.insert(reg);
        if matches!(use_kind, AsmRegUse::Input) {
            self.input_gp.insert(reg);
        }
    }

    /// Reserve an XMM register for allocation
    ///
    /// Marks register as used and adds to input set if it's an input operand.
    pub fn reserve_xmm(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_xmm.insert(reg);
        if matches!(use_kind, AsmRegUse::Input) {
            self.input_xmm.insert(reg);
        }
    }

    /// Check if a GP register is in the input set
    ///
    /// Used for early-clobber analysis to determine if a register
    /// can be reused for an output operand before all inputs are consumed.
    pub fn input_gp_contains(&self, reg: u8) -> bool {
        self.input_gp.contains(&reg)
    }

    /// Check if an XMM register is in the input set
    pub fn input_xmm_contains(&self, reg: u8) -> bool {
        self.input_xmm.contains(&reg)
    }

    /// Reassign a GP register for early-clobber outputs that conflict with inputs
    ///
    /// When an output has early-clobber (`&`) constraint and conflicts with
    /// an input register, this method attempts to allocate a different register.
    ///
    /// # Arguments
    /// * `old` - The currently allocated register that needs reassignment
    /// * `seed` - Starting index for finding alternative register
    ///
    /// # Returns
    /// Some(new_reg) if reassignment succeeded, None if no suitable register available
    ///
    /// # Behavior
    /// 1. If old register wasn't an input and isn't clobbered, release it
    /// 2. Find new register avoiding input registers (to prevent early-clobber violation)
    /// 3. Mark new register as used
    pub fn reassign_early_gp(&mut self, old: u8, seed: usize) -> Option<u8> {
        // Release old register if it's not critical (not input, not clobbered)
        if !self.input_gp.contains(&old) && !self.clobber_gp.contains(&old) {
            self.used_gp.remove(&old);
        }
        let reg = Self::pick(
            &Self::GP_POOL,
            &self.used_gp,
            &self.clobber_gp,
            seed,
            Some(&self.input_gp), // Avoid input registers
        )?;
        self.used_gp.insert(reg);
        Some(reg)
    }

    /// Reassign an XMM register for early-clobber outputs that conflict with inputs
    ///
    /// Similar to `reassign_early_gp` but for XMM registers.
    pub fn reassign_early_xmm(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_xmm.contains(&old) && !self.clobber_xmm.contains(&old) {
            self.used_xmm.remove(&old);
        }
        let reg = Self::pick(
            &Self::XMM_POOL,
            &self.used_xmm,
            &self.clobber_xmm,
            seed,
            Some(&self.input_xmm), // Avoid input registers
        )?;
        self.used_xmm.insert(reg);
        Some(reg)
    }

    /// Pick an unused register from the pool
    ///
    /// # Algorithm
    /// 1. **First pass**: Start from `seed` index and iterate forward (circular)
    ///    - Skip registers in `used` set
    ///    - Skip registers in `clobbered` set  
    ///    - Optionally skip registers in `avoid` set (for early-clobber)
    /// 2. 池子里没有一个空闲寄存器时返回 `None` —— 绝不复用已发出去的寄存器。
    ///
    /// # Parameters
    /// * `pool` - Available register candidates
    /// * `used` - Already allocated registers (excluded)
    /// * `clobbered` - Clobbered registers (excluded)
    /// * `seed` - Preferred starting position for round-robin
    /// * `avoid` - Optional set of registers to additionally avoid
    fn pick(
        pool: &[u8],
        used: &HashSet<u8>,
        clobbered: &HashSet<u8>,
        seed: usize,
        avoid: Option<&HashSet<u8>>,
    ) -> Option<u8> {
        // First pass: start from seed index (round-robin)
        for step in 0..pool.len() {
            let reg = pool[(seed + step) % pool.len()];
            if used.contains(&reg) || clobbered.contains(&reg) {
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
