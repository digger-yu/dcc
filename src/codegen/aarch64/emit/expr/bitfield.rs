// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::Type;
use crate::codegen::aarch64::codegen::{Aarch64Codegen, HfaInfo};
use crate::codegen::aarch64::insn::Insn;
use crate::ir::{ExprKind, TypedExpr};

impl Aarch64Codegen {
    pub(crate) fn gen_bitfield_store_aarch64(
        &mut self,
        base_expr: &TypedExpr,
        rhs: &TypedExpr,
        field_off: usize,
        bit_width: u32,
        bit_offset: u32,
        container_ty: &Type,
    ) {
        let csz = container_ty.size();
        // 1. Compute container address -> x0, save to stack
        if let ExprKind::Dereference = &base_expr.kind {
            self.gen_expr(&base_expr.children[0]);
        } else {
            self.gen_lvalue_addr(base_expr);
        }
        if field_off != 0 {
            self.emit_mov_x_reg(12, 0);
            self.emit_add_imm_x(0, 12, field_off as u32);
        }
        // x0 = container address, save to stack
        let addr_off = self.alloc_local("<bf_addr>", 8);
        self.emit_str_x(0, 31, addr_off as u32, 8);
        // 2. Evaluate rhs -> x0, mask to bit_width, save to stack
        self.gen_expr(rhs);
        if bit_width < 64 {
            let mask: u64 = (1u64 << bit_width) - 1;
            self.mov_x_reg_imm(12, mask);
            self.emit_and_x_reg(0, 0, 12);
        }
        let val_off = self.alloc_local("<bf_val>", 8);
        self.emit_str_x(0, 31, val_off as u32, 8);
        // 3. Load current container value
        self.emit_ldr_x(0, 31, addr_off as u32, 8); // x0 = addr
        self.emit_ldr_x(13, 0, 0, csz); // x13 = container
                                        // 4. Clear target bits in container
        let clear_mask: u64 = !(((1u64 << bit_width) - 1) << bit_offset);
        self.mov_x_reg_imm(11, clear_mask);
        self.emit_and_x_reg(13, 13, 11); // x13 &= clear_mask
                                         // 5. Shift new value into position and OR into container
        self.emit_ldr_x(14, 31, val_off as u32, 8); // x14 = new val
        if bit_offset > 0 {
            self.emit_lsl_x(14, 14, bit_offset as u8);
        }
        self.emit_or_x_reg(13, 13, 14); // x13 |= x14
                                        // 6. Store container back
        self.emit_ldr_x(0, 31, addr_off as u32, 8); // x0 = addr
        self.emit_str_x(13, 0, 0, csz); // [x0] = x13
                                        // 7. Result of assignment = rhs value
        self.emit_ldr_x(0, 31, val_off as u32, 8);
    }

    /// 将 HFA 从 v 寄存器组存储到栈 [sp+off] (用于 HFA 返回/传参)
    pub(crate) fn store_hfa_from_vregs(&mut self, off: i32, hfa: HfaInfo, base_reg: u8) {
        let elem_bytes = (hfa.elem_bits / 8) as i32;
        for i in 0..hfa.count {
            let elem_off = off + i as i32 * elem_bytes;
            let vreg = base_reg + i;
            if hfa.elem_bits == 32 {
                let imm12 = (elem_off / 4) as u32 & 0xfff;
                let insn = 0xbd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            } else {
                let imm12 = (elem_off / 8) as u32 & 0xfff;
                let insn = 0xfd000000u32 | imm12 << 10 | 31 << 5 | (vreg as u32 & 0x1f);
                self.emit_insn(&Insn::raw(insn));
            }
        }
    }

    // ========================================================================
    // TLS 访问序列 (TLSDESC / Initial Exec / Local Exec)
    // ========================================================================
}
