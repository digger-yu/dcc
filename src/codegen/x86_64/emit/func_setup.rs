// src/codegen/x86_64/emit/func_setup.rs
// Per-function setup helpers: frame bookkeeping, local analysis walks and
// small register/stack helpers used by compile_func. Manually migrated from
// the pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::x86_64::Codegen;
use std::collections::HashMap;

impl Codegen {
    pub fn reset_reg_tracker(&mut self) {
        self.reg_allocated = 0;
        self.callee_saved_used = 0;
    }

    pub fn stack_reserve_bytes(&self) -> i64 {
        // 向量/bitcast 临时槽需要 3 个 16 字节槽: slot0 是
        // vector 结果约定, slot1/2 用于同尺寸重解释 cast。
        let vector_scratch = 48;
        if self.target.red_zone {
            vector_scratch
        } else {
            128 + vector_scratch
        }
    }

    pub fn parse_reg_name(name: &str) -> Option<u8> {
        match name.to_lowercase().as_str() {
            "al" | "ah" | "ax" | "eax" | "rax" | "%rax" | "%eax" | "%ax" => Some(0),
            "bl" | "bh" | "bx" | "ebx" | "rbx" | "%rbx" | "%ebx" | "%bx" => Some(3),
            "cl" | "ch" | "cx" | "ecx" | "rcx" | "%rcx" | "%ecx" | "%cx" => Some(1),
            "dl" | "dh" | "dx" | "edx" | "rdx" | "%rdx" | "%edx" | "%dx" => Some(2),
            "sil" | "si" | "esi" | "rsi" | "%rsi" | "%esi" | "%si" => Some(6),
            "dil" | "di" | "edi" | "rdi" | "%rdi" | "%edi" | "%di" => Some(7),
            "spl" | "sp" | "esp" | "rsp" | "%rsp" | "%esp" | "%sp" => Some(4),
            "bpl" | "bp" | "ebp" | "rbp" | "%rbp" | "%ebp" | "%bp" => Some(5),
            "r8" | "r8d" | "r8w" | "r8b" | "%r8" => Some(8),
            "r9" | "r9d" | "r9w" | "r9b" | "%r9" => Some(9),
            "r10" | "r10d" | "r10w" | "r10b" | "%r10" => Some(10),
            "r11" | "r11d" | "r11w" | "r11b" | "%r11" => Some(11),
            "r12" | "r12d" | "r12w" | "r12b" | "%r12" => Some(12),
            "r13" | "r13d" | "r13w" | "r13b" | "%r13" => Some(13),
            "r14" | "r14d" | "r14w" | "r14b" | "%r14" => Some(14),
            "r15" | "r15d" | "r15w" | "r15b" | "%r15" => Some(15),
            // APX EGPR (Extended General Purpose Registers) r16-r31
            "r16" | "r16d" | "r16w" | "r16b" | "%r16" => Some(16),
            "r17" | "r17d" | "r17w" | "r17b" | "%r17" => Some(17),
            "r18" | "r18d" | "r18w" | "r18b" | "%r18" => Some(18),
            "r19" | "r19d" | "r19w" | "r19b" | "%r19" => Some(19),
            "r20" | "r20d" | "r20w" | "r20b" | "%r20" => Some(20),
            "r21" | "r21d" | "r21w" | "r21b" | "%r21" => Some(21),
            "r22" | "r22d" | "r22w" | "r22b" | "%r22" => Some(22),
            "r23" | "r23d" | "r23w" | "r23b" | "%r23" => Some(23),
            "r24" | "r24d" | "r24w" | "r24b" | "%r24" => Some(24),
            "r25" | "r25d" | "r25w" | "r25b" | "%r25" => Some(25),
            "r26" | "r26d" | "r26w" | "r26b" | "%r26" => Some(26),
            "r27" | "r27d" | "r27w" | "r27b" | "%r27" => Some(27),
            "r28" | "r28d" | "r28w" | "r28b" | "%r28" => Some(28),
            "r29" | "r29d" | "r29w" | "r29b" | "%r29" => Some(29),
            "r30" | "r30d" | "r30w" | "r30b" | "%r30" => Some(30),
            "r31" | "r31d" | "r31w" | "r31b" | "%r31" => Some(31),
            _ => None,
        }
    }

    /// 回填本函数序言/尾声的 callee-saved 保存/恢复指令.
    /// 单遍编译下序言先于函数体发射, 无法预知函数体是否使用这些寄存器;
    /// 函数体生成完毕后 callee_saved_used 已完整, 将未使用寄存器的
    /// 4 字节 mov 改写为 4×0x90 (nop), 消除叶子函数的冗余保存/恢复.
    /// 尾声可能在每个 return 处内联发射, 统一在此处回填, 不依赖生成顺序.
    /// sites_base: compile_func 开头记录的 sites 起始下标 (嵌套函数隔离).
    pub fn patch_callee_saves(&mut self, sites_base: usize) {
        let used = self.callee_saved_used;
        for (pos, bit) in self.callee_save_patch_sites.drain(sites_base..) {
            if used & bit == 0 {
                let p = pos as usize;
                self.code[p..p + 4].fill(0x90);
            }
        }
    }

    pub fn emit_store_rax_r13(&mut self, off: i64) {
        self.store_stack(0, off);
    }

    pub fn ends_with_return(items: &[BlockItem]) -> bool {
        match items.last() {
            Some(BlockItem::Stmt(Stmt::Return { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Break { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Continue { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Block { b, .. })) => Self::ends_with_return(&b.items),
            Some(BlockItem::Stmt(Stmt::If { then_, else_, .. })) => {
                Self::ends_with_return(&[BlockItem::Stmt(*then_.clone())])
                    && else_
                        .as_ref()
                        .is_some_and(|e| Self::ends_with_return(&[BlockItem::Stmt(*e.clone())]))
            },
            _ => false,
        }
    }

    /// Collect all label names defined in a statement tree.
    /// Used for non-local goto detection in nested functions.
    pub fn collect_local_labels_stmt(stmt: &Stmt, out: &mut std::collections::HashSet<String>) {
        match stmt {
            Stmt::Label { name, stmt, .. } => {
                out.insert(name.name.clone());
                Self::collect_local_labels_stmt(stmt, out);
            },
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        Self::collect_local_labels_stmt(s, out);
                    }
                }
            },
            Stmt::If { then_, else_, .. } => {
                Self::collect_local_labels_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_local_labels_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_local_labels_stmt(body, out);
            },
            Stmt::For { body, .. } => Self::collect_local_labels_stmt(body, out),
            Stmt::Switch { cases, .. } => {
                for c in cases {
                    for item in &c.body.items {
                        if let BlockItem::Stmt(s) = item {
                            Self::collect_local_labels_stmt(s, out);
                        }
                    }
                }
            },
            _ => {},
        }
    }

    // 扫描最大表达式嵌套深度 (binop/call 各+1)
    pub fn max_expr_depth_stmt(items: &[BlockItem]) -> u32 {
        items
            .iter()
            .map(|item| match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    v.init.as_ref().map_or(0, Self::max_expr_depth_expr)
                },
                BlockItem::Stmt(s) => Self::max_expr_depth_stmt_inner(s),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn max_expr_depth_stmt_inner(s: &Stmt) -> u32 {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::max_expr_depth_expr(e)
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                let d = Self::max_expr_depth_expr(cond);
                d.max(Self::max_expr_depth_stmt_inner(then_)).max(
                    else_
                        .as_ref()
                        .map_or(0, |e| Self::max_expr_depth_stmt_inner(e)),
                )
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::max_expr_depth_expr(cond).max(Self::max_expr_depth_stmt_inner(body))
            },
            Stmt::For { cond, body, .. } => cond
                .as_ref()
                .map_or(0, Self::max_expr_depth_expr)
                .max(Self::max_expr_depth_stmt_inner(body)),
            Stmt::Block { b, .. } => Self::max_expr_depth_stmt(&b.items),
            _ => 0,
        }
    }

    fn max_expr_depth_expr(e: &Expr) -> u32 {
        match e {
            Expr::Binop { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Call { callee, args, .. } => {
                1 + Self::max_expr_depth_expr(callee).max(
                    args.iter()
                        .map(Self::max_expr_depth_expr)
                        .max()
                        .unwrap_or(0),
                )
            },
            Expr::Comma { l, r, .. } => {
                Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Assign { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Unary { e, .. }
            | Expr::Cast { e, .. }
            | Expr::AddrOf { e, .. }
            | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => Self::max_expr_depth_expr(e),
            Expr::Index { arr, idx, .. } => {
                Self::max_expr_depth_expr(arr).max(Self::max_expr_depth_expr(idx))
            },
            Expr::Member { expr, .. } => Self::max_expr_depth_expr(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => Self::max_expr_depth_expr(cond)
                .max(Self::max_expr_depth_expr(then_))
                .max(Self::max_expr_depth_expr(else_)),
            _ => 0,
        }
    }

    pub fn pre_register_vars(
        var_map: &mut HashMap<String, (i64, Type)>,
        items: &[BlockItem],
        temp_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
    ) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    // extern 声明引用全局符号, 不分配栈槽
                    if v.is_extern && v.init.is_none() {
                        continue;
                    }
                    if v.is_static {
                        // 由分析 pass 预注册的 static local 跳过栈分配
                        if static_local_map.is_static_local(func_name, &v.name.name) {
                            continue;
                        }
                    }
                    // VLA: 追加 16 字节元信息槽 (与 register_var 一致:
                    // 基址 + 运行时行大小 + 声明时总大小)。
                    let is_vla = Self::type_has_vla(&v.ty);
                    let alloc_sz = if is_vla {
                        v.ty.size().saturating_add(16)
                    } else {
                        v.ty.size()
                    };
                    let slots = ((alloc_sz.saturating_add(7) / 8).max(1)) as i64;
                    // 同名变量在不同作用域出现多次时, or_insert 不插入但
                    // total_slots 仍递增会导致偏移计算错误。
                    if !var_map.contains_key(&v.name.name) {
                        let used = total_slots.saturating_mul(8);
                        let hdr = 48i64 + temp_slots as i64 * 8;
                        let off = -(used + hdr) - (slots.saturating_sub(1) * 8);
                        var_map.insert(v.name.name.clone(), (off, v.ty.clone()));
                        *total_slots = total_slots.saturating_add(slots);
                        let rz_slots = kasan_rz_slots as i64;
                        *total_slots = total_slots.saturating_add(rz_slots);
                        for j in 0..kasan_rz_slots {
                            let rz_name = format!("__kasan_rz_{}_{}", v.name.name, j);
                            let rz_off = off - slots * 8 - j as i64 * 8;
                            var_map.entry(rz_name).or_insert((rz_off, Type::Void));
                        }
                    }
                },
                BlockItem::Stmt(s) => {
                    Self::pre_register_stmt(
                        var_map,
                        s,
                        temp_slots,
                        kasan_rz_slots,
                        static_local_map,
                        func_name,
                        total_slots,
                    );
                },
                _ => {},
            }
        }
    }

    fn pre_register_stmt(
        var_map: &mut HashMap<String, (i64, Type)>,
        s: &Stmt,
        temp_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
    ) {
        match s {
            Stmt::Block { b, .. } => {
                Self::pre_register_vars(
                    var_map,
                    &b.items,
                    temp_slots,
                    kasan_rz_slots,
                    static_local_map,
                    func_name,
                    total_slots,
                );
            },
            Stmt::If { then_, else_, .. } => {
                Self::pre_register_stmt(
                    var_map,
                    then_,
                    temp_slots,
                    kasan_rz_slots,
                    static_local_map,
                    func_name,
                    total_slots,
                );
                if let Some(e) = else_ {
                    Self::pre_register_stmt(
                        var_map,
                        e,
                        temp_slots,
                        kasan_rz_slots,
                        static_local_map,
                        func_name,
                        total_slots,
                    );
                }
            },
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::pre_register_stmt(
                    var_map,
                    body,
                    temp_slots,
                    kasan_rz_slots,
                    static_local_map,
                    func_name,
                    total_slots,
                );
            },
            Stmt::Switch { cases, .. } => {
                for case in cases {
                    Self::pre_register_vars(
                        var_map,
                        &case.body.items,
                        temp_slots,
                        kasan_rz_slots,
                        static_local_map,
                        func_name,
                        total_slots,
                    );
                }
            },
            _ => {},
        }
    }

    pub fn collect_direct_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Func(f)) if f.body.is_some() => out.push(f.clone()),
                BlockItem::Stmt(s) => Self::collect_direct_nested_funcs_stmt(s, out),
                _ => {},
            }
        }
    }

    fn collect_direct_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_direct_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_direct_nested_funcs_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_direct_nested_funcs_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                Self::collect_direct_nested_funcs_stmt(body, out);
            },
            _ => {},
        }
    }

    pub fn emit_zero_init_frame(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        // 保存 rcx, rdi, rax (它们可能在后续代码中使用)
        self.emit(&[0x50]); // push rax
        self.emit(&[0x51]); // push rcx
        self.emit(&[0x57]); // push rdi

        // xor eax, eax (rax = 0)
        self.emit(&[0x31, 0xC0]);
        // mov rcx, frame_size / 8
        self.emit(&[0x48, 0xB9]);
        self.emit(&((frame_size as u64) / 8).to_le_bytes());
        // rdi = r13 - frame_size (帧底部)
        self.emit_lea_r13(7, -frame_size);
        // rep stosq: F3 48 AB
        self.emit(&[0xF3, 0x48, 0xAB]);

        // 恢复 rdi, rcx, rax
        self.emit(&[0x5F]); // pop rdi
        self.emit(&[0x59]); // pop rcx
        self.emit(&[0x58]); // pop rax
    }

    pub fn emit_stack_clash_probes(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        let page: i64 = 4096;
        let pages = frame_size / page;
        let remaining = frame_size % page;
        if pages > 0 {
            self.mov_imm(11, pages as i128);
            let loop_start = self.pos();
            self.emit(&[0x48, 0x81, 0xEC, 0x00, 0x10, 0x00, 0x00]); // sub rsp, 4096
            self.emit(&[0xC6, 0x04, 0x24, 0x00]); // mov byte ptr [rsp], 0
            self.emit(&[0x49, 0xFF, 0xCB]); // dec r11
            let loop_end = self.pos();
            let disp = loop_start as i64 - loop_end as i64 - 2;
            self.emit(&[disp as i8 as u8]); // jnz loop
        }
        self.emit_sub_rsp(remaining);
    }

    pub fn emit_xmm_r13_disp(&mut self, pfx: u8, op: u8, xmm: u8, off: i64) {
        let rex = 0x41 | if xmm >= 8 { 0x04 } else { 0 };
        if pfx != 0 {
            self.emit(&[pfx, rex, 0x0F, op]);
        } else {
            self.emit(&[rex, 0x0F, op]);
        }
        if Self::fits_disp32(off) {
            self.emit_r13_modrm_disp(xmm, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = 0x41 | if xmm >= 8 { 0x04 } else { 0 } | 0x01;
            if pfx != 0 {
                self.emit(&[pfx, rex, 0x0F, op]);
            } else {
                self.emit(&[rex, 0x0F, op]);
            }
            self.emit_reg_indirect_modrm(xmm, 11);
        }
    }

    pub fn store_xmm32_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0xF3, 0x11, xmm, off);
    }

    pub fn emit_func_epilogue(&mut self) {
        // Stack canary 检查: 在恢复 rsp 前验证金丝雀值
        if self.target.stack_protector && self.frame_size >= 8 {
            // mov r11, [r13 - 8]  → 加载存储的 canary 到临时寄存器
            self.emit(&[0x4d, 0x8b, 0x9d, 0xf8, 0xff, 0xff, 0xff]);
            // xor r11, seg:[0x28]  → 与原始 canary 比较
            // seg = fs (0x64) or gs (0x65) based on guard register
            let seg_prefix = match self.target.stack_protector_guard_reg {
                crate::cli::StackGuardReg::Gs => 0x65u8,
                _ => 0x64u8, // Fs is default
            };
            self.emit(&[seg_prefix, 0x4c, 0x33, 0x1c, 0x25, 0x28, 0x00, 0x00, 0x00]);
            // Always call __stack_chk_fail on canary mismatch.
            // Both ET_REL and ET_EXEC use the same path:
            // __stack_chk_fail is emitted as an undefined global
            // symbol (provided by libc / kernel runtime).
            self.emit(&[0x74, 0x05]); // je .+5 (canary OK → skip)
            self.call_label("__stack_chk_fail");
        }
        // 恢复被调用者保存寄存器: rbx, r12, r14, r15
        // 同序言: 无条件发射, 未使用的寄存器由 patch_callee_saves 改为 nop
        // mov rbx, [r13 - 16]  (REX.WB: r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x01));
        self.emit(&[0x49, 0x8b, 0x5d, 0xf0]);
        // mov r12, [r13 - 24]  (REX.WRB: r12 needs REX.R, r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x02));
        self.emit(&[0x4d, 0x8b, 0x65, 0xe8]);
        // mov r14, [r13 - 32]  (REX.WRB: r14 needs REX.R, r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x04));
        self.emit(&[0x4d, 0x8b, 0x75, 0xe0]);
        // mov r15, [r13 - 40]  (REX.WRB: r15 needs REX.R+REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x08));
        self.emit(&[0x4f, 0x8b, 0x7d, 0xd8]);
        // mov rsp, r13: 恢复栈指针
        self.emit(&[0x49, 0x8b, 0xe5]);
        self.emit(&[0x41, 0x5d]); // pop r13
        if self.target.frame_pointer {
            self.emit(&[0x5D]); // pop rbp
        }
        // noreturn: 在 ret 前插入 trap 指令 (ud2)
        // 防止函数未正常返回时 (如 panic/abort/longjmp)
        // 执行流落入调用者后续代码
        if self.current_is_noreturn {
            self.emit(&[0x0F, 0x0B]); // ud2 - undefined instruction trap
        }
        self.emit_return_tail();
    }

    pub fn emit_return_tail(&mut self) {
        if self.target.function_return_thunk_extern {
            self.jmp_external("__x86_return_thunk");
        } else {
            self.emit(&[0xC3]);
        }
    }
}
