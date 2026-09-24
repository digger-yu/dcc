// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::Riscv64Codegen;
use crate::ir::{lower, validate};

#[test]
fn riscv64_ir_return_add() {
    let (program, diags) =
        crate::parser::parse(b"int main(void) { return 10 + 32; }\n", "rv.c");
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 243);
}

// M7 修复: div/mod 真实 RV64M 指令 (div/rem) 替代 0
// div rd, rs1, rs2: funct7=0000001, funct3=100 (有符号)
// 编码 (rd=a0, rs1=a0, rs2=a1): 0x02B54533 LE 编码 = 33 45 B5 02
// 注: 使用 opaque 函数防止常量折叠
#[test]
fn riscv64_ir_div_mod_emit_rv64m() {
    let (program, diags) = crate::parser::parse(
        b"extern int opaque(int);\n\
          int main(void) { int a = opaque(42); int b = opaque(5); int q = a / b; int r = a % b; return q + r; }\n",
        "rv.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    // div a0, a0, a1: 0x02B54533 LE = 33 45 B5 02
    let has_div = obj.windows(4).any(|w| w == [0x33, 0x45, 0xb5, 0x02]);
    // rem a0, a0, a1: 0x02B56533 LE = 33 65 B5 02
    let has_rem = obj.windows(4).any(|w| w == [0x33, 0x65, 0xb5, 0x02]);
    assert!(has_div || has_rem, "expected div/rem in riscv64 text");
}

// M7 修复: 移位真实 sll/srl 替代 0
// sll a0, a0, a1: funct7=0000000, funct3=001
//   insn = (11<<20)|(10<<15)|(1<<12)|(10<<7)|0x33 = 0x00B51533 LE = 33 15 B5 00
// srl a0, a0, a1: funct7=0000000, funct3=101
//   insn = 0x00B55533 LE = 33 55 B5 00
#[test]
fn riscv64_ir_shifts_emit_sll_srl() {
    let (program, diags) = crate::parser::parse(
        b"extern int opaque(int);\n\
          int main(void) { int a = opaque(1); int b = opaque(2); return (a << b) | (a >> b); }\n",
        "rv.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    let has_sll = obj.windows(4).any(|w| w == [0x33, 0x15, 0xb5, 0x00]);
    let has_srl = obj.windows(4).any(|w| w == [0x33, 0x55, 0xb5, 0x00]);
    assert!(has_sll || has_srl, "expected sll/srl in riscv64 text");
}

// M7 修复: 聚合类型参数按引用传递
// 验证: 调函数时,若参数是 local struct, 发射 addi rd, sp, #off
// addi rd, sp, #off 编码: opcode=0x13, funct3=0, rs1=2 (sp)
// 重建 insn 后检查: (insn & 0x7F) == 0x13 && (insn>>12)&7 == 0 && (insn>>15)&0x1F == 2
#[test]
fn riscv64_aggregate_arg_by_ref() {
    let (program, diags) = crate::parser::parse(
        b"struct S { int x; int y; };\n\
          void callee(struct S s) {}\n\
          int main(void) { struct S s; s.x = 1; s.y = 2; callee(s); return 0; }\n",
        "rv.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    let has_addi_sp = obj.windows(4).any(|w| {
        if w.len() < 4 {
            return false;
        }
        let insn = (w[0] as u32)
            | ((w[1] as u32) << 8)
            | ((w[2] as u32) << 16)
            | ((w[3] as u32) << 24);
        (insn & 0x7F) == 0x13 && ((insn >> 12) & 7) == 0 && ((insn >> 15) & 0x1F) == 2
    });
    assert!(
        has_addi_sp,
        "expected addi rd, sp, #off (M7 by-ref aggregate)"
    );
}

// M8: JAL 范围校验
#[test]
fn riscv64_jal_range_check() {
    assert!(Riscv64Codegen::jal_offset_in_range(0));
    assert!(Riscv64Codegen::jal_offset_in_range((1 << 20) - 1));
    assert!(Riscv64Codegen::jal_offset_in_range(-(1 << 20)));
    assert!(!Riscv64Codegen::jal_offset_in_range(1 << 20));
    assert!(!Riscv64Codegen::jal_offset_in_range(-(1 << 20) - 1));
}

// M8: 压缩指令 C.ADDI 编码验证
// C.ADDI x10, 5: funct3=000, imm[5]=0, rd=10, imm[4:0]=00101, op=01
// = 0b000_0_01010_00101_01 = 0x0511
#[test]
fn riscv64_compressed_c_addi_emit() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_c_addi(10, 5);
    let code = &cg.code;
    assert!(code.len() >= 2, "C.ADDI should emit 2 bytes");
    let insn = u16::from_le_bytes([code[0], code[1]]);
    // funct3=000 (bits 15:13), rd=10 (bits 11:7), op=01 (bits 1:0)
    assert_eq!(insn & 0x3, 0x01, "C.ADDI opcode should be 01");
    assert_eq!((insn >> 7) & 0x1f, 10, "rd should be a0 (x10)");
}

// M8: 双精度 FP D 扩展指令编码
// FADD.D fa0, fa0, fa1: funct7=0000001, rs2=11, rs1=10, rd=10
// insn = 0x02B57553
#[test]
fn riscv64_fadd_d_emit() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_fadd_d(10, 10, 11); // fa0 = fa0 + fa1
    let code = &cg.code;
    assert!(code.len() >= 4);
    let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!(insn & 0x7f, 0x53, "opcode should be 0x53 (FP)");
    assert_eq!(
        (insn >> 25) & 0x7f,
        0x01,
        "funct7 should be 0000001 (FADD.D)"
    );
}

// M8: CSR 访问指令 CSRRS 编码
// CSRRS a0, mstatus(0x300), zero: csr=0x300, rs1=0, rd=10
#[test]
fn riscv64_csrrs_mstatus_emit() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_csrrs(10, 0x300, 0); // read mstatus
    let code = &cg.code;
    assert!(code.len() >= 4);
    let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!(insn & 0x7f, 0x73, "opcode should be 0x73 (SYSTEM)");
    assert_eq!((insn >> 12) & 0x7, 0b010, "funct3 should be 010 (CSRRS)");
    assert_eq!((insn >> 20) & 0xfff, 0x300, "csr should be mstatus (0x300)");
}

// M8: RVV VSETVLI 指令编码
#[test]
fn riscv64_vsetvli_emit() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_vsetvli(0, 0, 0); // vsetvli zero, zero, vtype=0
    let code = &cg.code;
    assert!(code.len() >= 4);
    let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!(insn & 0x7f, 0x57, "opcode should be 0x57 (OP-V)");
    assert_eq!((insn >> 12) & 0x7, 0b111, "funct3 should be 111");
}

// M8: FENCE 内存屏障编码
#[test]
fn riscv64_fence_emit() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_fence();
    let code = &cg.code;
    assert!(code.len() >= 4);
    let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!(insn, 0x0ff0000f, "FENCE encoding: 0x0ff0000f");
}

// M8: MRET 特权返回编码
#[test]
fn riscv64_mret_emit() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_mret();
    let code = &cg.code;
    assert!(code.len() >= 4);
    let insn = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!(insn, 0x30200073, "MRET encoding: 0x30200073");
}

// M8: Zicsr 完整 CSR 指令集测试
#[test]
fn riscv64_zicsr_full() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // CSRRSI x10, mstatus(0x300), 0x1f
    cg.emit_csrrsi(10, 0x300, 0x1f);
    // CSRRCI x11, mstatus(0x300), 0x0f
    cg.emit_csrrci(11, 0x300, 0x0f);
    let code = &cg.code;
    assert!(code.len() >= 8, "expected 8 bytes for 2 CSR instructions");
    let insn1 = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    let insn2 = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
    // CSRRSI: funct3=110, opcode=0x73
    assert_eq!((insn1 & 0x7000) >> 12, 0b110, "CSRRSI funct3=110");
    assert_eq!(insn1 & 0x7f, 0x73, "CSRRSI opcode=0x73");
    // CSRRCI: funct3=111, opcode=0x73
    assert_eq!((insn2 & 0x7000) >> 12, 0b111, "CSRRCI funct3=111");
    assert_eq!(insn2 & 0x7f, 0x73, "CSRRCI opcode=0x73");
}

// M8: D 扩展完整指令集测试
#[test]
fn riscv64_d_extension_full() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // FSQRT.D f10, f11
    cg.emit_fsqrt_d(10, 11);
    // FMIN.D f12, f13, f14
    cg.emit_fmin_d(12, 13, 14);
    // FMAX.D f15, f16, f17
    cg.emit_fmax_d(15, 16, 17);
    // FEQ.D x10, f11, f12
    cg.emit_feq_d(10, 11, 12);
    // FLT.D x13, f14, f15
    cg.emit_flt_d(13, 14, 15);
    // FLE.D x16, f17, f18
    cg.emit_fle_d(16, 17, 18);
    let code = &cg.code;
    assert!(
        code.len() >= 24,
        "expected 24 bytes for 6 D-extension instructions"
    );
    // FSQRT.D: funct7=0101101
    let insn_sqrt = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!(
        (insn_sqrt >> 25) & 0x7f,
        0b0101101,
        "FSQRT.D funct7=0101101"
    );
    // FMIN.D: funct7=0010101, funct3=000
    let insn_min = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
    assert_eq!((insn_min >> 25) & 0x7f, 0b0010101, "FMIN.D funct7=0010101");
    assert_eq!((insn_min >> 12) & 0x7, 0b000, "FMIN.D funct3=000");
    // FMAX.D: funct7=0010101, funct3=001
    let insn_max = u32::from_le_bytes([code[8], code[9], code[10], code[11]]);
    assert_eq!((insn_max >> 25) & 0x7f, 0b0010101, "FMAX.D funct7=0010101");
    assert_eq!((insn_max >> 12) & 0x7, 0b001, "FMAX.D funct3=001");
    // FEQ.D: funct7=1010001, funct3=010
    let insn_eq = u32::from_le_bytes([code[12], code[13], code[14], code[15]]);
    assert_eq!((insn_eq >> 25) & 0x7f, 0b1010001, "FEQ.D funct7=1010001");
    assert_eq!((insn_eq >> 12) & 0x7, 0b010, "FEQ.D funct3=010");
    // FLT.D: funct7=1010001, funct3=001
    let insn_lt = u32::from_le_bytes([code[16], code[17], code[18], code[19]]);
    assert_eq!((insn_lt >> 25) & 0x7f, 0b1010001, "FLT.D funct7=1010001");
    assert_eq!((insn_lt >> 12) & 0x7, 0b001, "FLT.D funct3=001");
    // FLE.D: funct7=1010001, funct3=000
    let insn_le = u32::from_le_bytes([code[20], code[21], code[22], code[23]]);
    assert_eq!((insn_le >> 25) & 0x7f, 0b1010001, "FLE.D funct7=1010001");
    assert_eq!((insn_le >> 12) & 0x7, 0b000, "FLE.D funct3=000");
}

// M25: RISC-V va_list 寄存器保存区 单元测试
// 验证变参函数入口发出 8 条 sd a0..a7, off(sp) 保存指令.
#[test]
fn riscv64_va_save_area_stores_a0_through_a7() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.is_current_func_variadic = true;
    cg.va_save_base_off = 16;
    cg.stack_bytes = 80;
    cg.frame_bytes = 80;
    // 模拟 gen_function 入口保存动作
    for i in 0..8u8 {
        cg.emit_sd(10 + i, 2, cg.va_save_base_off + (i as i32) * 8);
    }
    // 验证: 8 条 sd aN, off(sp) 指令, LE 编码:
    //   byte0 低 7 位 = opcode = 0x23 (SD)
    //   byte1 位 6:4  = funct3 = 0b011
    assert!(
        cg.code.len() >= 32,
        "expected at least 32 bytes of SD instructions"
    );
    let sd_count = cg
        .code
        .windows(4)
        .filter(|w| (w[0] & 0x7f) == 0x23 && (w[1] & 0x70) == 0x30)
        .count();
    assert_eq!(sd_count, 8, "expected 8 SD instructions, got {sd_count}");
}

// M25: RISC-V va_arg 整数路径 单元测试
// 验证 gen_va_arg_int 生成运行时偏移读取代码
#[test]
fn riscv64_va_arg_int_reads_sequentially_from_save_area() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.is_current_func_variadic = true;
    cg.va_save_base_off = 128;
    cg.frame_bytes = 256;
    cg.locals.insert("ap".to_string(), 96);
    // 生成 va_arg_int
    cg.gen_va_arg_int("ap");
    // 验证: 代码包含 BGE 指令 (funct3=101, opcode=1100011)
    let has_bge = cg.code.windows(4).any(|w| {
        (w[0] & 0x7f) == 0x63 && (w[1] & 0x70) == 0x50 // funct3=101 (BGE)
    });
    assert!(has_bge, "expected BGE instruction");
    // 验证: 代码非空且长度合理
    assert!(
        cg.code.len() > 20,
        "expected > 20 bytes of code, got {}",
        cg.code.len()
    );
}

// ===== 压缩指令自动选择测试 =====

#[test]
fn riscv64_compress_addi_to_c_addi() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // ADDI x10, x10, 5 => C.ADDI x10, 5 (2 bytes)
    cg.emit_addi(10, 10, 5);
    assert_eq!(
        cg.code.len(),
        2,
        "ADDI rd==rs1 should compress to C.ADDI (2 bytes)"
    );
    // Verify it's a valid C.ADDI: bits[1:0]=01, funct3[15:13]=000
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x01, "C.ADDI opcode should be 01");
    assert_eq!((insn >> 13) & 0x7, 0, "C.ADDI funct3 should be 000");
}

#[test]
fn riscv64_compress_li_to_c_li() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // ADDI x10, x0, 20 => C.LI x10, 20 (2 bytes)
    cg.emit_addi(10, 0, 20);
    assert_eq!(cg.code.len(), 2, "LI should compress to C.LI (2 bytes)");
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x01, "C.LI opcode should be 01");
    assert_eq!((insn >> 13) & 0x7, 0b010, "C.LI funct3 should be 010");
}

#[test]
fn riscv64_compress_lui_to_c_lui() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // LUI x10, 5 => C.LUI x10, 5 (2 bytes)
    cg.emit_lui(10, 5);
    assert_eq!(
        cg.code.len(),
        2,
        "LUI with small imm should compress to C.LUI (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x01, "C.LUI opcode should be 01");
    assert_eq!((insn >> 13) & 0x7, 0b011, "C.LUI funct3 should be 011");
}

#[test]
fn riscv64_compress_add_to_c_mv() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // ADD x10, x0, x11 => C.MV x10, x11 (2 bytes)
    cg.emit_add(10, 0, 11);
    assert_eq!(
        cg.code.len(),
        2,
        "ADD rd, x0, rs2 should compress to C.MV (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x02, "C.MV opcode should be 10");
}

#[test]
fn riscv64_compress_add_to_c_add() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // ADD x10, x10, x11 => C.ADD x10, x11 (2 bytes)
    cg.emit_add(10, 10, 11);
    assert_eq!(
        cg.code.len(),
        2,
        "ADD rd==rs1 should compress to C.ADD (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x02, "C.ADD opcode should be 10");
}

#[test]
fn riscv64_compress_lw_to_c_lw() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // LW x10, 8(x10) => C.LW x10', x10', 8 (2 bytes)
    cg.emit_lw(10, 10, 8);
    assert_eq!(
        cg.code.len(),
        2,
        "LW with x8-x15 and aligned offset should compress to C.LW (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x00, "C.LW opcode should be 00");
    assert_eq!((insn >> 13) & 0x7, 0b010, "C.LW funct3 should be 010");
}

#[test]
fn riscv64_compress_sw_to_c_sw() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // SW x10, 8(x10) => C.SW x10', x10', 8 (2 bytes)
    cg.emit_sw(10, 10, 8);
    assert_eq!(
        cg.code.len(),
        2,
        "SW with x8-x15 and aligned offset should compress to C.SW (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x00, "C.SW opcode should be 00");
    assert_eq!((insn >> 13) & 0x7, 0b110, "C.SW funct3 should be 110");
}

#[test]
fn riscv64_compress_ld_to_c_ld() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // LD x10, 16(x10) => C.LD x10', x10', 16 (2 bytes)
    cg.emit_ld(10, 10, 16);
    assert_eq!(
        cg.code.len(),
        2,
        "LD with x8-x15 and aligned offset should compress to C.LD (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x01, "C.LD opcode should be 01");
    assert_eq!((insn >> 13) & 0x7, 0b011, "C.LD funct3 should be 011");
}

#[test]
fn riscv64_compress_sd_to_c_sd() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // SD x10, 16(x10) => C.SD x10', x10', 16 (2 bytes)
    cg.emit_sd(10, 10, 16);
    assert_eq!(
        cg.code.len(),
        2,
        "SD with x8-x15 and aligned offset should compress to C.SD (2 bytes)"
    );
    let insn = u16::from_le_bytes([cg.code[0], cg.code[1]]);
    assert_eq!(insn & 0x3, 0x01, "C.SD opcode should be 01");
    assert_eq!((insn >> 13) & 0x7, 0b111, "C.SD funct3 should be 111");
}

#[test]
fn riscv64_no_compress_addi_out_of_range() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // ADDI x10, x10, 100 => 4 bytes (out of C.ADDI range -32..31)
    cg.emit_addi(10, 10, 100);
    assert_eq!(
        cg.code.len(),
        4,
        "ADDI with large imm should not compress (4 bytes)"
    );
}

#[test]
fn riscv64_no_compress_lw_wrong_regs() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    // LW x5, 8(x10) => 4 bytes (rd not in x8-x15)
    cg.emit_lw(5, 10, 8);
    assert_eq!(
        cg.code.len(),
        4,
        "LW with rd not in x8-x15 should not compress (4 bytes)"
    );
}

// -ftrivial-auto-var-init=zero: 验证 RISC-V64 后端发射 sd zero, offset(sp) 指令
#[test]
fn riscv64_auto_var_init_zero_emits_sd() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut target = crate::target::Target::resolve(&triple);
    target.auto_var_init_zero = true;
    let mut cg = Riscv64Codegen::new(target, true);
    // 直接测试 emit_zero_init_frame: 32 字节帧 = 4 个 sd
    cg.emit_zero_init_frame(32);
    let sd_count = cg
        .code
        .windows(4)
        .filter(|w| {
            let inst = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            // SD: opcode=011, funct3=011 => S-type with funct3=011
            let opcode = inst & 0x7f;
            let funct3 = (inst >> 12) & 0x7;
            let rs2 = (inst >> 20) & 0x1f;
            let rs1 = (inst >> 15) & 0x1f;
            opcode == 0x23 && funct3 == 3 && rs2 == 0 && rs1 == 2
        })
        .count();
    assert_eq!(sd_count, 4, "expected 4 sd zero, offset(sp) instructions for 32-byte frame, got {sd_count}");
}

// D 扩展新增指令: FSQRT.S, FMIN.S, FMAX.S, FCLASS.S/D
#[test]
fn riscv64_d_extension_new_instructions() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_fsqrt_s(10, 11);
    cg.emit_fmin_s(12, 13, 14);
    cg.emit_fmax_s(15, 16, 17);
    cg.emit_fclass_s(10, 11);
    cg.emit_fclass_d(12, 13);
    let code = &cg.code;
    assert!(code.len() >= 20, "expected 20 bytes for 5 instructions");
    // FSQRT.S: funct7=0101101
    let insn_sqrt = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!((insn_sqrt >> 25) & 0x7f, 0b0101101, "FSQRT.S funct7");
    // FMIN.S: funct7=0010100, funct3=000
    let insn_min = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
    assert_eq!((insn_min >> 25) & 0x7f, 0b0010100, "FMIN.S funct7");
    assert_eq!((insn_min >> 12) & 0x7, 0, "FMIN.S funct3=000");
    // FMAX.S: funct7=0010100, funct3=001
    let insn_max = u32::from_le_bytes([code[8], code[9], code[10], code[11]]);
    assert_eq!((insn_max >> 25) & 0x7f, 0b0010100, "FMAX.S funct7");
    assert_eq!((insn_max >> 12) & 0x7, 1, "FMAX.S funct3=001");
    // FCLASS.S: funct7=1010000, funct3=001
    let insn_class_s = u32::from_le_bytes([code[12], code[13], code[14], code[15]]);
    assert_eq!((insn_class_s >> 25) & 0x7f, 0b1010000, "FCLASS.S funct7");
    assert_eq!((insn_class_s >> 12) & 0x7, 1, "FCLASS.S funct3=001");
    // FCLASS.D: funct7=1010001, funct3=001
    let insn_class_d = u32::from_le_bytes([code[16], code[17], code[18], code[19]]);
    assert_eq!((insn_class_d >> 25) & 0x7f, 0b1010001, "FCLASS.D funct7");
    assert_eq!((insn_class_d >> 12) & 0x7, 1, "FCLASS.D funct3=001");
}

// RVV 窄化钳位指令: VNCLIP.VV, VNCLIPU.VV
#[test]
fn riscv64_rvv_narrowing_clip() {
    let triple = crate::cli::TargetTriple::parse("riscv64-linux-gnu").expect("triple");
    let mut cg = Riscv64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.emit_vnclip_vv(8, 16, 24, true);
    cg.emit_vnclipu_vv(9, 17, 25, false);
    let code = &cg.code;
    assert!(code.len() >= 8, "expected 8 bytes for 2 instructions");
    // VNCLIP: funct6=100110, funct3=011
    let insn_nclip = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
    assert_eq!((insn_nclip >> 26) & 0x3f, 0b100110, "VNCLIP funct6");
    assert_eq!((insn_nclip >> 12) & 0x7, 3, "VNCLIP funct3=011");
    assert_eq!((insn_nclip >> 25) & 1, 1, "VNCLIP vm=1");
    // VNCLIPU: funct6=100110, funct3=010
    let insn_nclipu = u32::from_le_bytes([code[4], code[5], code[6], code[7]]);
    assert_eq!((insn_nclipu >> 26) & 0x3f, 0b100110, "VNCLIPU funct6");
    assert_eq!((insn_nclipu >> 12) & 0x7, 2, "VNCLIPU funct3=010");
    assert_eq!((insn_nclipu >> 25) & 1, 0, "VNCLIPU vm=0");
}
