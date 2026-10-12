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
use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{MemOperand, Operand, SimdWidth, X86_64Opcode, X87Op};

impl Codegen {
    // ════════════════════════════════════════════════════════════════════════════
    // a6: `long double` 值通道的**单一判据**与 Layer0 原语
    // ════════════════════════════════════════════════════════════════════════════

    /// **唯一判据**：该类型的 rvalue 是否交付在 x87 `st(0)`（而不是 xmm0）。
    ///
    /// 这是本次迁移立起来的新不变量，取代旧的
    /// 「`long double` 局部槽低 8 字节 = f64 位型、高 8 字节 = 0，rvalue 走 xmm0-f64」。
    /// 判据只有两条来源，**不得在任何调用点再叠加局部条件**：
    /// - `long double` 恒为真：80-bit 扩展精度无法由 xmm0-f64 通道无损承载；
    /// - 其余浮点类型仅在 `-mno-sse` 目标下为真 —— 复用既有 x87 通路，
    ///   不新造第二套并行约定。
    pub fn float_in_st0(&self, ty: &Type) -> bool {
        matches!(ty, Type::LongDouble) || !self.target.sse
    }

    /// 是否为“标量浮点”类型（值走 xmm/st(0) 而非 GP 寄存器）。
    /// 单一判据：帧访问出口用它决定“要不要把 rax 里的整数值搬回 dst”。
    pub fn is_scalar_float_channel_ty(&self, ty: &Type) -> bool {
        matches!(ty, Type::Float | Type::Double | Type::LongDouble)
    }

    /// x87 内存访存指令的 `[base + disp32]` 形态（fld/fstp/fild 通用）。
    /// 步骤 4: 改为构造 X86_64Opcode::X87, 由 encode() 逐字节算出.
    /// 仍保留原签名 (opcode, reg_field, base, disp, rexw) 因为调用点已经直接传
    /// escap 字节; encode() 内按 (escap, reg_field) 反查是冗余但不引入新分支.
    fn emit_x87_mem_at(&mut self, opcode: u8, reg_field: u8, base: u8, disp: i32, rexw: bool) {
        // 按原 (opcode, reg_field, rexw) 三元组映射到 X87Op 变体.
        // 这条路径是原 fld/fstp/fild 共用的内建发射点; encode() 已覆盖全部
        // (D9 /0 / DD /0 / DB /5 for fld; D9 /3 / DD /3 / DB /7 for fstp;
        //  DB /0 for fild) 形态, 含任意 base (含 r12 / rsp 的 SIB 兜底).
        let width: u8 = match (opcode, reg_field, rexw) {
            (0xD9, 0, false) => 4,    // fld m32fp
            (0xDD, 0, false) => 8,    // fld m64fp
            (0xDB, 5, false) => 10,   // fld m80fp
            (0xD9, 3, false) => 4,     // fstp m32fp
            (0xDD, 3, false) => 8,     // fstp m64fp
            (0xDB, 7, false) => 10,    // fstp m80fp
            (0xDB, 0, false) => 4,    // fild m32int
            (0xDB, 0, true) => 8,     // fild m64int
            _ => unreachable!("emit_x87_mem_at: unknown opcode/reg_field/rexw tuple (op={opcode:#x}, reg={reg_field}, rexw={rexw})"),
        };
        let op = match (opcode, reg_field) {
            (0xD9, 0) | (0xDD, 0) | (0xDB, 5) => X87Op::FldMem { width },
            (0xD9, 3) | (0xDD, 3) | (0xDB, 7) => X87Op::FstpMem { width },
            (0xDB, 0) => X87Op::FildMem { width },
            _ => unreachable!(),
        };
        let mem = MemOperand {
            disp,
            base: Some(base),
            index: None,
            seg: None,
        };
        let insn = Insn::new(
            X86_64Opcode::X87 { op, mem: Some(mem) },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// `fld` 的 `[base + disp]` 形态：float=D9 /0, double=DD /0, long double=DB /5。
    pub fn emit_x87_fld_at(&mut self, base: u8, disp: i32, ty: &Type) {
        let (opcode, reg_field) = match ty {
            Type::Float => (0xD9u8, 0u8),
            Type::LongDouble => (0xDBu8, 5u8),
            _ => (0xDDu8, 0u8),
        };
        self.emit_x87_mem_at(opcode, reg_field, base, disp, false);
    }

    /// `fstp` 的 `[base + disp]` 形态：float=D9 /3, double=DD /3, long double=DB /7。
    pub fn emit_x87_fstp_at(&mut self, base: u8, disp: i32, ty: &Type) {
        let (opcode, reg_field) = match ty {
            Type::Float => (0xD9u8, 3u8),
            Type::LongDouble => (0xDBu8, 7u8),
            _ => (0xDDu8, 3u8),
        };
        self.emit_x87_mem_at(opcode, reg_field, base, disp, false);
    }

    /// xmm0（float/double）→ st(0)。f32/f64 是 80-bit 的真子集，拓宽无损。
    /// 经 16 字节栈scratch 中转（x86-64 没有 xmm→st 的直接搬送指令）；
    /// sub/add 成对，对嵌套求值与 outgoing 参数区不可见。
    pub fn emit_xmm0_to_st0(&mut self, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp, 16
        if is_float {
            self.emit_insn(&Insn::simd_mov_mem(true, SimdWidth::SS, 0, 0)); // movss %xmm0, (%rsp)
            self.emit_x87_fld_at(4, 0, &Type::Float); // fld dword (%rsp)
        } else {
            self.emit_insn(&Insn::simd_mov_mem(true, SimdWidth::SD, 0, 0)); // movsd %xmm0, (%rsp)
            self.emit_x87_fld_at(4, 0, &Type::Double); // fld qword (%rsp)
        }
        self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp, 16
    }

    /// 整数 rvalue → st(0)。80-bit 有 64 位显式尾数，
    /// 任意 int64 / u64 都能无损表示，所以这条通道**不会**引入精度损失。
    /// `src` 是当前持有该整数值的通用寄存器（`gen_expr(e, dst)` 可以落在非 rax）。
    pub fn emit_int_to_st0(&mut self, ty: &Type, src: u8) {
        let sz = ty.size();
        let unsigned = matches!(
            ty,
            Type::Bool
                | Type::UChar
                | Type::UShort
                | Type::UInt
                | Type::ULong
                | Type::ULongLong
                | Type::Char16
                | Type::Char32
        );
        if src != 0 {
            self.mov(0, src); // 统一以 rax 为输入，下面只使用 rax + 内存 scratch
        }
        self.emit_insn(&Insn::sub_rsp_imm(32)); // sub rsp, 32（16 字节对齐不变）
        if unsigned && sz >= 8 {
            // u64 拆半：x = 2*(x>>1) + (x&1)，两半都落在 i64 范围内。
            // 只用 rax + 内存运算，不占用任何额外 GP 寄存器（rdx/rcx 可能正持待用值）。
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsp], rax        ; x
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(4), index: None, seg: None,
                        }
                    ),
                },
                &[],
            )); // mov rax, [rsp]        ; x
            self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 0, 4, 8)); // mov [rsp+8], rax
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0xD3, 0x6C, 0x24, 0x08])); // shr qword [rsp+8], 1  ; h = x>>1
            self.emit_insn(&Insn::alu_rm_reg_imm8(4, 0, 1)); // and eax, 1                  ; b = x&1
            self.emit_insn(&Insn::mov_rm_mem(false, crate::codegen::shared::asm_operand::Width::B64, 0, 4, 16)); // mov [rsp+16], rax
            self.emit_x87_mem_at(0xDB, 0, 4, 8, true); // fild qword [rsp+8]   st0 = h
            self.emit_insn(&Insn::x87_fixed(0xD9, 0xC8)); // fadd st(0), st(0)            → 2h
            self.emit_x87_mem_at(0xDB, 0, 4, 16, true); // fild qword [rsp+16]  st0 = b
            self.emit_insn(&Insn::x87_fixed(0xDE, 0xC1)); // faddp                          → 2h + b = x
        } else {
            if unsigned {
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov eax, eax → 零扩展高 32 位
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsp], rax
                self.emit_x87_mem_at(0xDB, 0, 4, 0, true); // fild qword [rsp]
            } else if sz >= 8 {
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); // mov [rsp], rax
                self.emit_x87_mem_at(0xDB, 0, 4, 0, true); // fild qword [rsp]
            } else {
                // 窄有符号：只取内存里低 4 字节，fild 自行符号扩展，
                // 不依赖 rax 高位是否已规范化。
                // 注意助记符陷阱（实测 objdump 已确认）：`fild m32int` 是 **DB /0**，
                // 而 `D9 /0` 是 `fld m32fp`——写成 D9 会把整数的**位模式当 float32**
                // 装载（3 变成 4.2e-45），不报错但语义全错。
                self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); // mov [rsp], eax
                self.emit_x87_mem_at(0xDB, 0, 4, 0, false); // fild dword [rsp]
            }
        }
        self.emit_insn(&Insn::add_rsp_imm(32)); // add rsp, 32
    }

    /// 把一个任意类型的 rvalue 搬到 x87 `st(0)`。
    /// 调用前：整数在 `src`（默认 rax）、float/double 在 xmm0（sse 模式）或 st(0)（-mno-sse）、
    /// `long double` 在 st(0)。本函数是双通道交界的**唯一入口**。
    pub fn emit_operand_to_st0(&mut self, ty: &Type, src: u8) {
        if self.float_in_st0(ty) {
            return; // 已在 st(0)
        }
        match ty {
            Type::Float | Type::Double => self.emit_xmm0_to_st0(ty),
            _ => self.emit_int_to_st0(ty, src),
        }
    }

    /// 标量浮点 **lvalue → 值通道**：从 `[base]` 装载。
    /// 单一真源：取代此前散在 `lvalue.rs` / `mem/access.rs` /
    /// `instructions.rs` 的“`long double` 用 `movsd` 读低 8 字节”写法（那依赖
    /// “槽高 8 字节必为 0且内容就是 f64 位型”的旧不变量，真 80-bit 落槽后必读错）。
    pub fn emit_load_scalar_float_at(&mut self, ty: &Type, base: u8) {
        if self.float_in_st0(ty) {
            self.emit_x87_fld_at(base, 0, ty); // fld tbyte / qword / dword [base]
            return;
        }
        let pfx: u8 = if matches!(ty, Type::Float) { 0xF3 } else { 0xF2 };
        // 把整条指令拼成**一条** raw_with_gas 发射（否则 REX/操作码/ModRM 各自落 record,
        // gas_len_map 只给到首段长度, 后续 pos 漂移 → 后续指令全部 fallback/错位).
        let base_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(base, 64)
            .unwrap_or_else(|| "%rax".to_string());
        let op_name = if matches!(ty, Type::Float) { "movss" } else { "movsd" };
        let mut bytes: Vec<u8> = Vec::new();
        if base >= 8 {
            bytes.push(0x41); // REX.B
        }
        bytes.push(pfx);
        bytes.extend_from_slice(&[0x0F, 0x10]);
        // mod=00, reg=xmm0(0), r/m=base & 7
        let rm = base & 7;
        if rm == 4 {
            bytes.extend_from_slice(&[0x04, 0x24]); // SIB [rsp]
        } else if rm == 5 {
            bytes.extend_from_slice(&[0x45, 0x00]);
        } else {
            bytes.push(rm);
        }
        // 注意: 操作数顺序 AT&T: 源 (mem) → 目标 (xmm0)
        let gas = format!("{} ({}), %xmm0", op_name, base_name);
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    /// 标量浮点 **值通道 → lvalue**：存到 `[base]`（`fstp tbyte` 或 `movss/movsd`）。
    /// 前置：`src_ty` 类型的 rvalue 已按通道约定求值完毕（整数在 rax、float/double 在
    /// xmm0、`long double` 在 st(0)），且 `dst_ty` 是标量浮点类型。
    /// 此处收浮了 `dispatch.rs`/`init.rs`/`mem/access.rs` 共 12+ 份
    /// “`emit_scalar_to_xmm0_if_needed` + `movss/movsd [rcx], xmm0`”影子副本：
    /// 值通道改 st(0) 后，那份写法会把**陈旧的 xmm0** 写进 ld 对象（静默错），
    /// 并把 st(0) 漏在 x87 栈上。
    pub fn emit_store_scalar_float_at(&mut self, src_ty: &Type, dst_ty: &Type, base: u8) {
        if self.float_in_st0(dst_ty) {
            self.emit_operand_to_st0(src_ty, 0); // 整数 / f32 / f64 → st(0)（精确拓宽）
            self.emit_x87_fstp_at(base, 0, dst_ty);
            return;
        }
        // 目标在 xmm 通道：源若落在 st(0)（long double 表达式，或 -mno-sse 目标），
        // 单步 80→64 / 80→32 舍入，不经中间格式。
        if self.float_in_st0(src_ty) {
            if matches!(dst_ty, Type::Float) {
                self.emit_x87_to_xmm0_f32();
            } else {
                self.emit_x87_to_xmm0();
            }
        } else {
            self.emit_scalar_to_xmm0_if_needed_ty(src_ty, dst_ty);
        }
        let pfx: u8 = if matches!(dst_ty, Type::Float) { 0xF3 } else { 0xF2 };
        // 整条拼成一条 raw_with_gas (防 gas_len 漂移, 同 load).
        let base_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(base, 64)
            .unwrap_or_else(|| "%rax".to_string());
        let op_name = if matches!(dst_ty, Type::Float) { "movss" } else { "movsd" };
        let mut bytes: Vec<u8> = Vec::new();
        if base >= 8 {
            bytes.push(0x41); // REX.B
        }
        bytes.push(pfx);
        bytes.extend_from_slice(&[0x0F, 0x11]);
        let rm = base & 7;
        if rm == 4 {
            bytes.extend_from_slice(&[0x04, 0x24]);
        } else if rm == 5 {
            bytes.extend_from_slice(&[0x45, 0x00]);
        } else {
            bytes.push(rm);
        }
        let gas = format!("{} %xmm0, ({})", op_name, base_name);
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    /// 内存中的标量浮点 lvalue → 指定 xmm（inline asm 的 xmm 约束输入）。
    /// `float_in_st0` 类型（long double / -mno-sse）先 fld 到 st(0)，
    /// 再经 16 字节栈 scratch 完成 80→32/64 单步舍入并 movss/movsd 进 xmm；
    /// f32/f64 直接 movss/movsd。旧写法对 long double 发 movq，
    /// 取走 80-bit 位型的低 8 字节当 double，值完全失真。
    pub fn emit_mem_float_to_xmm(&mut self, base: u8, disp: i32, ty: &Type, xmm: u8) {
        if self.float_in_st0(ty) {
            // sub rsp,16（scratch，同时保持调用对齐）
            self.emit_insn(&Insn::sub_rsp_imm(16));
            self.emit_x87_fld_at(base, disp, ty);
            self.emit_x87_fstp_at(4, 0, ty); // fstp tbyte [rsp]
                                             // 按 xmm 目标宽度舍入重读到 xmm
            let is_float = matches!(ty, Type::Float);
            // REX 必须在 mandatory prefix F2/F3 之后、操作码之前
            let width = if is_float { SimdWidth::SS } else { SimdWidth::SD };
            self.emit_insn(&Insn::simd_mov_mem_base(false, width, xmm, 4, 0));
            self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp,16
            return;
        }
        let width = if matches!(ty, Type::Float) { SimdWidth::SS } else { SimdWidth::SD };
        self.emit_insn(&Insn::simd_mov_mem_base(false, width, xmm, base, disp));
    }

    /// xmm（f32/f64）→ 内存中的 `float_in_st0` lvalue（inline asm xmm
    /// 约束写回 long double 槽）：经 16 字节 scratch 拓宽到 st(0) 后
    /// fstp tbyte，与 f64→80 的扩展语义一致。
    pub fn emit_xmm_to_mem_float(
        &mut self,
        xmm: u8,
        src_ty: &Type,
        base: u8,
        disp: i32,
        dst_ty: &Type,
    ) {
        debug_assert!(self.float_in_st0(dst_ty));
        self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp,16
        let width = if matches!(src_ty, Type::Float) { SimdWidth::SS } else { SimdWidth::SD };
        self.emit_insn(&Insn::simd_mov_mem_base(true, width, xmm, 4, 0)); // mov [rsp], xmm
        self.emit_x87_fld_at(4, 0, src_ty); // fld [rsp]（精确拓宽）
        self.emit_x87_fstp_at(base, disp, dst_ty); // fstp tbyte/dword/qword [base+disp]
        self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp,16
    }

    /// x87 浮点比较: fucomip + 标志修正 -> rax (0/1)
    /// 调用前: 需要加载左操作数和右操作数到 x87 栈
    pub fn gen_float_cmp_x87(&mut self, op: BinOp, ty: &Type, l: &Expr, r: &Expr) {
        let is_ld = matches!(ty, Type::LongDouble);
        // 固定 16 字节: 既够装 tbyte, 又保证嵌套求值里 `call` 时 rsp 仍 16 字节对齐
        // （旧代码非 ld 时用 8, 会在比较表达式含调用时把对齐破坏给被调方）。
        let save_bytes: u8 = 16;
        let _ = is_ld;
        // 加载左操作数到 st(0)
        self.gen_expr(l, Self::EVAL_BASE); // st(0) = l
        self.emit_operand_to_st0(&l.ty(), 0);
        self.emit_insn(&Insn::sub_rsp_imm(save_bytes as i64)); // sub rsp, 16
        if is_ld {
            self.emit_x87_fstp_at(4, 0, &Type::LongDouble); // fstp tbyte [rsp]
        } else {
            self.emit_x87_fstp_at(4, 0, &Type::Double); // fstp qword [rsp]
        }
        self.expr_depth += 1;
        // 加载右操作数到 st(0)
        self.gen_expr(r, Self::EVAL_BASE); // st(0) = r
        self.emit_operand_to_st0(&r.ty(), 0);
        self.expr_depth -= 1;
        // 重新加载左操作数: st(0) = l, st(1) = r
        if is_ld {
            self.emit_x87_fld_at(4, 0, &Type::LongDouble); // fld tbyte [rsp]
        } else {
            self.emit_x87_fld_at(4, 0, &Type::Double); // fld qword [rsp]
        }
        self.emit_insn(&Insn::add_rsp_imm(save_bytes as i64)); // add rsp, 16
                                                    // 现在 st(0) = l, st(1) = r
                                                    // fucomip st(0), st(1): 比较 st(0) vs st(1), 设置 EFLAGS, 弹出 st(0)
        self.emit_insn(&Insn::x87_fixed(0xDF, 0xE9)); // fucomip st(0), st(1)
                                  // 实测（fnstsw 读 TOP 位）：FUCOMIP 只弹**一次**，
                                  // 弹出的是左操作数，右操作数仍留在 st(0)。不弹掉它，
                                  // 每个浮点比较就漏一层 x87 栈（8 层后溢出成静默错值，
                                  // 表现为“前 7 次比较正确、第 8 次开始全错”）。
                                  // `fstp st(0)` 不修改 EFLAGS，所以可以安全放在 setcc 之前。
        self.emit_x87_fstp_st0();
        self.emit_x87_setcc_flags(op);
    }

    /// 把 `fucomip` 留下的 CF/ZF/PF 翻译成 rax = 0/1。
    ///
    /// x87 比较**没有 SF**，因此 `setl/setg` 一类有符号 cc 不可用；而 unordered
    /// 时 ZF=PF=CF=1，所以 `<`/`<=`/`==`/`!=` 必须额外用 PF 排除 unordered，
    /// 否则 NaN 会判成“小于/小于等于/相等”（旧的裸 `setb/setbe/sete` 就是错的）。
    /// `>`/`>=` 用 `seta`/`setae`：unordered 有 CF=1，天然为假。
    pub fn emit_x87_setcc_flags(&mut self, op: BinOp) {
        match op {
            BinOp::Gt => {
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::A, 0)); // seta al
            }
            BinOp::Ge => {
                self.emit_insn(&Insn::setcc_reg(crate::codegen::x86_64::opcode::CondCode::Ae, 0)); // setae al
            }
            _ => {
                let invert = matches!(op, BinOp::Ne);
                use crate::codegen::x86_64::opcode::CondCode;
                let cc: CondCode = match op {
                    BinOp::Lt => CondCode::B,    // setb  (CF=1)
                    BinOp::Le => CondCode::Be,   // setbe (CF=1 || ZF=1)
                    _ => CondCode::Eq,           // sete  (ZF=1) —— EqEq 与 Ne 共用
                };
                self.emit_insn(&Insn::setcc_reg(cc, 0)); // setcc al
                self.emit_insn(&Insn::setcc_reg(CondCode::Np, 1)); // setnp cl  (PF=0 → 1)
                self.emit_insn(&Insn::alu_rm_reg_reg(
                    crate::codegen::x86_64::opcode::AluOp::And,
                    crate::codegen::shared::asm_operand::Width::B8,
                    0,
                    1,
                )); // and al, cl  → 排除 unordered
                if invert {
                    // xor al, 1 = 34 01 (no REX). 80 /6 ib would grow the insn; keep raw.
                    self.emit_insn(&Insn::raw_bytes(&[0x34, 0x01]));
                }
                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                    sign: false,
                    from: crate::codegen::shared::asm_operand::Width::B8,
                    to: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
                }, &[])); // movzx rax, al
            },
        }
        if matches!(op, BinOp::Gt | BinOp::Ge) {
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                sign: false,
                from: crate::codegen::shared::asm_operand::Width::B8,
                to: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B8),
            }, &[])); // movzx rax, al
        }
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
            self.emit_insn(&Insn::raw_bytes(&[0x41, opcode]));
            self.emit_r13_modrm_disp(reg_field, off);
        } else {
            self.emit_lea_r13(11, off);
            self.emit_insn(&Insn::raw_bytes(&[0x41, opcode]));
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
            self.emit_insn(&Insn::raw_bytes(&[0x41, opcode]));
            self.emit_r13_modrm_disp(reg_field, off);
        } else {
            self.emit_lea_r13(11, off);
            self.emit_insn(&Insn::raw_bytes(&[0x41, opcode]));
            self.emit_reg_indirect_modrm(reg_field, 11);
        }
    }

    /// x87 fstp st(0): 彈出棧頂 (不存儲)
    pub fn emit_x87_fstp_st0(&mut self) {
        // fstp st(0): DD D9
        self.emit_insn(&Insn::new(
            X86_64Opcode::X87 {
                op: X87Op::Fixed { escap: 0xDD, modrm: 0xD9 },
                mem: None,
            },
            &[],
        ));
    }

    /// x87 st(0) (long double) → xmm0 (double): 先存成 f64 再装载。
    /// 自带 `sub/add rsp,16` 平衡：外层 `gen_float_binop` 会把操作数溢出到
    /// `[rsp]`，不能直接踩现有栈顶（旧版无 sub 的写法会静默覆盖那个溢出槽）。
    pub fn emit_x87_to_xmm0(&mut self) {
        self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp, 16
        self.emit_x87_fstp_at(4, 0, &Type::Double); // fstp qword (%rsp)  ← 硬件完成 80→64 舍入
        self.emit_insn(&Insn::simd_mov_mem(false, SimdWidth::SD, 0, 0)); // movsd xmm0, (%rsp)
        self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp, 16
    }

    /// x87 st(0) (long double) → xmm0 (float)：单步 80→32 舍入，
    /// 不经 f64 双舍入（`fstp dword` 直接产出 float 位型）。
    pub fn emit_x87_to_xmm0_f32(&mut self) {
        self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp, 16
        self.emit_x87_fstp_at(4, 0, &Type::Float); // fstp dword (%rsp)
        self.emit_insn(&Insn::simd_mov_mem(false, SimdWidth::SS, 0, 0)); // movss xmm0, (%rsp)
        self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp, 16
    }

    /// x87 fldz: 加載 +0.0 到 st(0)
    pub fn emit_x87_fldz(&mut self) {
        // fldz: D9 EE
        self.emit_insn(&Insn::new(
            X86_64Opcode::X87 {
                op: X87Op::Fixed { escap: 0xD9, modrm: 0xEE },
                mem: None,
            },
            &[],
        ));
    }

    /// x87 fld1: 加載 +1.0 到 st(0)
    pub fn emit_x87_fld1(&mut self) {
        // fld1: D9 E8
        self.emit_insn(&Insn::new(
            X86_64Opcode::X87 {
                op: X87Op::Fixed { escap: 0xD9, modrm: 0xE8 },
                mem: None,
            },
            &[],
        ));
    }

    /// x87 立即數加載: 將浮點常量存入 .rodata, 用 fld 加載到 st(0)
    pub fn emit_x87_float_imm(&mut self, c: crate::ast::FloatConst, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        let bytes = match c {
            // long double: 直接落真 80-bit 位型（10 字节），不做任何
            // f64 中转，消除 `0.1L` 的末位精度差异。
            crate::ast::FloatConst::LongDouble(ref b) => b.to_vec(),
            crate::ast::FloatConst::F32(f) => f.to_le_bytes().to_vec(),
            crate::ast::FloatConst::F64(f) => {
                if is_float {
                    (f as f32).to_le_bytes().to_vec()
                } else if is_ld {
                    f64_to_x87_80(f)
                } else {
                    f.to_le_bytes().to_vec()
                }
            },
        };
        let key = format!(
            "x87f_{}",
            bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
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
        self.emit_insn(&Insn::raw_bytes(&[opcode, modrm, 0, 0, 0, 0]));
        // x87 fld [rip+disp32] 是 6 字节指令 (disp32 @ byte 2),
        // 不同于 SSE movsd/movss 的 8 字节 (disp32 @ byte 4).
        self.x87_float_refs.push((at, off as u64));
    }

    /// x87 浮點二元運算: st(0) = st(0) op st(1), 然後彈出 st(0)
    /// 調用前: st(0) = 右操作數, st(1) = 左操作數
    pub fn emit_x87_binop(&mut self, op: BinOp, is_float: bool) {
        let (escap, modrm): (u8, u8) = match op {
            BinOp::Add => (0xDE, 0xC1),    // faddp st(1), st(0)
            BinOp::Sub => (0xDE, 0xE9),    // fsubrp st(1), st(0)
            BinOp::Mul => (0xDE, 0xC9),    // fmulp st(1), st(0)
            BinOp::Div => (0xDE, 0xF9),    // fdivrp st(1), st(0)
            _ => return,
        };
        self.emit_insn(&Insn::new(
            X86_64Opcode::X87 {
                op: X87Op::Fixed { escap, modrm },
                mem: None,
            },
            &[],
        ));
        let _ = is_float;
    }

    /// x87 浮點比較: 設置 EFLAGS 供 setcc 使用
    /// 調用前: st(0) = 右操作數, st(1) = 左操作數
    pub fn emit_x87_cmp_and_pop(&mut self) {
        // fucomip st(0), st(1): DF E9
        self.emit_insn(&Insn::new(
            X86_64Opcode::X87 {
                op: X87Op::Fixed { escap: 0xDF, modrm: 0xE9 },
                mem: None,
            },
            &[],
        ));
    }

    /// x87 浮點取負: st(0) = -st(0)
    pub fn emit_x87_chs(&mut self) {
        // fchs: D9 E0
        self.emit_insn(&Insn::new(
            X86_64Opcode::X87 {
                op: X87Op::Fixed { escap: 0xD9, modrm: 0xE0 },
                mem: None,
            },
            &[],
        ));
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
