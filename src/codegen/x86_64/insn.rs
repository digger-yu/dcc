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

/// P12-A: explicit expect on `emit_insn() -> bool`.
///
/// `emit_insn` returns `false` when `encode()` returns `None` (an uncovered
/// opcode shape). Callers must not silently drop that signal.  Typed
/// constructors whose declared shapes all encode successfully should call
/// `.expect("...")` to panic loudly at the first uncovered shape.
pub trait BoolExpect {
    #[track_caller]
    fn expect(self, msg: &str);
}

impl BoolExpect for bool {
    #[track_caller]
    fn expect(self, msg: &str) {
        if !self {
            panic!("emit_insn encode returned None: {}", msg);
        }
    }
}

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

    /// P11 Bucket AK: 段覆盖前缀 (FS=0x64 / GS=0x65). 单字节, 仅作用于下一条
    /// 指令. 取代散落的 `Insn::raw_bytes(&[seg])` 调用点.
    pub fn seg_prefix(seg: u8) -> Self {
        Insn::raw_bytes(&[seg])
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
            X86_64Opcode::GprXmm {
                to_xmm,
                width,
                dst,
                src,
            },
            &[],
        )
    }

    /// P11 Bucket N6: BSF/BSR reg, reg (0F BC/BD).
    /// is_bsr=true → BSR (0F BD); false → BSF (0F BC).
    /// width=B64 发 REX.W; B32 不发. dst ModRM.reg, src ModRM.r/m.
    pub fn bit_scan_reg_reg(
        is_bsr: bool,
        width: crate::codegen::shared::asm_operand::Width,
        dst_idx: u8,
        src_idx: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::Operand;
        Insn::new(
            X86_64Opcode::BitScan {
                is_bsr,
                width,
                dst: Operand::Reg(dst_idx, width),
                src: Operand::Reg(src_idx, width),
            },
            &[],
        )
    }

    /// P11 Bucket N6: CMOVcc reg, reg (B64/B32).
    pub fn cmovcc_reg_reg(
        cc: crate::codegen::x86_64::opcode::CondCode,
        width: crate::codegen::shared::asm_operand::Width,
        dst_idx: u8,
        src_idx: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::Operand;
        Insn::new(
            X86_64Opcode::Cmovcc {
                cc,
                width,
                dst: Operand::Reg(dst_idx, width),
                src: Operand::Reg(src_idx, width),
            },
            &[],
        )
    }

    /// P11 Bucket O3: LOCK CMPXCHG [base], reg — 手写形态 (base-only, disp=0).
    pub fn lock_cmpxchg_base(
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        base: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::MemOperand;
        Insn::new(
            X86_64Opcode::LockCmpxchgRm {
                width,
                reg_idx,
                mem: MemOperand {
                    disp: 0,
                    base: Some(base),
                    index: None,
                    seg: None,
                },
            },
            &[],
        )
    }

    /// P11 Bucket O3: LOCK XCHG [base], reg — 手写形态 (base-only, disp=0).
    pub fn lock_xchg_base(
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        base: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::MemOperand;
        Insn::new(
            X86_64Opcode::LockXchgRm {
                width,
                reg_idx,
                mem: MemOperand {
                    disp: 0,
                    base: Some(base),
                    index: None,
                    seg: None,
                },
            },
            &[],
        )
    }

    /// P11 Bucket N8: LOCK XADD [base], reg — 手写形态 (base-only, disp=0).
    pub fn lock_xadd_base(
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        base: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::MemOperand;
        Insn::new(
            X86_64Opcode::LockXaddRm {
                width,
                reg_idx,
                mem: MemOperand {
                    disp: 0,
                    base: Some(base),
                    index: None,
                    seg: None,
                },
            },
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

    /// P11 Bucket Z: add/sub rsp, imm8 with attached GAS text.
    pub fn add_rsp_imm_gas(imm: i64, gas: &str) -> Self {
        let v = imm as i8 as u8;
        let mut insn = Insn::raw_bytes(&[0x48, 0x83, 0xC4, v]);
        insn.gas_text = Some(gas.to_string());
        insn
    }

    pub fn sub_rsp_imm_gas(imm: i64, gas: &str) -> Self {
        let v = imm as i8 as u8;
        let mut insn = Insn::raw_bytes(&[0x48, 0x83, 0xEC, v]);
        insn.gas_text = Some(gas.to_string());
        insn
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
                rip_relative: false,
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
        Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: false,
                width,
                dst,
                src,
            },
            &[],
        )
    }

    /// P11 Bucket AE: MovRm reg <-> mem [base+index*scale] (load or store, disp=0).
    /// scale ∈ {1,2,4,8}. base: Intel reg number 0..15 (含 r11=11 等).
    pub fn mov_rm_mem_sib(
        load: bool,
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        base: u8,
        index: u8,
        scale: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let reg = Operand::Reg(reg_idx, width);
        let mem = Operand::Mem(MemOperand {
            disp: 0,
            base: Some(base),
            index: Some((index, scale)),
            seg: None,
        });
        let (dst, src) = if load { (reg, mem) } else { (mem, reg) };
        Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: false,
                width,
                dst,
                src,
            },
            &[],
        )
    }

    /// P11 Bucket O8: MovRm reg <-> [rip+disp32] (RIP-relative load/store).
    /// load=true: mov reg, [rip+disp32]; load=false: mov [rip+disp32], reg.
    /// width: B8/B16/B32/B64. disp: 32-bit displacement (usually 0, patched later).
    pub fn mov_rm_rip(
        load: bool,
        width: crate::codegen::shared::asm_operand::Width,
        reg_idx: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let reg = Operand::Reg(reg_idx, width);
        let mem = Operand::Mem(MemOperand {
            disp,
            base: None,
            index: None,
            seg: None,
        });
        let (dst, src) = if load { (reg, mem) } else { (mem, reg) };
        Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: true,
                width,
                dst,
                src,
            },
            &[],
        )
    }

    /// P11 Bucket O8: MovExt reg <- [rip+disp32] (RIP-relative movzx/movsx/movsxd).
    /// sign: true=movsx/movsxd, false=movzx.
    /// from: source memory width (B8/B16/B32). to: dest width (B32/B64).
    pub fn mov_ext_rip(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        disp: i32,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        Insn::new(
            X86_64Opcode::MovExt {
                rip_relative: true,
                sign,
                from,
                to,
                dst: Operand::Reg(dst_reg, to),
                src: Operand::Mem(MemOperand {
                    disp,
                    base: None,
                    index: None,
                    seg: None,
                }),
            },
            &[],
        )
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
    pub fn stack_op_reg(op: crate::codegen::x86_64::opcode::StackOp, reg_idx: u8) -> Self {
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
    pub fn lea_reg_base_disp(dst_reg: u8, base: u8, disp: i32) -> Self {
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

    /// P11 Bucket AK: mov eax, imm32 (B8 id, no ModRM). 用于 ifunc CPUID 分类链.
    pub fn mov_eax_imm32(v: u32) -> Self {
        let imm32 = v.to_le_bytes();
        let mut out = Vec::with_capacity(5);
        out.push(0xB8);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket AK: mov ecx, imm32 (B9 id, no ModRM). 用于 ifunc CPUID 分类链.
    pub fn mov_ecx_imm32(v: u32) -> Self {
        let imm32 = v.to_le_bytes();
        let mut out = Vec::with_capacity(5);
        out.push(0xB9);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket AK: and eax, imm32 (25 id, no ModRM). 固定 32 位立即数形式,
    /// 用于 XCR0 掩码 (如 0xE6) 超出 sign-extended imm8 范围的场景.
    pub fn and_eax_imm32(v: u32) -> Self {
        let imm32 = v.to_le_bytes();
        let mut out = Vec::with_capacity(5);
        out.push(0x25);
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

    /// P11 Bucket O2: `mov dword [base+disp], imm32` (C7 /0 id).
    /// 手写约定 (va_arg_store / builtins): REX.B 置位当 base>=8;
    /// mod=01 disp8 if fits, 否则 mod=10 disp32. 无 REX.W (imm32 形式).
    pub fn store_imm32_mem(base: u8, disp: i32, imm: i32) -> Self {
        let mut rex: u8 = 0x40;
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
        // /0 = C7 mov m32, imm32.
        let modrm = (modrm_mod << 6) | 0x00 | rm_field;
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(11);
        out.push(rex);
        out.push(0xC7);
        out.push(modrm);
        out.extend_from_slice(&disp_bytes[..disp_len]);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket N7: `mov [base+disp], imm` (C6 /0 ib / C7 /0 id).
    /// width ∈ B8/B16/B32/B64. base: 0..15. disp: 任意 i32. imm: 立即数 (按宽度截断).
    /// 手写约定 (builtins.rs / atomic clear / longjmp / fenv default env):
    ///   B8  → C6 /0 ib (imm8)
    ///   B16 → 66 C7 /0 id (imm16 经 imm32 槽)
    ///   B32 → C7 /0 id (imm32)
    ///   B64 → REX.W C7 /0 id (imm32 符号扩展)
    /// 当 base=4 (rsp) 时自动发 SIB; base=5 (rbp) 且 disp=0 时强制 disp8 (mod=01).
    /// 仅 base>=8 时发 REX.B (B64 时 OR REX.W=0x48). 保持逐字节等价.
    pub fn store_imm_mem(
        width: crate::codegen::shared::asm_operand::Width,
        base: u8,
        disp: i32,
        imm: i64,
    ) -> Self {
        use crate::codegen::shared::asm_operand::Width::*;
        let is_b8 = matches!(width, B8);
        let is_b16 = matches!(width, B16);
        let is_b32 = matches!(width, B32);
        let is_b64 = matches!(width, B64);
        let opcode: u8 = if is_b8 { 0xC6 } else { 0xC7 };
        // REX: W=B64, B=base>=8. 对于 B8 spl/bpl/sil/dil 高字节不需要 REX
        // (因为 dst 是 memory, 不是 register high-byte).
        let mut rex: u8 = 0x40;
        if is_b64 {
            rex |= 0x08;
        }
        let need_rex_b = base >= 8;
        if need_rex_b {
            rex |= 0x01;
        }
        let need_rex = is_b64 || need_rex_b;
        let rm_field = base & 7;
        // SIB: base 物理号 = 4 (rsp) 时必须发 SIB; 其余 base 走普通 ModRM.
        let need_sib = rm_field == 4;
        // mod=00/01/10 选择: base=rbp(5) 且无 SIB 时 mod=00 = rip-relative,
        // 必须走 disp. 其余 disp=0 可走 mod=00.
        let modrm_mod: u8;
        let disp_bytes: [u8; 4];
        let disp_len: usize;
        if disp == 0 && rm_field != 5 || need_sib {
            // mod=00, 无 disp. base=rbp(5) 有 SIB 时 mod=00 仍合法.
            modrm_mod = 0;
            disp_len = 0;
            disp_bytes = [0; 4];
        } else if let Ok(d) = i8::try_from(disp) {
            modrm_mod = 1;
            disp_len = 1;
            disp_bytes = [d as u8, 0, 0, 0];
        } else {
            modrm_mod = 2;
            disp_len = 4;
            disp_bytes = disp.to_le_bytes();
        }
        let modrm_rm = if need_sib { 4 } else { rm_field };
        // /0 = mov m8/m16/m32/m64, imm. reg field = 0.
        let modrm = (modrm_mod << 6) | 0x00 | modrm_rm;
        let mut out = Vec::with_capacity(16);
        if is_b16 {
            out.push(0x66);
        }
        if need_rex {
            out.push(rex);
        }
        out.push(opcode);
        out.push(modrm);
        if need_sib {
            // SIB: scale=0 (scale=1), index=4 (ESP=无 index), base=rm_field.
            let sib = (0u8 << 6) | (4u8 << 3) | rm_field;
            out.push(sib);
        }
        out.extend_from_slice(&disp_bytes[..disp_len]);
        if is_b8 {
            out.push((imm & 0xFF) as u8);
        } else if is_b16 {
            // imm16 经 imm32 槽 (C7 固定 id 形式, 66 前缀).
            out.extend_from_slice(&(imm as i16).to_le_bytes());
        } else if is_b32 || is_b64 {
            out.extend_from_slice(&(imm as i32).to_le_bytes());
        }
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

    /// P11 Bucket N9: B16 `shl/shr/sar reg, imm8` (66 C1 /r ib, no REX).
    /// op 编码: /4=shl, /5=shr, /7=sar.
    pub fn shift_reg_imm8_c1_b16(op_field: u8, reg_idx: u8, imm8: u8) -> Self {
        let modrm = 0xC0 | ((op_field & 0x7) << 3) | (reg_idx & 0x7);
        Insn::raw_bytes(&[0x66, 0xC1, modrm, imm8])
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

    /// P11 Bucket AI: short conditional jump (jcc rel8, 2 bytes).
    /// cc_byte = 0x70..0x7F (jo/jno/jb/jae/je/jne/jbe/ja/js/jns/jp/jnp/jl/jge/jle/jg).
    /// rel8: signed byte displacement (already accounts for next-instruction PC).
    /// Used by fixed-offset loops/skips (mem/access.rs, builtins.rs, lvalue.rs,
    /// func_setup.rs canary). The displacement is a compile-time constant.
    pub fn short_jump_rel8(cc_byte: u8, rel8: i8) -> Self {
        Insn::raw_bytes(&[cc_byte, rel8 as u8])
    }

    /// P11 Bucket P11R: direct call rel32 (E8 00 00 00 00).
    /// Emits 5 bytes; rel32 is patched later via pending_func_rels.
    /// Used by trapv/ubsan call sites that emit a call to a known symbol.
    pub fn call_rel32() -> Self {
        Insn::new(X86_64Opcode::CallRel32, &[])
    }

    /// P11 Bucket P11R: direct jmp rel32 (E9 00 00 00 00).
    pub fn jmp_rel32() -> Self {
        Insn::new(X86_64Opcode::JmpRel32, &[])
    }

    /// P11 Bucket P11R: jmp r/m64 reg form (FF /4).
    /// jmp rax = FF E0, jmp r8 = 41 FF E0.
    pub fn jmp_rm_reg(reg_idx: u8) -> Self {
        Insn::new(
            X86_64Opcode::Jmp {
                target: crate::codegen::x86_64::opcode::Operand::Reg(
                    reg_idx,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                pending_rel: false,
            },
            &[],
        )
    }

    /// P11 Bucket P11R: jmp r/m64 mem form (FF /4 [base+disp]).
    /// jmp [rdi] = FF 27. modrm_mod auto-selected by disp size.
    pub fn jmp_rm_mem(base: u8, disp: i32) -> Self {
        Insn::new(X86_64Opcode::JmpRmMem { base, disp }, &[])
    }

    /// P11 Bucket P11R: ud2 (0F 0B).
    pub fn ud2() -> Self {
        Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Ud2)
    }

    /// P11 Bucket P11R: LEA reg64, [rip+disp32] (mod=00 r/m=101).
    /// disp32 placeholder 0; patched by pending_data_rels/label_addr_refs.
    pub fn lea_rip(dst_reg: u8) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand};
        Insn::new(
            X86_64Opcode::Lea {
                dst: Operand::Reg(dst_reg, crate::codegen::shared::asm_operand::Width::B64),
                mem: MemOperand {
                    disp: 0,
                    base: None,
                    index: None,
                    seg: None,
                },
                rip_relative: true,
            },
            &[],
        )
    }

    /// P11 Bucket P11R: LEA reg64, [base + index*scale=1] (SIB form, disp=0).
    /// Example: lea r11, [rbx+r10] = 4F 8D 1C D3.
    pub fn lea_base_index(dst: u8, base: u8, index: u8) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand};
        Insn::new(
            X86_64Opcode::Lea {
                dst: Operand::Reg(dst, crate::codegen::shared::asm_operand::Width::B64),
                mem: MemOperand {
                    disp: 0,
                    base: Some(base),
                    index: Some((index, 1)),
                    seg: None,
                },
                rip_relative: false,
            },
            &[],
        )
    }

    /// P11 Bucket P11R: push m64 [rsp+disp8] (FF /6, SIB form).
    /// push [rsp+disp] = FF 74 24 disp.
    pub fn push_mem_disp8(base: u8, disp: i8) -> Self {
        Insn::new(X86_64Opcode::PushMemDisp8 { base, disp }, &[])
    }

    /// P11 Bucket P11R: movss/movsd xmm, [rip+disp32] load/store.
    /// store=true: movss/movsd [rip+disp32], xmm (0F 11).
    /// store=false: movss/movsd xmm, [rip+disp32] (0F 10).
    /// width: SimdWidth::SS (F3 pfx) or SD (F2 pfx).
    pub fn simd_mov_rip(
        store: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        xmm: u8,
    ) -> Self {
        Insn::new(X86_64Opcode::SimdMovRip { store, width, xmm }, &[])
    }

    /// P11 Bucket AI: popcnt r64, r64 reflexive (F3 [REX.W] 0F B8 /r).
    /// dst==src (both held by `reg_idx`); reflexive ModRM. Self-absorbs REX.R+B
    /// when reg>=8 (0x4D when reg>=8, 0x48 otherwise).
    pub fn popcnt_reg_reg_b64(reg_idx: u8) -> Self {
        let rex: u8 = 0x48 | if reg_idx >= 8 { 0x05 } else { 0 };
        let modrm = 0xC0 | ((reg_idx & 7) << 3) | (reg_idx & 7);
        Insn::raw_bytes(&[0xF3, rex, 0x0F, 0xB8, modrm])
    }

    /// P11 Bucket AI: movmskps r32, xmm (0F 50 /r).
    /// dst_idx: Intel GPR register number (0=eax..15=r15d). src_idx: xmm register.
    pub fn movmskps_gpr_xmm(dst_idx: u8, src_idx: u8) -> Self {
        let mut rex: u8 = 0x40;
        if dst_idx >= 8 {
            rex |= 0x01;
        }
        if src_idx >= 8 {
            rex |= 0x04;
        }
        let need_rex = dst_idx >= 8 || src_idx >= 8;
        let modrm = 0xC0 | ((dst_idx & 7) << 3) | (src_idx & 7);
        let mut out = Vec::with_capacity(4);
        if need_rex {
            out.push(rex);
        }
        out.push(0x0F);
        out.push(0x50);
        out.push(modrm);
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

    /// P11 Bucket Z: B64 `add/and/or/sub/xor/cmp rax, imm32` accumulator-only
    /// form (REX.W 0x05 / 0x25 / 0x0D / 0x2D / 0x35 / 0x3D, no ModRM).
    /// op_encoding: 0x05=add, 0x0D=or, 0x11=adc (unused), 0x19=sbb,
    /// 0x21=and, 0x29=sub, 0x31=xor, 0x39=cmp (rm-reg) etc. For imm32-only
    /// use the accumulator opcodes: 0x05 add / 0x0D or / 0x13 adc /
    /// 0x1B sbb / 0x25 and / 0x2D sub / 0x35 xor / 0x3D cmp.
    pub fn alu_acc_imm32_b64_acc(opcode_no_modrm: u8, imm: i32) -> Self {
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(6);
        out.push(0x48);
        out.push(opcode_no_modrm);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket N9: B32 `add/and/or/sub/xor/cmp eax, imm32` accumulator-only
    /// form (0x05 / 0x25 / 0x0D / 0x2D / 0x35 / 0x3D, no REX, no ModRM).
    /// op_encoding same as `alu_acc_imm32_b64_acc` but without REX.W.
    pub fn alu_acc_imm32_b32_acc(opcode_no_modrm: u8, imm: i32) -> Self {
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(5);
        out.push(opcode_no_modrm);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket N9: B16 `add/and/or/sub/xor/cmp ax, imm16` accumulator-only
    /// form (0x66 0x05 / 0x25 / 0x0D / 0x2D / 0x35 / 0x3D, no REX, no ModRM).
    /// imm16 LE. op_encoding same as `alu_acc_imm32_b32_acc` but with 0x66 prefix.
    pub fn alu_acc_imm16_b16_acc(opcode_no_modrm: u8, imm: i16) -> Self {
        let imm16 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(4);
        out.push(0x66);
        out.push(opcode_no_modrm);
        out.extend_from_slice(&imm16);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket Z: accumulator ALU with gas text (preserves -S rendering).
    /// Constructs the same bytes as [`Self::alu_acc_imm32_b64_acc`] but
    /// attaches the provided GAS text so the -S renderer emits it verbatim.
    pub fn alu_acc_imm32_b64_acc_gas(opcode_no_modrm: u8, imm: i32, gas: &str) -> Self {
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(6);
        out.push(0x48);
        out.push(opcode_no_modrm);
        out.extend_from_slice(&imm32);
        let mut insn = Insn::raw_bytes(&out);
        insn.gas_text = Some(gas.to_string());
        insn
    }

    /// P11 Bucket Z: B64 `cmovcc r_dst, r_src` (REX.W 0F 40+cc mod=11).
    /// cc: condition code byte offset, e.g. 0x47 = cmova. Self-absorbs
    /// REX.R (dst>=8) / REX.B (src>=8).
    pub fn cmovcc_reg_reg_b64(cc: u8, dst_reg: u8, src_reg: u8) -> Self {
        let mut rex: u8 = 0x48;
        if dst_reg >= 8 {
            rex |= 0x04;
        }
        if src_reg >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | ((dst_reg & 7) << 3) | (src_reg & 7);
        Insn::raw_bytes(&[rex, 0x0F, cc, modrm])
    }

    /// P11 Bucket Z: encode movzx/movsx prefix (REX.B, optional REX.R if dst>=8)
    /// plus 0F B6/B7/BE/BF, without the ModRM byte — used by upvar reload fallbacks
    /// that emit ModRM in a separate emit_insn call. REX.B=0x01 (src/base>=8).
    pub fn mov_ext_prefix_no_modrm(rex_b: u8, opcode: u8) -> Self {
        Insn::raw_bytes(&[rex_b, 0x0F, opcode])
    }

    /// P11 Bucket Z: alu reg64, imm32 (REX.W 81 /0 add /5 sub) — imm32 LE.
    /// op_ext: 0=add (/0), 5=sub (/5). REX.W=0x48, REX.B=0x01 if reg>=8.
    pub fn alu_rm_reg_imm32_b64_full(op_ext: u8, reg_idx: u8, imm: i32) -> Self {
        let mut rex: u8 = 0x48;
        if reg_idx >= 8 {
            rex |= 0x01;
        }
        let modrm = 0xC0 | (op_ext << 3) | (reg_idx & 7);
        let imm32 = imm.to_le_bytes();
        let mut out = Vec::with_capacity(7);
        out.push(rex);
        out.push(0x81);
        out.push(modrm);
        out.extend_from_slice(&imm32);
        Insn::raw_bytes(&out)
    }

    /// P11 Bucket Z: alu reg64, imm32 with attached gas text.
    pub fn alu_rm_reg_imm32_b64_full_gas(op_ext: u8, reg_idx: u8, imm: i32, gas: &str) -> Self {
        let mut insn = Self::alu_rm_reg_imm32_b64_full(op_ext, reg_idx, imm);
        insn.gas_text = Some(gas.to_string());
        insn
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

    /// P11 Bucket AE: SimdMov [base + index*scale] load/store (SIB form).
    pub fn simd_mov_mem_base_sib(
        store: bool,
        width: crate::codegen::x86_64::opcode::SimdWidth,
        xmm_reg: u8,
        base: u8,
        index: u8,
        scale: u8,
    ) -> Self {
        use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
        let mem = Operand::Mem(MemOperand {
            disp: 0,
            base: Some(base),
            index: Some((index, scale)),
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
    pub fn x87_mem(op: crate::codegen::x86_64::opcode::X87Op, base: u8, disp: i32) -> Self {
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

    /// P11 Bucket AE: x87 mem [base + index*scale] SIB form. disp=0.
    pub fn x87_mem_sib(
        op: crate::codegen::x86_64::opcode::X87Op,
        base: u8,
        index: u8,
        scale: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op,
                mem: Some(crate::codegen::x86_64::opcode::MemOperand {
                    disp: 0,
                    base: Some(base),
                    index: Some((index, scale)),
                    seg: None,
                }),
            },
            &[],
        )
    }

    /// P11 Bucket N8: x87 fnstenv/fldenv [base+disp].
    ///   store=true  → fnstenv [mem] (D9 /7).
    ///   store=false → fldenv  [mem] (D9 /4).
    pub fn x87_env_mem(store: bool, base: u8, disp: i32) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::X87 {
                op: crate::codegen::x86_64::opcode::X87Op::EnvMem { store },
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
    pub fn setcc_reg(cc: crate::codegen::x86_64::opcode::CondCode, reg_idx: u8) -> Self {
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
                gpr_64: true,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_idx, Width::B128),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_idx, Width::B128),
            },
            &[],
        )
    }

    /// P11 Bucket AL: B32 variant of cvt2sd (no REX.W, e.g. cvttss2si r32, xmm0).
    pub fn cvt2sd_reg_b32(
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
                gpr_64: false,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_idx, Width::B128),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_idx, Width::B128),
            },
            &[],
        )
    }

    /// P11 Bucket AH: xchg reg, reg (B8/B16/B32/B64).
    /// prefer_shortcut=true → dst=rax 走 accumulator-shortcut 0x90+src;
    /// prefer_shortcut=false → 强制 ModRM form (与 int_ops.rs 手写 48 87 C1 对齐).
    pub fn xchg_reg(
        width: crate::codegen::shared::asm_operand::Width,
        dst_idx: u8,
        src_idx: u8,
        prefer_shortcut: bool,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::XchgRm {
                width,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_idx, width),
                src: crate::codegen::x86_64::opcode::Operand::Reg(src_idx, width),
                prefer_shortcut,
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
                rip_relative: false,
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
                rip_relative: false,
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
    /// `dst, [base+index*scale]`. scale ∈ {1,2,4,8}. 其余走手写回退.
    pub fn mov_ext_mem_sib(
        sign: bool,
        from: crate::codegen::shared::asm_operand::Width,
        to: crate::codegen::shared::asm_operand::Width,
        dst_reg: u8,
        base: u8,
        index: u8,
        scale: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                rip_relative: false,
                sign,
                from,
                to,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(dst_reg, to),
                src: crate::codegen::x86_64::opcode::Operand::Mem(
                    crate::codegen::x86_64::opcode::MemOperand {
                        disp: 0,
                        base: Some(base),
                        index: Some((index, scale)),
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

    /// P11: DIV r/m64 (REX.W F7 /6). Same encoding shape as IDIV but /6.
    pub fn div_rm_reg_b64(reg_idx: u8) -> Self {
        let rex = 0x48u8 | if reg_idx >= 8 { 0x01 } else { 0 };
        Insn::raw_bytes(&[rex, 0xF7, 0xF0 | (reg_idx & 7)])
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

    /// P11 Bucket AF: cmpss/cmpsd xmm_dst, xmm_src, imm8 predicate.
    /// width=SimdWidth::SS → cmpss (F3 prefix); SimdWidth::SD → cmpsd (F2 prefix).
    pub fn simd_cmp_imm8(
        width: crate::codegen::x86_64::opcode::SimdWidth,
        imm8: u8,
        dst_xmm: u8,
        src_xmm: u8,
    ) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdCmpImm8 {
                width,
                imm8,
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

    /// P11 Bucket AF: pmulld xmm_dst, xmm_src (66 0F 38 40 /r).
    pub fn pmulld_xmm(dst_xmm: u8, src_xmm: u8) -> Self {
        Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Pmulld {
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
        let bytes = Insn::test_rm_reg_imm32_b64(0, 0x8000)
            .encode_bytes()
            .unwrap();
        assert_eq!(&bytes[..3], &[0x48, 0xF7, 0xC0]);
        assert_eq!(&bytes[3..], &[0x00, 0x80, 0x00, 0x00]);
        // test r8d: REX.W+B, F7, C0|0= C0, imm32
        let bytes = Insn::test_rm_reg_imm32_b64(8, 0x1234)
            .encode_bytes()
            .unwrap();
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
        let bytes = Insn::movabs_reg_imm64(11, 0xDEADBEEF)
            .encode_bytes()
            .unwrap();
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
        assert_eq!(Insn::idiv_rm_reg_b64(1).encode_bytes(), Some(vec![0x48, 0xF7, 0xF9]),);
        // idiv r8: REX.W+B, F7, F8|0 = F8
        assert_eq!(Insn::idiv_rm_reg_b64(8).encode_bytes(), Some(vec![0x49, 0xF7, 0xF8]),);
    }

    /// P11 Bucket AN: div_rm_reg_b64 = REX.W F7 /6.
    #[test]
    fn div_rm_reg_b64_basic() {
        // div rcx: 48 F7 F1 (reg field = /6 = 110b << 3 = 0x30; r/m = 1)
        assert_eq!(Insn::div_rm_reg_b64(1).encode_bytes(), Some(vec![0x48, 0xF7, 0xF1]),);
        // div r8: REX.W+B, F7, F0|0 = F0
        assert_eq!(Insn::div_rm_reg_b64(8).encode_bytes(), Some(vec![0x49, 0xF7, 0xF0]),);
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
        assert_eq!(Insn::bswap_reg(B32, 1).encode_bytes(), Some(vec![0x0F, 0xC9]));
        // bswap rcx: 48 0F C9
        assert_eq!(Insn::bswap_reg(B64, 1).encode_bytes(), Some(vec![0x48, 0x0F, 0xC9]));
        // bswap rax: 48 0F C8
        assert_eq!(Insn::bswap_reg(B64, 0).encode_bytes(), Some(vec![0x48, 0x0F, 0xC8]));
    }

    #[test]
    fn rol_reg_imm8_b16_basic() {
        // rol cx, 8: 66 C1 C1 08
        assert_eq!(Insn::rol_reg_imm8_b16(1, 8).encode_bytes(), Some(vec![0x66, 0xC1, 0xC1, 0x08]));
        // rol ax, 8: 66 C1 C0 08
        assert_eq!(Insn::rol_reg_imm8_b16(0, 8).encode_bytes(), Some(vec![0x66, 0xC1, 0xC0, 0x08]));
    }

    #[test]
    fn shift_reg_imm8_c1_b32_basic() {
        // sar ecx, 31: C1 F9 1F
        assert_eq!(
            Insn::shift_reg_imm8_c1_b32(0b111, 1, 0x1F).encode_bytes(),
            Some(vec![0xC1, 0xF9, 0x1F])
        );
    }

    #[test]
    fn shift_reg_imm8_c1_b16_basic() {
        // shl ax, 3: 66 C1 E0 03
        assert_eq!(
            Insn::shift_reg_imm8_c1_b16(0b100, 0, 3).encode_bytes(),
            Some(vec![0x66, 0xC1, 0xE0, 0x03])
        );
    }

    #[test]
    fn alu_acc_imm32_b64_acc_basic() {
        // add rax, 0x1234: 48 05 34 12 00 00
        assert_eq!(
            Insn::alu_acc_imm32_b64_acc(0x05, 0x1234).encode_bytes(),
            Some(vec![0x48, 0x05, 0x34, 0x12, 0x00, 0x00]),
        );
        // and rax, 0xFF: 48 25 FF 00 00 00
        assert_eq!(
            Insn::alu_acc_imm32_b64_acc(0x25, 0xFF).encode_bytes(),
            Some(vec![0x48, 0x25, 0xFF, 0x00, 0x00, 0x00]),
        );
    }

    #[test]
    fn alu_acc_imm32_b32_acc_basic() {
        // and eax, 0x3F: 25 3F 00 00 00
        assert_eq!(
            Insn::alu_acc_imm32_b32_acc(0x25, 0x3F).encode_bytes(),
            Some(vec![0x25, 0x3F, 0x00, 0x00, 0x00]),
        );
        // or eax, 0x1F80: 0D 80 1F 00 00
        assert_eq!(
            Insn::alu_acc_imm32_b32_acc(0x0D, 0x1F80).encode_bytes(),
            Some(vec![0x0D, 0x80, 0x1F, 0x00, 0x00]),
        );
    }

    #[test]
    fn alu_acc_imm16_b16_acc_basic() {
        // and ax, 0x3F: 66 25 3F 00
        assert_eq!(
            Insn::alu_acc_imm16_b16_acc(0x25, 0x3F).encode_bytes(),
            Some(vec![0x66, 0x25, 0x3F, 0x00]),
        );
    }

    #[test]
    fn cmovcc_reg_reg_b64_basic() {
        // cmova rax, rcx: 48 0F 47 C1 (dst=0, src=1)
        assert_eq!(
            Insn::cmovcc_reg_reg_b64(0x47, 0, 1).encode_bytes(),
            Some(vec![0x48, 0x0F, 0x47, 0xC1]),
        );
        // cmova r8, r9: REX.W|R+B = 4D, 0F 47 C1 (dst=8→0 field, src=9→1 field)
        assert_eq!(
            Insn::cmovcc_reg_reg_b64(0x47, 8, 9).encode_bytes(),
            Some(vec![0x4D, 0x0F, 0x47, 0xC1]),
        );
    }

    #[test]
    fn mov_ext_prefix_no_modrm_basic() {
        // movzbl (upvar reload fallback): REX.B=0x41, 0F B6
        assert_eq!(
            Insn::mov_ext_prefix_no_modrm(0x41, 0xB6).encode_bytes(),
            Some(vec![0x41, 0x0F, 0xB6]),
        );
    }

    #[test]
    fn alu_rm_reg_imm32_b64_full_basic() {
        // add rax, 1 (op_ext=0): 48 81 C0 01 00 00 00
        assert_eq!(
            Insn::alu_rm_reg_imm32_b64_full(0, 0, 1).encode_bytes(),
            Some(vec![0x48, 0x81, 0xC0, 0x01, 0x00, 0x00, 0x00]),
        );
        // add rcx, 1: 48 81 C1 01 00 00 00
        assert_eq!(
            Insn::alu_rm_reg_imm32_b64_full(0, 1, 1).encode_bytes(),
            Some(vec![0x48, 0x81, 0xC1, 0x01, 0x00, 0x00, 0x00]),
        );
        // sub rdx, 1 (op_ext=5): 48 81 EA 01 00 00 00
        assert_eq!(
            Insn::alu_rm_reg_imm32_b64_full(5, 2, 1).encode_bytes(),
            Some(vec![0x48, 0x81, 0xEA, 0x01, 0x00, 0x00, 0x00]),
        );
        // add r8, 1: REX.W+B=49, 81 C0 (r/m=0 field, reg=0 field), 01 00 00 00
        assert_eq!(
            Insn::alu_rm_reg_imm32_b64_full(0, 8, 1).encode_bytes(),
            Some(vec![0x49, 0x81, 0xC0, 0x01, 0x00, 0x00, 0x00]),
        );
    }

    /// P11 Bucket N6: bit_scan_reg_reg (BSF/BSR B64/B32 reflexive + cross).
    #[test]
    fn bit_scan_reg_reg_basic() {
        use crate::codegen::shared::asm_operand::Width::*;
        // bsr rax, rax: 48 0F BD C0
        assert_eq!(
            Insn::bit_scan_reg_reg(true, B64, 0, 0).encode_bytes(),
            Some(vec![0x48, 0x0F, 0xBD, 0xC0]),
        );
        // bsf r8, r8: reflexive r8 → REX.W|R+B = 4D, 0F BC C0
        assert_eq!(
            Insn::bit_scan_reg_reg(false, B64, 8, 8).encode_bytes(),
            Some(vec![0x4D, 0x0F, 0xBC, 0xC0]),
        );
        // bsr eax, eax: 0F BD C0 (no REX.W)
        assert_eq!(
            Insn::bit_scan_reg_reg(true, B32, 0, 0).encode_bytes(),
            Some(vec![0x0F, 0xBD, 0xC0]),
        );
        // bsf ecx, eax: 0F BC C8 (dst=1 in reg field, src=0 in r/m; modrm=C0|(1<<3)|0=C8)
        assert_eq!(
            Insn::bit_scan_reg_reg(false, B32, 1, 0).encode_bytes(),
            Some(vec![0x0F, 0xBC, 0xC8]),
        );
        // bsf r8, rcx: dst=8 (reg field=0, REX.R=0x04), src=1 (r/m field=1), B64.
        //   REX=0x48|0x04=0x4C; modrm=C0|(0<<3)|1=C1.
        assert_eq!(
            Insn::bit_scan_reg_reg(false, B64, 8, 1).encode_bytes(),
            Some(vec![0x4C, 0x0F, 0xBC, 0xC1]),
        );
    }

    /// P11 Bucket N6: cmovcc_reg_reg (B64/B32).
    #[test]
    fn cmovcc_reg_reg_basic() {
        use crate::codegen::shared::asm_operand::Width::*;
        use crate::codegen::x86_64::opcode::CondCode::*;
        // cmove rax, rdx: 48 0F 44 C2 (dst=0 reg, src=2 r/m)
        assert_eq!(
            Insn::cmovcc_reg_reg(Eq, B64, 0, 2).encode_bytes(),
            Some(vec![0x48, 0x0F, 0x44, 0xC2]),
        );
        // cmovne eax, edi: 0F 45 C7 (dst=0, src=7)
        assert_eq!(
            Insn::cmovcc_reg_reg(Ne, B32, 0, 7).encode_bytes(),
            Some(vec![0x0F, 0x45, 0xC7]),
        );
        // cmovne eax, esi: 0F 45 C6
        assert_eq!(
            Insn::cmovcc_reg_reg(Ne, B32, 0, 6).encode_bytes(),
            Some(vec![0x0F, 0x45, 0xC6]),
        );
        // cmovne eax, ebp: 0F 45 C5
        assert_eq!(
            Insn::cmovcc_reg_reg(Ne, B32, 0, 5).encode_bytes(),
            Some(vec![0x0F, 0x45, 0xC5]),
        );
    }

    /// P11 Bucket N7: store_imm_mem C6 /0 / C7 /0.
    #[test]
    fn store_imm_mem_basic() {
        use crate::codegen::shared::asm_operand::Width::*;
        // mov byte [rdi], 0: C6 07 00 (base=7=rdi, mod=00, /0, rm=7)
        assert_eq!(Insn::store_imm_mem(B8, 7, 0, 0).encode_bytes(), Some(vec![0xC6, 0x07, 0x00]),);
        // mov word [rdi], 0: 66 C7 07 00 00
        assert_eq!(
            Insn::store_imm_mem(B16, 7, 0, 0).encode_bytes(),
            Some(vec![0x66, 0xC7, 0x07, 0x00, 0x00]),
        );
        // mov dword [rdi], 0: C7 07 00 00 00 00
        assert_eq!(
            Insn::store_imm_mem(B32, 7, 0, 0).encode_bytes(),
            Some(vec![0xC7, 0x07, 0x00, 0x00, 0x00, 0x00]),
        );
        // mov qword [rdi], 0: 48 C7 07 00 00 00 00
        assert_eq!(
            Insn::store_imm_mem(B64, 7, 0, 0).encode_bytes(),
            Some(vec![0x48, 0xC7, 0x07, 0x00, 0x00, 0x00, 0x00]),
        );
        // mov qword [rsp], 1: 48 C7 04 24 01 00 00 00 (SIB)
        assert_eq!(
            Insn::store_imm_mem(B64, 4, 0, 1).encode_bytes(),
            Some(vec![0x48, 0xC7, 0x04, 0x24, 0x01, 0x00, 0x00, 0x00]),
        );
        // mov dword [rsp], 0x1F80: C7 04 24 80 1F 00 00
        assert_eq!(
            Insn::store_imm_mem(B32, 4, 0, 0x1F80).encode_bytes(),
            Some(vec![0xC7, 0x04, 0x24, 0x80, 0x1F, 0x00, 0x00]),
        );
        // mov word [rsp], 0x037F: 66 C7 04 24 7F 03
        assert_eq!(
            Insn::store_imm_mem(B16, 4, 0, 0x037F).encode_bytes(),
            Some(vec![0x66, 0xC7, 0x04, 0x24, 0x7F, 0x03]),
        );
    }

    /// P11 Bucket AF: simd_cmp_imm8 cmpss/cmpsd reg-reg with imm8 predicate.
    #[test]
    fn simd_cmp_imm8_basic() {
        use crate::codegen::x86_64::opcode::SimdWidth;
        // cmpss xmm0, xmm1, 1 (pred=lt): F3 0F C2 C1 01 (modrm C1: reg=0, r/m=1)
        assert_eq!(
            Insn::simd_cmp_imm8(SimdWidth::SS, 1, 0, 1).encode_bytes(),
            Some(vec![0xF3, 0x0F, 0xC2, 0xC1, 0x01]),
        );
        // cmpsd xmm0, xmm1, 0 (eq): F2 0F C2 C1 00
        assert_eq!(
            Insn::simd_cmp_imm8(SimdWidth::SD, 0, 0, 1).encode_bytes(),
            Some(vec![0xF2, 0x0F, 0xC2, 0xC1, 0x00]),
        );
        // cmpss xmm8, xmm9, 4: REX.R (xmm8 reg) | REX.B (xmm9 r/m) = 0x04 | 0x01 = 0x45
        // modrm C0 | ((8&7)<<3) | (9&7) = C0 | 0 | 1 = C1
        assert_eq!(
            Insn::simd_cmp_imm8(SimdWidth::SS, 4, 8, 9).encode_bytes(),
            Some(vec![0xF3, 0x45, 0x0F, 0xC2, 0xC1, 0x04]),
        );
    }

    /// P11 Bucket AF: pmulld xmm_dst, xmm_src (66 0F 38 40 /r).
    #[test]
    fn pmulld_basic() {
        // pmulld xmm0, xmm1: 66 0F 38 40 C1
        assert_eq!(Insn::pmulld_xmm(0, 1).encode_bytes(), Some(vec![0x66, 0x0F, 0x38, 0x40, 0xC1]),);
        // pmulld xmm8, xmm9: REX.R (dst reg=xmm8) | REX.B (src r/m=xmm9) = 0x45
        // modrm C0 | ((8&7)<<3) | (9&7) = C0 | 0 | 1 = C1
        assert_eq!(
            Insn::pmulld_xmm(8, 9).encode_bytes(),
            Some(vec![0x66, 0x45, 0x0F, 0x38, 0x40, 0xC1]),
        );
    }

    /// P11 Bucket AI: short_jump_rel8 (jcc rel8, 2 bytes).
    #[test]
    fn short_jump_rel8_basic() {
        // jz +16: 74 10
        assert_eq!(Insn::short_jump_rel8(0x74, 0x10).encode_bytes(), Some(vec![0x74, 0x10]),);
        // jnz -16: 75 F0 (i8=-16 → u8=0xF0)
        assert_eq!(Insn::short_jump_rel8(0x75, -16).encode_bytes(), Some(vec![0x75, 0xF0]),);
        // jns +3: 79 03
        assert_eq!(Insn::short_jump_rel8(0x79, 3).encode_bytes(), Some(vec![0x79, 0x03]),);
    }

    /// P11 Bucket AI: popcnt_reg_reg_b64 reflexive (F3 [REX.W] 0F B8 /r).
    #[test]
    fn popcnt_reg_reg_b64_basic() {
        // popcnt rax, rax: F3 48 0F B8 C0 (reg=0, reflexive)
        assert_eq!(
            Insn::popcnt_reg_reg_b64(0).encode_bytes(),
            Some(vec![0xF3, 0x48, 0x0F, 0xB8, 0xC0]),
        );
        // popcnt rcx, rcx: F3 48 0F B8 C9 (reg=1, reflexive modrm=C0|(1<<3)|1=C9)
        assert_eq!(
            Insn::popcnt_reg_reg_b64(1).encode_bytes(),
            Some(vec![0xF3, 0x48, 0x0F, 0xB8, 0xC9]),
        );
        // popcnt r8, r8: reflexive r8 → REX.W|R+B = 4D, modrm C0|0|0 = C0
        assert_eq!(
            Insn::popcnt_reg_reg_b64(8).encode_bytes(),
            Some(vec![0xF3, 0x4D, 0x0F, 0xB8, 0xC0]),
        );
    }

    /// P11 Bucket AI: movmskps_gpr_xmm (0F 50 /r).
    #[test]
    fn movmskps_gpr_xmm_basic() {
        // movmskps eax, xmm0: 0F 50 C0 (no REX)
        assert_eq!(Insn::movmskps_gpr_xmm(0, 0).encode_bytes(), Some(vec![0x0F, 0x50, 0xC0]),);
        // movmskps eax, xmm1: 0F 50 C1
        assert_eq!(Insn::movmskps_gpr_xmm(0, 1).encode_bytes(), Some(vec![0x0F, 0x50, 0xC1]),);
        // movmskps r8d, xmm0: REX.B=0x41, 0F 50 C0
        assert_eq!(Insn::movmskps_gpr_xmm(8, 0).encode_bytes(), Some(vec![0x41, 0x0F, 0x50, 0xC0]),);
        // movmskps eax, xmm8: REX.R=0x44, 0F 50 C0 (src field=0 with REX.R)
        assert_eq!(Insn::movmskps_gpr_xmm(0, 8).encode_bytes(), Some(vec![0x44, 0x0F, 0x50, 0xC0]),);
    }

    /// P11 Bucket P11R: call_rel32 emits E8 00 00 00 00 (patched later).
    #[test]
    fn call_rel32_basic() {
        assert_eq!(Insn::call_rel32().encode_bytes(), Some(vec![0xE8, 0, 0, 0, 0]));
    }

    /// P11 Bucket P11R: jmp_rel32 emits E9 00 00 00 00.
    #[test]
    fn jmp_rel32_basic() {
        assert_eq!(Insn::jmp_rel32().encode_bytes(), Some(vec![0xE9, 0, 0, 0, 0]));
    }

    /// P11 Bucket P11R: jmp_rm_reg (FF /4).
    #[test]
    fn jmp_rm_reg_basic() {
        // jmp rax: FF E0 (mod=11, /4, r/m=0)
        assert_eq!(Insn::jmp_rm_reg(0).encode_bytes(), Some(vec![0xFF, 0xE0]));
        // jmp r8: 41 FF E0 (REX.B)
        assert_eq!(Insn::jmp_rm_reg(8).encode_bytes(), Some(vec![0x41, 0xFF, 0xE0]));
    }

    /// P11 Bucket P11R: jmp_rm_mem (FF /4 [base+disp]).
    #[test]
    fn jmp_rm_mem_basic() {
        // jmp [rdi]: FF 27 (mod=00, /4, r/m=rdi=7)
        assert_eq!(Insn::jmp_rm_mem(7, 0).encode_bytes(), Some(vec![0xFF, 0x27]));
        // jmp [rdi+8]: FF 67 08 (mod=01 disp8)
        assert_eq!(Insn::jmp_rm_mem(7, 8).encode_bytes(), Some(vec![0xFF, 0x67, 0x08]));
        // jmp [rax+0x1000]: FF A0 00 10 00 00 (mod=10, /4=0x20, r/m=0)
        assert_eq!(
            Insn::jmp_rm_mem(0, 0x1000).encode_bytes(),
            Some(vec![0xFF, 0xA0, 0x00, 0x10, 0x00, 0x00]),
        );
    }

    /// P11 Bucket P11R: ud2 (0F 0B).
    #[test]
    fn ud2_basic() {
        assert_eq!(Insn::ud2().encode_bytes(), Some(vec![0x0F, 0x0B]));
    }

    /// P11 Bucket P11R: lea_rip (REX.W 8D /r mod=00 r/m=101 + disp32).
    #[test]
    fn lea_rip_basic() {
        // lea rax, [rip+0]: 48 8D 05 00 00 00 00
        assert_eq!(
            Insn::lea_rip(0).encode_bytes(),
            Some(vec![0x48, 0x8D, 0x05, 0x00, 0x00, 0x00, 0x00]),
        );
        // lea rdi, [rip+0]: 48 8D 3D 00 00 00 00 (modrm reg=rdi=7)
        assert_eq!(
            Insn::lea_rip(7).encode_bytes(),
            Some(vec![0x48, 0x8D, 0x3D, 0x00, 0x00, 0x00, 0x00]),
        );
        // lea rsi, [rip+0]: REX.W+R = 4C, 8D 35 (modrm reg=rsi=6)
        assert_eq!(
            Insn::lea_rip(6).encode_bytes(),
            Some(vec![0x48, 0x8D, 0x35, 0x00, 0x00, 0x00, 0x00]),
        );
    }

    /// P11 Bucket P11R: lea_base_index (SIB form).
    #[test]
    fn lea_base_index_basic() {
        // lea r11, [rbx+r10]: 4F 8D 1C D3
        //   REX.W=0x48 | R(dst=r11>=8)=0x04 | X(index=r10>=8)=0x02 | B(base=rbx<8)=0x00 = 0x4E
        //   Wait, let's verify: r11 = 11 >= 8, rbx = 3, r10 = 10 >= 8.
        //   rex: 0x48 | R(dst>=8) | X(index>=8) | B(base>=8)
        //   = 0x48 | 0x04 | 0x02 | 0x00 = 0x4E
        //   modrm: mod=00, reg=11&7=3, r/m=100(SIB) = 0x00 | (3<<3) | 0x04 = 0x1C
        //   sib: scale=0, index=10&7=2, base=3&7=3 = 0x00 | (2<<3) | 0x03 = 0x13
        //   Hmm, expected: 4F 8D 1C D3. Let's recompute:
        //   Actually looking at mem/access.rs L660: raw_bytes [0x4F, 0x8D, 0x1C, 0xD3]
        //   modrm = 0x1C: mod=00, reg=011(=r11&7=3), r/m=100(SIB follow)
        //   sib = 0xD3: scale=11 (scale=8?), index=1010(r10&7=2 <<3 = 0x10), base=011(rbx=3)
        //   Wait: D3 = 1101 0011. scale=11, index=010=2, base=011=3. scale=11 means scale=8.
        //   But the user said scale=1 fixed. Let me re-check:
        //   Looking at complex.rs pairs: lea r11,[rbx+r10] is for VLA.
        //   But the user explicitly said scale=1. Let's check actual bytes:
        //   0xD3 = 1101_0011. scale bits (top 2) = 11 = 3 → scale=8. Hmm.
        //   Actually wait - the byte 0xD3 was emitted by existing raw_bytes. The user
        //   says "例: lea r11,[rbx+r10] = 4F 8D 1C D3" - but scale=8 is wrong
        //   for rbx+r10 (no scale). However since rbx+r10*8 is just the bytes they
        //   were emitting, and our encode path uses scale=1, we'd emit 0xD3? No:
        //   our scale=1 → scale bits=0 → SIB = 0x00 | (2<<3) | 3 = 0x13.
        //   This is a discrepancy. Let me NOT auto-replace mem/access.rs L660 —
        //   that's the "fallback" path and the bytes don't match our scale=1
        //   encoding. Keep it as raw_bytes.
        //
        // For this test we use a simpler case: lea rax, [rbx+rcx] (scale=1).
        //   rex: 0x48 (B64), no REX.R/X/B since all regs < 8 = 0x48
        //   modrm: mod=00, reg=0, r/m=4(SIB) = 0x04
        //   sib: scale=0, index=1, base=3 = 0x00 | (1<<3) | 3 = 0x0B
        assert_eq!(
            Insn::lea_base_index(0, 3, 1).encode_bytes(),
            Some(vec![0x48, 0x8D, 0x04, 0x0B]),
        );
    }

    /// P11 Bucket P11R: push_mem_disp8 (FF /6 [rsp+disp8] SIB).
    #[test]
    fn push_mem_disp8_basic() {
        // push [rsp+0]: FF 74 24 00 (mod=01, /6, r/m=4 SIB, sib=24, disp8=0)
        assert_eq!(
            Insn::push_mem_disp8(4, 0).encode_bytes(),
            Some(vec![0xFF, 0x74, 0x24, 0x00]),
        );
        // push [rsp+8]: FF 74 24 08
        assert_eq!(
            Insn::push_mem_disp8(4, 8).encode_bytes(),
            Some(vec![0xFF, 0x74, 0x24, 0x08]),
        );
    }

    /// P11 Bucket P11R: simd_mov_rip (movss/movsd xmm, [rip+disp32]).
    #[test]
    fn simd_mov_rip_basic() {
        // movsd xmm0, [rip+0]: F2 0F 10 05 00 00 00 00
        assert_eq!(
            Insn::simd_mov_rip(false, crate::codegen::x86_64::opcode::SimdWidth::SD, 0)
                .encode_bytes(),
            Some(vec![0xF2, 0x0F, 0x10, 0x05, 0x00, 0x00, 0x00, 0x00]),
        );
        // movss xmm0, [rip+0]: F3 0F 10 05 00 00 00 00
        assert_eq!(
            Insn::simd_mov_rip(false, crate::codegen::x86_64::opcode::SimdWidth::SS, 0)
                .encode_bytes(),
            Some(vec![0xF3, 0x0F, 0x10, 0x05, 0x00, 0x00, 0x00, 0x00]),
        );
        // movsd [rip+0], xmm0 (store): F2 0F 11 05 00 00 00 00
        assert_eq!(
            Insn::simd_mov_rip(true, crate::codegen::x86_64::opcode::SimdWidth::SD, 0)
                .encode_bytes(),
            Some(vec![0xF2, 0x0F, 0x11, 0x05, 0x00, 0x00, 0x00, 0x00]),
        );
        // movss [rip+0], xmm8 (store, xmm>=8): F3 44 0F 11 05 00 00 00 00
        assert_eq!(
            Insn::simd_mov_rip(true, crate::codegen::x86_64::opcode::SimdWidth::SS, 8)
                .encode_bytes(),
            Some(vec![0xF3, 0x44, 0x0F, 0x11, 0x05, 0x00, 0x00, 0x00, 0x00]),
        );
    }
}
