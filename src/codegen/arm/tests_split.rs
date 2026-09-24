// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use super::*;
use crate::ir::{Function, ValueType};
use crate::target::Target;

#[test]
fn arm_push_encoding() {
    let mut cg = ArmCodegen::new(
        Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap()),
        true,
    );
    cg.emit_push(vec![11, 14]);
    assert_eq!(cg.code.len(), 4);
    // push {fp, lr} => 0xE92D4800
    let inst = u32::from_le_bytes(cg.code[0..4].try_into().unwrap());
    assert_eq!(inst, 0xE92D4800);
}

#[test]
fn arm_mov_encoding() {
    let mut cg = ArmCodegen::new(
        Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap()),
        true,
    );
    cg.emit_mov_reg(0, 0);
    // mov r0, r0 => 0xE1A00000
    let inst = u32::from_le_bytes(cg.code[0..4].try_into().unwrap());
    assert_eq!(inst, 0xE1A00000);
}

#[test]
fn arm_pop_encoding() {
    let mut cg = ArmCodegen::new(
        Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap()),
        true,
    );
    cg.emit_pop(vec![11, 15]);
    // pop {fp, pc} => 0xE8BD8800
    let inst = u32::from_le_bytes(cg.code[0..4].try_into().unwrap());
    assert_eq!(inst, 0xE8BD8800);
}

#[test]
fn arm_gen_simple_return() {
    let target =
        Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap());
    let mut cg = ArmCodegen::new(target, true);
    let func = Function {
        name: "f".to_string(),
        return_type: ValueType::Integer {
            bits: 32,
            signed: true,
        },
        params: vec![],
        body: vec![],
        span: crate::diagnostics::Span::point("t.c", 1, 1),
    };
    cg.gen_function(&func, None);
    // Should emit prologue + mov r0, #0 + epilogue
    assert!(cg.code.len() > 12);
    assert!(cg.labels.contains_key("f"));
}

// M25: ARM va_arg 整数路径单元测试
// 验证 gen_va_arg_int 从保存区顺序读 r0
#[test]
fn arm_va_arg_int_reads_sequentially_from_save_area() {
    let mut cg = ArmCodegen::new(
        Target::resolve(
            &crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap(),
        ),
        true,
    );
    cg.is_current_func_variadic = true;
    cg.va_save_base_off = 0;
    cg.va_gp_offs = 0;
    cg.frame_bytes = 16;
    // 读 3 个变参
    cg.gen_va_arg_int("ap");
    cg.gen_va_arg_int("ap");
    cg.gen_va_arg_int("ap");
    // 验证: 3 条 ldr r0, [fp, #off] 指令
    // ARM LDR rd=r0, [fp=r11, #imm12]: E59B 0000 | imm12
    let ldr_count = cg
        .code
        .windows(4)
        .filter(|w| {
            let inst = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            // 匹配高 20 位: cond=E, P=1,U=1,B=0,W=0,L=1, Rn=fp(11), Rd=r0(0)
            (inst & 0xFFFFF000) == 0xE59B0000
        })
        .count();
    assert_eq!(
        ldr_count, 3,
        "expected 3 ldr r0, [fp, #off] instructions, got {ldr_count}"
    );
    // 验证 va_gp_offs 推进
    assert_eq!(cg.va_gp_offs, 3, "va_gp_offs should be 3 after 3 reads");
}

// -ftrivial-auto-var-init=zero: 验证 ARM 后端发射 str r0, [fp, #off] 指令
#[test]
fn arm_auto_var_init_zero_emits_str() {
    let mut target =
        Target::resolve(&crate::cli::TargetTriple::parse("arm-linux-gnueabihf").unwrap());
    target.auto_var_init_zero = true;
    let mut cg = ArmCodegen::new(target, true);
    // 直接测试 emit_zero_init_frame: 16 字节帧 = 4 个 str (4 字节每个)
    cg.emit_zero_init_frame(16);
    // 验证: push r0 + mov r0, #0 + 4x str r0, [fp, #off] + pop r0
    // STR rd=r0, [fp=r11, #imm12]: E58B 0000 | imm12
    let str_count = cg
        .code
        .windows(4)
        .filter(|w| {
            let inst = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            // 匹配 STR r0, [fp, #imm12]: cond=E, P=1, U=1, B=0, W=0, L=0
            // Rn=fp(11), Rd=r0(0)
            (inst & 0xFFFFF000) == 0xE58B0000
        })
        .count();
    assert_eq!(
        str_count, 4,
        "expected 4 str r0, [fp, #off] instructions for 16-byte frame, got {str_count}"
    );
}
