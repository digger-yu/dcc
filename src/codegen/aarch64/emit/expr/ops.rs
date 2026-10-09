// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{BinOp, Expr, Type};
use crate::codegen::aarch64::codegen::{align_rodata, align_up_u64, Aarch64Codegen, BranchKind};
use crate::codegen::aarch64::insn::Insn;
use crate::codegen::shared::elf::{self, ElfRela, R_AARCH64_ADR_PREL_LO21};
use crate::ir::ValueType;

impl Aarch64Codegen {
    pub(crate) fn load_global_addr_w0(&mut self, name: &str) {
        // TLS 全局变量: 分发 TLS 访问序列 (获取 TLS 变量地址到 x0)
        if self.tls_globals.contains_key(name) {
            if self.relocatable && !self._target.kernel_code_model {
                // 可重定位模式 (非内核): 发出经典 TLSDESC 4 指令序列 + 3 重定位
                self.emit_tlsdesc_access(name);
            } else {
                // ET_EXEC 模式或内核代码模型: 使用 Local Exec 序列 (直接 TP + offset)
                // 内核不使用动态 TLS, 即使 ET_REL 也直接用 LE
                self.emit_tls_le_access(name);
            }
            return;
        }
        if self.relocatable {
            let pos = self.pos();
            self.emit_adr_placeholder(0);
            if let Some(&idx) = self.sym_index.get(name) {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: idx,
                    rel_type: elf::R_AARCH64_ADR_PREL_LO21,
                    addend: 0,
                });
            }
        } else if let Some(&off) = self.global_rodata.get(name) {
            let rodata_start = self.entry + align_up_u64(self.code.len() as u64, 8);
            let target = rodata_start + off;
            let pos = self.pos();
            self.emit_adr_placeholder(0);
            self.pending_adr_exec.push((pos, 0, target));
        }
    }

    /// 整数提升: 将小于 int 的整数类型提升到 32 位
    /// char/short → 符号扩展 (SXTB/SXTH); uchar/ushort/bool → 零扩展 (UXTB/UXTH)
    pub(crate) fn gen_int_promote(&mut self, ty: &Type) {
        match ty {
            Type::Char | Type::SChar | Type::Bool => {
                // SXTB W0, W0: 0x13000000
                self.emit_insn(&Insn::raw(0x13000000));
            },
            Type::Short => {
                // SXTH W0, W0: 0x13000400
                self.emit_insn(&Insn::raw(0x13000400));
            },
            Type::UChar => {
                // UXTB W0, W0: 0x53000000
                self.emit_insn(&Insn::raw(0x53000000));
            },
            Type::UShort => {
                // UXTH W0, W0: 0x53000400
                self.emit_insn(&Insn::raw(0x53000400));
            },
            _ => {}, // int/long/ptr 等不需要提升
        }
    }

    /// IR 级别整数提升: ValueType 版本的整数提升
    pub(crate) fn gen_int_promote_ir(&mut self, ty: &ValueType) {
        if let ValueType::Integer { bits, signed } = ty {
            match (*bits, *signed) {
                (1, _) | (8, true) => {
                    // SXTB W0, W0 (bool/char → sign-extend from 8 bits)
                    self.emit_insn(&Insn::raw(0x13000000));
                },
                (8, false) => {
                    // UXTB W0, W0 (uchar → zero-extend from 8 bits)
                    self.emit_insn(&Insn::raw(0x53000000));
                },
                (16, true) => {
                    // SXTH W0, W0 (short → sign-extend from 16 bits)
                    self.emit_insn(&Insn::raw(0x13000400));
                },
                (16, false) => {
                    // UXTH W0, W0 (ushort → zero-extend from 16 bits)
                    self.emit_insn(&Insn::raw(0x53000400));
                },
                _ => {}, // 32-bit 及以上不需要提升
            }
        }
    }

    pub(crate) fn store_result(&mut self, off: i32, ty: &crate::ast::Type) {
        if matches!(ty, Type::Float | Type::Double) {
            self.str_s0_sp(off);
        } else if matches!(ty, Type::Vector(_, _)) {
            self.str_q0_sp(off);
        } else {
            self.store_w0(off);
        }
    }

    pub(crate) fn alloc_local(&mut self, name: &str, bytes: i32) -> i32 {
        let align = if bytes >= 16 { 16 } else { 8 };
        let off = (self.stack_bytes + align - 1) & !(align - 1);
        let aligned = (bytes + align - 1) & !(align - 1);
        self.stack_bytes = off + aligned;
        self.locals.insert(name.to_string(), off);
        off
    }

    pub(crate) fn truncate_w0_to(&mut self, ty: &ValueType) {
        match ty {
            ValueType::Integer {
                bits: 8,
                signed: true,
            } => {
                self.emit_insn(&Insn::raw(0x13001800));
            },
            ValueType::Integer {
                bits: 8,
                signed: false,
            } => {
                self.emit_insn(&Insn::raw(0x13001400));
            },
            ValueType::Integer {
                bits: 16,
                signed: true,
            } => {
                self.emit_insn(&Insn::raw(0x12001800));
            },
            ValueType::Integer {
                bits: 16,
                signed: false,
            } => {
                self.emit_insn(&Insn::raw(0x12001400));
            },
            ValueType::Integer { bits: 32, .. } => {},
            _ => {},
        }
    }

    /// 判断 AST 类型是否为有符号整数 (用于移位等有符号性判定)
    pub(crate) fn is_signed_ast_type(ty: &Type) -> bool {
        matches!(
            ty,
            Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong | Type::Int128
        )
    }

    pub(crate) fn binop_w0_w1(&mut self, op: BinOp, is_signed: bool) {
        match op {
            BinOp::Add => {
                self.emit_insn(&Insn::raw(0x0b010000));
            }
            BinOp::Sub => {
                self.emit_insn(&Insn::raw(0x4b010000));
            }
            BinOp::Mul => {
                self.emit_insn(&Insn::raw(0x1b017c00));
            }
            BinOp::And => {
                self.emit_insn(&Insn::raw(0x0a010020));
            }
            BinOp::Or => {
                self.emit_insn(&Insn::raw(0x2a010020));
            }
            BinOp::Xor => {
                self.emit_insn(&Insn::raw(0x4a010020));
            }
            // M7 修复: 移位指令正确编码
            // lslv w0, w0, w1: 0x1A302000 LE = 00 20 30 1A
            //   bits[31:21] = 0b00011010001 (lslv op)
            //   bits[20:16] = Rm (1 for w1)
            //   bits[15:10] = 0b001000
            //   bits[9:5] = Rn (0 for w0)
            //   bits[4:0] = Rd (0 for w0)
            // lsrv w0, w0, w1: 0x1A342000 LE = 00 20 34 1A
            //   bits[23:21] = 0b010 (lsrv op)
            // asrv w0, w0, w1: 0x1A742000 LE = 00 20 74 1A
            //   bits[23:21] = 0b110 (asrv op, 算术右移, 符号扩展)
            // 32 位变量移位 (LSLV/LSRV/ASRV) 硬件自动掩码移位量
            // 到 5 位 (shamt & 31), 与 C 标准 int 宽度一致.
            // 常量求值阶段 (const_eval) 已拒绝无效移位量.
            BinOp::Shl => {
                self.emit_insn(&Insn::raw(0x1a302000));
            } // lslv w0, w0, w1
            BinOp::Shr => {
                if is_signed {
                    // asrv: 算术右移, 高位补符号位 (C11 6.5.7)
                    self.emit_insn(&Insn::raw(0x1a742000));
                } else {
                    // lsrv: 逻辑右移, 高位补 0
                    self.emit_insn(&Insn::raw(0x1a342000));
                }
            },
            // M7 修复: 真 sdiv / msub 替代 0
            // binop_w0_w1 之前已把 w0=left, w1=right, w14=left (保存)
            //   Div: sdiv w0, w0, w1   => 0x9AC00C20
            //   Mod: sdiv w0, w0, w1   (w0 = quotient)
            //        msub w0, w1, w0, w14 (w0 = w14 - w1*w0 = left mod right)
            // 编码来源: ARMv8 AArch64 指令集, sdiv 32-bit 形式
            // 限制: 不区分有/无符号 (始终有符号);调用方保证 IR 已 folding 常量
            BinOp::Div => {
                self.emit_insn(&Insn::raw(0x9ac00c20));
            } // sdiv w0, w0, w1
            BinOp::Mod => {
                self.emit_insn(&Insn::raw(0x9ac00c20)); // sdiv w0, w0, w1
                self.emit_insn(&Insn::raw(0x9bb083a0)); // msub w0, w1, w0, w14
            },
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.cmp_w0_w1();
                let cond = match op {
                    BinOp::EqEq => 0x1,
                    BinOp::Ne => 0x0,
                    BinOp::Lt => 0xb,
                    BinOp::Le => 0xa,
                    BinOp::Gt => 0xc,
                    BinOp::Ge => 0xd,
                    _ => 0x1,
                };
                let insn = 0x1a9f_0000u32 | (cond << 12) | 0x20;
                self.emit_insn(&Insn::raw(insn));
            },
            BinOp::AndAnd | BinOp::OrOr => self.cmp_w0_zero(),
        }
    }

    pub(crate) fn float_binop_s0_s1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => {
                self.emit_insn(&Insn::raw(0x1e210020));
            } // fadd s0, s0, s1
            BinOp::Sub => {
                self.emit_insn(&Insn::raw(0x1e410020));
            }
            BinOp::Mul => {
                self.emit_insn(&Insn::raw(0x1e610020));
            }
            BinOp::Div => {
                self.emit_insn(&Insn::raw(0x1e810020));
            }
            BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.fcmp_s0_s1();
                self.fset_w0_from_fcmp(op);
            },
            _ => {
                self.emit_insn(&Insn::raw(0x1e210020));
            }
        }
    }

    pub(crate) fn vector_binop_v0_v1(&mut self, op: BinOp) {
        match op {
            BinOp::Add => {
                self.emit_insn(&Insn::raw(0x4e211420));
            } // add v0.4s, v0.4s, v1.4s
            BinOp::Sub => {
                self.emit_insn(&Insn::raw(0x4e411420));
            }
            BinOp::Mul => {
                self.emit_insn(&Insn::raw(0x4e819420));
            }
            _ => {
                self.emit_insn(&Insn::raw(0x4e211420));
            }
        }
    }

    /// Extract source span from any AST Expr variant (for diagnostics).
    pub(crate) fn ast_expr_span(e: &Expr) -> crate::diagnostics::Span {
        match e {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::String { span, .. }
            | Expr::Char { span, .. }
            | Expr::Ident { span, .. }
            | Expr::Binop { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Cond { span, .. }
            | Expr::Cast { span, .. }
            | Expr::SizeofTy { span, .. }
            | Expr::SizeofExpr { span, .. }
            | Expr::Index { span, .. }
            | Expr::Member { span, .. }
            | Expr::AddrOf { span, .. }
            | Expr::Deref { span, .. }
            | Expr::PostInc { span, .. }
            | Expr::PostDec { span, .. }
            | Expr::PreInc { span, .. }
            | Expr::PreDec { span, .. }
            | Expr::CompoundLit { span, .. }
            | Expr::InitList { span, .. }
            | Expr::AlignofTy { span, .. }
            | Expr::AlignofExpr { span, .. }
            | Expr::StmtExpr { span, .. }
            | Expr::Comma { span, .. }
            | Expr::AsmExpr { span, .. }
            | Expr::LabelAddr { span, .. }
            | Expr::VaArg { span, .. }
            | Expr::Offsetof { span, .. } => span.clone(),
            _ => crate::diagnostics::Span::point("", 0, 0),
        }
    }

    /// 发射 NOP 填充到对齐边界 (AArch64: NOP = 0xD503201F, 4 字节)
    pub(crate) fn emit_align_padding(&mut self, alignment: u32) {
        if alignment <= 1 {
            return;
        }
        let pos = self.code.len() as u64;
        let aligned = (pos + alignment as u64 - 1) & !(alignment as u64 - 1);
        let pad = (aligned - pos) as usize;
        // AArch64 NOP 是 4 字节指令, 只能按 4 字节边界填充
        let nop_count = pad / 4; // 整数个 NOP
        for _ in 0..nop_count {
            self.emit_insn(&Insn::raw(0xd503201f)); // NOP
        }
    }

    /// 如果配置了 align_loops, 在循环头部标签前发射对齐填充
    pub(crate) fn maybe_align_loop(&mut self) {
        if self._target.align_loops > 1 {
            self.emit_align_padding(self._target.align_loops);
        }
    }

    /// 如果配置了 align_jumps, 在跳转目标标签前发射对齐填充
    pub(crate) fn maybe_align_jump(&mut self) {
        if self._target.align_jumps > 1 {
            self.emit_align_padding(self._target.align_jumps);
        }
    }

    pub(crate) fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{prefix}{}", self.label_serial)
    }

    pub(crate) fn pos(&self) -> u64 {
        self.code.len() as u64
    }

    pub(crate) fn label(&mut self, name: &str) {
        let pos = self.pos();
        self.labels.insert(name.to_string(), pos);
        let bl_pending: Vec<_> = self
            .pending_bl_calls
            .iter()
            .filter(|(_, l)| l == name)
            .cloned()
            .collect();
        for (at, _) in bl_pending {
            self.patch_bl(at, pos);
        }
        self.pending_bl_calls.retain(|(_, l)| l != name);
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::B => self.patch_b26(at, pos),
                BranchKind::Cbz => self.patch_cbz(at, pos),
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != name);
        // 修补条件分支
        let bcond_pending: Vec<_> = self
            .pending_bcond
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, cond) in bcond_pending {
            self.patch_bcond(at, pos, cond);
        }
        self.pending_bcond.retain(|(_, l, _)| l != name);
    }

    /// 底层字节发射 (唯一出口); 只供 `emit_insn` 使用, 其它调用必须走 `emit_insn`.
    pub(crate) fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    pub(crate) fn load_float_s0(&mut self, v: f32) {
        let off = align_rodata(&mut self.rodata, 4);
        let bits = v.to_bits();
        self.rodata.extend_from_slice(&bits.to_le_bytes());
        if self.relocatable {
            let rodata_idx = self.sym_index.get(".rodata").copied().unwrap_or(1);
            let pos = self.pos();
            self.emit_adr_placeholder(9);
            if rodata_idx > 0 {
                self.relocations.push(ElfRela {
                    offset: pos,
                    sym_index: rodata_idx,
                    rel_type: R_AARCH64_ADR_PREL_LO21,
                    addend: off as i64,
                });
            }
            // ldr s0, [x9] — 绝对偏移 0
            self.emit_insn(&Insn::raw(0xbd400020));
        } else {
            let pos = self.pos();
            self.emit_ldr_s0_u32_placeholder();
            self.float_refs.push((pos, off));
        }
    }
}
