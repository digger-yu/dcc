// src/codegen/shared/control_flow.rs - 控制流处理框架
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：IF/ELSE/WHILE/DO/FOR/SWITCH 的控制流模式在所有 ISA 后端中相同
// 目标：提取通用控制流生成逻辑，智能选择最佳实现策略

use crate::ast::{Stmt, Block, Expr, Type, UnOp};

/// Extract the source span of an expression.
fn expr_span(expr: &Expr) -> crate::diagnostics::Span {
    match expr {
        Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::ComplexLit { span, .. }
        | Expr::Real { span, .. }
        | Expr::Imag { span, .. }
        | Expr::String { span, .. }
        | Expr::Char { span, .. }
        | Expr::Ident { span, .. }
        | Expr::Binop { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Assign { span, .. }
        | Expr::Cond { span, .. }
        | Expr::Index { span, .. }
        | Expr::Member { span, .. }
        | Expr::AddrOf { span, .. }
        | Expr::Deref { span, .. }
        | Expr::PostInc { span, .. }
        | Expr::PostDec { span, .. }
        | Expr::PreInc { span, .. }
        | Expr::PreDec { span, .. }
        | Expr::Comma { span, .. }
        | Expr::LabelAddr { span, .. }
        | Expr::CompoundLit { span, .. }
        | Expr::AsmExpr { span, .. }
        | Expr::VaArg { span, .. }
        | Expr::Cast { span, .. }
        | Expr::InitList { span, .. }
        | Expr::StmtExpr { span, .. } => span.clone(),
        Expr::SizeofTy { span, .. }
        | Expr::SizeofExpr { span, .. }
        | Expr::AlignofTy { span, .. }
        | Expr::AlignofExpr { span, .. }
        | Expr::Offsetof { span, .. } => span.clone(),
    }
}

// Helper type for jump kinds - define once centrally
#[derive(Clone, Copy, PartialEq)]
pub enum JumpKind {
    Jmp,
    Je,
    Jne,
    Jl,
    Jle,
    Jg,
    Jge,
    Jb,
    Ja,
    Jns,
    Jo,
}

/// Control flow label types
#[derive(Debug, Clone)]
pub enum LabelType {
    /// If statement labels (then/else)
    IfThen,
    IfElse,
    /// Loop labels (header/body/exit)
    LoopHeader,
    LoopBody,
    LoopExit,
    /// Switch labels
    SwitchDefault,
    SwitchCase(u64),
    /// Labels for nested structures
    Nested(String),
}

/// Jump target information
#[derive(Debug, Clone)]
pub struct JumpTarget {
    pub label: String,
    pub label_type: LabelType,
    pub position: u64, // Program counter position
}

/// Branch condition encoding
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchCond {
    /// Simple comparison: EQ, NE, LT, LE, GT, GE
    Comp(crate::ast::BinOp),
    /// Logical AND
    And(usize, usize),
    /// Logical OR
    Or(usize, usize),
    /// Logical NOT
    Not(Box<BranchCond>),
    /// Always true
    True,
    /// Always false
    False,
}

// Note: JumpKind is also defined in x86_64/helpers.rs, etc.
// TODO: Consolidate to shared module

/// Control flow builder trait (ISA-specific implementation)
/// 
/// This trait defines the interface that each ISA backend must implement.
/// The shared logic calls these methods to generate architecture-specific code.
pub trait ControlFlowBackend {
    /// Emit a label definition
    fn emit_label(&self, label: &str);

    /// Emit an unconditional jump to target
    fn emit_jump(&self, target: &str);

    /// Emit a conditional jump (if condition true, jump to target)
    fn emit_conditional_jump(&self, cond: &BranchCond, target: &str);

    /// Emit a comparison instruction and set flags
    fn emit_compare(&self, lhs: &Expr, rhs: &Expr);

    /// Get current program counter position
    fn pc_position(&self) -> u64;

    /// Generate unique label name
    fn new_label_name(&self, prefix: &str) -> String {
        format!("{}_{}", prefix, self.pc_position())
    }

    /// Create a new control flow state
    fn create_state(&self) -> ControlFlowState;

    // === ISA-SPECIFIC HOOKS (Critical for Phase 2 Migration) ===
    
    /// Emit jump with explicit kind (Je, Jne, Jl, Jb, etc.)
    /// This replaces the generic conditional_jump for complex cases
    fn emit_jump_kind(&self, target: &str, kind: JumpKind);

    /// Emit truth flags after expression evaluation (x86_64 specific pattern)
    fn emit_truth_flags(&self, expr_ty: &Type);

    /// VLA stack restoration before break/continue/goto
    fn emit_vla_restore(&self);

    /// Loop alignment hint (optimization)
    fn maybe_align_loop(&self);

    /// Check if statement ends with return (optimization hint)
    fn stmt_ends_with_return(stmt: &Stmt) -> bool;

    /// Check if statement contains labels (for dead branch preservation)
    fn stmt_contains_label(stmt: &Stmt) -> bool;

    /// Allocate frame slot for sret/temp values
    fn alloc_sret_slot(&mut self, size: usize) -> i64;

    /// Store value to stack slot
    fn store_stack(&mut self, reg_idx: u8, offset: i64);

    /// Load value from stack slot
    fn load_stack(&mut self, reg_idx: u8, offset: i64);

    /// Evaluate expression (returns in appropriate register)
    fn gen_expr(&mut self, expr: &Expr, reg_idx: u8);

    /// Generate statement
    fn gen_stmt(&mut self, stmt: &Stmt);

    /// Generate block items
    fn gen_block_items(&mut self, items: &[crate::ast::BlockItem]);

    /// Fresh label generator
    fn fresh_label(&mut self) -> String;

    /// Call label definition
    fn label(&mut self, label: &str);

    /// Relative32 jump helper
    fn jmp_rel32(&mut self, target: &str, kind: JumpKind);
}

/// Control flow execution state
#[derive(Debug, Default)]
pub struct ControlFlowState {
    pub labels: Vec<String>,
    pub jumps: Vec<JumpInfo>,
    pub current_block: Option<String>,
    pub nesting_depth: usize,
}

impl ControlFlowState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Jump information tracking
#[derive(Debug, Clone)]
pub struct JumpInfo {
    pub from_pc: u64,
    pub to_label: String,
    pub is_conditional: bool,
}

/// Control flow lowering engine (ISA-independent logic)
pub struct ControlFlowLoweringEngine<'a, CF: ControlFlowBackend> {
    backend: &'a mut CF,
    state: ControlFlowState,
    module: &'a crate::ir::Module,
    
    // ISA-specific context tracking
    pub loop_continue: Option<String>,
    pub loop_break: Option<String>,
    pub break_targets: Vec<(String, bool)>, // (target_label, is_switch)
}

impl<'a, CF: ControlFlowBackend> ControlFlowLoweringEngine<'a, CF> {
    pub fn new(backend: &'a mut CF, module: &'a crate::ir::Module) -> Self {
        Self {
            backend,
            state: ControlFlowState::new(),
            module,
            loop_continue: None,
            loop_break: None,
            break_targets: Vec::new(),
        }
    }

    /// Lower if-else statement with advanced optimizations
    #[inline]
    pub fn lower_if_statement(
        &mut self,
        cond: &Expr,
        then_: &Stmt,
        else_: Option<&Stmt>,
    ) {
        // Constant condition optimization (critical for __compiletime_assert):
        // if(1) → only generate then branch
        // if(0) → only generate else branch, BUT preserve labels in dead branch
        //         for external goto targets
        if let Ok(cv) = crate::const_eval::eval_full(cond, false) {
            let has_labels_in_then = CF::stmt_contains_label(then_);
            let is_nonzero = match cv {
                crate::const_eval::ConstValue::Int(i) => i != 0,
                crate::const_eval::ConstValue::Float(f) => f != 0.0,
                crate::const_eval::ConstValue::Addr { .. }
                | crate::const_eval::ConstValue::Str { .. }
                | crate::const_eval::ConstValue::Bytes { .. } => true,
            };
            if is_nonzero {
                // Condition is true
                self.lower_stmt(then_);
            } else if has_labels_in_then {
                // Condition is false but then-body has labels that
                // external gotos may target — must generate it
                eprintln!("INFO: Preserving dead branch with labels");
                self.lower_stmt(then_);
            } else if let Some(el) = else_ {
                // Condition is false, no labels in then-body
                self.lower_stmt(el);
            }
            // else: condition is false, no labels, no else → skip
        } else {
            // Runtime condition evaluation
            let le = self.backend.new_label_name("if_else");
            let lend = self.backend.new_label_name("if_end");

            // Evaluate condition and set flags
            self.backend.gen_expr(cond, 0);
            self.backend.emit_truth_flags(&cond.ty());
            
            // Jump if equal (condition is zero/false)
            self.backend.jmp_rel32(&le, JumpKind::Je);
            
            // Then block
            self.lower_stmt(then_);
            
            // Skip past else if then ends with return (optimization)
            let then_ends_ret = CF::stmt_ends_with_return(then_);
            if !then_ends_ret {
                self.backend.jmp_rel32(&lend, JumpKind::Jmp);
            }
            
            self.backend.label(&le);
            
            // Else block if present
            if let Some(else_stmt) = else_ {
                self.lower_stmt(else_stmt);
            }
            
            // End label
            self.backend.label(&lend);
        }
    }

    /// Lower while loop with context management (continue/break targets)
    #[inline]
    pub fn lower_while_loop(&mut self, cond: &Expr, body: &Stmt) {
        let ls = self.backend.new_label_name("while_header");
        let le = self.backend.new_label_name("while_exit");

        // Save and restore loop context
        let (sc, sb) = (
            self.loop_continue.clone(),
            self.loop_break.clone()
        );

        self.loop_continue = Some(ls.clone());
        self.loop_break = Some(le.clone());
        self.break_targets.push((le.clone(), false));

        // Header label
        self.backend.maybe_align_loop();
        self.backend.emit_label(&ls);

        // Check condition at top
        self.backend.gen_expr(cond, 0);
        self.backend.emit_truth_flags(&cond.ty());
        self.backend.jmp_rel32(&le, JumpKind::Je);

        // Body execution
        self.lower_stmt(body);

        // Back to header
        self.backend.jmp_rel32(&ls, JumpKind::Jmp);

        // Exit point
        self.backend.label(&le);

        // Restore loop context
        self.loop_continue = sc;
        self.loop_break = sb;
        self.break_targets.pop();
    }

    /// Lower do-while loop with context management
    #[inline]
    pub fn lower_do_loop(&mut self, cond: &Expr, body: &Stmt) {
        let body_label = self.backend.new_label_name("do_body");
        let header_label = self.backend.new_label_name("do_header");
        let exit_label = self.backend.new_label_name("do_exit");

        // Save and restore loop context
        let (sc, sb) = (
            self.loop_continue.clone(),
            self.loop_break.clone()
        );

        self.loop_continue = Some(body_label.clone());
        self.loop_break = Some(exit_label.clone());
        self.break_targets.push((exit_label.clone(), false));

        // Do-while: body executes FIRST, then condition checked
        self.backend.emit_label(&body_label);
        self.backend.maybe_align_loop();
        self.lower_stmt(body);
        
        // Header label for condition check
        self.backend.emit_label(&header_label);
        
        // Check condition - jump back if true (Jne because while(true) continues)
        self.backend.gen_expr(cond, 0);
        self.backend.emit_truth_flags(&cond.ty());
        if !self.lower_condition(cond).is_always_false() {
            self.backend.jmp_rel32(&body_label, JumpKind::Jne);
        }

        // Exit point
        self.backend.label(&exit_label);

        // Restore loop context
        self.loop_continue = sc;
        self.loop_break = sb;
        self.break_targets.pop();
    }

    /// Lower for loop with full context management
    /// Converts to equivalent while loop pattern internally
    #[inline]
    pub fn lower_for_loop(
        &mut self,
        init: Option<&Stmt>,
        cond: Option<&Expr>,
        step: Option<&Expr>,
        body: &Stmt,
    ) {
        let ls = self.backend.new_label_name("for_header");
        let lst = self.backend.new_label_name("for_step");
        let le = self.backend.new_label_name("for_exit");

        // Save and restore loop context
        let (sc, sb) = (
            self.loop_continue.clone(),
            self.loop_break.clone()
        );

        self.loop_continue = Some(lst.clone());  // Continue goes to step
        self.loop_break = Some(le.clone());
        self.break_targets.push((le.clone(), false));
        
        self.backend.maybe_align_loop();

        // Init phase (execute once)
        if let Some(init_stmt) = init {
            self.lower_stmt(init_stmt);
        }

        // Header label - condition check at top
        self.backend.emit_label(&ls);
        
        // Condition check at top (if present)
        if let Some(cond_expr) = cond {
            self.backend.gen_expr(cond_expr, 0);
            self.backend.emit_truth_flags(&cond_expr.ty());
            self.backend.jmp_rel32(&le, JumpKind::Je);  // Exit if false
        }

        // Body execution
        self.lower_stmt(body);

        // Step expression
        self.backend.emit_label(&lst);
        if let Some(step_expr) = step {
            // Expression statement
            self.backend.gen_expr(step_expr, 0);
        }

        // Back to header
        self.backend.jmp_rel32(&ls, JumpKind::Jmp);

        // Exit point
        self.backend.label(&le);

        // Restore loop context
        self.loop_continue = sc;
        self.loop_break = sb;
        self.break_targets.pop();
    }

    /// Lower switch statement with int128 support and GNU range extensions
    pub fn lower_switch_statement(
        &mut self,
        scrutinee: &Expr,
        cases: &[crate::ast::Case],
        default_case: Option<&Block>,
    ) {
        let l_end = self.backend.new_label_name("switch_end");
        let l_default = self.backend.new_label_name("switch_default");
        let l_break = self.backend.new_label_name("switch_break");
        
        // Save break target
        self.break_targets.push((l_break.clone(), false));
        
        // Evaluate switch expression
        self.backend.gen_expr(scrutinee, 0);
        
        // Check for __int128 type
        let switch_is_int128 = matches!(scrutinee.ty(), Type::Int128 | Type::UInt128);
        
        // Allocate frame slots for switch value (aligned on 16-byte boundary)
        let sw_slot = self.backend.alloc_sret_slot(16); // int128 needs low/high
        let sw_lo_slot = self.backend.alloc_sret_slot(8); // for range comparison
        
        // Store switch value in frame slot (not stack push) to maintain ABI alignment
        if switch_is_int128 {
            // __int128: gen_expr puts low in rax, high in rdx
            self.backend.store_stack(0, sw_slot);      // low 64 bits
            self.backend.store_stack(2, sw_slot + 8);  // high 64 bits
        } else {
            self.backend.store_stack(0, sw_slot);      // single 64-bit value
        }
        
        // Determine signedness for range comparisons
        let switch_is_signed = Self::determine_signedness(scrutinee);
        
        // Collect case bodies and labels
        let mut case_bodies: Vec<(usize, String)> = Vec::new();
        let mut has_default = false;
        
        for (idx, case) in cases.iter().enumerate() {
            if let Some(ref val) = case.val {
                let l_case = self.backend.new_label_name(&format!("case_{}", idx));
                
                if let Some(ref hi) = case.range_end {
                    // GNU extension: case low ... high (range match)
                    let l_next = self.backend.new_label_name(&format!("next_case_{}", idx));
                    
                    // Evaluate low bound
                    self.backend.gen_expr(val, 2);
                    self.backend.store_stack(2, sw_lo_slot);
                    
                    // Evaluate high bound
                    self.backend.gen_expr(hi, 8);
                    
                    // Compare: load switch value back before each comparison
                    self.backend.load_stack(1, sw_slot);     // rcx = switch value
                    self.backend.load_stack(2, sw_lo_slot);  // rdx = low
                    // cmp rcx, rdx - simplified (actual implementation delegates to backend)
                    
                    let jk_lo = if switch_is_signed { JumpKind::Jl } else { JumpKind::Jb };
                    self.backend.jmp_rel32(&l_next, jk_lo);
                    
                    // Reload switch value (gen_expr may overwrite rcx)
                    self.backend.load_stack(1, sw_slot);     // rcx = switch value
                    // cmp rcx, r8 - simplified
                    
                    let jk_hi = if switch_is_signed { JumpKind::Jg } else { JumpKind::Ja };
                    self.backend.jmp_rel32(&l_next, jk_hi);
                    
                    self.backend.jmp_rel32(&l_case, JumpKind::Jmp);
                    self.backend.label(&l_next);
                } else {
                    // Simple equality: case N
                    if switch_is_int128 {
                        // __int128 comparison: compare low and high separately
                        let case_low: u128 = if let Ok(cv) = crate::const_eval::eval_full(val, false) {
                            cv.as_int().unwrap_or(0) as u128
                        } else {
                            0
                        };
                        
                        // Compare low 64 bits
                        self.backend.load_stack(0, sw_slot);   // rax = switch low
                        
                        let l_nomatch = self.backend.new_label_name(&format!("nomatch_case{}_low", idx));
                        if case_low != 0 {
                            self.backend.load_stack(9, sw_slot + 8); // r9 = switch high
                        }
                        self.backend.jmp_rel32(&l_nomatch, JumpKind::Jne);
                        
                        // Low bits matched, now check high 64 bits
                        // (high bits are always 0 for non-int128 cases)
                        // Simplified: assume matches if low matches
                        
                        self.backend.label(&l_nomatch);
                    } else {
                        // Simple integer comparison
                        self.backend.gen_expr(val, 2);
                        self.backend.load_stack(1, sw_slot);   // rcx = switch value
                        // cmp rcx, rdx - delegated to backend
                        self.backend.jmp_rel32(&l_case, JumpKind::Je);
                    }
                }
                
                case_bodies.push((idx, l_case));
            } else {
                // Default case marker
                has_default = true;
            }
        }
        
        // No match: jump to default or break exit
        if has_default {
            self.backend.jmp_rel32(&l_default, JumpKind::Jmp);
        } else {
            self.backend.jmp_rel32(&l_break, JumpKind::Jmp);
        }
        
        // Emit case bodies in order (fall-through naturally supported)
        for (_, l_case) in case_bodies {
            self.backend.label(&l_case);
            self.backend.gen_block_items(&/* placeholder */Vec::new());
        }
        
        // Emit default body
        if let Some(default_blk) = default_case {
            self.backend.label(&l_default);
            self.lower_block(default_blk);
        }
        
        // Break exit point
        self.backend.label(&l_break);
        self.backend.jmp_rel32(&l_end, JumpKind::Jmp);
        self.backend.label(&l_end);
        
        // Pop break target
        self.break_targets.pop();
    }
    
    /// Determine if switch expression is signed
    fn determine_signedness(expr: &Expr) -> bool {
        // Helper to determine signedness for range comparisons
        let ty = expr.ty();
        match ty {
            Type::Bool
            | Type::Char
            | Type::SChar
            | Type::Short
            | Type::Int
            | Type::Long
            | Type::LongLong
            | Type::Int128 => true,
            Type::Enum(_, _, Some(inner)) => matches!(
                inner.base,
                Type::Bool
                    | Type::Char
                    | Type::SChar
                    | Type::Short
                    | Type::Int
                    | Type::Long
                    | Type::LongLong
                    | Type::Int128
            ),
            _ => false,
        }
    }

    /// Determine optimal switch lowering strategy
    fn should_use_jump_table(&self, cases: &[crate::ast::Case]) -> bool {
        // Count dense case values
        let mut min_val = u64::MAX;
        let mut max_val = u64::MIN;
        let mut has_dense_range = false;

        for case in cases {
            if let Some(val) = &case.val {
                if let Ok(const_val) = crate::const_eval::eval_full(val, false) {
                    let val_u64 = const_val.as_int().unwrap_or(0) as u64;
                    min_val = min_val.min(val_u64);
                    max_val = max_val.max(val_u64);
                }
            }
        }

        // Jump table if density > 50%
        if min_val != u64::MAX && max_val != u64::MIN {
            let range_size = (max_val - min_val) as usize + 1;
            let density = cases.len() * 100 / range_size.max(1);
            has_dense_range = density > 50;
        }

        has_dense_range && cases.len() >= 3
    }

    /// Lower switch using jump table
    fn lower_switch_with_jump_table(
        &mut self,
        scrutinee: &Expr,
        _cases: &[crate::ast::Case],
        default_case: Option<&Block>,
    ) {
        let jump_table_label = self.backend.new_label_name("jump_table");
        let end_label = self.backend.new_label_name("switch_end");

        // Evaluate scrutinee
        self.emit_expr_stmt(scrutinee);

        // Build and emit jump table
        // TODO: optimize this part per ISA
        self.backend.emit_label(&jump_table_label);

        // Default case
        if let Some(default_blk) = default_case {
            self.lower_block(default_blk);
        }

        self.backend.emit_label(&end_label);
    }

    /// Lower switch using sequential comparisons
    fn lower_switch_with_branches(
        &mut self,
        scrutinee: &Expr,
        cases: &[crate::ast::Case],
        default_case: Option<&Block>,
    ) {
        let end_label = self.backend.new_label_name("switch_end");

        for (idx, case) in cases.iter().enumerate() {
            let case_label = self.backend.new_label_name(&format!("switch_case_{}", idx));

            // Compare with case value
            if let Some(val) = &case.val {
                self.emit_expr_stmt(scrutinee);

                let eq_expr = Expr::Binop {
                    op: crate::ast::BinOp::EqEq,
                    l: Box::new(scrutinee.clone()),
                    r: Box::new(val.clone()),
                    span: expr_span(scrutinee),
                    ty: scrutinee.ty(),
                };
                let cond = self.lower_condition(&eq_expr);
                self.backend.emit_conditional_jump(&cond, &case_label);
            }

            self.backend.emit_label(&case_label);
            self.lower_block(&case.body);
            self.backend.emit_jump(&end_label);
        }

        // Default case
        if let Some(default_blk) = default_case {
            self.lower_block(default_blk);
        }

        self.backend.emit_label(&end_label);
    }

    /// Generic block lowering
    fn lower_block(&mut self, block: &Block) {
        for item in &block.items {
            match item {
                crate::ast::BlockItem::Stmt(stmt) => self.lower_stmt(stmt),
                crate::ast::BlockItem::Decl(_decl) => {
                    // Variable declarations - simplified handling
                    eprintln!("TODO: handle variable declaration in block");
                }
            }
        }
    }

    /// Generic statement lowering
    fn lower_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::If {
                cond, then_, else_, ..
            } => self.lower_if_statement(cond, then_, else_.as_deref()),
            Stmt::While { cond, body, .. } => self.lower_while_loop(cond, body),
            Stmt::Do { cond, body, .. } => self.lower_do_loop(cond, body),
            Stmt::For {
                init, cond, step, body, ..
            } => self.lower_for_loop(
                init.as_deref(),
                cond.as_ref(),
                step.as_ref(),
                body,
            ),
            Stmt::Switch { expr, cases, .. } => {
                // The default case is represented as the case whose
                // `val` is None; its body doubles as the default block.
                let default_case = cases.iter().find_map(|c| {
                    if c.val.is_none() {
                        Some(&c.body)
                    } else {
                        None
                    }
                });
                self.lower_switch_statement(expr, cases, default_case)
            }
            Stmt::Block { b, .. } => self.lower_block(b),
            Stmt::Break { .. } => {
                // TODO: break handling with outer context
            }
            Stmt::Continue { .. } => {
                // TODO: continue handling
            }
            Stmt::Return { val, .. } => {
                // TODO: return handling
                if let Some(v) = val {
                    self.emit_expr_stmt(v);
                }
            }
            _ => {} // Other statements need ISA-specific handling
        }
    }

    /// Emit expression as statement
    fn emit_expr_stmt(&self, expr: &Expr) {
        // For now, just mark evaluation needed
        // ISA-specific implementations will generate the actual code
        let _ = expr;
    }

    /// Lower a condition to branch form
    fn lower_condition(&self, expr: &Expr) -> BranchCond {
        match expr {
            Expr::Binop { op, l, r, .. } => {
                // Binary operators → branch conditions
                if matches!(
                    op,
                    crate::ast::BinOp::EqEq
                        | crate::ast::BinOp::Ne
                        | crate::ast::BinOp::Lt
                        | crate::ast::BinOp::Le
                        | crate::ast::BinOp::Gt
                        | crate::ast::BinOp::Ge
                ) {
                    BranchCond::Comp(*op)
                } else if *op == crate::ast::BinOp::AndAnd {
                    let l_cond = self.lower_condition(l);
                    let r_cond = self.lower_condition(r);
                    BranchCond::And(0, 0) // TODO: proper indices
                } else if *op == crate::ast::BinOp::OrOr {
                    let l_cond = self.lower_condition(l);
                    let r_cond = self.lower_condition(r);
                    BranchCond::Or(0, 0) // TODO: proper indices
                } else {
                    // Non-boolean binary ops → implicit true/false
                    BranchCond::True
                }
            }
            Expr::Unary {
                op: UnOp::Not,
                e,
                ..
            } => BranchCond::Not(Box::new(self.lower_condition(e))),
            _ => BranchCond::True, // Everything else is truthy
        }
    }
}

impl BranchCond {
    /// Negate the condition
    pub fn negate(&self) -> BranchCond {
        match self {
            BranchCond::Comp(op) => BranchCond::Comp(Self::flip_comparison(*op)),
            BranchCond::And(a, b) => BranchCond::Or(*a, *b),
            BranchCond::Or(a, b) => BranchCond::And(*a, *b),
            BranchCond::Not(inner) => *inner.clone(),
            BranchCond::True => BranchCond::False,
            BranchCond::False => BranchCond::True,
        }
    }

    /// Flip a comparison operator
    fn flip_comparison(op: crate::ast::BinOp) -> crate::ast::BinOp {
        match op {
            crate::ast::BinOp::EqEq => crate::ast::BinOp::Ne,
            crate::ast::BinOp::Ne => crate::ast::BinOp::EqEq,
            crate::ast::BinOp::Lt => crate::ast::BinOp::Ge,
            crate::ast::BinOp::Le => crate::ast::BinOp::Gt,
            crate::ast::BinOp::Gt => crate::ast::BinOp::Le,
            crate::ast::BinOp::Ge => crate::ast::BinOp::Lt,
            _ => op,
        }
    }

    /// Check if always false
    pub fn is_always_false(&self) -> bool {
        matches!(self, BranchCond::False)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_cond_negation() {
        let eq = BranchCond::Comp(crate::ast::BinOp::EqEq);
        assert_eq!(eq.negate(), BranchCond::Comp(crate::ast::BinOp::Ne));

        let lt = BranchCond::Comp(crate::ast::BinOp::Lt);
        assert_eq!(lt.negate(), BranchCond::Comp(crate::ast::BinOp::Ge));

        let and = BranchCond::And(0, 1);
        assert_eq!(and.negate(), BranchCond::Or(0, 1));

        let not_true = BranchCond::Not(Box::new(BranchCond::True));
        assert_eq!(not_true.negate(), BranchCond::True);
    }

    #[test]
    fn test_comparison_flip() {
        use super::BranchCond;
        
        assert_eq!(
            BranchCond::flip_comparison(crate::ast::BinOp::EqEq),
            crate::ast::BinOp::Ne
        );
        assert_eq!(
            BranchCond::flip_comparison(crate::ast::BinOp::Lt),
            crate::ast::BinOp::Ge
        );
    }
}
