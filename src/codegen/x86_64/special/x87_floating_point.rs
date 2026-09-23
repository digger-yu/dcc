// src/codegen/x86_64/special/x87_floating_point.rs
// x87 FPU（-mno-sse / long double）指令發射与浮点比较 codegen。
//
// 本文件由 mod.rs 逐字搬迁而来 (batch C)：`gen_float_cmp_x87` 与
// `emit_x87_fld_mem` / `emit_x87_fstp_mem` / `emit_x87_fstp_mem_typed`
// / `emit_x87_fstp_st0` / `emit_x87_to_xmm0` / `emit_x87_fldz` / `emit_x87_fld1`
// / `emit_x87_float_imm` / `emit_x87_binop` / `emit_x87_cmp_and_pop` / `emit_x87_chs`
// 家族，以及顶层 `f64_to_x87_80` 原样剪切，函数体一字未改，
// 仅把 `fn` 提为 `pub fn`（mod.rs 是祖先模块，需要可见性）。
//
// 说明：原影子里的 `emit_x87_fld` / `emit_x87_fst` 与生产命名不一致
// （生产为 `emit_x87_fld_mem` / `emit_x87_fstp_mem*`），按"以生产代码为准"作废。
// inline-asm 相关的 `asm_constraint_wants_x87` / `prepare_asm_x87_binding`
// / `emit_asm_x87_mem` / `emit_asm_x87_st` 属 asm/ 内联汇编子系统，
// 与本 x87 发射家族不同源，本批不动。

use crate::ast::*;
use crate::codegen::x86_64::Codegen;


impl Codegen {
    /// x87 浮点比较: fucomip + setcc -> al -> movzx rax, al
    /// 调用前: 需要加载左操作数和右操作数到 x87 栈
    pub fn gen_float_cmp_x87(&mut self, op: BinOp, ty: &Type, l: &Expr, r: &Expr) {
        let is_ld = matches!(ty, Type::LongDouble);
        let save_bytes = if is_ld { 16 } else { 8 };
        // 加载左操作数到 st(0)
        self.gen_expr(l, 0); // st(0) = l
        // 保存到临时栈 (long double 用 tbyte, 否则 qword)
        self.emit(&[0x48, 0x83, 0xEC, save_bytes as u8]); // sub rsp, save_bytes
        if is_ld {
            self.emit(&[0xDB, 0x3C, 0x24]); // fstp tbyte [rsp]
        } else {
            self.emit(&[0xDD, 0x1C, 0x24]); // fstp qword [rsp]
        }
        self.expr_depth += 1;
        // 加载右操作数到 st(0)
        self.gen_expr(r, 0); // st(0) = r
        self.expr_depth -= 1;
        // 重新加载左操作数: st(0) = l, st(1) = r
        if is_ld {
            self.emit(&[0xDB, 0x2C, 0x24]); // fld tbyte [rsp]
        } else {
            self.emit(&[0xDD, 0x04, 0x24]); // fld qword [rsp]
        }
        self.emit(&[0x48, 0x83, 0xC4, save_bytes as u8]); // add rsp, save_bytes
        // 现在 st(0) = l, st(1) = r
        // fucomip st(0), st(1): 比较 st(0) vs st(1), 设置 EFLAGS, 弹出 st(0)
        self.emit(&[0xDF, 0xE9]); // fucomip st(0), st(1)
        // 设置条件码 (fucomip 设置的是与 ucomisd 相同的标志)
        let cc = match op {
            BinOp::Lt => 0x92,   // jb (below)
            BinOp::Le => 0x96,   // jbe (below or equal)
            BinOp::Gt => 0x97,   // ja (above)
            BinOp::Ge => 0x93,   // jae (above or equal)
            BinOp::EqEq => 0x94, // je
            BinOp::Ne => 0x95,   // jne
            _ => 0x94,
        };
        // setcc al
        self.emit(&[0x0F, cc, 0xC0]);
        // movzx eax, al
        self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // x87 FPU 指令發射 (-mno-sse 模式專用)
    // ════════════════════════════════════════════════════════════════════════════

    /// x87 fld: 加載內存到 st(0)
    /// float (32-bit): D9 /0, double (64-bit): DD /0, long double (80-bit): DB /5
    pub fn emit_x87_fld_mem(&mut self, off: i64, ty: &Type) {
        let (opcode, reg_field) = match ty {
            Type::Float => (0xD9u8, 0u8),
            Type::LongDouble => (0xDBu8, 5u8),
            _ => (0xDDu8, 0u8),
        };
        if Self::fits_disp32(off) {
            self.emit(&[0x41, opcode]);
            self.emit_r13_modrm_disp(reg_field, off);
        } else {
            self.emit_lea_r13(11, off);
            self.emit(&[0x41, opcode]);
            self.emit_reg_indirect_modrm(reg_field, 11);
        }
    }

    /// x87 fstp: 存儲 st(0) 到內存並彈出棧
    /// float (32-bit): D9 /3, double (64-bit): DD /3, long double (80-bit): DB /7
    pub fn emit_x87_fstp_mem(&mut self, off: i64) {
        self.emit_x87_fstp_mem_typed(off, &Type::Double)
    }
    pub fn emit_x87_fstp_mem_typed(&mut self, off: i64, ty: &Type) {
        let (opcode, reg_field) = match ty {
            Type::Float => (0xD9u8, 3u8),
            Type::LongDouble => (0xDBu8, 7u8),
            _ => (0xDDu8, 3u8),
        };
        if Self::fits_disp32(off) {
            self.emit(&[0x41, opcode]);
            self.emit_r13_modrm_disp(reg_field, off);
        } else {
            self.emit_lea_r13(11, off);
            self.emit(&[0x41, opcode]);
            self.emit_reg_indirect_modrm(reg_field, 11);
        }
    }

    /// x87 fstp st(0): 彈出棧頂 (不存儲)
    pub fn emit_x87_fstp_st0(&mut self) {
        // fstp st(0): DD D9
        self.emit(&[0xDD, 0xD9]);
    }

    /// x87 st(0) (long double) → xmm0 (double): 先存储为 double 再加载到 XMM
    pub fn emit_x87_to_xmm0(&mut self) {
        // fstp qword [rsp]: 将 st(0) 截断为 double 存储
        self.emit(&[0xDD, 0x1C, 0x24]);
        // movsd xmm0, [rsp]: 加载到 xmm0
        self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]);
    }

    /// x87 fldz: 加載 +0.0 到 st(0)
    pub fn emit_x87_fldz(&mut self) {
        // fldz: D9 EE
        self.emit(&[0xD9, 0xEE]);
    }

    /// x87 fld1: 加載 +1.0 到 st(0)
    pub fn emit_x87_fld1(&mut self) {
        // fld1: D9 E8
        self.emit(&[0xD9, 0xE8]);
    }

    /// x87 立即數加載: 將浮點常量存入 .rodata, 用 fld 加載到 st(0)
    pub fn emit_x87_float_imm(&mut self, v: f64, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        let bytes = if is_float {
            let f = v as f32;
            f.to_le_bytes().to_vec()
        } else if is_ld {
            // long double: 将 f64 转换为 x87 80-bit 扩展精度格式
            let mut b = f64_to_x87_80(v);
            b.extend_from_slice(&[0u8; 6]); // 填充到 16 字节
            b
        } else {
            v.to_le_bytes().to_vec()
        };
        let key = format!(
            "x87f_{}",
            bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>()
        );
        let key_bytes = key.into_bytes();
        if !self.string_pool.contains_key(&key_bytes) {
            let o = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&bytes);
            self.string_pool.insert(key_bytes.clone(), o);
        }
        let off = self.string_pool[&key_bytes] as i64;
        // fld [rip + disp32]
        // float: D9 /0 = fld m32fp, ModRM=0x05
        // double: DD /0 = fld m64fp, ModRM=0x05
        // long double: DB /5 = fld m80fp, ModRM=0x2D (reg=101, rm=101)
        let (opcode, modrm) = if is_float {
            (0xD9u8, 0x05u8)
        } else if is_ld {
            (0xDBu8, 0x2Du8)
        } else {
            (0xDDu8, 0x05u8)
        };
        let at = self.pos();
        self.emit(&[opcode, modrm, 0, 0, 0, 0]);
        // x87 fld [rip+disp32] 是 6 字节指令 (disp32 @ byte 2),
        // 不同于 SSE movsd/movss 的 8 字节 (disp32 @ byte 4).
        self.x87_float_refs.push((at, off as u64));
    }

    /// x87 浮點二元運算: st(0) = st(0) op st(1), 然後彈出 st(0)
    /// 調用前: st(0) = 右操作數, st(1) = 左操作數
    pub fn emit_x87_binop(&mut self, op: BinOp, is_float: bool) {
        match op {
            BinOp::Add => {
                // faddp st(1), st(0): DE C1
                self.emit(&[0xDE, 0xC1]);
            }
            BinOp::Sub => {
                // fsubp: st(1) = st(1) - st(0), 然後彈出
                // 需要交換: st(0) 是右, st(1) 是左
                // fsubrp st(1), st(0): DE E9 (反向減: st(1) = st(0) - st(1))
                // 或 fsubp st(1), st(0): DE E9
                self.emit(&[0xDE, 0xE9]);
            }
            BinOp::Mul => {
                // fmulp st(1), st(0): DE C9
                self.emit(&[0xDE, 0xC9]);
            }
            BinOp::Div => {
                // fdivrp st(1), st(0): DE F9 (反向除: st(1) = st(0) / st(1))
                self.emit(&[0xDE, 0xF9]);
            }
            _ => {}
        }
        let _ = is_float;
    }

    /// x87 浮點比較: 設置 EFLAGS 供 setcc 使用
    /// 調用前: st(0) = 右操作數, st(1) = 左操作數
    pub fn emit_x87_cmp_and_pop(&mut self) {
        // fucomip st(0), st(1): DF E9
        // 比較 st(0) vs st(1), 設置 EFLAGS, 然後彈出 st(0)
        self.emit(&[0xDF, 0xE9]);
    }

    /// x87 浮點取負: st(0) = -st(0)
    pub fn emit_x87_chs(&mut self) {
        // fchs: D9 E0
        self.emit(&[0xD9, 0xE0]);
    }

}

/// 将 f64 (IEEE 754 双精度) 转换为 x87 80-bit 扩展精度格式 (10 字节, LE)
/// x87 格式: 1 bit sign + 15 bits exponent (bias 16383) + 64 bits significand (explicit integer bit)
fn f64_to_x87_80(v: f64) -> Vec<u8> {
    let bits = v.to_bits();
    let sign = (bits >> 63) & 1;
    let exp_f64 = ((bits >> 52) & 0x7FF) as i64;
    let frac_f64 = bits & 0x000F_FFFF_FFFF_FFFF;

    if exp_f64 == 0 && frac_f64 == 0 {
        // Zero
        let mut result = vec![0u8; 10];
        result[9] = (sign as u8) << 7;
        return result;
    }

    if exp_f64 == 0x7FF {
        // Infinity or NaN
        let sign_byte = (sign as u8) << 7;
        if frac_f64 == 0 {
            // Infinity
            let mut result = vec![0u8; 10];
            result[8] = 0x80;
            result[9] = 0xFF | sign_byte;
            return result;
        } else {
            // NaN
            let mut result = vec![0u8; 10];
            let sig_x87 = 0x8000_0000_0000_0000u64 | (frac_f64 << 11);
            result[0..8].copy_from_slice(&sig_x87.to_le_bytes());
            result[9] = 0xFF | sign_byte;
            return result;
        }
    }

    // Normal number: f64 bias=1023, x87 bias=16383
    let exp_x87 = (exp_f64 - 1023 + 16383) as u16;
    let sig_x87 = 0x8000_0000_0000_0000u64 | (frac_f64 << 11);

    let mut result = vec![0u8; 10];
    result[0..8].copy_from_slice(&sig_x87.to_le_bytes());
    result[8] = (exp_x87 & 0xFF) as u8;
    result[9] = ((sign as u8) << 7) | ((exp_x87 >> 8) as u8);
    result
}
