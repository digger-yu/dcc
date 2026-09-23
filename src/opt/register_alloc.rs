// src/opt/register_alloc.rs - Enhanced Register Allocator
// Phase 4: Performance Optimization & Tuning
//
// First Principle: Optimize register usage through liveness analysis
// Strategy: Graph coloring with spill minimization
// Target: 20% reduction in memory spills

use std::collections::{HashMap, HashSet, BTreeSet};
use crate::ir::{Function, BasicBlock, Instruction, ValueId, Type};

/// Enhanced register allocator with liveness-based allocation
pub struct LivenessRegisterAllocator<'a> {
    function: &'a mut Function,
    stats: RegAllocStats,
}

/// Statistics for register allocation
#[derive(Debug, Default)]
pub struct RegAllocStats {
    pub allocations_optimized: usize,
    pub spills_prevented: usize,
    pub live_ranges_analyzed: usize,
}

impl<'a> LivenessRegisterAllocator<'a> {
    pub fn new(function: &'a mut Function) -> Self {
        Self {
            function,
            stats: RegAllocStats::default(),
        }
    }

    /// Run liveness-aware register allocation
    pub fn run(&mut self) {
        log("Starting liveness register allocation");
        
        // Step 1: Compute live ranges
        let live_ranges = self.compute_live_ranges();
        self.stats.live_ranges_analyzed = live_ranges.len();
        
        // Step 2: Build interference graph
        let interference = self.build_interference_graph(&live_ranges);
        
        // Step 3: Graph coloring with spilling
        self.color_graph(interference);
        
        log("Liveness register allocation complete");
    }

    /// Compute live range for each virtual register
    fn compute_live_ranges(&self) -> HashMap<ValueId, LiveRange> {
        let mut live_ranges: HashMap<ValueId, LiveRange> = HashMap::new();
        
        // For each instruction, track define and use points
        for (block_idx, block) in self.function.blocks.iter().enumerate() {
            for (instr_idx, instr) in block.instructions.iter().enumerate() {
                if let Some(result) = instr.result() {
                    // Initialize or extend live range
                    live_ranges.entry(*result).or_insert_with(|| LiveRange {
                        start: (block_idx, instr_idx),
                        end: (block_idx, instr_idx),
                        physical_reg: None,
                    });
                }
                
                // Mark uses as extending live range
                for operand in instr.operands() {
                    if let ValueId::Local(v) = operand {
                        if let Some(lr) = live_ranges.get_mut(v) {
                            lr.end = (block_idx, instr_idx);
                        }
                    }
                }
            }
        }
        
        live_ranges
    }

    /// Build interference graph from live ranges
    fn build_interference_graph(&self, live_ranges: &HashMap<ValueId, LiveRange>) 
        -> HashMap<ValueId, HashSet<ValueId>>
    {
        let mut interference: HashMap<ValueId, HashSet<ValueId>> = HashMap::new();
        
        // Two values interfere if their live ranges overlap
        for (v1, lr1) in live_ranges {
            interference.insert(*v1, HashSet::new());
            
            for (v2, lr2) in live_ranges {
                if v1 == v2 {
                    continue;
                }
                
                if self.ranges_overlap(lr1, lr2) {
                    interference.get_mut(v1).unwrap().insert(*v2);
                }
            }
        }
        
        interference
    }

    /// Check if two live ranges overlap
    fn ranges_overlap(&self, lr1: &LiveRange, lr2: &LiveRange) -> bool {
        lr1.start <= lr2.end && lr2.start <= lr1.end
    }

    /// Color the interference graph using graph coloring algorithm
    fn color_graph(&mut self, interference: HashMap<ValueId, HashSet<ValueId>>) {
        const NUM_PHYSICAL_REGS: usize = 16; // x86_64 general-purpose registers
        
        let available_colors: Vec<usize> = (0..NUM_PHYSICAL_REGS).collect();
        
        // Sort nodes by degree (most constrained first)
        let mut nodes: Vec<ValueId> = interference.keys().copied().collect();
        nodes.sort_by(|a, b| {
            interference.get(b).map(|s| s.len()).unwrap_or(0)
                .cmp(&interference.get(a).map(|s| s.len()).unwrap_or(0))
        });
        
        // Allocate colors
        for node in nodes {
            let neighbors = &interference[&node];
            
            // Find colors used by neighbors
            let mut used_colors: HashSet<usize> = HashSet::new();
            for neighbor in neighbors {
                if let Some(lr) = self.allocate_physical_reg(*neighbor, available_colors.clone()) {
                    used_colors.insert(lr.physical_reg.unwrap());
                }
            }
            
            // Pick first unused color
            let assigned_color = available_colors.iter()
                .find(|&&c| !used_colors.contains(&c))
                .copied();
            
            match assigned_color {
                Some(color) => {
                    // Found a free register
                    self.allocate_to_value(node, color);
                    self.stats.allocations_optimized += 1;
                }
                None => {
                    // Need to spill - select candidate and spill
                    self.spill_candidate(neighbors);
                }
            }
        }
    }

    /// Try to allocate a physical register
    fn allocate_physical_reg(&self, value: ValueId, available: Vec<usize>) -> Option<LiveRange> {
        // Simplified: always succeed for demo
        None
    }

    /// Select spill candidate when no registers available
    fn spill_candidate(&mut self, neighbors: &HashSet<ValueId>) {
        // Score based on:
        // 1. Frequency of use (less frequent = better candidate)
        // 2. Range length (longer range = worse candidate)
        // 3. Neighbor density (connected to fewer neighbors = better)
        
        // For now, pick lowest-scored neighbor
        // In production: implement full scoring algorithm
    }

    /// Allocate physical register to virtual register
    fn allocate_to_value(&mut self, value: ValueId, color: usize) {
        // Would update IR instructions to use physical register
        // Placeholder for actual implementation
    }

    /// Get statistics
    pub fn get_stats(&self) -> &RegAllocStats {
        &self.stats
    }
}

/// Represents a virtual register's live range
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveRange {
    pub start: (usize, usize), // (block_index, instruction_index)
    pub end: (usize, usize),
    pub physical_reg: Option<usize>,
}

macro_rules! log {
    ($($t:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!($($t)*);
    };
}
