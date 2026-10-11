// src/codegen/x86_64/insn.rs
//
// x86_64 后端 - 指令记录模型 (统一发射入口).
//
// 第一性原理: x86_64 指令**变长** (1~15 字节, 含 REX / ModRM / SIB / disp / imm / 前缀),
// 不能像定长 ISA (aarch64/mips/arm/riscv32) 那样用 `u32` 字面值装. 因此本模型用
// `Raw` 承接迁移期所有 `self.emit(&[..])` 调用点; P9 步骤 1 起, 常用族 (AluRm /
// MovRm / UnaryRm / Shift / Lea / Call / Jmp / Jcc / Ret / Stack / SimdArith /
// SimdMov / X87) 逐步抽为类型化构造子, 其 encode/render 真源集中在 `opcode.rs`.
//
// 迁移纪律 (G11): 纯移动, 构造子编码 == 现有字节. 不变更任何位型.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};
use crate::codegen::x86_64::opcode::X86_64Opcode;

#[derive(Clone, Debug)]
pub struct Insn {
    pub op: X86_64Opcode,
    pub ops: [Operand; 4],
    /// 兼容字段: 迁移期内 Raw 变体的字节序.
    /// 类型化变体的字节由 op.encode() 现场算, 不存这里.
    pub bytes: Vec<u8>,
    /// P9 方案 C: 发射点携带的 -S GAS 渲染文本.
    ///
    /// 真源语义: 一旦某条指令在 emit_insn 时带了 gas_text, -S 渲染器就**直接**
    /// 拿这一行输出 (不含前导 tab / 换行), 不再跑 disasm 反推. 没有这个字段,
    /// 渲染器必须退回字节反推. 这就是最终删除 disasm.rs 的前置条件.
    ///
    /// 注意: 这个字段**只影响 -S 文本**, 不参与 .text 段字节生成. 所以 G10 快照
    /// (校验 .text sha256) 对它零敏感.
    pub gas_text: Option<String>,
}

impl Insn {
    pub fn new(op: X86_64Opcode, ops: &[Operand]) -> Self {
        let mut arr = [Operand::None, Operand::None, Operand::None, Operand::None];
        let n = ops.len().min(4);
        arr[..n].clone_from_slice(&ops[..n]);
        Insn {
            op,
            ops: arr,
            bytes: Vec::new(),
            gas_text: None,
        }
    }

    /// 直接用字节切片构造 (迁移期兜底, 逐步替换为类型化构造子).
    /// x86_64 变长, 因此直接保留字节切片, 不做 u32/u16 拼装.
    ///
    /// P10 收敛: 原生发射路径一律用 raw_bytes (语义等价于 raw), 保留 raw
    /// 这个名字仅供 asm/ 内联汇编编码器继续使用 (那里走 raw_with_mnemonic).
    /// 新代码不要直接调这个构造子, 用 raw_with_gas(bytes, "gas text") 携带
    /// 发射点已知的 GAS 文本, 或类型化构造子 Insn::new(X86_64Opcode::...).
    #[doc(hidden)]
    pub fn raw_bytes(b: &[u8]) -> Self {
        Insn::new(
            X86_64Opcode::Raw {
                bytes: b.to_vec(),
                mnemonic: None,
            },
            &[],
        )
    }

    /// P11 Bucket I: 无操作数/固定前缀指令 (cld/mfence/rep movsb/stosb/movsq).
    pub fn noarg(op: crate::codegen::x86_64::opcode::NoArgOp) -> Self {
        Insn::new(X86_64Opcode::NoArg { op }, &[])
    }

    /// P11 Bucket K: movd/movq GPR↔XMM reg-reg.
    /// to_xmm=true: GPR→XMM (0x6E). width=B64 → movq (66 REX.W); B32 → movd.
    pub fn gpr_xmm(
        to_xmm: bool,
        width: crate::codegen::shared::asm_operand::Width,
        gpr_idx: u8,
        xmm_idx: u8,
    ) -> Self {
        let (dst, src) = if to_xmm {
            (
                crate::codegen::x86_64::opcode::Operand::Reg(
                    xmm_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                crate::codegen::x86_64::opcode::Operand::Reg(gpr_idx, width),
            )
        } else {
            (
                crate::codegen::x86_64::opcode::Operand::Reg(gpr_idx, width),
                crate::codegen::x86_64::opcode::Operand::Reg(
                    xmm_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            )
        };
        Insn::new(
            X86_64Opcode::GprXmm { to_xmm, width, dst, src },
            &[],
        )
    }

    /// asm/ 内联汇编路径专用 (P10 起原生路径已改用 raw_bytes).
    /// 语义与 raw_bytes 一致, 仅名字不同 —— 保留以避免改动内联汇编编码器的
    /// 几百处字面调用.
    pub fn raw(b: &[u8]) -> Self {
        Insn::raw_bytes(b)
    }

    pub fn nop() -> Self {
        Insn::new(
            X86_64Opcode::Raw {
                bytes: vec![0x90],
                mnemonic: Some("nop".into()),
            },
            &[],
        )
    }

    /// P9 步骤 5: 内联汇编发射完一段字节后, 用 (bytes, mnemonic) 就地包装成 Raw。
    /// 与 raw_bytes 的差别只是 mnemonic 字段填 Some, 供 render() 出 GAS 文本。
    /// 不改变任何发射字节 —— 只影响 -S 文本渲染 (步骤 6 起读 asm_insn_log)。
    pub fn raw_with_mnemonic(bytes: &[u8], mnemonic: &str) -> Self {
        let ops = [Operand::None, Operand::None, Operand::None, Operand::None];
        Insn {
            op: X86_64Opcode::Raw {
                bytes: bytes.to_vec(),
                mnemonic: Some(mnemonic.to_string()),
            },
            ops,
            bytes: Vec::new(),
            gas_text: Some(mnemonic.to_string()),
        }
    }

    /// P9 方案 C: 发射点已经知道这条指令最终在 -S 里要渲染成哪一行 GAS 文本,
    /// 直接携带. 渲染器不再需要反推字节.
    pub fn raw_with_gas(bytes: &[u8], gas_line: &str) -> Self {
        let ops = [Operand::None, Operand::None, Operand::None, Operand::None];
        Insn {
            op: X86_64Opcode::Raw {
                bytes: bytes.to_vec(),
                mnemonic: None,
            },
            ops,
            bytes: Vec::new(),
            gas_text: Some(gas_line.to_string()),
        }
    }

    /// P11: 给已构造的类型化 Insn 附加 GAS 文本 (等价于 raw_with_gas, 但不走 Raw 兜底).
    pub fn with_gas(mut self, gas_line: String) -> Self {
        self.gas_text = Some(gas_line);
        self
    }

    /// P11: 构造 `sub rsp, imm8` (imm 可装进 i8, 走 0x83 短形式, 48 83 EC ib).
    /// 注意: 这个形态**不**走 AluRm.encode() (因为 AluRm 默认走 0x81 imm32),
    /// 直接内联手写字节以保持 G10 快照逐字节一致.
    pub fn sub_rsp_imm(imm: i64) -> Self {
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[0x48, 0x83, 0xEC, v])
    }

    /// P11: 构造 `add rsp, imm8` (imm 可装进 i8, 走 0x83 短形式, 48 83 C4 ib).
    pub fn add_rsp_imm(imm: i64) -> Self {
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[0x48, 0x83, 0xC4, v])
    }

    /// P11 Bucket A: mov reg, reg (mod=11). width controls REX.W (B64) + REX.B/R (reg>=8).
    /// Encoding: MOV r/m, r (0x89 for non-byte; 0x88 for B8).
    pub fn mov_rm_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11 Bucket A: MovRm reg <-> mem [base+disp] (load or store).
    /// load=true: mov reg, [base+disp] (0x8B / 0x8A). load=false: mov [base+disp], reg (0x89 / 0x88).
    /// width: B8/B16/B32/B64. base: Intel reg number 0..15. disp: byte displacement (sign-ext).
    pub fn mov_rm_mem(
        load: bool,
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let reg = Operand::Reg(reg_idx, width);
        let mem = Operand::Mem(MemOperand {
            disp,
            base: Some(base),
            index: None,
            seg: None,
        });
        let (dst, src) = if load { (reg, mem) } else { (mem, reg) };
        Insn::new(X86_64Opcode::MovRm { width, dst, src }, &[])
    }

    /// P11 Bucket B: AluRm reg, reg (mod=11) — add/sub/and/or/xor/cmp.
    pub fn alu_rm_reg_reg(
        op: crate::codegen::x86_64::opcode::AluOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11 Bucket C: AluRm reg, [base+disp] (reg <- mem) — add/sub/and/or/xor/cmp.
    /// width: B64 (REX.W) or B32. base: Intel reg number 0..15. disp: byte displacement.
    pub fn alu_rm_mem(
        op: crate::codegen::x86_64::opcode::AluOp,
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        Insn::new(
            X86_64Opcode::AluRm {
                op,
                width,
                dst: Operand::Reg(dst_reg, width),
                src: Operand::Mem(MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket D: push/pop reg64 via Stack typed opcode (50+rd / 41 50+(rd&7)).
    pub fn stack_op_reg(
        op: crate::codegen::x86_64::opcode::StackOp,
        reg_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    reg_idx,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket V: LEA reg64, [base+disp]. disp 可为任意大小.
    /// 复用 Lea.encode() 路径 A/A2 (无 index).
    pub fn lea_reg_base_disp(
        dst_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        Insn::new(
            X86_64Opcode::Lea {
                dst: Operand::Reg(dst_reg, crate::codegen::shared::asm_operand::Width::B64),
                mem: MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                },
                rip_relative: false,
            },
            &[],
        )
    }

    /// P11: 构造 B32 `and/eor/or/cmp eax|ecx|edx, imm8` (0x83 / ib, mod=11).
    /// modrm_reg 字段编码 AluOp, modrm_rm 字段编码低 3 位寄存器索引.
    /// 与 rsp 不同, 非 rsp 寄存器 0x83 形态不常带 REX.W (G10 快照逐字节约定).
    pub fn alu_rm_reg_imm8(op: u8, reg_idx: u8, imm: i64) -> Self {
        let modrm = 0xC0 | ((op & 0x7) << 3) | (reg_idx & 0x7);
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[0x83, modrm, v])
    }

    /// P11 Bucket K: 构造 B64 `add/or/and/sub/xor/cmp reg, imm8` (REX.W 0x83 / ib, 短形式).
    /// 与 `alu_rm_reg_imm8` (B32 无 REX.W) 不同: 本构造器固定 REX.W, 用于
    /// 全局地址/帧指针等 64 位寄存器的小立即数加 (如 `add rax, offset`).
    /// op 编码: /0=add, /1=or, /4=and, /5=sub, /6=xor, /7=cmp.
    pub fn alu_rm_reg_imm8_b64(op: u8, reg_idx: u8, imm: i64) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((op & 0x7) << 3) | (reg_idx & 0x7);
        let v = imm as i8 as u8;
        Insn::raw_bytes(&[rex, 0x83, modrm, v])
    }

    /// P11 Bucket B: 构造 B64 `add/or/and/sub/xor/cmp reg, imm32` (0x81 / id 形式).
    /// 与 rsp 的 0x81 imm32 一致, 但任意 reg; 手写常拆成两条 emit_insn (opcode 三元组 + imm32).
    /// op 编码: /0=add, /1=or, /4=and, /5=sub, /6=xor, /7=cmp.
    pub fn alu_rm_reg_imm32_b64(op: u8, reg_idx: u8, imm: i64) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((op & 0x7) << 3) | (reg_idx & 0x7);
        let imm32 = (imm as i32).to_le_bytes();
        let mut out = Vec::with_capacity(7);
        out.push(rex);
        out.push(0x81);
        out.push(modrm);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket X: cmp eax, imm32 (0x3D id, no ModRM). 用于 va_arg 中与
    /// fp_offset / gp_offset 边界值的直接比较. imm 是 32 位有符号数.
    pub fn cmp_eax_imm32(imm: i32) -> Self {
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(5);
        out.push(0x3D);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket X: B32 `alu dword [base+disp], imm8` (0x83 / ib, no REX.W).
    /// 用于 va_arg gp_offset / fp_offset 字段的就地加 (如 `add dword [r13+disp], imm8`).
    /// op 编码: /0=add, /4=and, /5=sub, /7=cmp. base>=8 时自动加 REX.B.
    /// disp 必须能装入 i32 (本函数不做长度选择; 由 callsite 决定 mod=01/10).
    /// 当 disp 落在 i8 时走 mod=01, 否则 mod=10 disp32.
    pub fn alu_mem_imm8_b32(op: u8, base: u8, disp: i32, imm: i32) -> Self {
        let mut rex: u8 = 0x40;
        if base >= 8 {
            rex |= 0x01;
        }
        let need_rex = base >= 8;
        let rm_field = base & 7;
        let (modrm_mod, disp_bytes, disp_len): (u8, [u8; 4], usize) =
            if let Ok(d) = i8::try_from(disp) {
                let mut b = [0u8; 4];
                b[0] = d as u8;
                (1, b, 1)
            } else {
                (2, disp.to_le_bytes(), 4)
            };
        let modrm = (modrm_mod << 6) | ((op & 0x7) << 3) | rm_field;
        let v = imm as i8 as u8;
        let mut out = Vec::with_capacity(8);
        if need_rex {
            out.push(rex);
        }
        out.push(0x83);
        out.push(modrm);
        out.extend_from_slice(&disp_bytes[..disp_len]);
        out.push(v);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket X: B64 `alu qword [base+disp], imm32` (0x81 / id, REX.W).
    /// 用于 va_arg overflow_arg_area / reg_save_area 字段的就地加.
    pub fn alu_mem_imm32_b64(op: u8, base: u8, disp: i32, imm: i32) -> Self {
        let mut rex: u8 = 0x48;
        if base >= 8 {
            rex |= 0x01;
        }
        let rm_field = base & 7;
        let (modrm_mod, disp_bytes, disp_len): (u8, [u8; 4], usize) =
            if let Ok(d) = i8::try_from(disp) {
                let mut b = [0u8; 4];
                b[0] = d as u8;
                (1, b, 1)
            } else {
                (2, disp.to_le_bytes(), 4)
            };
        let modrm = (modrm_mod << 6) | ((op & 0x7) << 3) | rm_field;
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(11);
        out.push(rex);
        out.push(0x81);
        out.push(modrm);
        out.extend_from_slice(&disp_bytes[..disp_len]);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket T: 构造 B64 `shl/shr/sar reg, imm8` 强制 C1 形式 (总是带 imm8 字节).
    /// 与 typed `Shift{amount: Imm8(1)}` 自动走 D1 短形式不同, 位域读取点
    /// (lvalue.rs 位域右移) 固定使用 `REX.W C1 /r ib` 形态 (即使 imm=1 也发 4 字节).
    /// op 编码: /4=shl, /5=shr, /7=sar.
    pub fn shift_reg_imm8_c1_b64(op_field: u8, reg_idx: u8, imm8: u8) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((op_field & 0x7) << 3) | (reg_idx & 0x7);
        Insn::raw_bytes(&[rex, 0xC1, modrm, imm8])
    }

    /// P11 Bucket V: B64 `shl/shr/sar reg, cl` 强制 D3 形式 (REX.W D3 /r).
    /// op 编码: /4=shl, /5=shr, /7=sar.
    pub fn shift_reg_cl_b64(op_field: u8, reg_idx: u8) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((op_field & 0x7) << 3) | (reg_idx & 0x7);
        Insn::raw_bytes(&[rex, 0xD3, modrm])
    }

    /// P11 Bucket W: B32 `shl/shr/sar reg, imm8` (C1 /r ib, no REX.W).
    /// op 编码: /4=shl, /5=shr, /7=sar.
    pub fn shift_reg_imm8_c1_b32(op_field: u8, reg_idx: u8, imm8: u8) -> Self {
        let rex: u8 = 0x40;
        let need_rex = reg_idx >= 8;
        let modrm = 0xC0 | ((op_field & 0x7) << 3) | (reg_idx & 0x7);
        let mut out = Vec::with_capacity(4);
        if need_rex {
            out.push(rex | 0x01);
        }
        out.push(0xC1);
        out.push(modrm);
        out.push(imm8);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket T: 构造 B64 `test reg, imm32` (REX.W F7 /0 id).
    /// 与 `test_rm_reg` (reg,reg 形态 85) 不同, 这是 TEST r/m64, imm32.
    /// 位域符号位测试 (sign_bit <= 0xFFFFFFFF) 固定走这个形态.
    pub fn test_rm_reg_imm32_b64(reg_idx: u8, imm: i64) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        // F7 /0: modrm_reg_field=/0, r/m=reg_idx.
        let modrm = 0xC0 | (reg_idx & 0x7);
        let imm32 = (imm as i32).to_le_bytes();
        let mut out = Vec::with_capacity(7);
        out.push(rex);
        out.push(0xF7);
        out.push(modrm);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket T: 构造 `movabs r64, imm64` (REX.W B8+rd, 10 字节).
    /// 与 `mov_imm` 不同: mov_imm 在 |imm|<=i32 时走 C7 /0 符号扩展短形式;
    /// 位域 mask/inv_mask 路径需要立即数就是完整 64 位 (不能符号扩展),
    /// 故固定走 B8+rd 形式.
    pub fn movabs_reg_imm64(reg_idx: u8, imm: u64) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm_opc = 0xB8 | (reg_idx & 0x7);
        let imm64 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(10);
        out.push(rex);
        out.push(modrm_opc);
        out.extend_from_slice(&imm64);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket W: bswap r32/r64 (0F C8+rd / REX.W 0F C8+rd).
    /// width: B32 (no REX) or B64 (REX.W). reg_idx: intel register number.
    pub fn bswap_reg(width: crate::codegen::shared::asm_operand::Width, reg_idx: u8) -> Self {
        let mut out = Vec::with_capacity(3);
        if matches!(width, crate::codegen::shared::asm_operand::Width::B64) {
            let mut rex: u8 = 0x48;
            if reg_idx >= 8 {
                rex |= 0x01;
            }
            out.push(rex);
        }
        out.push(0x0F);
        out.push(0xC8 | (reg_idx & 0x7));
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket W: rol r16, imm8 (66 C1 /0 ib). Used for SSO 2-byte
    /// byte-swap: rol cx, 8 (or any other r16).
    pub fn rol_reg_imm8_b16(reg_idx: u8, imm8: u8) -> Self {
        let mut rex: u8 = 0;
        let mut body = Vec::with_capacity(5);
        body.push(0x66);
        if reg_idx >= 8 {
            rex = 0x41;
        }
        body.push(0xC1);
        // modrm: mod=11, reg field=/0 (rol), r/m=reg_idx.
        body.push(0xC0 | (reg_idx & 0x7));
        body.push(imm8);
        let mut out = Vec::with_capacity(body.len() + 1);
        if rex != 0 {
            out.push(rex);
        }
        out.extend_from_slice(&body);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket W: mov r64, imm32 (REX.W C7 /0 id, sign-extended 32-bit
    /// immediate). Used for small immediates (rep counters, small ints)
    /// where the imm64 form would waste bytes. Self-absorbs REX.B for
    /// reg_idx >= 8.
    pub fn mov_reg_imm32_b64(reg_idx: u8, imm: i32) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        // C7 /0 id: mod=11, reg field=0 (/0), r/m=reg_idx.
        let modrm = 0xC0 | (reg_idx & 0x7);
        let mut out = Vec::with_capacity(7);
        out.push(rex);
        out.push(0xC7);
        out.push(modrm);
        out.extend_from_slice(&imm.to_le_bytes());
        Insn::raw_bytes(&out)
    }

    /// P11: 构造 `movsd xmm_dst, xmm_src` (F2 0F 10 mod=11).
    /// 源/目的都在 xmm0..xmm15 (>=8 时带 REX.R/B 由 ModRM 低 3 位 + 前缀编码).
    pub fn movsd_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        // F2 0F 10 modrm; mod=11, reg=dst, r/m=src.
        Self::simd_xmm_reg_reg(0xF2, 0x10, dst_xmm, src_xmm)
    }

    /// P11: 构造 `movss xmm_dst, xmm_src` (F3 0F 10 mod=11).
    pub fn movss_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Self::simd_xmm_reg_reg(0xF3, 0x10, dst_xmm, src_xmm)
    }

    /// P11: 构造 `movsd xmm_dst, xmm_src` 存储方向 (F2 0F 11 mod=11):
    ///   mod=11, reg=src (源 xmm), r/m=dst (目的 xmm).
    pub fn movsd_xmm_store(dst_xmm: u8, src_xmm: u8) -> Self {
        Self::simd_xmm_reg_reg(0xF2, 0x11, dst_xmm, src_xmm)
    }

    /// P11: 构造 `movss xmm_dst, xmm_src` 存储方向 (F3 0F 11 mod=11).
    pub fn movss_xmm_store(dst_xmm: u8, src_xmm: u8) -> Self {
        Self::simd_xmm_reg_reg(0xF3, 0x11, dst_xmm, src_xmm)
    }

    /// P11 Bucket B/E3: SimdMov [base+disp] store/load.
    /// store=true: movss/movsd/movups [base+disp], xmm (0F 11).
    /// store=false: movss/movsd/movups xmm, [base+disp] (0F 10).
    /// width: SimdWidth::{SS, SD, O/PS, PD, DQ}. base=gpr(4=rsp), disp=i32.
    pub fn simd_mov_mem_base(
        store: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        xmm_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let mem = Operand::Mem(MemOperand {
            disp,
            base: Some(base),
            index: None,
            seg: None,
        });
        let reg = Operand::Reg(xmm_reg, crate::codegen::shared::asm_operand::Width::B128);
        let (dst, src) = if store { (mem, reg) } else { (reg, mem) };
        Insn::new(
            X86_64Opcode::SimdMov {
                width,
                opcode10: store,
                dst,
                src,
            },
            &[],
        )
    }

    /// P11 Bucket B: SimdMov [rsp+disp] store/load. base 固定=4 (rsp).
    pub fn simd_mov_mem(
        store: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        xmm_reg: u8,
        disp: i32,
    ) -> Self {
        Self::simd_mov_mem_base(store, width, xmm_reg, 4, disp)
    }

    /// P11 Bucket G: movapd xmm_dst, xmm_src (66 0F 28 mod=11, load direction).
    pub fn movapd_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                width: crate::codegen::x86_64::opcode::SimdWidth::PD,
                opcode10: true,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket G: movaps xmm_dst, xmm_src (0F 28 mod=11, load direction).
    pub fn movaps_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                width: crate::codegen::x86_64::opcode::SimdWidth::PS,
                opcode10: true,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket C+E: SimdArith reg-reg (addsd/subsd/mulsd/divsd/addss/subss/mulss/divss/xorps/xorpd).
    /// op: SimdArithOp::{Add, Sub, Mul, Div, Xorps, Xorpd}. width: SimdWidth::{SD, SS}.
    pub fn simd_arith_reg(
        op: crate::codegen::x86_64::opcode::SimdArithOp,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        dst_xmm: u8,
        src_xmm: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdArith {
                op,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket G2: psllq/psrlq/psrad xmm, imm8.
    ///   width: SimdWidth::Q=psllq, D=psrlq, W=psrad.
    pub fn simd_shift_imm8(
        width: crate::codegen::x86_64::opcode::SimdWidth,
        dst_xmm: u8,
        imm8: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdShiftImm8 {
                op: crate::codegen::x86_64::opcode::SimdArithOp::PShiftImm8,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_xmm,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                imm8,
            },
            &[],
        )
    }

    /// P11 Bucket E: stmxcsr/ldmxcsr [base+disp] (0F AE /3 or /2).
    pub fn mxcsr_mem(store: bool, base: u8, disp: i32) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op: crate::codegen::x86_64::opcode::X87Op::MxcsrMem { store },
                mem: Some(crate::codegen::x86_64::opcode::MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket E: fnstcw/fldcw [base+disp] (D9 /7 or /5).
    pub fn fcw_mem(store: bool, base: u8, disp: i32) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op: crate::codegen::x86_64::opcode::X87Op::FcwMem { store },
                mem: Some(crate::codegen::x86_64::opcode::MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket H: fixed x87 escap sequence with no memory operand.
    /// Example: faddp=(0xDE,0xC1), fmulp=(0xDE,0xC9), fsubrp=(0xDE,0xE1),
    /// fdivrp=(0xDE,0xF1), fucomip=(0xDF,0xE9), fxch st(1)=(0xD9,0xC9).
    pub fn x87_fixed(escap: u8, modrm: u8) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op: crate::codegen::x86_64::opcode::X87Op::Fixed { escap, modrm },
                mem: None,
            },
            &[],
        )
    }

    /// P11 Bucket W: x87 load/store memory form (fld/fstp/fild [base+disp]).
    /// width: 4=m32fp, 8=m64fp, 10=m80fp. base: intel reg number (4=rsp).
    pub fn x87_mem(
        op: crate::codegen::x86_64::opcode::X87Op,
        base: u8,
        disp: i32,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op,
                mem: Some(crate::codegen::x86_64::opcode::MemOperand {
                    disp,
                    base: Some(base),
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket H: setcc r8 (0F 9x /0, mod=11).
    /// reg_idx: Intel reg number (0=al,1=cl,2=dl,3=bl,4=ah..15=r15b).
    pub fn setcc_reg(
        cc: crate::codegen::x86_64::opcode::CondCode,
        reg_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Setcc {
                cc,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    reg_idx,
                    crate::codegen::shared::asm_operand::Width::B8,
                ),
            },
            &[],
        )
    }

    /// P11 Bucket B: cvtsi2sd/cvtsi2ss (r64->xmm) or cvttsd2si/cvttss2si (xmm->r64).
    /// to_xmm=true: r64->xmm (cvtsi2sd/cvtsi2ss). to_xmm=false: xmm->r64 (cvttsd2si/cvttss2si).
    /// width=SimdWidth::SD uses F2 prefix, width=SimdWidth::SS uses F3 prefix.
    pub fn cvt2sd_reg(
        to_xmm: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        dst_idx: u8,
        src_idx: u8,
    ) -> Self {
        use crate::codegen::shared::asm_operand::Width;
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Cvt2Sd {
                to_xmm,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_idx, Width::B128),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_idx, Width::B128),
            },
            &[],
        )
    }

    /// P11 Bucket D: roundsd/roundss reg-reg with imm8 (rounding mode).
    /// width=SimdWidth::SD → roundsd (66 0F 3A 0B /r ib).
    /// width=SimdWidth::SS → roundss (66 0F 3A 0A /r ib).
    pub fn simd_round_reg(
        width: crate::codegen::x86_64::opcode::SimdWidth,
        imm8: u8,
        dst_idx: u8,
        src_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdRound {
                width,
                imm8,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    dst_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Reg(
                    src_idx,
                    crate::codegen::shared::asm_operand::Width::B128,
                ),
            },
            &[],
        )
    }

    /// Common reg↔reg xmm↔xmm encoder.
    /// opcode2 = 0x10 (mov load direction dst<-src) or 0x11 (store direction dst<-src).
    /// Both dst/src are register fields in ModRM: reg field = dst_xmm, r/m = src_xmm.
    fn simd_xmm_reg_reg(pfx: u8, opcode2: u8, dst_xmm: u8, src_xmm: u8) -> Self {
        use crate::codegen::x86_64::opcode::{Operand, SimdWidth, X86_64Opcode};
        let width = match pfx {
            0xF3 => SimdWidth::SS,
            0xF2 => SimdWidth::SD,
            _ => SimdWidth::PS,
        };
        Insn::new(
            X86_64Opcode::SimdMov {
                width,
                opcode10: opcode2 == 0x10,
                dst: Operand::Reg(dst_xmm, crate::codegen::shared::asm_operand::Width::B128),
                src: Operand::Reg(src_xmm, crate::codegen::shared::asm_operand::Width::B128),
            },
            &[],
        )
    }

    /// P11: cqo / cdq / cwd sign-extend rax→rdx:rax.
    /// width=B64 → 48 99 (cqo); width=B32 → 99 (cdq); width=B16 → 66 99 (cwd).
    pub fn sign_extend_rax_to_rdx(width: crate::codegen::shared::asm_operand::Width) -> Self {
        let mut out = Vec::with_capacity(3);
        match width {
            crate::codegen::shared::asm_operand::Width::B64 => out.push(0x48),
            crate::codegen::shared::asm_operand::Width::B16 => out.push(0x66),
            _ => {},
        }
        out.push(0x99);
        Insn::raw_bytes(&out)
    }

    /// P11: movzx/movsx/movsxd via MovExt typed opcode (reg-reg form).
    /// sign: true=movsx/movsxd, false=movzx.
    /// from: source width (B8/B16/B32). to: dest width (B32/B64).
    pub fn mov_ext(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, to),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, from),
            },
            &[],
        )
    }

    /// P11 Bucket 1: movzx/movsx/movsxd via MovExt typed opcode (mem-load form).
    /// sign: true=movsx/movsxd, false=movzx.
    /// from: source memory width (B8/B16/B32). to: dest width (B32/B64).
    /// base: x86_64 physical register index 0..15. disp: byte displacement.
    pub fn mov_ext_mem(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        base: u8,
        disp: i32,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, to),
                src: crate::codegen::x86_64::opcode::Operand::Mem(
                    crate::codegen::x86_64::opcode::MemOperand {
                        disp,
                        base: Some(base),
                        index: None,
                        seg: None,
                    },
                ),
            },
            &[],
        )
    }

    /// P11 Bucket X: movzx/movsx/movsxd via MovExt typed opcode, mem-load SIB form
    /// `dst, [base+index*1]`. index_scale 仅接受 1 (SIB scale=0); 其余走手写回退.
    pub fn mov_ext_mem_sib(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        base: u8,
        index: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign,
                from,
                to,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, to),
                src: crate::codegen::x86_64::opcode::Operand::Mem(
                    crate::codegen::x86_64::opcode::MemOperand {
                        disp: 0,
                        base: Some(base),
                        index: Some((index, 1)),
                        seg: None,
                    },
                ),
            },
            &[],
        )
    }

    /// P11: NOT / NEG single-register unary via UnaryRm typed opcode.
    /// op: UnaryOp::Not or UnaryOp::Neg. width: B16/B32/B64. reg_idx: x86_64
    /// physical register index 0..15.
    pub fn unary_rm_reg(
        op: crate::codegen::x86_64::opcode::UnaryOp,
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::UnaryRm {
                op,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(reg_idx, width),
            },
            &[],
        )
    }

    /// P11: TEST reg, reg via AluRm typed opcode.
    /// width: B32/B64 (common forms). dst/src: x86_64 physical register indices 0..15.
    pub fn test_rm_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op: crate::codegen::x86_64::opcode::AluOp::Test,
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11: IMUL r64, r/m64, imm32 (REX.W 69 /r id). Reflexive form where
    /// dst==src (both held by `reg_idx`); self-absorbs REX.R+B when reg>=8.
    pub fn imul_rm_reg_imm32_b64(reg_idx: u8, imm: i32) -> Self {
        let rex = 0x48u8 | if reg_idx >= 8 { 0x05 } else { 0 };
        let modrm = 0xC0 | ((reg_idx & 7) << 3) | (reg_idx & 7);
        let mut bytes = Vec::with_capacity(2 + 1 + 4);
        bytes.push(rex);
        bytes.push(0x69);
        bytes.push(modrm);
        bytes.extend_from_slice(&imm.to_le_bytes());
        Insn::raw_bytes(&bytes)
    }

    /// P11: IDIV r/m64 (REX.W F7 /7). Only r/m field holds `reg_idx`; reg
    /// field is the /7 opcode extension. Self-absorbs REX.B when reg>=8
    /// (do NOT set REX.R -- it would shift /7 out of range).
    pub fn idiv_rm_reg_b64(reg_idx: u8) -> Self {
        let rex = 0x48u8 | if reg_idx >= 8 { 0x01 } else { 0 };
        Insn::raw_bytes(&[rex, 0xF7, 0xF8 | (reg_idx & 7)])
    }

    /// P11: IMUL reg, reg (0F AF /r) via ImulRm typed opcode.
    /// width: B32/B64. dst/src: x86_64 physical register indices 0..15.
    pub fn imul_rm_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        src_reg: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::ImulRm {
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_reg, width),
            },
            &[],
        )
    }

    /// P11 Bucket G: unpcklps xmm_dst, xmm_src (0F 14 /r).
    pub fn unpcklps_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::simd_arith_reg(
            crate::codegen::x86_64::opcode::SimdArithOp::Unpcklps,
            crate::codegen::x86_64::opcode::SimdWidth::PS,
            dst_xmm,
            src_xmm,
        )
    }

    pub fn encode_bytes(&self) -> Option<Vec<u8>> {
        // 类型化变体: 让 opcode.rs 现场算字节. 尚未实现的变体返回 None →
        // 发射方必须切回 raw_bytes 兜底; 当前没有任何调用点依赖 None 分支.
        self.op.encode()
    }

    pub fn to_rec(&self, pc: u64) -> InsnRec {
        // 迁移期: opcode_id 统一记 0 (架构中性 InsnRec 只在跨后端通道用,
        // 生产路径尚未切到). 步骤 6 完成后再按变体映射到 u16 空间.
        let mut rec = InsnRec::new(0, &self.ops[..]);
        rec.pc = pc;
        rec.bytes_len = self.bytes.len() as u8;
        rec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_bytes_passthrough() {
        // x86_64 本机小端, 字节原样落盘
        assert_eq!(
            Insn::raw_bytes(&[0x48, 0x8B, 0x05, 0x34, 0x12, 0x00, 0x00]).encode_bytes(),
            Some(vec![0x48, 0x8B, 0x05, 0x34, 0x12, 0x00, 0x00]),
        );
    }

    #[test]
    fn nop_encode() {
        assert_eq!(Insn::nop().encode_bytes(), Some(vec![0x90]));
    }

    #[test]
    fn raw_bytes_empty() {
        assert_eq!(Insn::raw_bytes(&[]).encode_bytes(), Some(Vec::new()));
    }

    /// P11 Bucket T: shift_reg_imm8_c1_b64 强制 C1 形式 (即使 imm=1 也带 imm8 字节).
    #[test]
    fn shift_reg_imm8_c1_b64_basic() {
        // shr rax, 1: 48 C1 E8 01 (reg=0=rax, op=/5, imm8=1)
        assert_eq!(
            Insn::shift_reg_imm8_c1_b64(0b101, 0, 1).encode_bytes(),
            Some(vec![0x48, 0xC1, 0xE8, 0x01]),
        );
        // shr r8, 3: REX.W+B, C1, E8|(8&7=0), 03
        assert_eq!(
            Insn::shift_reg_imm8_c1_b64(0b101, 8, 3).encode_bytes(),
            Some(vec![0x49, 0xC1, 0xE8, 0x03]),
        );
        // shl rbx, 4: op=/4, reg=3=rbx → modrm=C0|(4<<3)|3 = C0|0x20|3 = 0xE3
        assert_eq!(
            Insn::shift_reg_imm8_c1_b64(0b100, 3, 4).encode_bytes(),
            Some(vec![0x48, 0xC1, 0xE3, 0x04]),
        );
    }

    /// P11 Bucket T: test_rm_reg_imm32_b64 = REX.W F7 /0 id.
    #[test]
    fn test_rm_reg_imm32_b64_basic() {
        // test rax, 0x8000 (0x00008000, imm32 LE = 00 80 00 00)
        let bytes = Insn::test_rm_reg_imm32_b64(0, 0x8000).encode_bytes().unwrap();
        assert_eq!(&bytes[..3], &[0x48, 0xF7, 0xC0]);
        assert_eq!(&bytes[3..], &[0x00, 0x80, 0x00, 0x00]);
        // test r8d: REX.W+B, F7, C0|0= C0, imm32
        let bytes = Insn::test_rm_reg_imm32_b64(8, 0x1234).encode_bytes().unwrap();
        assert_eq!(&bytes[..3], &[0x49, 0xF7, 0xC0]);
        assert_eq!(&bytes[3..], &[0x34, 0x12, 0x00, 0x00]);
    }

    /// P11 Bucket T: movabs_reg_imm64 = REX.W B8+rd imm64.
    #[test]
    fn movabs_reg_imm64_basic() {
        // movabs rax, 0x0000000080000000
        let imm: u64 = 0x0000000080000000;
        let bytes = Insn::movabs_reg_imm64(0, imm).encode_bytes().unwrap();
        assert_eq!(bytes.len(), 10);
        assert_eq!(&bytes[..2], &[0x48, 0xB8]);
        assert_eq!(&bytes[2..], &imm.to_le_bytes());
        // movabs r11, imm: REX.W+B, BB (B8|3=BB)
        let bytes = Insn::movabs_reg_imm64(11, 0xDEADBEEF).encode_bytes().unwrap();
        assert_eq!(&bytes[..2], &[0x49, 0xBB]);
    }

    /// P11 Bucket K: alu_rm_reg_imm8_b64 = REX.W 0x83 / ib.
    #[test]
    fn alu_rm_reg_imm8_b64_basic() {
        // add rax, 8: 48 83 C0 08 (op=/0, reg=0=rax)
        assert_eq!(
            Insn::alu_rm_reg_imm8_b64(0, 0, 8).encode_bytes(),
            Some(vec![0x48, 0x83, 0xC0, 0x08]),
        );
        // sub rdx, 16: 48 83 EA 10 (op=/5<<3|2 = 0x28|2 = 0x2A, modrm=C0|0x2A = EA)
        assert_eq!(
            Insn::alu_rm_reg_imm8_b64(0b101, 2, 16).encode_bytes(),
            Some(vec![0x48, 0x83, 0xEA, 0x10]),
        );
        // add r8, -4: REX.W+B, 83, C0|0 = C0, fc
        assert_eq!(
            Insn::alu_rm_reg_imm8_b64(0, 8, -4).encode_bytes(),
            Some(vec![0x49, 0x83, 0xC0, 0xFC]),
        );
    }

    /// P11 Bucket L: imul_rm_reg_imm32_b64 reflexive = REX.W 69 /r id.
    #[test]
    fn imul_rm_reg_imm32_b64_basic() {
        // imul rcx, rcx, 4: 48 69 C9 04 00 00 00 (reg=1=rcx)
        assert_eq!(
            Insn::imul_rm_reg_imm32_b64(1, 4).encode_bytes(),
            Some(vec![0x48, 0x69, 0xC9, 0x04, 0x00, 0x00, 0x00]),
        );
        // imul r8, r8, 1: REX.W|R|B, 69, C0 (reg=8=0, both fields 0)
        assert_eq!(
            Insn::imul_rm_reg_imm32_b64(8, 1).encode_bytes(),
            Some(vec![0x4D, 0x69, 0xC0, 0x01, 0x00, 0x00, 0x00]),
        );
    }

    /// P11 Bucket L: idiv_rm_reg_b64 = REX.W F7 /7.
    #[test]
    fn idiv_rm_reg_b64_basic() {
        // idiv rcx: 48 F7 F9 (reg field = /7 = 111b << 3 = 0x38; r/m = 1)
        assert_eq!(
            Insn::idiv_rm_reg_b64(1).encode_bytes(),
            Some(vec![0x48, 0xF7, 0xF9]),
        );
        // idiv r8: REX.W+B, F7, F8|0 = F8
        assert_eq!(
            Insn::idiv_rm_reg_b64(8).encode_bytes(),
            Some(vec![0x49, 0xF7, 0xF8]),
        );
    }

    /// P11 Bucket W: mov_reg_imm32_b64 = REX.W C7 /0 id.
    #[test]
    fn mov_reg_imm32_b64_basic() {
        // mov rcx, 0x10: 48 C7 C1 10 00 00 00
        assert_eq!(
            Insn::mov_reg_imm32_b64(1, 0x10).encode_bytes(),
            Some(vec![0x48, 0xC7, 0xC1, 0x10, 0x00, 0x00, 0x00]),
        );
        // mov r8, 5: REX.W+B, C7, C0 (mod=11, /0, r/m=0 with REX.B), 05 00 00 00
        assert_eq!(
            Insn::mov_reg_imm32_b64(8, 5).encode_bytes(),
            Some(vec![0x49, 0xC7, 0xC0, 0x05, 0x00, 0x00, 0x00]),
        );
    }

    #[test]
    fn bswap_reg_basic() {
        use crate::codegen::shared::asm_operand::Width::*;
        // bswap ecx: 0F C9
        assert_eq!(Insn::bswap_reg(B32, 1).encode_bytes(),
            Some(vec![0x0F, 0xC9]));
        // bswap rcx: 48 0F C9
        assert_eq!(Insn::bswap_reg(B64, 1).encode_bytes(),
            Some(vec![0x48, 0x0F, 0xC9]));
        // bswap rax: 48 0F C8
        assert_eq!(Insn::bswap_reg(B64, 0).encode_bytes(),
            Some(vec![0x48, 0x0F, 0xC8]));
    }

    #[test]
    fn rol_reg_imm8_b16_basic() {
        // rol cx, 8: 66 C1 C1 08
        assert_eq!(Insn::rol_reg_imm8_b16(1, 8).encode_bytes(),
            Some(vec![0x66, 0xC1, 0xC1, 0x08]));
        // rol ax, 8: 66 C1 C0 08
        assert_eq!(Insn::rol_reg_imm8_b16(0, 8).encode_bytes(),
            Some(vec![0x66, 0xC1, 0xC0, 0x08]));
    }

    #[test]
    fn shift_reg_imm8_c1_b32_basic() {
        // sar ecx, 31: C1 F9 1F
        assert_eq!(Insn::shift_reg_imm8_c1_b32(0b111, 1, 0x1F).encode_bytes(),
            Some(vec![0xC1, 0xF9, 0x1F]));
    }
}
