// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::{value_type_size, Aarch64Codegen};
use crate::ir::{ExprLiteral, TypedExpr, ValueType};

impl Aarch64Codegen {
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
    /// 参数约定: args[0]=ptr, args[1]=val (对于 store/exchange/cmpxchg 有不同)
    /// 结果返回在 x0
    pub(crate) fn gen_atomic_builtin(&mut self, name: &str, args: &[TypedExpr]) -> bool {
        match name {
            "__atomic_load_n" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[1]);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(1, 0); // x1 = ptr (保存)
                match ord {
                    0 | 3 => {
                        // RELAXED/RELEASE: plain LDR
                        self.emit_ldr_x(0, 1, 0, sz);
                    },
                    _ => {
                        // SEQ_CST/ACQUIRE/ACQ_REL: LDAR
                        self.emit_ldar(sz, 0, 1);
                    },
                }
                true
            },
            "__atomic_store_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                let ord = self.eval_mem_order(&args[2]);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(1, 0); // x1 = ptr
                                      // x0 = val
                self.gen_expr(&args[1]);
                match ord {
                    0 => {
                        // RELAXED: plain STR
                        self.emit_str_x(0, 1, 0, sz);
                    },
                    _ => {
                        // SEQ_CST/ACQUIRE/RELEASE/ACQ_REL: STLR
                        self.emit_stlr(sz, 0, 1);
                    },
                }
                true
            },
            "__atomic_exchange_n" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                                      // x0 = val
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                                      // CAS loop: ldaxr/stlxr retry loop
                let l_retry = self.fresh_label("atomic_xchg");
                let l_done = self.fresh_label("atomic_xchg_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_stlxr(sz, 3, 1, 2);
                self.emit_cbnz(3, &l_retry);
                self.label(&l_done);
                // x0 已包含旧值
                true
            },
            "__atomic_fetch_add" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                                      // x0 = val
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                                      // CAS loop: x0 = old value
                let l_retry = self.fresh_label("atomic_add");
                let l_done = self.fresh_label("atomic_add_done");
                self.label(&l_retry);
                // ldaxr x0, [x2]
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 + x1 (new = old + val)
                self.emit_add_x_reg(3, 0, 1);
                // stlxr w4, x3, [x2]
                self.emit_stlxr(sz, 4, 3, 2);
                // cbnz w4, retry
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                // x0 已包含旧值
                true
            },
            "__atomic_fetch_sub" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                                      // x0 = val
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                                      // CAS loop
                let l_retry = self.fresh_label("atomic_sub");
                let l_done = self.fresh_label("atomic_sub_done");
                self.label(&l_retry);
                // ldaxr x0, [x2]
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 - x1 (new = old - val)
                self.emit_sub_x_reg(3, 0, 1);
                // stlxr w4, x3, [x2]
                self.emit_stlxr(sz, 4, 3, 2);
                // cbnz w4, retry
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            },
            "__atomic_fetch_and" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let l_retry = self.fresh_label("atomic_and");
                let l_done = self.fresh_label("atomic_and_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 & x1
                self.emit_and_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            },
            "__atomic_fetch_or" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let l_retry = self.fresh_label("atomic_or");
                let l_done = self.fresh_label("atomic_or_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 | x1
                self.emit_or_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            },
            "__atomic_fetch_xor" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let l_retry = self.fresh_label("atomic_xor");
                let l_done = self.fresh_label("atomic_xor_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                // x3 = x0 ^ x1
                self.emit_eor_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                true
            },
            "__atomic_fetch_nand" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0); // x1 = val
                let l_retry = self.fresh_label("atomic_nand");
                let l_done = self.fresh_label("atomic_nand_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2); // x0 = old
                self.emit_and_x_reg(3, 0, 1); // x3 = old & val
                self.emit_mvn_x(3, 3); // x3 = ~(old & val)
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &l_retry);
                self.label(&l_done);
                // x0 = old value
                true
            },
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                                      // x0 = *expected (解引用 expected 指针)
                self.gen_expr(&args[1]);
                self.mov_x_reg(4, 0); // x4 = expected_ptr
                                      // 加载 *expected 到 x1
                self.emit_ldr_x(1, 4, 0, sz); // x1 = *expected_ptr
                                              // x0 = desired
                self.gen_expr(&args[2]);
                self.mov_x_reg(3, 0); // x3 = desired
                                      // CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                // ldaxr x0, [x2] (加载当前值)
                self.emit_ldaxr(sz, 0, 2);
                // cmp x0, x1 (比较当前值与期望值)
                self.emit_cmp_x_reg(0, 1);
                // b.ne fail
                self.emit_bcond(&l_fail, 1); // NE = 1
                                             // stlxr w5, x3, [x2] (store desired)
                self.emit_stlxr(sz, 5, 3, 2);
                // cbnz w5, retry
                self.emit_cbnz(5, &l_retry);
                // 成功: x0 = 1
                self.mov_w0_imm(1);
                // b done
                self.b(&l_done);
                // 失败: 将当前值写入 *expected, x0 = 0
                self.label(&l_fail);
                // stlxr 失败, 需要清除 exclusive monitor
                // 重新加载当前值并写入 expected
                self.emit_ldxr(sz, 0, 2); // 读取当前值 (非 atomic, 仅清除 monitor)
                self.emit_clrex(); // 清除 exclusive monitor
                self.emit_str_x(0, 4, 0, sz); // *expected_ptr = current
                self.mov_w0_imm(0);
                self.label(&l_done);
                true
            },
            "__atomic_compare_exchange" => {
                // Non-_n variant: desired is also a pointer (must dereference)
                // args: (ptr, expected_ptr, desired_ptr, weak, succ_order, fail_order)
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                // x0 = ptr
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                                      // x0 = expected_ptr
                self.gen_expr(&args[1]);
                self.mov_x_reg(4, 0); // x4 = expected_ptr
                                      // load *expected_ptr into x1
                self.emit_ldr_x(1, 4, 0, sz); // x1 = *expected_ptr
                                              // x0 = desired_ptr -> dereference to get desired value
                self.gen_expr(&args[2]);
                self.mov_x_reg(6, 0); // x6 = desired_ptr
                self.emit_ldr_x(3, 6, 0, sz); // x3 = *desired_ptr
                                              // CAS loop
                let l_retry = self.fresh_label("atomic_cas");
                let l_fail = self.fresh_label("atomic_cas_fail");
                let l_done = self.fresh_label("atomic_cas_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_cmp_x_reg(0, 1);
                self.emit_bcond(&l_fail, 1); // NE
                self.emit_stlxr(sz, 5, 3, 2);
                self.emit_cbnz(5, &l_retry);
                self.mov_w0_imm(1);
                self.b(&l_done);
                self.label(&l_fail);
                self.emit_ldxr(sz, 0, 2);
                self.emit_clrex();
                self.emit_str_x(0, 4, 0, sz); // *expected_ptr = current
                self.mov_w0_imm(0);
                self.label(&l_done);
                true
            },
            "__atomic_thread_fence" => {
                // args: (mem_order)
                self.emit_dmb_ish();
                true
            },
            "__atomic_signal_fence" => {
                // Compiler barrier only, no hardware instruction
                true
            },
            "__atomic_test_and_set" => {
                // args: (ptr, mem_order)
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0); // x2 = ptr
                self.mov_w0_imm(1);
                self.mov_x_reg(1, 0); // x1 = 1
                let l_retry = self.fresh_label("atomic_tas");
                let l_done = self.fresh_label("atomic_tas_done");
                self.label(&l_retry);
                self.emit_ldaxr(sz, 0, 2); // x0 = old
                self.emit_stlxr(sz, 3, 1, 2);
                self.emit_cbnz(3, &l_retry);
                self.label(&l_done);
                true
            },
            "__atomic_clear" => {
                // args: (ptr, mem_order)
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(1, 0); // x1 = ptr
                self.mov_w0_imm(0);
                self.emit_stlr(sz, 0, 1);
                true
            },
            "__atomic_is_lock_free" => {
                // On AArch64, all standard integer sizes are always lock-free
                self.mov_w0_imm(1);
                true
            },
            // ===== __sync_* legacy atomic operations =====
            "__sync_fetch_and_add" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_fa");
                let ld = self.fresh_label("sy_fa_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_add_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_fetch_and_sub" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_fs");
                let ld = self.fresh_label("sy_fs_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_sub_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_fetch_and_or" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_fo");
                let ld = self.fresh_label("sy_fo_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_or_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_fetch_and_and" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_fan");
                let ld = self.fresh_label("sy_fan_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_and_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_fetch_and_xor" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_fx");
                let ld = self.fresh_label("sy_fx_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_eor_x_reg(3, 0, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_fetch_and_nand" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_fn");
                let ld = self.fresh_label("sy_fn_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_and_x_reg(3, 0, 1);
                self.emit_mvn_x(3, 3);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_add_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_af");
                let ld = self.fresh_label("sy_af_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_add_x_reg(0, 0, 1);
                self.emit_stlxr(sz, 4, 0, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_sub_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_sf");
                let ld = self.fresh_label("sy_sf_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_sub_x_reg(0, 0, 1);
                self.emit_stlxr(sz, 4, 0, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_or_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_of");
                let ld = self.fresh_label("sy_of_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_or_x_reg(0, 0, 1);
                self.emit_stlxr(sz, 4, 0, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_and_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_anf");
                let ld = self.fresh_label("sy_anf_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_and_x_reg(0, 0, 1);
                self.emit_stlxr(sz, 4, 0, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_xor_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_xf");
                let ld = self.fresh_label("sy_xf_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_eor_x_reg(0, 0, 1);
                self.emit_stlxr(sz, 4, 0, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_nand_and_fetch" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_nf");
                let ld = self.fresh_label("sy_nf_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_and_x_reg(0, 0, 1);
                self.emit_mvn_x(0, 0);
                self.emit_stlxr(sz, 4, 0, 2);
                self.emit_cbnz(4, &lr);
                self.label(&ld);
                true
            },
            "__sync_bool_compare_and_swap" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                self.gen_expr(&args[2]);
                self.mov_x_reg(3, 0);
                let lr = self.fresh_label("sy_cas");
                let lf = self.fresh_label("sy_cas_f");
                let ld = self.fresh_label("sy_cas_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_cmp_x_reg(0, 1);
                self.emit_bcond(&lf, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.mov_w0_imm(1);
                self.b(&ld);
                self.label(&lf);
                self.emit_ldxr(sz, 0, 2);
                self.emit_clrex();
                self.mov_w0_imm(0);
                self.label(&ld);
                true
            },
            "__sync_val_compare_and_swap" => {
                if args.len() < 3 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                self.gen_expr(&args[2]);
                self.mov_x_reg(3, 0);
                let lr = self.fresh_label("sy_casv");
                let lf = self.fresh_label("sy_casv_f");
                let ld = self.fresh_label("sy_casv_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_cmp_x_reg(0, 1);
                self.emit_bcond(&lf, 1);
                self.emit_stlxr(sz, 4, 3, 2);
                self.emit_cbnz(4, &lr);
                self.b(&ld);
                self.label(&lf);
                self.emit_ldxr(sz, 0, 2);
                self.emit_clrex();
                self.label(&ld);
                true
            },
            "__sync_lock_test_and_set" => {
                if args.len() < 2 {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(2, 0);
                self.gen_expr(&args[1]);
                self.mov_x_reg(1, 0);
                let lr = self.fresh_label("sy_lt");
                let ld = self.fresh_label("sy_lt_d");
                self.label(&lr);
                self.emit_ldaxr(sz, 0, 2);
                self.emit_stlxr(sz, 3, 1, 2);
                self.emit_cbnz(3, &lr);
                self.label(&ld);
                true
            },
            "__sync_lock_release" => {
                if args.is_empty() {
                    return false;
                }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0]);
                self.mov_x_reg(1, 0);
                self.mov_w0_imm(0);
                self.emit_str_x(0, 1, 0, sz);
                true
            },
            "__sync_synchronize" => {
                self.emit_dmb_ish();
                true
            },
            _ => false,
        }
    }
}
