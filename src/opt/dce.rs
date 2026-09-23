// src/opt/dce.rs - Dead Code Elimination Pass
// Phase 4: Performance Optimization & Tuning
// 
// First Principle: Remove code whose results are never used
// Strategy: Backward data flow analysis to track liveness
// Target: 15-20% reduction in dead instructions

use crate::ir::{Module, Function, BasicBlock, Instruction, ValueId};
use std::collections::{HashMap, HashSet};

/// Dead Code Elimination optimizer
pub struct DCEOptimizer<'a> {
    module: &'a mut Module,
    stats: DCEStats,
}

/// Statistics tracking for DCE optimizations
#[derive(Debug, Default)]
pub struct DCEStats {
    pub functions_optimized: usize,
    pub instructions_eliminated: usize,
    pub blocks_merged: usize,
    pub unused_phi_removed: usize,
}

impl<'a> DCEOptimizer<'a> {
    pub fn new(module: &'a mut Module) -> Self {
        Self {
            module,
            stats: DCEStats::default(),
        }
    }

    /// Run full DCE optimization pass
    pub fn run(&mut self) {
        log("Starting DCE optimization");
        
        // Process each function
        for func in &mut self.module.functions {
            if self.optimize_function(func) {
                self.stats.functions_optimized += 1;
            }
        }
        
        log(&format!("DCE complete: {} funcs, {} instrs removed",
                    self.stats.functions_optimized,
                    self.stats.instructions_eliminated));
    }

    /// Optimize a single function
    fn optimize_function(&mut self, func: &mut Function) -> bool {
        let mut changed = false;
        
        // Multiple passes until no more changes
        loop {
            let before_count = func.total_instructions();
            
            // Run basic block level DCE
            for block in &mut func.blocks {
                if self.optimize_block(block) {
                    changed = true;
                }
            }
            
            let after_count = func.total_instructions();
            if before_count == after_count && !changed {
                break;
            }
        }
        
        changed
    }

    /// Optimize a single basic block
    fn optimize_block(&mut self, block: &mut BasicBlock) -> bool {
        // Track which values are actually used
        let mut live_values: HashSet<ValueId> = HashSet::new();
        
        // Start with uses from successor blocks (phi nodes)
        for succ_idx in &block.successors {
            let succ = &self.module.functions[0].blocks[*succ_idx]; // Simplified
            for phi in &succ.phis {
                if let Some(value) = phi.from_value.as_ref() {
                    live_values.insert(*value);
                }
            }
        }
        
        // Worklist algorithm - process instructions backward
        let mut worklist: Vec<usize> = block.instructions.iter().map(|(_, id)| *id).collect();
        worklist.reverse();
        
        let mut eliminated = 0;
        
        while let Some(instr_id) = worklist.pop() {
            if let Some(instr) = block.get_instruction_mut(instr_id) {
                // Check what this instruction produces
                let result = instr.result();
                
                if let Some(res) = result {
                    // Is anything using this result?
                    if live_values.contains(res) {
                        // Yes - mark operands as live
                        for operand in instr.operands() {
                            if let ValueId::Local(v) = operand {
                                live_values.insert(*v);
                                worklist.push(*v);
                            }
                        }
                    } else {
                        // Nothing uses this - it's dead!
                        block.mark_for_deletion(instr_id);
                        eliminated += 1;
                    }
                }
            }
        }
        
        // Clean up dead instructions
        block.remove_dead();
        
        self.stats.instructions_eliminated += eliminated;
        self.stats.unused_phi_removed += block.cleanup_unused_phis();
        
        eliminated > 0
    }

    /// Get total statistics
    pub fn get_stats(&self) -> &DCEStats {
        &self.stats
    }
}

/// Helper macro for logging during development
macro_rules! log {
    ($($t:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!($($t)*);
    };
}
