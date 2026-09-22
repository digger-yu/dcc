// src/codegen/x86_64/asm/planner/mod.rs
// Register allocation and planning for inline assembly
// Extracted from src/codegen/x86_64/mod.rs (PR-A2 Phase - Part 3)

use std::collections::HashSet;

/// Register use classification
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AsmRegUse {
    Input,
    Output,
}

/// Register planner for x86_64 inline assembly operand mapping
/// 
/// Allocates GP, XMM, MMX registers while respecting clobbers
/// Handles early-clobber constraints and input register conflicts.
pub struct AsmRegisterPlanner {
    used_gp: HashSet<u8>,           // All allocated GP registers
    used_xmm: HashSet<u8>,          // All allocated XMM registers  
    used_mmx: HashSet<u8>,          // Allocated MMX registers
    input_gp: HashSet<u8>,          // GP registers used by inputs (for early-clobber check)
    input_xmm: HashSet<u8>,         // XMM registers used by inputs (for early-clobber check)
    clobber_gp: HashSet<u8>,        // GP registers marked as clobbered
    clobber_xmm: HashSet<u8>,       // XMM registers marked as clobbered
}

impl AsmRegisterPlanner {
    /// Register pool constants
    const GP_POOL: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];   // rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8-r11
    const GP_LEGACY_POOL: [u8; 4] = [0, 3, 1, 2];             // rax, rbx, rcx, rdx for q/Q constraints
    const XMM_POOL: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    const MMX_POOL: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

    /// Create new register planner with clobber sets excluded from allocation
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
    pub fn alloc_gp(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(
            &Self::GP_POOL,
            &self.used_gp,
            &self.clobber_gp,
            seed,
            None,
        )
        .unwrap_or(Self::GP_POOL[seed % Self::GP_POOL.len()]);
        self.reserve_gp(reg, use_kind);
        reg
    }

    /// Allocate a specific fixed GP register based on GCC constraint letters (a/b/c/d/S/D/q/Q)
    pub fn alloc_gp_for_constraint(
        &mut self,
        constraint: &str,
        seed: usize,
        use_kind: AsmRegUse,
    ) -> Option<(u8, bool)> {
        let core = constraint.trim_start_matches(['=', '+', '&', '%']);
        if core.contains('{') || core.starts_with("@cc") {
            return None;
        }
        let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if letters.iter().any(|c| matches!(c, 'r' | 'R' | 'g')) {
            return None;
        }
        
        let fixed = if letters.contains(&'a') {
            Some(0)
        } else if letters.contains(&'b') {
            Some(3)
        } else if letters.contains(&'c') {
            Some(1)
        } else if letters.contains(&'d') {
            Some(2)
        } else if letters.contains(&'S') {
            Some(6)
        } else if letters.contains(&'D') {
            Some(7)
        } else {
            None
        };

        if let Some(reg) = fixed {
            self.reserve_gp(reg, use_kind);
            return Some((reg, true));
        }

        if letters.iter().any(|c| matches!(c, 'q' | 'Q')) {
            let reg = Self::pick(
                &Self::GP_LEGACY_POOL,
                &self.used_gp,
                &self.clobber_gp,
                seed,
                None,
            )
            .unwrap_or(Self::GP_LEGACY_POOL[seed % Self::GP_LEGACY_POOL.len()]);
            self.reserve_gp(reg, use_kind);
            return Some((reg, false));
        }

        None
    }

    /// Allocate an XMM (SSE/AVX) register
    pub fn alloc_xmm(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(
            &Self::XMM_POOL,
            &self.used_xmm,
            &self.clobber_xmm,
            seed,
            None,
        )
        .unwrap_or(Self::XMM_POOL[seed % Self::XMM_POOL.len()]);
        self.reserve_xmm(reg, use_kind);
        reg
    }

    /// Allocate an MMX register
    pub fn alloc_mmx(&mut self, seed: usize) -> u8 {
        let reg = Self::pick(
            &Self::MMX_POOL,
            &self.used_mmx,
            &HashSet::new(),
            seed,
            None,
        )
        .unwrap_or(Self::MMX_POOL[seed % Self::MMX_POOL.len()]);
        self.used_mmx.insert(reg);
        reg
    }

    /// Reserve a GP register for allocation
    pub fn reserve_gp(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_gp.insert(reg);
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

    /// Check if a GP register is in the input set (used for early-clobber reassignment)
    pub fn input_gp_contains(&self, reg: u8) -> bool {
        self.input_gp.contains(&reg)
    }

    /// Check if an XMM register is in the input set (used for early-clobber reassignment)
    pub fn input_xmm_contains(&self, reg: u8) -> bool {
        self.input_xmm.contains(&reg)
    }

    /// Reassign a GP register for early-clobber outputs that conflict with inputs
    /// Returns the new register allocation, or None if no suitable register found
    pub fn reassign_early_gp(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_gp.contains(&old) && !self.clobber_gp.contains(&old) {
            self.used_gp.remove(&old);
        }
        let reg = Self::pick(
            &Self::GP_POOL,
            &self.used_gp,
            &self.clobber_gp,
            seed,
            Some(&self.input_gp),
        )?;
        self.used_gp.insert(reg);
        Some(reg)
    }

    /// Reassign an XMM register for early-clobber outputs that conflict with inputs
    /// Returns the new register allocation, or None if no suitable register found
    pub fn reassign_early_xmm(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_xmm.contains(&old) && !self.clobber_xmm.contains(&old) {
            self.used_xmm.remove(&old);
        }
        let reg = Self::pick(
            &Self::XMM_POOL,
            &self.used_xmm,
            &self.clobber_xmm,
            seed,
            Some(&self.input_xmm),
        )?;
        self.used_xmm.insert(reg);
        Some(reg)
    }

    /// Pick an unused register from the pool, avoiding clobbered and optionally input registers
    fn pick(
        pool: &[u8],
        used: &HashSet<u8>,
        clobbered: &HashSet<u8>,
        seed: usize,
        avoid: Option<&HashSet<u8>>,
    ) -> Option<u8> {
        // First pass: start from seed index
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

        // Second pass: try all registers regardless of seed (fallback)
        for &reg in pool {
            if clobbered.contains(&reg) || avoid.is_some_and(|set| set.contains(&reg)) {
                continue;
            }
            return Some(reg);
        }

        None
    }
}
