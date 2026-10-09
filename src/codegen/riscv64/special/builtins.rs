// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::{aggregate_size, Riscv64Codegen};
use crate::ir::{TypedExpr, ValueType};

impl Riscv64Codegen {
    /// fenv.h 内联实现 (C11 7.6): RISCV64 CSR 指令序列
    /// frm CSR (0x002): 舍入模式 [7:5]
    /// fflags CSR (0x001): 异常标志 [4:0]
    /// fcsr CSR (0x003): 完整浮点控制/状态寄存器
    pub(crate) fn gen_fenv_builtin(&mut self, name: &str, args: &[TypedExpr]) {
        match name {
            // fegetround(): CSRRW a0, frm, x0; SRLI a0, a0, 5
            "fegetround" => {
                // CSRRW a0, frm, x0 = 0xC010F573
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0xC010F573));
                // SRLI a0, a0, 5
                self.emit_srli(10, 10, 5);
            },
            // fesetround(mode): SLLI a0, a0, 5; CSRRW x0, frm, a0; li a0, 0
            "fesetround" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = mode
                self.emit_slli(10, 10, 5); // 移到 frm 位 [7:5]
                                           // CSRRW x0, frm, a0 = 0xC0151073
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0xC0151073));
                self.li_a0(0); // 返回 0
            },
            // feclearexcept(excepts): CSRC x0, fflags, a0; li a0, 0
            "feclearexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = excepts
                                         // ANDI a0, a0, 0x1F (保留 5 位)
                self.emit_andi(10, 10, 0x1F);
                // CSRC x0, fflags, a0 = 0x40153073
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0x40153073));
                self.li_a0(0);
            },
            // feraiseexcept(excepts): CSRS x0, fflags, a0; li a0, 0
            "feraiseexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]);
                self.emit_andi(10, 10, 0x1F);
                // CSRS x0, fflags, a0 = 0x40152073
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0x40152073));
                self.li_a0(0);
            },
            // fetestexcept(excepts): CSRRS a0, fflags, x0; AND a0, a0, a1
            "fetestexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = excepts
                self.emit_andi(10, 10, 0x1F);
                // MV a1, a0 (保存)
                self.mv_a1_a0();
                // CSRRS a0, fflags, x0 = 0xC0102573
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0xC0102573));
                // AND a0, a0, a1
                self.emit_and(10, 10, 11);
            },
            // fegetenv/fesetenv/feholdexcept/feupdateenv:
            // 简化实现: 保存/恢复 fcsr 到/从内存
            "fegetenv" | "feholdexcept" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = envp
                                         // CSRRS a1, fcsr, x0 (读取 fcsr)
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0xC03025F3)); // CSRRS a1, fcsr, x0
                                                         // SW a1, 0(a0) (存储到 [envp])
                self.emit_sw(11, 10, 0);
                if name == "feholdexcept" {
                    // CSRW fcsr, x0 (清除所有标志)
                    self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0xC0301073));
                }
                self.li_a0(0);
            },
            "fesetenv" | "feupdateenv" => {
                if args.is_empty() {
                    self.li_a0(0);
                    return;
                }
                self.gen_expr(&args[0]); // a0 = envp
                                         // LW a1, 0(a0) (从 [envp] 加载)
                self.emit_lw(11, 10, 0);
                // CSRRW x0, fcsr, a1 (写入 fcsr)
                self.emit_insn(&crate::codegen::riscv64::insn::Insn::raw(0xC0359073));
                self.li_a0(0);
            },
            _ => {
                self.li_a0(0);
            },
        }
    }

    // 初始化 RISC-V va_list 变量.
    // 简化: ap = 指向保存区的指针 (addi a0, sp, va_save_base_off).
    // va_list 现在是 24 字节结构体:
    //   { gp_offset(4B), fp_offset(4B), overflow_arg_area(8B), reg_save_area(8B) }
    pub(crate) fn gen_va_start(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // gp_offset = 0
        self.li_reg(10, 0); // a0 = 0
        self.emit_sw(10, 2, ap_off); // sw a0, ap_off(sp)
                                     // fp_offset = 0
        self.emit_sw(10, 2, ap_off + 4); // sw a0, ap_off+4(sp)
                                         // overflow_arg_area = sp + frame_bytes
        self.li_reg(10, self.frame_bytes);
        self.emit_add(10, 2, 10); // a0 = sp + frame_bytes
        self.emit_sd(10, 2, ap_off + 8); // sd a0, ap_off+8(sp)
                                         // reg_save_area = sp + va_save_base_off
        self.li_reg(10, self.va_save_base_off);
        self.emit_add(10, 2, 10); // a0 = sp + va_save_base_off
        self.emit_sd(10, 2, ap_off + 16); // sd a0, ap_off+16(sp)
    }

    // 读取下一个变参 (整数路径) 到 a0.
    // 从 va_list 结构体读取 gp_offset, 判断寄存器保存区还是溢出区.
    pub(crate) fn gen_va_arg_int(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // a5 = va_list 地址
        self.emit_addi(15, 2, ap_off); // addi a5, sp, ap_off
                                       // a0 = gp_offset (从 va_list[0])
        self.emit_lw(10, 15, 0); // lw a0, 0(a5)
                                 // 比较: a6 = 48
        self.li_reg(11, 48); // a1 = 48
        let overflow = self.fresh_label("va_int_of");
        let done = self.fresh_label("va_int_done");
        // bge a0, a1, overflow
        self.emit_bge_label(10, 11, &overflow);
        // ---- 寄存器保存区路径 ----
        // a1 = reg_save_area (从 va_list[16])
        self.emit_ld(11, 15, 16); // ld a1, 16(a5)
                                  // a0 = a1 + a0 (reg_save_area + gp_offset)
        self.emit_add(10, 11, 10);
        // a0 = [a0] (加载参数值)
        self.emit_ld(10, 10, 0); // ld a0, 0(a0)
                                 // gp_offset += 8
        self.emit_lw(11, 15, 0); // a1 = 旧 gp_offset
        self.emit_addi(11, 11, 8); // a1 += 8
        self.emit_sw(11, 15, 0); // sw a1, 0(a5)
        self.j(&done);
        // ---- 溢出区路径 ----
        self.label(&overflow);
        // a1 = overflow_arg_area (从 va_list[8])
        self.emit_ld(11, 15, 8); // ld a1, 8(a5)
                                 // a0 = [a1]
        self.emit_ld(10, 11, 0); // ld a0, 0(a1)
                                 // a1 += 8
        self.emit_addi(11, 11, 8);
        // 写回 overflow_arg_area
        self.emit_sd(11, 15, 8); // sd a1, 8(a5)
        self.label(&done);
    }

    // 读取下一个变参 (浮点路径) 到 fa0.
    // 从 va_list 结构体读取 fp_offset, 判断寄存器保存区还是溢出区.
    pub(crate) fn gen_va_arg_float(&mut self, ap_name: &str) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        // a5 = va_list 地址
        self.emit_addi(15, 2, ap_off); // addi a5, sp, ap_off
                                       // a0 = fp_offset (从 va_list[4])
        self.emit_lw(10, 15, 4); // lw a0, 4(a5)
                                 // 比较: a1 = 128
        self.li_reg(11, 128); // a1 = 128
        let overflow = self.fresh_label("va_flt_of");
        let done = self.fresh_label("va_flt_done");
        // bge a0, a1, overflow
        self.emit_bge_label(10, 11, &overflow);
        // ---- 寄存器保存区路径 ----
        // a1 = reg_save_area + 64 (float 保存区)
        self.emit_ld(11, 15, 16); // ld a1, 16(a5) = reg_save_area
        self.emit_addi(11, 11, 64); // a1 += 64
                                    // a0 = a1 + a0 (float_save + fp_offset)
        self.emit_add(10, 11, 10);
        // fa0 = [a0] (float load)
        self.emit_fld(10, 10, 0); // fld fa0, 0(a0)
                                  // fp_offset += 8
        self.emit_lw(11, 15, 4); // a1 = 旧 fp_offset
        self.emit_addi(11, 11, 8); // a1 += 8
        self.emit_sw(11, 15, 4); // sw a1, 4(a5)
        self.j(&done);
        // ---- 溢出区路径 ----
        self.label(&overflow);
        // a1 = overflow_arg_area (从 va_list[8])
        self.emit_ld(11, 15, 8); // ld a1, 8(a5)
                                 // fa0 = [a1]
        self.emit_fld(10, 11, 0); // fld fa0, 0(a1)
                                  // a1 += 8
        self.emit_addi(11, 11, 8);
        // 写回 overflow_arg_area
        self.emit_sd(11, 15, 8); // sd a1, 8(a5)
        self.label(&done);
    }

    /// 聚合类型 va_arg: va_arg(ap, struct S)
    /// RISC-V 变参规则:
    /// - size <= 16: 尝试 GP 保存区 (gp_offset 预算 64 字节), 超出走 overflow
    /// - 超出预算或 > 16 字节：从 overflow_arg_area 读取
    ///
    /// 结果在 a0 = 栈槽地址 (与 sret 调用约定一致)
    pub(crate) fn gen_va_arg_struct(&mut self, ap_name: &str, arg_ty: &ValueType) {
        let ap_off = match self.locals.get(ap_name) {
            Some(&o) => o,
            None => return,
        };
        let size = aggregate_size(arg_ty).unwrap_or(8);
        let slot_size = size.div_ceil(8).max(1) as i32 * 8;
        let slot = self.alloc_local("<va_aggr>", slot_size);

        if size <= 16 {
            let aligned = size.div_ceil(8) as i32 * 8;
            let budget = 64 - aligned;

            // a5 = va_list 地址
            self.mov_a_reg_sp_off(15, ap_off);
            // a2 = gp_offset
            self.emit_lw(12, 15, 0); // lw a2, 0(a5)
            let lbl_overflow = self.fresh_label("va_gp_of");
            let lbl_done = self.fresh_label("va_gp_done");
            // bge a2, budget, overflow
            self.li_reg(11, budget);
            self.emit_bge_label(12, 11, &lbl_overflow);

            // 寄存器路径: a3 = reg_save_area + gp_offset
            self.emit_ld(13, 15, 16); // ld a3, 16(a5) (reg_save_area)
            self.emit_add(13, 13, 12); // add a3, a3, a2
                                       // a4 = slot 地址
            self.mov_a_reg_sp_off(14, slot);
            // 逐字复制
            for i in 0..(aligned / 8) {
                let off = i * 8;
                self.emit_ld(10, 13, off); // ld a0, off(a3)
                self.emit_sd(10, 14, off); // sd a0, off(a4)
            }
            // gp_offset += aligned
            self.emit_addi(12, 12, aligned);
            self.emit_sw(12, 15, 0); // sw a2, 0(a5)
            self.j(&lbl_done);

            // 溢出路径
            self.label(&lbl_overflow);
            self.emit_ld(13, 15, 8); // ld a3, 8(a5) (overflow_arg_area)
            self.mov_a_reg_sp_off(14, slot);
            for i in 0..(slot_size / 8) {
                let off = i * 8;
                self.emit_ld(10, 13, off);
                self.emit_sd(10, 14, off);
            }
            // overflow_arg_area += slot_size
            self.emit_addi(13, 13, slot_size);
            self.emit_sd(13, 15, 8); // sd a3, 8(a5)
            self.label(&lbl_done);
        } else {
            // 大结构体 (> 16 字节): 直接从 overflow_arg_area 读取
            self.mov_a_reg_sp_off(15, ap_off);
            self.emit_ld(13, 15, 8); // ld a3, 8(a5)
            self.mov_a_reg_sp_off(14, slot);
            for i in 0..(slot_size / 8) {
                let off = i * 8;
                self.emit_ld(10, 13, off);
                self.emit_sd(10, 14, off);
            }
            self.emit_addi(13, 13, slot_size);
            self.emit_sd(13, 15, 8);
        }

        // a0 = slot 地址 (与 sret 调用约定一致)
        self.mov_a_reg_sp_off(10, slot);
    }
}
