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
    /// 本条 asm **实际发出去过**的 GP 号（不含构造时进表的 clobber 与已有主人）。
    /// 有了它，调用方才能把 callee-saved 那几个登记回占用真源（`RegBank::claim`）；
    /// 只看 `used_gp` 不行，它里面混着传进来的排除集，区分不了「谁发的」。
    chosen_gp: HashSet<u8>,
    /// 兜底是否真的发生过：发生过就说明有操作数**复用**了同一个号（值会互相盖）。
    /// 调用方拿它把这种情形报成 `impossible constraint`（与 legacy `q` 池耗空那条同口径），
    /// 而不是静默交出一个错值。
    exhausted_gp: bool,
}

impl AsmRegisterPlanner {
    /// Register pool constants for x86_64
    ///
    /// 通用 GP 池：前九个是 caller-saved（rax, rcx, rdx, rsi, rdi, r8-r11），后面四个是
    /// callee-saved（rbx, r12, r14, r15）。为什么把 callee-saved 也纳进来：**过去只有九个
    /// 可选，操作数多于九个时 `alloc_gp` 会回退到「复用已发出去的号」**（实测十个 `"=r"`
    /// 输出得 `s=64`，gcc 是 55：第十个输出落在了第一个的家上），那是静默错值。
    /// 取用顺序仍是「先 caller-saved」，所以只有真的不够时才动 callee-saved；而一旦动了，
    /// 调用方必须把那个号登记回 `RegBank`（见 `Codegen::own_asm_operand_gp`：序言保存不得被
    /// 改成 nop，溅出池也不得再发同一个号）。`r13`/`rbp` 永远不进池：它们是帧基址与帧指针。
    ///
    /// 这一条以前记的是「扩池已被实测否决」（只扩池仍错、再补登记更错）：那两组数字是在
    /// `pick` 还会忽略 `used` 集合、且 `owned_gp()` 还不是构造参数时测的；那两个前提修好后
    /// （同一句 asm 内不复用 + 已有主人不从池里发）扩池在本轮实测里是有效的。
    const GP_POOL: [u8; 13] = [0, 1, 2, 6, 7, 8, 9, 10, 11, 3, 12, 14, 15];

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
    /// * `owned_gp` - 本条 asm **之外**已有主人的 GP 号（由 `RegBank::owned_gp()` 一次性
    ///   给出：`register asm` 变量钉住的号 + 外层表达式正握着的溅出槽）
    ///
    /// 这些寄存器都不会被分配。两个集合的含义不同，不得混用：
    ///   * `owned_gp` 只进 `used_gp`，**不进** `clobber_gp` —— 它们不是被本条 asm
    ///     毁掉的，asm 之后原占用者还要继续用它；写进 clobber 名单会让它变假
    ///     （§7.17 立的那条规矩）。
    ///   * Phase 1 之前这一步是 `mark_unavailable_gp()` 在构造后手工转达的；那种
    ///     「先建好再补一句」的形式就是 H4 说的第四套判据，新增调用点很容易漏掉。
    ///     现在事实是构造参数，漏传直接编译不过。
    pub fn new(clobber_gp: &HashSet<u8>, clobber_xmm: &HashSet<u8>, owned_gp: &[u8]) -> Self {
        let mut used_gp = clobber_gp.clone();
        used_gp.extend(owned_gp.iter().copied());
        Self {
            used_gp,
            used_xmm: clobber_xmm.clone(),
            used_mmx: HashSet::new(),
            input_gp: HashSet::new(),
            input_xmm: HashSet::new(),
            clobber_gp: clobber_gp.clone(),
            clobber_xmm: clobber_xmm.clone(),
            chosen_gp: HashSet::new(),
            exhausted_gp: false,
        }
    }

    /// 本条 asm 里是否出现过「GP 池已空，只能复用已发出去的号」。
    pub fn gp_exhausted(&self) -> bool {
        self.exhausted_gp
    }

    /// 本条 asm 发出去过的 GP 号（升序，确定序）。
    pub fn chosen_gp(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.chosen_gp.iter().copied().collect();
        v.sort_unstable();
        v
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
    /// 下面那条 `unwrap_or_else` 仍是**兜底**：池子（含 callee-saved 四个）全被占或全被
    /// clobber 时它还会复用同一个号。那个情形下 GCC 会直接报
    /// `asm operand has impossible constraints`，所以这条兜底是「宁可编下去也不报错」的
    /// 旧选择，它的残留风险由池扩大到 13 个号后已大幅降低；彻底解决要重写操作数暂存约定
    /// （设计 §11.15 与 §11.16 记的 Phase 6.1 那两条），不能拿兜底当常态。
    pub fn alloc_gp(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let picked = Self::pick(&Self::GP_POOL, &self.used_gp, &self.clobber_gp, seed, None);
        if picked.is_none() {
            // 池（13 个号）全被占或全被 clobber：这一步仍会复用同号，但这件事**不得静默**：
            // 登记下来交给调用方点名（见 `gp_exhausted`）。
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
    ///
    /// # Constraint Mapping
    /// - `a/b/c/d/S/D` → Fixed registers rax/rbx/rcx/rdx/rsi/rdi
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
        self.chosen_gp.insert(reg);
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
