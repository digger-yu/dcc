// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::Type;
use crate::codegen::riscv64::codegen::Riscv64Codegen;
use crate::ir::{ExprKind, TypedExpr};

impl Riscv64Codegen {
    /// Bitfield read-modify-write for riscv64.
    /// Uses a0=addr, a1=container, a2=new_val, a3=scratch.
    pub(crate) fn gen_bitfield_store_riscv64(
        &mut self,
        base_expr: &TypedExpr,
        rhs: &TypedExpr,
        field_off: usize,
        bit_width: u32,
        bit_offset: u32,
        container_ty: &Type,
    ) {
        let csz = container_ty.size();
        // 1. Compute container address -> a0, save to stack
        if let ExprKind::Dereference = &base_expr.kind {
            self.gen_expr(&base_expr.children[0]);
        } else {
            self.gen_lvalue_addr(base_expr);
        }
        if field_off != 0 {
            self.emit_addi(10, 10, field_off as i32);
        }
        let addr_off = self.alloc_local("<bf_addr>", 8);
        self.emit_sd(10, 2, addr_off); // save addr
                                       // 2. Evaluate rhs -> a0, mask to bit_width, save to stack
        self.gen_expr(rhs);
        if bit_width < 64 {
            let mask: u64 = (1u64 << bit_width) - 1;
            if mask <= 0x7FF {
                self.emit_andi(10, 10, mask as i32);
            } else {
                let sv = self.alloc_local("<bf_rv>", 8);
                self.emit_sd(10, 2, sv);
                self.load_const64_a0(mask);
                self.emit_addi(11, 10, 0); // a1 = mask
                self.emit_ld(10, 2, sv); // a0 = rhs
                self.emit_and(10, 10, 11); // a0 &= mask
            }
        }
        let val_off = self.alloc_local("<bf_val>", 8);
        self.emit_sd(10, 2, val_off); // save masked rhs
                                      // 3. Load current container value
        self.emit_ld(10, 2, addr_off); // a0 = addr
        self.emit_load_sz(11, 10, 0, csz); // a1 = container
                                           // 4. Clear target bits: a1 &= ~((mask) << bit_offset)
        let clear_mask: u64 = !(((1u64 << bit_width) - 1) << bit_offset);
        if clear_mask as i64 >= -2048 && clear_mask as i64 <= 2047 {
            self.emit_andi(11, 11, clear_mask as i32);
        } else {
            let sv2 = self.alloc_local("<bf_c>", 8);
            self.emit_sd(11, 2, sv2); // save container
            self.load_const64_a0(clear_mask); // a0 = clear_mask
            self.emit_addi(12, 10, 0); // a2 = clear_mask
            self.emit_ld(11, 2, sv2); // a1 = container
            self.emit_and(11, 11, 12); // a1 &= clear_mask
        }
        // 5. Shift new value and OR into container
        self.emit_ld(12, 2, val_off); // a2 = masked rhs
        if bit_offset > 0 {
            self.emit_slli(12, 12, bit_offset as u8); // a2 <<= bit_offset
        }
        self.emit_or(11, 11, 12); // a1 |= a2
                                  // 6. Store container back
        self.emit_ld(10, 2, addr_off); // a0 = addr
        self.emit_store_sz(11, 10, 0, csz); // [a0] = a1
                                            // 7. Result = rhs value
        self.emit_ld(10, 2, val_off);
    }
}
