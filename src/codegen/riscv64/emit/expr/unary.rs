// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{Riscv64Codegen, align_up_u64, value_type_size};
use crate::codegen::shared::elf::{self, ElfRela};
use crate::ir::{ExprLiteral, TypedExpr, ValueType};

impl Riscv64Codegen {
    pub(crate) fn load_global_addr_a0(&mut self, name: &str) {
        // TLS 全局变量: 分发 TLS 访问序列 (获取 TLS 变量地址到 a0)
        if self.tls_globals.contains_key(name) {
            self.emit_tls_le_access(name);
            return;
        }
        if self.relocatable {
            let pos = self.pos();
            self.emit_auipc_a0(0);
            if let Some(&idx) = self.sym_index.get(name) {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: idx,
                    rel_type: elf::R_RISCV_PCREL_HI20,
                    addend: 0,
                });
                let pos2 = self.pos();
                self.emit_ld_a0_a0(0);
                self.rodata_relocations.push(ElfRela {
                    offset: pos2,
                    sym_index: idx,
                    rel_type: elf::R_RISCV_PCREL_LO12_I,
                    addend: 0,
                });
            }
        } else if let Some(&off) = self.global_rodata.get(name) {
            let addr = self.entry + align_up_u64(self.code.len() as u64, 8) + off;
            self.li_a0(addr as i32);
        }
    }

    // ========================================================================
    // 原子操作内建函数 (__atomic_* 系列)
    // ========================================================================

    /// 获取指针参数所指元素的大小
    pub(crate) fn atomic_elem_size(&self, args: &[TypedExpr], idx: usize) -> usize {
        if idx >= args.len() {
            return 8;
        }
        match &args[idx].ty {
            ValueType::Pointer(inner) => value_type_size(inner),
            ValueType::Array(inner, _) => value_type_size(inner),
            _ => 8,
        }
    }

    /// 尝试从 IR 表达式中提取原子内存序常量值.
    pub(crate) fn eval_mem_order(&self, arg: &TypedExpr) -> i32 {
        match &arg.literal {
            Some(ExprLiteral::Int(v)) => *v as i32,
            _ => 5, // default SEQ_CST for safety
        }
    }

    /// 原子内建函数 codegen: 返回 true 表示已处理
    /// 参数约定: args[0]=ptr (a0), args[1]=val (a0)
    /// 结果返回在 a0
    pub(crate) fn gen_atomic_builtin(&mut self, name: &str, args: &[TypedExpr]) -> bool {
        match name {
            "__atomic_load_n" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[1]);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr (save)
                match ord {
                    0 => {} // RELAXED: no fence
                    2 => { // ACQUIRE: fence r, rw
                        self.emit_fence();
                    }
                    _ => { // SEQ_CST/RELEASE: fence rw, rw
                        self.emit_fence();
                    }
                }
                // load from [a1] to a0
                self.emit_load_sz(10, 11, 0, sz);
                true
            }
            "__atomic_store_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[2]);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = val
                self.gen_expr(&args[1]);
                match ord {
                    0 => {} // RELAXED: no fence
                    _ => { // SEQ_CST/ACQUIRE/RELEASE: fence before store
                        self.emit_fence();
                    }
                }
                // store a0 to [a1]
                self.emit_store_sz(10, 11, 0, sz);
                true
            }
            "__atomic_exchange_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = val
                self.gen_expr(&args[1]);
                self.mv_a_reg(12); // a2 = val
                // amoswap [a1], a2 -> a0 (old value)
                self.emit_amoswap(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_add" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoadd [a1], a2 -> a0 (old value)
                self.emit_amoadd(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_sub" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                // negate a0 (a0 = -val)
                self.emit_neg(10, 10);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoadd [a1], a2 -> a0 (old value, adding negative)
                self.emit_amoadd(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_and" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoand [a1], a2 -> a0
                self.emit_amoand(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_or" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoor [a1], a2 -> a0
                self.emit_amoor(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_xor" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.mv_a_reg(12);
                // fence rw, rw only for SEQ_CST before AMO
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                // amoxor [a1], a2 -> a0
                self.emit_amoxor(sz, 10, 12, 11);
                true
            }
            "__atomic_fetch_nand" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                self.gen_expr(&args[1]);
                self.mv_a_reg(12); // a2 = val
                let ord = self.eval_mem_order(&args[2]);
                if ord == 5 { self.emit_fence(); }
                let l_retry = self.fresh_label("atomic_nand");
                let l_done = self.fresh_label("atomic_nand_done");
                self.label(&l_retry);
                self.emit_lr(sz, 10, 11);    // a0 = old
                self.emit_and(13, 10, 12);    // a3 = old & val
                self.emit_xori(13, 13, -1);   // a3 = ~(old & val)
                self.emit_sc(sz, 15, 13, 11);
                self.emit_bnez(15, &l_retry);
                self.label(&l_done);
                true
            }
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = *expected (dereference expected pointer)
                self.gen_expr(&args[1]);
                self.mv_a_reg(13); // a3 = expected_ptr
                // load *expected_ptr to a2
                self.emit_load_sz(12, 13, 0, sz); // a2 = *expected
                // a0 = desired
                self.gen_expr(&args[2]);
                self.mv_a_reg(14); // a4 = desired
                // fence rw, rw only for SEQ_CST before LR/SC
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                if sord == 5 { self.emit_fence(); }
                // LR/SC CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                // lr.w/d a0, [a1] (load current value)
                self.emit_lr(sz, 10, 11);
                // bne a0, a2, fail (compare current with expected)
                self.emit_bne(10, 12, &l_fail);
                // sc.w/d a5, a4, [a1] (store desired)
                self.emit_sc(sz, 15, 14, 11);
                // bnez a5, retry (if store failed, retry)
                self.emit_bnez(15, &l_retry);
                // success: a0 = 1
                self.li_a0(1);
                self.j(&l_done);
                // failure: store current to *expected, a0 = 0
                self.label(&l_fail);
                self.emit_store_sz(10, 13, 0, sz); // *expected = current
                self.li_a0(0);
                self.label(&l_done);
                true
            }
            "__atomic_compare_exchange" => {
                // Non-_n variant: desired is also a pointer (must dereference)
                // args: (ptr, expected_ptr, desired_ptr, weak, succ_order, fail_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // a0 = ptr
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                // a0 = expected_ptr
                self.gen_expr(&args[1]);
                self.mv_a_reg(13); // a3 = expected_ptr
                // load *expected_ptr to a2
                self.emit_load_sz(12, 13, 0, sz); // a2 = *expected
                // a0 = desired_ptr -> dereference to get desired
                self.gen_expr(&args[2]);
                self.mv_a_reg(15); // a5 = desired_ptr (reuse for temp)
                self.emit_load_sz(14, 15, 0, sz); // a4 = *desired_ptr
                // fence only for SEQ_CST
                let sord = if args.len() > 4 {
                    self.eval_mem_order(&args[4])
                } else {
                    5
                };
                if sord == 5 { self.emit_fence(); }
                // LR/SC CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                self.emit_lr(sz, 10, 11);
                self.emit_bne(10, 12, &l_fail);
                self.emit_sc(sz, 15, 14, 11);
                self.emit_bnez(15, &l_retry);
                self.li_a0(1);
                self.j(&l_done);
                self.label(&l_fail);
                self.emit_store_sz(10, 13, 0, sz); // *expected = current
                self.li_a0(0);
                self.label(&l_done);
                true
            }
            "__atomic_thread_fence" => {
                self.emit_fence();
                true
            }
            "__atomic_signal_fence" => {
                // Compiler barrier only, no hardware instruction
                true
            }
            "__atomic_test_and_set" => {
                // args: (ptr, mem_order)
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                self.li_reg(12, 1); // a2 = 1
                self.emit_amoswap(sz, 10, 12, 11); // a0 = old
                true
            }
            "__atomic_clear" => {
                // args: (ptr, mem_order)
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mv_a_reg(11); // a1 = ptr
                self.li_reg(12, 0); // a2 = 0
                let ord = self.eval_mem_order(&args[1]);
                if ord == 5 { self.emit_fence(); }
                self.emit_store_sz(10, 11, 0, sz);
                true
            }
            "__atomic_is_lock_free" => {
                self.li_a0(1);
                true
            }
            // ===== __sync_* legacy atomic operations =====
            "__sync_fetch_and_add" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.emit_amoadd(sz, 10, 12, 11); true
            }
            "__sync_fetch_and_sub" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]);
                self.emit_neg(10, 10); self.mv_a_reg(12);
                self.emit_amoadd(sz, 10, 12, 11); true
            }
            "__sync_fetch_and_or" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.emit_amoor(sz, 10, 12, 11); true
            }
            "__sync_fetch_and_and" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.emit_amoand(sz, 10, 12, 11); true
            }
            "__sync_fetch_and_xor" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.emit_amoxor(sz, 10, 12, 11); true
            }
            "__sync_fetch_and_nand" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_fn"); let ld = self.fresh_label("sy_fn_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_and(13, 10, 12); self.emit_xori(13, 13, -1);
                self.emit_sc(sz, 15, 13, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_add_and_fetch" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_af"); let ld = self.fresh_label("sy_af_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_add(10, 10, 12); self.emit_sc(sz, 15, 10, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_sub_and_fetch" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_sf"); let ld = self.fresh_label("sy_sf_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_sub(10, 10, 12); self.emit_sc(sz, 15, 10, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_or_and_fetch" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_of"); let ld = self.fresh_label("sy_of_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_or(10, 10, 12); self.emit_sc(sz, 15, 10, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_and_and_fetch" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_anf"); let ld = self.fresh_label("sy_anf_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_and(10, 10, 12); self.emit_sc(sz, 15, 10, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_xor_and_fetch" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_xf"); let ld = self.fresh_label("sy_xf_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_xor(10, 10, 12); self.emit_sc(sz, 15, 10, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_nand_and_fetch" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                let lr = self.fresh_label("sy_nf"); let ld = self.fresh_label("sy_nf_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_and(10, 10, 12); self.emit_xori(10, 10, -1);
                self.emit_sc(sz, 15, 10, 11);
                self.emit_bnez(15, &lr); self.label(&ld); true
            }
            "__sync_bool_compare_and_swap" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.gen_expr(&args[2]); self.mv_a_reg(13);
                let lr = self.fresh_label("sy_cas");
                let lf = self.fresh_label("sy_cas_f");
                let ld = self.fresh_label("sy_cas_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_bne(10, 12, &lf);
                self.emit_sc(sz, 15, 13, 11);
                self.emit_bnez(15, &lr);
                self.li_a0(1); self.j(&ld);
                self.label(&lf); self.li_a0(0);
                self.label(&ld); true
            }
            "__sync_val_compare_and_swap" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.gen_expr(&args[2]); self.mv_a_reg(13);
                let lr = self.fresh_label("sy_casv");
                let lf = self.fresh_label("sy_casv_f");
                let ld = self.fresh_label("sy_casv_d");
                self.label(&lr); self.emit_lr(sz, 10, 11);
                self.emit_bne(10, 12, &lf);
                self.emit_sc(sz, 15, 13, 11);
                self.emit_bnez(15, &lr);
                self.j(&ld);
                self.label(&lf); // a0 = old value from lr
                self.label(&ld); true
            }
            "__sync_lock_test_and_set" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.gen_expr(&args[1]); self.mv_a_reg(12);
                self.emit_amoswap(sz, 10, 12, 11); true
            }
            "__sync_lock_release" => {
                if args.is_empty() { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]); self.mv_a_reg(11);
                self.li_reg(12, 0);
                self.emit_store_sz(10, 11, 0, sz); true
            }
            "__sync_synchronize" => {
                self.emit_fence(); true
            }
            _ => false,
        }
    }

}
