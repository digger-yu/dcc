// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::{aggregate_size, classify_hfa, Aarch64Codegen};
use crate::ir::{ExprKind, TypedExpr, ValueType};

impl Aarch64Codegen {
    pub(crate) fn gen_fenv_builtin(&mut self, name: &str, args: &[TypedExpr]) {
        match name {
            // fegetround(): 返回当前舍入模式 (不覆盖为 0)
            "fegetround" => {
                // MRS X0, FPCR
                self.emit(&0xD53B4400u32.to_le_bytes());
                // LSR W0, W0, #22 (UBFM W0,W0,#22,#31)
                self.emit(&0x531C0000u32.to_le_bytes());
                // AND W0, W0, #3
                self.emit(&0x12000003u32.to_le_bytes());
                // W0 已包含舍入模式, 直接返回 (不设 0)
            },
            // fesetround(mode): 设置 FPCR 舍入位, 返回 0
            "fesetround" => {
                if args.is_empty() {
                    self.mov_w0_imm(0);
                    return;
                }
                self.gen_expr(&args[0]); // W0 = mode
                                         // AND W0, W0, #3
                self.emit(&0x12000003u32.to_le_bytes());
                // MOV W1, W0 (保存)
                self.mov_w1_w0();
                // MRS X2, FPCR
                self.emit(&(0xD53B4400u32 | 2).to_le_bytes());
                // MOVZ W3, #0xFF3F (掩码低 16 位)
                self.emit(&0x529FFC03u32.to_le_bytes());
                // MOVK W3, #0xFF3F, LSL #16 (掩码高 16 位)
                self.emit(&0x729FFC03u32.to_le_bytes());
                // AND X2, X2, X3 (清除位 [23:22])
                self.emit(&0x8A030042u32.to_le_bytes());
                // LSL W1, W1, #22 (移到 FPCR 位)
                self.emit(&0x531C0021u32.to_le_bytes());
                // ORR X2, X2, X1 (合并新舍入模式)
                self.emit(&0xAA010042u32.to_le_bytes());
                // MSR FPCR, X2
                self.emit(&(0xD51B4400u32 | 2).to_le_bytes());
                self.mov_w0_imm(0);
            },
            // feclearexcept(excepts): 清除 FPSR 异常标志, 返回 0
            "feclearexcept" => {
                if args.is_empty() {
                    self.mov_w0_imm(0);
                    return;
                }
                self.gen_expr(&args[0]); // W0 = excepts
                                         // AND W0, W0, #0x1F
                self.emit(&0x12000800u32.to_le_bytes());
                // MRS X1, FPSR
                self.emit(&(0xD53B4420u32 | 1).to_le_bytes());
                // BIC X1, X1, X0 (清除选中的标志)
                self.emit(&0x8A200021u32.to_le_bytes());
                // MSR FPSR, X1
                self.emit(&(0xD51B4420u32 | 1).to_le_bytes());
                self.mov_w0_imm(0);
            },
            // feraiseexcept(excepts): 置位 FPSR 异常标志, 返回 0
            "feraiseexcept" => {
                if args.is_empty() {
                    self.mov_w0_imm(0);
                    return;
                }
                self.gen_expr(&args[0]); // W0 = excepts
                self.emit(&0x12000800u32.to_le_bytes()); // AND W0, W0, #0x1F
                                                         // MRS X1, FPSR
                self.emit(&(0xD53B4420u32 | 1).to_le_bytes());
                // ORR X1, X1, X0 (置位选中的标志)
                self.emit(&0xAA000021u32.to_le_bytes());
                // MSR FPSR, X1
                self.emit(&(0xD51B4420u32 | 1).to_le_bytes());
                self.mov_w0_imm(0);
            },
            // fetestexcept(excepts): 返回 FPSR 中选中的异常标志位与
            "fetestexcept" => {
                if args.is_empty() {
                    self.mov_w0_imm(0);
                    return;
                }
                self.gen_expr(&args[0]); // W0 = excepts
                self.emit(&0x12000800u32.to_le_bytes()); // AND W0, W0, #0x1F
                                                         // MRS X1, FPSR
                self.emit(&(0xD53B4420u32 | 1).to_le_bytes());
                // AND W0, W1, W0 (返回选中的标志)
                self.emit(&0x0A000020u32.to_le_bytes());
            },
            // fegetenv(envp): 保存 FPCR+FPSR 到 [envp]
            "fegetenv" | "feholdexcept" => {
                if args.is_empty() {
                    self.mov_w0_imm(0);
                    return;
                }
                self.gen_expr(&args[0]); // X0 = envp
                                         // MOV X10, X0 (保存 envp)
                self.emit_mov_x_reg(10, 0);
                // MRS X1, FPCR; STR X1, [X10]
                self.emit(&(0xD53B4400u32 | 1).to_le_bytes());
                self.emit(&0xF9000141u32.to_le_bytes());
                // MRS X1, FPSR; STR X1, [X10, #8]
                self.emit(&(0xD53B4420u32 | 1).to_le_bytes());
                self.emit(&0xF9000141u32.to_le_bytes() /* STR X1, [X10, #8] */);
                if name == "feholdexcept" {
                    // 清除异常标志: MSR FPSR, XZR
                    self.emit(&0xD51B443Fu32.to_le_bytes());
                }
                self.mov_w0_imm(0);
            },
            // fesetenv(envp): 从 [envp] 恢复 FPCR+FPSR
            "fesetenv" | "feupdateenv" => {
                if args.is_empty() {
                    self.mov_w0_imm(0);
                    return;
                }
                self.gen_expr(&args[0]); // X0 = envp
                                         // MOV X10, X0 (保存 envp)
                self.emit_mov_x_reg(10, 0);
                // LDR X1, [X10]; MSR FPCR, X1
                self.emit(&0xF9400141u32.to_le_bytes());
                self.emit(&(0xD51B4400u32 | 1).to_le_bytes());
                // LDR X1, [X10, #8]; MSR FPSR, X1
                self.emit(&0xF9400141u32.to_le_bytes() /* LDR X1, [X10, #8] */);
                self.emit(&(0xD51B4420u32 | 1).to_le_bytes());
                self.mov_w0_imm(0);
            },
            _ => {
                self.mov_w0_imm(0);
            },
        }
    }

    pub(crate) fn callee_name(&self, expr: &TypedExpr) -> String {
        match expr.kind {
            ExprKind::Local(ref n) => n.clone(),
            _ => "unknown".to_string(),
        }
    }

    // 初始化 AAPCS64 va_list 变量.
    // va_list 是指向寄存器保存区的 void* 指针 (本后端用单指针简化版).
    // ap 局部 (aarch64 上是一个指针) 被写为 va_save_base_off 的地址.
    // va_list 现在是 24 字节结构体:
    //   { gp_offset(4B), fp_offset(4B), overflow_arg_area(8B), reg_save_area(8B) }
    // gen_va_start 写入完整四字段。
    pub(crate) fn gen_va_start(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // gp_offset = 0
        self.mov_w0_imm(0);
        self.emit_str_x(0, 31, ap_off as u32, 4); // str w0, [sp, #ap_off]
                                                  // fp_offset = 0
        self.emit_str_x(0, 31, (ap_off + 4) as u32, 4); // str w0, [sp, #ap_off+4]
                                                        // overflow_arg_area = sp + frame_bytes
        self.mov_x_reg_sp_off(9, self.frame_bytes);
        self.emit_str_x(9, 31, (ap_off + 8) as u32, 8); // str x9, [sp, #ap_off+8]
                                                        // reg_save_area = sp + va_save_base_off
        self.mov_x_reg_sp_off(9, self.va_save_base_off);
        self.emit_str_x(9, 31, (ap_off + 16) as u32, 8); // str x9, [sp, #ap_off+16]
    }

    // 读取下一个变参 (整数路径) 到 w0.
    // 从 va_list 结构体读取 gp_offset, 判断寄存器保存区还是溢出区.
    pub(crate) fn gen_va_arg_int(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // x9 = va_list 地址
        self.mov_x_reg_sp_off(9, ap_off);
        // w0 = gp_offset (从 va_list[0])
        self.emit_ldr_x(0, 9, 0, 4);
        // cmp w0, #48
        self.emit_cmp_imm_w(0, 48);
        let overflow = self.fresh_label("va_int_of");
        let done = self.fresh_label("va_int_done");
        // bge overflow
        self.emit_bcond(&overflow, 0xa); // GE = 0xa
                                         // ---- 寄存器保存区路径 ----
                                         // x1 = reg_save_area (从 va_list[16])
        self.emit_ldr_x(1, 9, 16, 8);
        // x0 = x1 + x0 (reg_save_area + gp_offset)
        // gp_offset < 48, 高 32 位为 0, x0 即零扩展后的偏移
        self.emit_add_x_reg(0, 1, 0);
        self.emit_ldr_x(0, 0, 0, 4); // ldr w0, [x0]
                                     // gp_offset += 8
        self.emit_ldr_x(1, 9, 0, 4); // w1 = 旧 gp_offset (从 va_list[0] 重新加载)
        self.emit_add_imm_w(1, 1, 8);
        self.emit_str_x(1, 9, 0, 4); // str w1, [x9, #0]
        self.b(&done);
        // ---- 溢出区路径 ----
        self.emit_label_raw(&overflow);
        // x1 = overflow_arg_area (从 va_list[8])
        self.emit_ldr_x(1, 9, 8, 8);
        // w0 = [x1]
        self.emit_ldr_x(0, 1, 0, 4);
        // x1 += 8
        self.emit_add_imm_x(1, 1, 8);
        // 写回 overflow_arg_area
        self.emit_str_x(1, 9, 8, 8);
        self.emit_label_raw(&done);
    }

    // 读取下一个变参 (浮点路径) 到 s0.
    // 从 va_list 结构体读取 fp_offset, 判断寄存器保存区还是溢出区.
    pub(crate) fn gen_va_arg_float(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // x9 = va_list 地址
        self.mov_x_reg_sp_off(9, ap_off);
        // w0 = fp_offset (从 va_list[4])
        self.emit_ldr_x(0, 9, 4, 4);
        // cmp w0, #128
        self.emit_cmp_imm_w(0, 128);
        let overflow = self.fresh_label("va_flt_of");
        let done = self.fresh_label("va_flt_done");
        // bge overflow
        self.emit_bcond(&overflow, 0xa); // GE = 0xa
                                         // ---- 寄存器保存区路径 ----
                                         // x1 = reg_save_area + 64 (float 保存区起始)
        self.emit_ldr_x(1, 9, 16, 8); // x1 = reg_save_area
        self.emit_add_imm_x(1, 1, 64); // x1 += 64
                                       // x0 = x1 + x0 (float_save + fp_offset)
        self.emit_add_x_reg(0, 1, 0);
        // s0 = [x0] (单精度加载)
        // ldr s0, [x0]: 0xBD400000 | (0<<10) | (0<<5) | 0
        self.emit(&0xBD400000u32.to_le_bytes());
        // fp_offset += 8
        self.emit_ldr_x(1, 9, 4, 4); // w1 = 旧 fp_offset
        self.emit_add_imm_w(1, 1, 8);
        self.emit_str_x(1, 9, 4, 4); // str w1, [x9, #4]
        self.b(&done);
        // ---- 溢出区路径 ----
        self.emit_label_raw(&overflow);
        // x1 = overflow_arg_area (从 va_list[8])
        self.emit_ldr_x(1, 9, 8, 8);
        // s0 = [x1]
        self.emit(&0xBD400020u32.to_le_bytes()); // ldr s0, [x1]
                                                 // x1 += 8
        self.emit_add_imm_x(1, 1, 8);
        // 写回 overflow_arg_area
        self.emit_str_x(1, 9, 8, 8);
        self.emit_label_raw(&done);
    }

    /// 双精度浮点 va_arg: 与 gen_va_arg_float 对称, 但使用 ldr d0 (64-bit)
    /// 而非 ldr s0 (32-bit). 修复 va_arg(ap, double) 截断为 float 的 bug.
    pub(crate) fn gen_va_arg_double(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // x9 = va_list 地址
        self.mov_x_reg_sp_off(9, ap_off);
        // w0 = fp_offset (从 va_list[4])
        self.emit_ldr_x(0, 9, 4, 4);
        // cmp w0, #128
        self.emit_cmp_imm_w(0, 128);
        let overflow = self.fresh_label("va_dbl_of");
        let done = self.fresh_label("va_dbl_done");
        // bge overflow
        self.emit_bcond(&overflow, 0xa); // GE = 0xa
                                         // ---- 寄存器保存区路径 ----
                                         // x1 = reg_save_area + 64 (float 保存区起始)
        self.emit_ldr_x(1, 9, 16, 8); // x1 = reg_save_area
        self.emit_add_imm_x(1, 1, 64); // x1 += 64
                                       // x0 = x1 + x0 (float_save + fp_offset)
        self.emit_add_x_reg(0, 1, 0);
        // d0 = [x0] (双精度加载)
        // ldr d0, [x0]: 0xFD400000
        self.emit(&0xFD400000u32.to_le_bytes());
        // fp_offset += 8
        self.emit_ldr_x(1, 9, 4, 4); // w1 = 旧 fp_offset
        self.emit_add_imm_w(1, 1, 8);
        self.emit_str_x(1, 9, 4, 4); // str w1, [x9, #4]
        self.b(&done);
        // ---- 溢出区路径 ----
        self.emit_label_raw(&overflow);
        // x1 = overflow_arg_area (从 va_list[8])
        self.emit_ldr_x(1, 9, 8, 8);
        // d0 = [x1]
        self.emit(&0xFD400020u32.to_le_bytes()); // ldr d0, [x1]
                                                 // x1 += 8
        self.emit_add_imm_x(1, 1, 8);
        // 写回 overflow_arg_area
        self.emit_str_x(1, 9, 8, 8);
        self.emit_label_raw(&done);
    }

    /// 聚合类型 va_arg: va_arg(ap, struct S)
    /// AAPCS64 变参规则:
    /// - HFA (齐次浮点聚合): 从 FP 保存区读取, fp_offset 预算 64 字节
    /// - 非 HFA 且整体适合 GP: 从 GP 保存区读取, gp_offset 预算 64 字节
    /// - 超出预算或 > 16 字节：从 overflow_arg_area 读取
    ///
    /// 结果在 x0 = 栈槽地址 (与 sret 调用约定一致)
    pub(crate) fn gen_va_arg_struct(&mut self, ap_name: &str, arg_ty: &ValueType) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        let size = aggregate_size(arg_ty).unwrap_or(8);
        let hfa = classify_hfa(arg_ty, &self.aggregate_fields);
        let slot_size = (size.div_ceil(8).max(1) * 8) as i32;
        let slot = self.alloc_local("<va_aggr>", slot_size);

        if let Some(ref hfa_info) = hfa {
            // HFA 路径: 从 FP 保存区读取, fp_offset 预算 64 字节
            let total = hfa_info.count as i32 * 8; // 每元素占 8B 槽位
            let budget = 64 - total;

            // x9 = va_list 地址
            self.mov_x_reg_sp_off(9, ap_off);
            // x2 = fp_offset
            self.emit_ldr_x(2, 9, 4, 4); // ldr w2, [x9, #4]
            let lbl_overflow = self.fresh_label("va_hfa_of");
            let lbl_done = self.fresh_label("va_hfa_done");
            // cmp w2, #budget
            self.emit_cmp_imm_w(2, budget as u32);
            self.emit_bcond(&lbl_overflow, 0xa); // bge overflow

            // 寄存器路径: x3 = reg_save_area + 64 + fp_offset
            self.emit_ldr_x(3, 9, 16, 8); // ldr x3, [x9, #16] (reg_save_area)
            self.emit_add_imm_x(3, 3, 64); // add x3, x3, #64 (FP 区起始)
            self.emit_add_x_reg(3, 3, 2); // add x3, x3, x2 (+ fp_offset)
                                          // x4 = slot 地址
            self.mov_x_reg_sp_off(4, slot);
            // 逐元素复制
            for i in 0..hfa_info.count as i32 {
                let off = (i * 8) as u32;
                self.ldr_xt_indirect(1, 3, off as i32); // ldr x1, [x3, #off]
                self.str_xt_indirect(1, 4, off as i32); // str x1, [x4, #off]
            }
            // fp_offset += total
            self.emit_add_imm_w(2, 2, total as u32);
            self.emit_str_x(2, 9, 4, 4); // str w2, [x9, #4]
            self.b(&lbl_done);

            // 溢出路径: overflow_arg_area -> slot
            self.emit_label_raw(&lbl_overflow);
            self.emit_ldr_x(3, 9, 8, 8); // ldr x3, [x9, #8] (overflow_arg_area)
            self.mov_x_reg_sp_off(4, slot);
            for i in 0..(slot_size / 8) {
                let off = (i * 8) as u32;
                self.ldr_xt_indirect(1, 3, off as i32);
                self.str_xt_indirect(1, 4, off as i32);
            }
            // overflow_arg_area += slot_size
            self.emit_add_imm_x(3, 3, slot_size as u32);
            self.emit_str_x(3, 9, 8, 8); // str x3, [x9, #8]
            self.emit_label_raw(&lbl_done);
        } else if size <= 16 {
            // GP 路径：整体检查 gp_offset 预算
            let aligned = size.div_ceil(8) as i32 * 8;
            let budget = 64 - aligned;

            self.mov_x_reg_sp_off(9, ap_off);
            self.emit_ldr_x(2, 9, 0, 4); // ldr w2, [x9, #0] (gp_offset)
            let lbl_overflow = self.fresh_label("va_gp_of");
            let lbl_done = self.fresh_label("va_gp_done");
            self.emit_cmp_imm_w(2, budget as u32);
            self.emit_bcond(&lbl_overflow, 0xa); // bge overflow

            // 寄存器路径: x3 = reg_save_area + gp_offset
            self.emit_ldr_x(3, 9, 16, 8); // ldr x3, [x9, #16]
            self.emit_add_x_reg(3, 3, 2); // add x3, x3, x2
            self.mov_x_reg_sp_off(4, slot);
            for i in 0..(aligned / 8) {
                let off = i * 8;
                self.ldr_xt_indirect(1, 3, off);
                self.str_xt_indirect(1, 4, off);
            }
            // gp_offset += aligned
            self.emit_add_imm_w(2, 2, aligned as u32);
            self.emit_str_x(2, 9, 0, 4); // str w2, [x9, #0]
            self.b(&lbl_done);

            // 溢出路径
            self.emit_label_raw(&lbl_overflow);
            self.emit_ldr_x(3, 9, 8, 8); // ldr x3, [x9, #8]
            self.mov_x_reg_sp_off(4, slot);
            for i in 0..(slot_size / 8) {
                let off = i * 8;
                self.ldr_xt_indirect(1, 3, off);
                self.str_xt_indirect(1, 4, off);
            }
            self.emit_add_imm_x(3, 3, slot_size as u32);
            self.emit_str_x(3, 9, 8, 8);
            self.emit_label_raw(&lbl_done);
        } else {
            // 大结构体 (> 16 字节): 直接从 overflow_arg_area 读取
            self.mov_x_reg_sp_off(9, ap_off);
            self.emit_ldr_x(3, 9, 8, 8); // ldr x3, [x9, #8]
            self.mov_x_reg_sp_off(4, slot);
            for i in 0..(slot_size / 8) {
                let off = i * 8;
                self.ldr_xt_indirect(1, 3, off);
                self.str_xt_indirect(1, 4, off);
            }
            self.emit_add_imm_x(3, 3, slot_size as u32);
            self.emit_str_x(3, 9, 8, 8);
        }

        // x0 = slot 地址 (与 sret 调用约定一致)
        self.mov_x_reg_sp_off(0, slot);
    }
}
