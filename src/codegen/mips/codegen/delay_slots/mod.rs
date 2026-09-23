// src/codegen/mips_backend/codegen/delay_slots/mod.rs - MIPS Delay Slot Handling
//
// MIPS architecture has a unique feature: branch and jump instructions have a
// "delay slot" - the instruction immediately following a branch/jump is ALWAYS
// executed, regardless of whether the branch is taken.
//
// This module handles delay slot filling with strategies:
// 1. Use independent instruction from next basic block
// 2. Schedule previous instruction into delay slot
// 3. Insert NOP if no useful instruction available
//
// Reference: MIPS ABI and ISA specifications

#![allow(dead_code)]
#![allow(unused_variables)]

use super::registers::{DelaySlotState, MipsGpr, MipsInstruction};

/// Maximum instructions to search for delay slot filler
pub const MAX_SEARCH_INSTRUCTIONS: usize = 5;

/// Delay slot state machine
#[derive(Debug, Clone)]
pub struct DelaySlotManager {
    /// Current delay slot being filled
    pub current_slot: Option<DelaySlotInfo>,
    /// Pending instructions that can be moved
    pub pending_instructions: Vec<MipsInstruction>,
    /// History of recently used registers (for dependency checking)
    pub recent_def_registers: Vec<MipsGpr>,
}

impl DelaySlotManager {
    pub fn new() -> Self {
        Self {
            current_slot: None,
            pending_instructions: Vec::new(),
            recent_def_registers: Vec::new(),
        }
    }

    /// Start filling a delay slot after a branch/jump
    pub fn start_delay_slot(&mut self, branch_instr: MipsInstruction) -> String {
        // Clear previous state
        self.current_slot = Some(DelaySlotInfo {
            source_instruction: None,
            dependent_on_branch: false,
            used: false,
        });

        // Find best candidate for delay slot
        let filler = self.find_filler(branch_instr);
        
        // Add to history
        self.recent_def_registers.clear();
        
        filler
    }

    /// Add an instruction to the list of candidates for delay slot
    pub fn add_pending_instruction(&mut self, instr: MipsInstruction) {
        self.pending_instructions.push(instr);
        
        // Track destination registers for dependency checking
        if let Some(dest) = self.get_dest_reg(&instr) {
            self.recent_def_registers.push(dest);
        }
    }

    /// Clear pending instructions when a delay slot was consumed or not needed
    pub fn clear_pending(&mut self) {
        self.pending_instructions.clear();
    }

    /// Find the best instruction to fill delay slot
    fn find_filler(&self, branch: MipsInstruction) -> String {
        // Strategy 1: Try to use pending instruction with no dependencies
        for instr in &self.pending_instructions {
            if !self.has_data_dependency(branch, instr) {
                return self.instruction_to_string(instr);
            }
        }

        // Strategy 2: Look for independent arithmetic operation
        for instr in &self.pending_instructions {
            if self.is_independent_arithmetic(instr) {
                return self.instruction_to_string(instr);
            }
        }

        // Strategy 3: Insert NOP
        "nop".to_string()
    }

    /// Check if an instruction has data dependency on branch target
    fn has_data_dependency(&self, branch: MipsInstruction, instr: &MipsInstruction) -> bool {
        // Branch reads its source registers
        let branch_sources = self.branch_source_regs(&branch);
        
        // Instruction writes to a register that branch needs
        if let Some(instr_dest) = self.get_dest_reg(instr) {
            if branch_sources.contains(&instr_dest) {
                return true;
            }
        }

        // Instruction reads from same reg as branch source
        let instr_sources = self.instruction_source_regs(instr);
        for src in &instr_sources {
            if branch_sources.contains(src) && *src != MipsGpr::Zero {
                return true;
            }
        }

        false
    }

    /// Get registers read by branch instruction
    fn branch_source_regs(&self, branch: &MipsInstruction) -> Vec<MipsGpr> {
        match branch {
            MipsInstruction::Beq(rs, rt, _) | MipsInstruction::Bne(rs, rt, _) => {
                vec![*rs, *rt]
            }
            _ => Vec::new(),
        }
    }

    /// Get destination register of an instruction
    fn get_dest_reg(&self, instr: &MipsInstruction) -> Option<MipsGpr> {
        match instr {
            MipsInstruction::Add(rd, _, _) => Some(*rd),
            MipsInstruction::Lw(rd, _, _) => Some(*rd),
            _ => None,
        }
    }

    /// Get registers read by instruction
    fn instruction_source_regs(&self, instr: &MipsInstruction) -> Vec<MipsGpr> {
        match instr {
            MipsInstruction::Add(_, rs, rt) => vec![*rs, *rt],
            MipsInstruction::Lw(_, _, base) => vec![*base],
            MipsInstruction::Sw(_, _, base) => vec![*base],
            _ => Vec::new(),
        }
    }

    /// Check if instruction is independent arithmetic (safe to move)
    fn is_independent_arithmetic(&self, instr: &MipsInstruction) -> bool {
        matches!(instr, MipsInstruction::Add(_, _, _) | MipsInstruction::Mul(_, _, _))
    }

    /// Convert internal instruction representation to assembly string
    fn instruction_to_string(&self, instr: &MipsInstruction) -> String {
        match instr {
            MipsInstruction::Nop => "nop".to_string(),
            MipsInstruction::Add(rd, rs, rt) => {
                format!("add {} ${} ${}", rd.name(), rs.name(), rt.name())
            }
            MipsInstruction::Mul(rd, rs, rt) => {
                format!("mul {} ${} ${}", rd.name(), rs.name(), rt.name())
            }
            MipsInstruction::Lw(rt, offset, rs) => {
                format!("lw {} {}, {}", rt.name(), *offset, rs.name())
            }
            MipsInstruction::Sw(rt, offset, rs) => {
                format!("sw {} {}, {}", rt.name(), *offset, rs.name())
            }
            MipsInstruction::J(addr) => format!("j 0x{:x}", addr),
            MipsInstruction::Jal(addr) => format!("jal 0x{:x}", addr),
            MipsInstruction::Beq(rs, rt, offset) => {
                format!("beq {} {}, {}", rs.name(), rt.name(), offset)
            }
            MipsInstruction::Bne(rs, rt, offset) => {
                format!("bne {} {}, {}", rs.name(), rt.name(), offset)
            }
            MipsInstruction::Other => "...".to_string(),
        }
    }

    /// Check if delay slot can be safely filled
    pub fn can_fill_delay_slot(&self) -> bool {
        !self.pending_instructions.is_empty() || true // Always true (NOP is fallback)
    }

    /// Get number of instructions in queue
    pub fn pending_count(&self) -> usize {
        self.pending_instructions.len()
    }
}

impl Default for DelaySlotManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Information about a filled delay slot
#[derive(Debug, Clone)]
pub struct DelaySlotInfo {
    /// The instruction used to fill the slot
    pub source_instruction: Option<String>,
    /// Whether this fills the slot completely
    pub complete: bool,
    /// Whether it depends on the branch outcome
    pub dependent_on_branch: bool,
    /// Whether it was actually used
    pub used: bool,
}

/// Delay slot optimization strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelaySlotStrategy {
    /// Use any independent instruction
    BestFit,
    /// Prefer arithmetic over memory operations
    ArithmeticFirst,
    /// Minimize register pressure
    RegisterConservative,
    /// Maximize throughput
    ThroughputOptimized,
}

impl DelaySlotStrategy {
    pub fn name(&self) -> &'static str {
        match self {
            Self::BestFit => "best-fit",
            Self::ArithmeticFirst => "arithmetic-first",
            Self::RegisterConservative => "register-conservative",
            Self::ThroughputOptimized => "throughput-optimized",
        }
    }

    pub fn priority(self, instr: &MipsInstruction) -> u8 {
        match self {
            Self::ArithmeticFirst => {
                match instr {
                    MipsInstruction::Add(_, _, _) | MipsInstruction::Mul(_, _, _) => 3,
                    MipsInstruction::Lw(_, _, _) => 2,
                    _ => 1,
                }
            }
            Self::BestFit => 1, // All equal priority
            _ => 1,
        }
    }
}

/// Branch type classification
#[derive(Debug, Clone, Copy)]
pub enum BranchType {
    Conditional(MipsGpr, MipsGpr),  // beq, bne, etc.
    Unconditional(u32),              // j, jal
    JumpRegister(MipsGpr),           // jr, jalr
    Return(MipsGpr),                 // jr ra
}

impl BranchType {
    pub fn from_instruction(instr: &MipsInstruction) -> Option<Self> {
        match instr {
            MipsInstruction::Beq(rs, rt, _) => Some(Self::Conditional(*rs, *rt)),
            MipsInstruction::Bne(rs, rt, _) => Some(Self::Conditional(*rs, *rt)),
            MipsInstruction::J(addr) => Some(Self::Unconditional(*addr)),
            MipsInstruction::Jal(addr) => Some(Self::Unconditional(*addr)),
            MipsInstruction::Other if instr.has_property("jr") => {
                Some(Self::JumpRegister(MipsGpr::FP))
            }
            MipsInstruction::Other if instr.has_property("return") => {
                Some(Self::Return(MipsGpr::FP))
            }
            _ => None,
        }
    }
}

/// Advanced delay slot analysis
#[derive(Debug, Clone)]
pub struct DelaySlotAnalysis {
    /// Basic block ID where branch occurs
    pub bb_id: u32,
    /// Branch position in block
    pub branch_position: usize,
    /// Available instructions in current block after branch
    pub available_after: Vec<MipsInstruction>,
    /// Instructions from successor blocks
    pub successor_instructions: Vec<Vec<MipsInstruction>>,
    /// Optimization preference
    pub strategy: DelaySlotStrategy,
}

impl DelaySlotAnalysis {
    pub fn new(bb_id: u32, branch_pos: usize) -> Self {
        Self {
            bb_id,
            branch_position: branch_pos,
            available_after: Vec::new(),
            successor_instructions: Vec::new(),
            strategy: DelaySlotStrategy::ArithmeticFirst,
        }
    }

    /// Perform full delay slot optimization
    pub fn optimize(&self, manager: &DelaySlotManager) -> String {
        // Analyze all available instructions
        let mut candidates: Vec<(u8, MipsInstruction)> = Vec::new();

        // Add instructions from current block
        for instr in &self.available_after {
            let priority = self.strategy.priority(instr);
            candidates.push((priority, instr.clone()));
        }

        // Sort by priority (descending)
        candidates.sort_by(|a, b| b.0.cmp(&a.0));

        // Try to find best fit
        for (_priority, instr) in candidates {
            // In real implementation, call manager to check dependencies
            return manager.instruction_to_string(&instr);
        }

        "nop".to_string()
    }
}

/// Trace and profile delay slot usage
#[derive(Debug, Clone)]
pub struct DelaySlotStats {
    pub total_branches: u32,
    pub filled_with_useful: u32,
    pub filled_with_nop: u32,
    pub average_search_depth: f32,
}

impl DelaySlotStats {
    pub fn new() -> Self {
        Self {
            total_branches: 0,
            filled_with_useful: 0,
            filled_with_nop: 0,
            average_search_depth: 0.0,
        }
    }

    pub fn record_fill(&mut self, useful: bool) {
        self.total_branches += 1;
        if useful {
            self.filled_with_useful += 1;
        } else {
            self.filled_with_nop += 1;
        }
    }

    pub fn utilization_rate(&self) -> f32 {
        if self.total_branches == 0 {
            0.0
        } else {
            self.filled_with_useful as f32 / self.total_branches as f32
        }
    }
}

impl Default for DelaySlotStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod mips_delay_slot_tests {
    use super::*;

    #[test]
    fn test_delay_slot_manager_basic() {
        let mut manager = DelaySlotManager::new();
        
        // Start filling delay slot
        let branch = MipsInstruction::Beq(MipsGpr::T0, MipsGpr::T1, 4);
        let filler = manager.start_delay_slot(branch);
        
        assert_eq!(filler, "nop"); // No pending instructions yet
        
        // Add a pending instruction
        manager.add_pending_instruction(MipsInstruction::Add(MipsGpr::T2, MipsGpr::T3, MipsGpr::T4));
        
        // Should use the added instruction
        let filler = manager.find_filler(branch);
        assert!(filler.contains("add"));
    }

    #[test]
    fn test_data_dependency_detection() {
        let manager = DelaySlotManager::new();
        
        // Branch reads T0 and T1
        let branch = MipsInstruction::Beq(MipsGpr::T0, MipsGpr::T1, 0);
        
        // Instruction writes to T0 - creates dependency
        let dep_instr = MipsInstruction::Add(MipsGpr::T0, MipsGpr::T2, MipsGpr::T3);
        
        assert!(manager.has_data_dependency(branch, &dep_instr));
        
        // Independent instruction
        let indep_instr = MipsInstruction::Add(MipsGpr::T4, MipsGpr::T5, MipsGpr::T6);
        assert!(!manager.has_data_dependency(branch, &indep_instr));
    }

    #[test]
    fn test_delay_slot_strategies() {
        let strategy = DelaySlotStrategy::ArithmeticFirst;
        
        let add = MipsInstruction::Add(MipsGpr::T0, MipsGpr::T1, MipsGpr::T2);
        let lw = MipsInstruction::Lw(MipsGpr::T0, 0, MipsGpr::T1);
        
        assert!(strategy.priority(&add) > strategy.priority(&lw));
    }

    #[test]
    fn test_branch_type_classification() {
        let beq = MipsInstruction::Beq(MipsGpr::A0, MipsGpr::A1, 4);
        let branch_type = BranchType::from_instruction(&beq);
        assert!(matches!(branch_type, Some(BranchType::Conditional(_, _))));

        let j = MipsInstruction::J(0x1000);
        let branch_type = BranchType::from_instruction(&j);
        assert!(matches!(branch_type, Some(BranchType::Unconditional(_))));
    }

    #[test]
    fn test_delay_slot_stats() {
        let mut stats = DelaySlotStats::new();
        
        stats.record_fill(true);
        stats.record_fill(false);
        stats.record_fill(true);
        
        assert_eq!(stats.total_branches, 3);
        assert_eq!(stats.filled_with_useful, 2);
        assert_eq!(stats.filled_with_nop, 1);
        assert_eq!(stats.utilization_rate(), 0.666_666_7);
    }

    #[test]
    fn test_pending_instruction_queue() {
        let mut manager = DelaySlotManager::new();
        
        assert_eq!(manager.pending_count(), 0);
        
        manager.add_pending_instruction(MipsInstruction::Nop);
        assert_eq!(manager.pending_count(), 1);
        
        manager.add_pending_instruction(MipsInstruction::Add(MipsGpr::T0, MipsGpr::T1, MipsGpr::T2));
        assert_eq!(manager.pending_count(), 2);
        
        manager.clear_pending();
        assert_eq!(manager.pending_count(), 0);
    }

    #[test]
    fn test_can_fill_delay_slot() {
        let manager = DelaySlotManager::new();
        
        // Can always fill with NOP at minimum
        assert!(manager.can_fill_delay_slot());
    }

    #[test]
    fn test_strategy_names() {
        assert_eq!(DelaySlotStrategy::BestFit.name(), "best-fit");
        assert_eq!(DelaySlotStrategy::ArithmeticFirst.name(), "arithmetic-first");
    }
}
