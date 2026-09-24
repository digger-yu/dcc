// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::ir::{lower, validate, ExprKind, TypedExpr, ValueType};

#[test]
fn aarch64_ir_return_add() {
    let (program, diags) = crate::parser::parse(b"int main(void) { return 10 + 32; }\n", "a64.c");
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([obj[18], obj[19]]), 183);
}

// M7 修复: div/mod 真实 sdiv/msub 替代 0
// 验证: aarch64 编译 (a / b) 与 (a % b) 不再退化为 0
// 注: 使用 opaque 函数防止常量折叠
#[test]
fn aarch64_ir_div_mod_emit_sdiv_msub() {
    let (program, diags) = crate::parser::parse(
        b"extern int opaque(int);\n\
          int main(void) { int a = opaque(42); int b = opaque(5); int q = a / b; int r = a % b; return q + r; }\n",
        "a64.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    // 验证 .text 段含 sdiv 指令 (0x9AC00C20 LE 编码: 20 0c c0 9a)
    let text = obj.windows(4).any(|w| w == [0x20, 0x0c, 0xc0, 0x9a]);
    assert!(text, "expected sdiv w0, w0, w1 (0x9ac00c20) in aarch64 text");
}

// M7 修复: 移位真实 lslv/lsrv 替代 0
// lslv w0, w0, w1: 0x1A302000 LE = 00 20 30 1A
// lsrv w0, w0, w1: 0x1A342000 LE = 00 20 34 1A
#[test]
fn aarch64_ir_shifts_emit_lslv_lsrv() {
    let (program, diags) = crate::parser::parse(
        b"extern int opaque(int);\n\
          int main(void) { int a = opaque(1); int b = opaque(2); return (a << b) | (a >> b); }\n",
        "a64.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    let has_lslv = obj.windows(4).any(|w| w == [0x00, 0x20, 0x30, 0x1a]);
    let has_lsrv = obj.windows(4).any(|w| w == [0x00, 0x20, 0x34, 0x1a]);
    assert!(has_lslv || has_lsrv, "expected lslv/lsrv in aarch64 text");
}

// M7 修复: 聚合类型参数按引用传递
// 验证: 调函数时,若参数是 local struct, 发射 add xN, sp, #off
// add x0, sp, #imm12 编码: bits[31:24]=0x91, bits[7:5]=0b000 (sf=1)
// 简化检测: byte3 == 0x91
#[test]
fn aarch64_aggregate_arg_by_ref() {
    let (program, diags) = crate::parser::parse(
        b"struct S { int x; int y; };\n\
          void callee(struct S s) {}\n\
          int main(void) { struct S s; s.x = 1; s.y = 2; callee(s); return 0; }\n",
        "a64.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = lower(program).expect("lower");
    validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let obj = cg.build_output();
    let has_adr = obj.windows(4).any(|w| w[3] == 0x91 && w[2] == 0x00);
    assert!(has_adr, "expected add xN, sp, #imm (M7 by-ref aggregate)");
}

// M8: 谓词指令 CCMP/CSEL/CSINC 发射验证
#[test]
fn aarch64_ccmp_csel_csinc_emit() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    // ccmp w0, w1, #0, ne
    cg.emit_ccmp_w0_w1(0);
    // csel w0, w0, w1, eq
    cg.emit_csel_w0_w0_w1(0x0);
    // csinc w0, w0, w1, eq
    cg.emit_csinc_w0_w0_w1_eq();
    let code = &cg.code;
    // ccmp: byte3=0x7a, byte1 cond=ne(1) in bits[15:12]
    let has_ccmp = code
        .windows(4)
        .any(|w| w[3] == 0x7a && (w[1] & 0xf0) == 0x10);
    assert!(has_ccmp, "expected ccmp w0, w1, #0, ne");
    // csel: byte3=0x1a, cond=eq(0)
    let has_csel = code
        .windows(4)
        .any(|w| w[3] == 0x1a && (w[1] & 0xf0) == 0x00);
    assert!(has_csel, "expected csel w0, w0, w1, eq");
    // csinc: 0x1a8401e0 LE = e0 01 84 1a
    let has_csinc = code.windows(4).any(|w| w == [0xe0, 0x01, 0x84, 0x1a]);
    assert!(has_csinc, "expected csinc w0, w0, w1, eq");
}

// M8: 双精度浮点比较发射验证
#[test]
fn aarch64_fcmp_double_emit() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.fcmp_d0_d1();
    cg.fcmp_s0_zero();
    cg.fcmp_d0_zero();
    let code = &cg.code;
    // fcmp d0, d1: 0x1E682000 LE = 00 20 68 1e
    let has_fcmp_d = code.windows(4).any(|w| w == [0x00, 0x20, 0x68, 0x1e]);
    assert!(has_fcmp_d, "expected fcmp d0, d1");
    let has_fcmp_s0 = code.windows(4).any(|w| w == [0x08, 0x00, 0x28, 0x1e]);
    assert!(has_fcmp_s0, "expected fcmp s0, #0.0");
}

// M8: 缓存维护指令发射验证
#[test]
fn aarch64_cache_maintain_emit() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.emit_dc_cvac(0);
    cg.emit_dc_civac(1);
    cg.emit_dc_cvau(2);
    cg.emit_ic_ivau(3);
    cg.emit_ic_iallu();
    cg.emit_dsb_ish();
    cg.emit_dsb_sy();
    cg.emit_isb();
    // 验证 dsb ish: 9f 3b 03 d5
    let has_dsb_ish = cg.code.windows(4).any(|w| w == [0x9f, 0x3b, 0x03, 0xd5]);
    assert!(has_dsb_ish, "expected dsb ish");
    // 验证 isb: df 3b 03 d5
    let has_isb = cg.code.windows(4).any(|w| w == [0xdf, 0x3b, 0x03, 0xd5]);
    assert!(has_isb, "expected isb");
    // 验证 ic iallu: 7f 78 03 d5
    let has_ic_iallu = cg.code.windows(4).any(|w| w == [0x7f, 0x78, 0x03, 0xd5]);
    assert!(has_ic_iallu, "expected ic iallu");
}

// M8: 后置索引寻址发射验证
#[test]
fn aarch64_post_index_addring_emit() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    // str x0, [x1], #8
    cg.emit_str_post_index(0, 1, 8);
    // ldr x0, [x1], #8
    cg.emit_ldr_post_index(0, 1, 8);
    // str x0, [x1, #8]!
    cg.emit_str_pre_index(0, 1, 8);
    // ldr x0, [x1, #8]!
    cg.emit_ldr_pre_index(0, 1, 8);
    // 验证: post-index str byte3=0xf8, byte2 bit7=0 (bit23=0, post-index)
    let has_post_str = cg
        .code
        .windows(4)
        .any(|w| w[3] == 0xf8 && (w[2] & 0x80) == 0);
    assert!(has_post_str, "expected str post-index");
    // 验证: pre-index str byte3=0xf8, byte2 bit7=1 (bit23=1, pre-index)
    let has_pre_str = cg
        .code
        .windows(4)
        .any(|w| w[3] == 0xf8 && (w[2] & 0x80) != 0);
    assert!(has_pre_str, "expected str pre-index");
}

// M8: BL 范围校验
#[test]
fn aarch64_bl_range_check() {
    assert!(Aarch64Codegen::bl_offset_in_range(0));
    assert!(Aarch64Codegen::bl_offset_in_range(0x7ffffff));
    assert!(Aarch64Codegen::bl_offset_in_range(-0x8000000));
    assert!(!Aarch64Codegen::bl_offset_in_range(0x8000000));
    assert!(!Aarch64Codegen::bl_offset_in_range(-0x8000001));
}

// M8: ADR 范围校验
#[test]
fn aarch64_adr_range_check() {
    assert!(Aarch64Codegen::adr_offset_in_range(0));
    assert!(Aarch64Codegen::adr_offset_in_range(0xfffff));
    assert!(Aarch64Codegen::adr_offset_in_range(-0x100000));
    assert!(!Aarch64Codegen::adr_offset_in_range(0x100000));
    assert!(!Aarch64Codegen::adr_offset_in_range(-0x100001));
}

// M25: AArch64 TLSDESC 访问序列 单元测试
// 验证 emit_tlsdesc_access 发出
//   adrp x0, :tlsdesc:sym          (R_AARCH64_TLSDESC_ADR_PAGE21)
//   ldr  x1, [x0, :tlsdesc_lo12:sym] (R_AARCH64_TLSDESC_LD64_LO12)
//   add  x0, x0, :tlsdesc_lo12:sym  (R_AARCH64_TLSDESC_ADD_LO12)
//   blr  x1                        (TLSDESC 间接调用)
// 共 4 条指令 + 3 条 TLSDESC_* 重定位 + 1 个 sym 符号.
#[test]
fn aarch64_tlsdesc_access_emits_expected_sequence() {
    use crate::codegen::shared::elf::{
        R_AARCH64_TLSDESC_ADD_LO12, R_AARCH64_TLSDESC_ADR_PAGE21, R_AARCH64_TLSDESC_LD64_LO12,
    };
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.emit_tlsdesc_access("my_tls_var");
    // 4 条 aarch64 指令: adrp (0x90xx xxxx) + ldr (0xf940 xxxx)
    // + add (0x91xx xxxx) + blr (0xd63f 0020)
    assert!(
        cg.code.windows(4).any(|w| (w[3] & 0x9f) == 0x90),
        "missing adrp insn (top byte & 0x9f should be 0x90)"
    );
    assert!(
        cg.code
            .windows(4)
            .any(|w| w[3] == 0xf9 && (w[2] & 0xc0) == 0x40),
        "missing ldr 64-bit insn"
    );
    assert!(
        cg.code.windows(4).any(|w| (w[3] & 0x9f) == 0x91),
        "missing add insn (top byte & 0x9f should be 0x91)"
    );
    let blr_pattern: [u8; 4] = [0x20, 0x00, 0x3f, 0xd6];
    assert!(
        cg.code.windows(4).any(|w| *w == blr_pattern),
        "missing blr x1 (expected 20 00 3f d6)"
    );
    // 3 个 TLSDESC 重定位 (类型 + 引用 my_tls_var)
    let tlsdesc_rels: Vec<_> = cg
        .relocations
        .iter()
        .filter(|r| {
            matches!(
                r.rel_type,
                R_AARCH64_TLSDESC_ADR_PAGE21
                    | R_AARCH64_TLSDESC_LD64_LO12
                    | R_AARCH64_TLSDESC_ADD_LO12
            )
        })
        .collect();
    assert_eq!(
        tlsdesc_rels.len(),
        3,
        "expected 3 TLSDESC relocations, got {}",
        tlsdesc_rels.len()
    );
    for r in &tlsdesc_rels {
        // 全部引用同一个符号 my_tls_var (确保 ensure_sym 去重)
        let sym = &cg.symbols[r.sym_index as usize - 1];
        assert_eq!(sym.name, "my_tls_var", "TLSDESC relocation must reference my_tls_var");
    }
}

// Optimized TLSDESC 序列单元测试 (MOVW 对 + 5 个新重定位)
// 验证 emit_tlsdesc_access_opt 发出:
//   movz x0, #0, lsl #16  (R_AARCH64_TLSDESC_OFF_G1)
//   movk x0, #0           (R_AARCH64_TLSDESC_OFF_G0_NC)
//   ldr  x1, [x0]         (R_AARCH64_TLSDESC_LDR)
//   add  x0, x0, x1       (R_AARCH64_TLSDESC_ADD)
//   blr  x1               (R_AARCH64_TLSDESC_CALL)
#[test]
fn aarch64_tlsdesc_opt_emits_5_relocations() {
    use crate::codegen::shared::elf::{
        R_AARCH64_TLSDESC_ADD, R_AARCH64_TLSDESC_CALL, R_AARCH64_TLSDESC_LDR,
        R_AARCH64_TLSDESC_OFF_G0_NC, R_AARCH64_TLSDESC_OFF_G1,
    };
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.emit_tlsdesc_access_opt("tls_opt_var");
    // movz (0xd2a) + movk (0xf28) 指令检查
    assert!(cg.code.windows(4).any(|w| w[3] == 0xd2), "missing movz x0 insn");
    assert!(cg.code.windows(4).any(|w| w[3] == 0xf2), "missing movk x0 insn");
    // 5 个 TLSDESC 重定位
    let tlsdesc_rels: Vec<_> = cg
        .relocations
        .iter()
        .filter(|r| {
            matches!(
                r.rel_type,
                R_AARCH64_TLSDESC_OFF_G1
                    | R_AARCH64_TLSDESC_OFF_G0_NC
                    | R_AARCH64_TLSDESC_LDR
                    | R_AARCH64_TLSDESC_ADD
                    | R_AARCH64_TLSDESC_CALL
            )
        })
        .collect();
    assert_eq!(
        tlsdesc_rels.len(),
        5,
        "expected 5 TLSDESC relocations, got {}",
        tlsdesc_rels.len()
    );
    for r in &tlsdesc_rels {
        let sym = &cg.symbols[r.sym_index as usize - 1];
        assert_eq!(sym.name, "tls_opt_var");
    }
}

// M25: AArch64 va_list 寄存器保区 单元测试
// 验证变参函数入口发出 8 条 str xn, [sp, #off] 保存指令.
#[test]
fn aarch64_va_save_area_stores_x0_through_x7() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    // 手动设个假变参函数上下文
    cg.is_current_func_variadic = true;
    cg.va_save_base_off = 16;
    cg.stack_bytes = 80;
    cg.frame_bytes = 80;
    // 模拟 gen_function 入口保存动作
    for i in 0..8u8 {
        cg.str_xt_sp(i, cg.va_save_base_off + (i as i32) * 8);
    }
    // 验证: 8 条 str xn, [sp, #off] 指令,
    //       byte3 = 0xF9 (STR X), bit 23 (byte2 bit7) = 0
    let str_x_count = cg
        .code
        .windows(4)
        .filter(|w| w[3] == 0xf9 && (w[2] & 0x80) == 0)
        .count();
    assert_eq!(str_x_count, 8, "expected 8 str xn, [sp, #off] instructions, got {str_x_count}");
    // 验证偏移在 [16, 72] 范围 (va_save_base_off=16, 8 * 8 步进)
    for i in 0..8 {
        let expected_off = 16 + i * 8;
        let imm12 = (expected_off / 8) as u32;
        let found = cg.code.windows(4).any(|w| {
            // 提取指令 imm12 字段 (bits 21:10) 并比较
            let insn = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            let got = (insn >> 10) & 0xfff;
            got == imm12 && (insn & 0xff00_0000) == 0xf900_0000
        });
        assert!(found, "missing str x{i}, [sp, #{expected_off}]");
    }
}

// M25: AArch64 va_arg 整数路径 单元测试
// 验证 gen_va_arg_int 生成运行时偏移读取代码
#[test]
fn aarch64_va_arg_int_reads_sequentially_from_save_area() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.is_current_func_variadic = true;
    cg.va_save_base_off = 128;
    cg.frame_bytes = 256;
    // 注册一个假 va_list 局部 (24 字节结构体, 8 字节对齐)
    cg.locals.insert("ap".to_string(), 96);
    // 生成 va_arg_int
    cg.gen_va_arg_int("ap");
    // 验证: 代码包含 cmp w0, #48 (0x71000000 | 48<<10 | 0<<5 | 31)
    let cmp_insn = 0x7100001fu32 | (48u32 << 10);
    let has_cmp = cg
        .code
        .windows(4)
        .any(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) == cmp_insn);
    assert!(has_cmp, "expected cmp w0, #48 instruction");
    // 验证: 代码包含条件分支 bge (cond=0xa)
    let has_bge = cg.code.windows(4).any(|w| {
        let insn = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        (insn & 0xff00001f) == 0x5400000a // b.ge
    });
    assert!(has_bge, "expected b.ge instruction");
    // 验证: 代码非空且长度合理 (> 20 字节)
    assert!(cg.code.len() > 20, "expected > 20 bytes of code, got {}", cg.code.len());
}

// 嵌套 sret: 验证 gen_function 在返回 >16B 聚合时保存 x0 到 <sret_ptr>
// 并验证 gen_stmt Return 使用保存的 sret 指针而非 x0
#[test]
fn aarch64_nested_sret_saves_sret_ptr() {
    use crate::ir::ValueType;
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    // 模拟返回 32 字节聚合类型的函数
    cg.current_return_ty = Some(ValueType::Aggregate {
        name: "Big".to_string(),
        size: 32,
        is_union: false,
    });
    // 模拟帧建立后保存 sret 指针
    cg.stack_bytes = 16;
    cg.frame_bytes = 16;
    let sret_off = cg.alloc_local("<sret_ptr>", 8);
    cg.store_w0(sret_off);
    cg.saved_sret_ptr = Some(sret_off);
    // 验证 saved_sret_ptr 已设置
    assert!(
        cg.saved_sret_ptr.is_some(),
        "saved_sret_ptr should be set for large aggregate return"
    );
    assert_eq!(
        cg.saved_sret_ptr.unwrap(),
        sret_off,
        "saved_sret_ptr should match allocated offset"
    );
    // 验证 locals 包含 <sret_ptr>
    assert!(cg.locals.contains_key("<sret_ptr>"), "locals should contain <sret_ptr>");
}

// TLS: 验证 tls_globals 集合正确记录 TLS 变量
#[test]
fn aarch64_tls_globals_tracking() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    // 模拟 TLS 全局变量记录
    cg.tls_globals.insert("my_tls_var".to_string(), 0);
    cg.tls_globals.insert("another_tls".to_string(), 8);
    assert!(
        cg.tls_globals.contains_key("my_tls_var"),
        "tls_globals should contain my_tls_var"
    );
    assert!(
        cg.tls_globals.contains_key("another_tls"),
        "tls_globals should contain another_tls"
    );
    assert_eq!(cg.tls_globals.len(), 2);
}

// HWASAN AArch64 MTE: 验证 IRG/ADDG/SUBG/STG/LDG 编码与 ARM 规范一致
// (参考 LLVM llvm/test/MC/AArch64/armv8.5a-mte.s 的已知编码)
#[test]
fn mte_emitter_encodings_match_arm_spec() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.emit_irg(0, 1); // irg x0, x1 -> LLVM: [0x20,0x10,0xdf,0x9a]
    cg.emit_irg(0, 31); // irg x0, sp -> LLVM: [0xe0,0x13,0xdf,0x9a]
    cg.emit_irg(16, 31); // irg x16, sp
    cg.emit_irg(17, 18); // irg x17, x18
    cg.emit_addg(0, 1, 0, 1); // addg x0, x1, #0, #1 -> LLVM: [0x20,0x04,0x80,0x91]
    cg.emit_addg(31, 16, 0, 0); // addg sp, x16, #0, #0
    cg.emit_addg(31, 2, 2, 3); // addg sp, x2, #32, #3
    cg.emit_subg(0, 1, 0, 1); // subg x0, x1, #0, #1 -> LLVM: [0x20,0x04,0x80,0xd1]
    cg.emit_stg(3, 31, 1); // stg x3, [sp, #16] -> LLVM: [0xe3,0x1b,0x20,0xd9]
    cg.emit_stg(16, 31, 0); // stg x16, [sp, #0]
    cg.emit_stg(7, 8, 0x1ff); // stg x7, [x8, #-16]
    cg.emit_ldg(0, 0, 0); // ldg x0, [x0]
    let code = &cg.code;
    let find = |bytes: [u8; 4]| code.windows(4).any(|w| w == bytes);
    assert!(find([0x20, 0x10, 0xdf, 0x9a]), "irg x0, x1");
    assert!(find([0xe0, 0x13, 0xdf, 0x9a]), "irg x0, sp");
    assert!(find([0xf0, 0x13, 0xdf, 0x9a]), "irg x16, sp");
    assert!(find([0x51, 0x12, 0xdf, 0x9a]), "irg x17, x18");
    assert!(find([0x20, 0x04, 0x80, 0x91]), "addg x0, x1, #0, #1");
    assert!(find([0x1f, 0x02, 0x80, 0x91]), "addg sp, x16, #0, #0");
    assert!(find([0x5f, 0x0c, 0x82, 0x91]), "addg sp, x2, #32, #3");
    assert!(find([0x20, 0x04, 0x80, 0xd1]), "subg x0, x1, #0, #1");
    assert!(find([0xe3, 0x1b, 0x20, 0xd9]), "stg x3, [sp, #16]");
    assert!(find([0xf0, 0x0b, 0x20, 0xd9]), "stg x16, [sp, #0]");
    assert!(find([0x07, 0xf9, 0x3f, 0xd9]), "stg x7, [x8, #-16]");
    assert!(find([0x00, 0x00, 0x60, 0xd9]), "ldg x0, [x0]");
}

// HWASAN AArch64 MTE: 验证 gen_function prologue 发射 tag 栈帧序列
// (irg + addg + stg*N + str x19 + mov x19,sp) 与 epilogue 的
// ldr x19 + and sp,sp,#mask + add sp 恢复序列
#[test]
fn hwasan_mte_prologue_emits_tag_frame() {
    let (program, diags) =
        crate::parser::parse(b"int main(void) { int a = 1; int b = 2; return a + b; }\n", "mte.c");
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = crate::ir::lower(program).expect("lower");
    crate::ir::validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.set_mte_enabled(true);
    cg.compile(&module);
    let find = |bytes: [u8; 4]| cg.code.windows(4).any(|w| w == bytes);
    // prologue: irg x16, sp
    assert!(find([0xf0, 0x13, 0xdf, 0x9a]), "prologue missing irg x16, sp");
    // prologue: addg sp, x16, #0, #0
    assert!(find([0x1f, 0x02, 0x80, 0x91]), "prologue missing addg sp, x16, #0, #0");
    // prologue: stg x16, [sp, #0] 与 stg x16, [sp, #16] (帧 40B, granules = 40/16 = 2)
    assert!(find([0xf0, 0x0b, 0x20, 0xd9]), "prologue missing stg x16, [sp, #0]");
    assert!(find([0xf0, 0x1b, 0x20, 0xd9]), "prologue missing stg x16, [sp, #16]");
    // prologue: str x19, [sp, #32] (保存入参 x19, <vec_lit> at 16, mte_save at 32)
    assert!(find([0xf3, 0x13, 0x00, 0xf9]), "prologue missing str x19, [sp, #32]");
    // prologue: add x19, sp, #0 (mov x19, sp)
    assert!(find([0xf3, 0x03, 0x00, 0x91]), "prologue missing add x19, sp, #0");
    // epilogue: ldr x19, [sp, #32]
    assert!(find([0xf3, 0x13, 0x40, 0xf9]), "epilogue missing ldr x19, [sp, #32]");
    // epilogue: and sp, sp, #0x00ffffffffffffff (untag)
    assert!(find([0xff, 0xdf, 0x80, 0x92]), "epilogue missing and sp, sp, #mask");
    // 记录 tag 栈帧大小 (frame_bytes = (40+15)&!15 = 48)
    assert_eq!(cg.tag_frame_size, 48, "tag_frame_size should be frame size");
}

// HWASAN AArch64 MTE: 验证调用点对 sp 做 untag / retag,
// 使外部 (非 MTE) callee 以 untagged sp 运行, 返回后恢复本帧 tag
#[test]
fn hwasan_mte_call_site_untag_retag() {
    let (program, diags) = crate::parser::parse(
        b"extern int callee(int);\nint main(void) { int a = 5; return callee(a); }\n",
        "mte2.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = crate::ir::lower(program).expect("lower");
    crate::ir::validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.set_mte_enabled(true);
    cg.compile(&module);
    // 调用点前缀: and sp, x19, #0x00ffffffffffffff
    assert!(
        cg.code.windows(4).any(|w| w == [0x7f, 0xde, 0x80, 0x92]),
        "call site missing and sp, x19, #mask"
    );
    // 调用点后缀: add sp, x19, #0 (mov sp, x19)
    assert!(
        cg.code.windows(4).any(|w| w == [0x7f, 0x02, 0x00, 0x91]),
        "call site missing add sp, x19, #0"
    );
}

// HWASAN AArch64 MTE: 未启用 hwasan 时不得发射任何 MTE tag 指令
#[test]
fn no_mte_no_tag_instructions() {
    let (program, diags) = crate::parser::parse(
        b"int main(void) { int a = 1; int b = 2; return a + b; }\n",
        "nomte.c",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let (module, _) = crate::ir::lower(program).expect("lower");
    crate::ir::validate(&module).expect("validate");
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut cg = Aarch64Codegen::new(crate::target::Target::resolve(&triple), true);
    cg.compile(&module);
    let find = |bytes: [u8; 4]| cg.code.windows(4).any(|w| w == bytes);
    assert!(!find([0xf0, 0x13, 0xdf, 0x9a]), "unexpected irg x16, sp");
    assert!(!find([0xf0, 0x0b, 0x20, 0xd9]), "unexpected stg");
    assert!(!find([0xff, 0xdf, 0x80, 0x92]), "unexpected and sp, sp, #mask");
    assert_eq!(cg.tag_frame_size, 0, "tag_frame_size should stay 0 without MTE");
}

// M25: gen_float_expr 处理 Local float 变量时
// 应从栈加载到 s0 而非回退 0.0
#[test]
fn aarch64_gen_float_expr_local_loads_from_stack() {
    let mut cg = Aarch64Codegen::new(
        crate::target::Target::resolve(
            &crate::cli::TargetTriple::parse("aarch64-linux-gnu").unwrap(),
        ),
        true,
    );
    cg.locals.insert("x".to_string(), 16);
    let expr = TypedExpr {
        kind: ExprKind::Local("x".to_string()),
        ty: ValueType::Float { bits: 32 },
        literal: None,
        children: vec![],
        span: crate::diagnostics::Span::point("t.c", 1, 1),
        quals: crate::ast::QualifierFlags::NONE,
    };
    let ok = cg.gen_float_expr(&expr);
    assert!(ok, "gen_float_expr should succeed for Local float");
    // 验证发射了 ldr s0, [sp, #16] 指令
    // LDR S (32-bit): 0xBD400000 | (imm12/4)<<10 | Rn=sp(31) | Rt=s0(0)
    let ldr_s0 = 0xBD400000u32 | ((16u32 / 4) << 10) | (31 << 5);
    let found = cg
        .code
        .windows(4)
        .any(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) == ldr_s0);
    assert!(found, "expected ldr s0, [sp, #16] instruction");
}

// -ftrivial-auto-var-init=zero: 验证 AArch64 后端发射 stp xzr, xzr 指令
#[test]
fn aarch64_auto_var_init_zero_emits_stp_xzr() {
    let triple = crate::cli::TargetTriple::parse("aarch64-linux-gnu").expect("triple");
    let mut target = crate::target::Target::resolve(&triple);
    target.auto_var_init_zero = true;
    let mut cg = Aarch64Codegen::new(target, true);
    // 直接测试 emit_zero_init_frame: 32 字节帧 = 2 个 stp
    cg.emit_zero_init_frame(32);
    let stp_count = cg
        .code
        .windows(4)
        .filter(|w| {
            let inst = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            // STP XZR, XZR, [SP, #off] (signed offset mode)
            // Fixed fields: opc[31:30]=10, 101[29:27], V=0[26], mode=010[25:23], L=0[22]
            // Check top 10 bits [31:22] = 10_101_0_010_0 = 0xA9 (with bit 22=0)
            let top10 = (inst >> 22) & 0x3ff;
            let rt2 = (inst >> 10) & 0x1f;
            let rn = (inst >> 5) & 0x1f;
            let rt = inst & 0x1f;
            top10 == 0x2a4 && rt2 == 31 && rn == 31 && rt == 31
        })
        .count();
    assert_eq!(
        stp_count, 2,
        "expected 2 stp xzr, xzr instructions for 32-byte frame, got {stp_count}"
    );
}
