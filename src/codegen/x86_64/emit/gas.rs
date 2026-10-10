// src/codegen/x86_64/emit/gas.rs
//
// P9 方案 C 真闭环: 发射点携带的 GAS 文本构造辅助.
//
// 这个模块只做一件事: 给定寄存器号/立即数/偏移, 拼出 **与 disassemble_one_at
// 输出完全一致** 的 AT&T GAS 行. 渲染器直接拿这行输出, 不再反推字节.
//
// 设计纪律:
//   * 与 render_one.rs 内的 REG64/REG32/... 表**逐条对齐** (值必须相同).
//     两处都是硬编码字面量, 不对拍就是两份真相 —— 这里的表只服务发射点,
//     render_one.rs 的表服务渲染期反推 (fallback 路径), 二者不能因为一份改了
//     另一份没改而漂移.
//   * 不做任何编码决策: 寄存器号的 %r13 vs %r13b 由调用方按宽度显式传入,
//     本模块只负责「把号 + 宽度 → 名字」和「按 GAS 语序拼字符串」.
//   * 输出不含前导 tab / 不含末尾换行, 与 Insn::gas_text 字段语义一致.

/// 64 位 GP 寄存器名 (AT&T, 带 %). 索引 = 物理寄存器号 0..15.
/// 与 render_one.rs::REG64 逐条对拍.
pub(crate) const REG64: [&str; 16] = [
    "%rax", "%rcx", "%rdx", "%rbx", "%rsp", "%rbp", "%rsi", "%rdi", "%r8", "%r9", "%r10",
    "%r11", "%r12", "%r13", "%r14", "%r15",
];

/// 32 位 GP 寄存器名. 与 render_one.rs::REG32 对拍.
pub(crate) const REG32: [&str; 16] = [
    "%eax", "%ecx", "%edx", "%ebx", "%esp", "%ebp", "%esi", "%edi", "%r8d", "%r9d", "%r10d",
    "%r11d", "%r12d", "%r13d", "%r14d", "%r15d",
];

/// 16 位 GP 寄存器名.
pub(crate) const REG16: [&str; 16] = [
    "%ax", "%cx", "%dx", "%bx", "%sp", "%bp", "%si", "%di", "%r8w", "%r9w", "%r10w", "%r11w",
    "%r12w", "%r13w", "%r14w", "%r15w",
];

/// 8 位低半字节 GP 寄存器名 (含 %spl/%bpl/%sil/%dil; 只在有 REX 前缀时合法).
/// 高半字节 (%ah/%ch/%dh/%bh) 不由号反查, 所以不进这张表.
pub(crate) const REG8: [&str; 16] = [
    "%al", "%cl", "%dl", "%bl", "%spl", "%bpl", "%sil", "%dil", "%r8b", "%r9b", "%r10b",
    "%r11b", "%r12b", "%r13b", "%r14b", "%r15b",
];

/// XMM 寄存器名.
pub(crate) const XMM: [&str; 16] = [
    "%xmm0", "%xmm1", "%xmm2", "%xmm3", "%xmm4", "%xmm5", "%xmm6", "%xmm7", "%xmm8",
    "%xmm9", "%xmm10", "%xmm11", "%xmm12", "%xmm13", "%xmm14", "%xmm15",
];

/// 按宽度 (1/2/4/8 字节) 取 64/32/16/8 位 GP 名.
pub(crate) fn gp(reg: u8, sz: usize) -> &'static str {
    match sz {
        1 => &REG8[(reg & 15) as usize],
        2 => &REG16[(reg & 15) as usize],
        4 => &REG32[(reg & 15) as usize],
        _ => &REG64[(reg & 15) as usize],
    }
}

/// 立即数字面量: GAS 用 `$0x..` 或 `$..`. 与 render_one.rs 输出对拍选择 hex 还是 decimal.
///
/// 观察 baseline (float.c.s / ld_global_read.c.s):
///   `sub $976, %rsp` (decimal)
///   `add $8, %rsp`   (decimal)
///   `mov $0, %rax`   (decimal)
///   `mov $1, %rax`   (decimal)
/// 所以对于栈偏移 / 小立即数, 一律 decimal.
pub(crate) fn dec_imm(v: i64) -> String {
    format!("${v}")
}

/// 内存操作数: `disp(%base)` 形态. disp=0 时 GAS 仍写 `(%base)` (不丢括号).
pub(crate) fn mem_disp(base: u8, disp: i64) -> String {
    let base_name = REG64[(base & 15) as usize];
    if disp == 0 {
        format!("({base_name})")
    } else {
        format!("{disp}({base_name})")
    }
}

/// 按 (modrm_reg_field, base_reg) 手工拼出 [base+disp] 的 ModRM+disp 字节序列.
/// 返回 (modrm_byte, disp_bytes) —— 调用方自己拼前缀/Rex. 只服务 disp8/disp32 两路.
///
/// 约定: base_reg 用物理号 (0..15), reg_field 即 ModRM.reg 字段 (低 3 位 +
/// 来自 REX.R 的高位). base_reg==13 (%r13) 在 mod=0 时需要 disp32 (不是 (r13) 裸形式).
pub(crate) fn modrm_disp_bytes(reg_field: u8, base: u8, disp: i64) -> (u8, Vec<u8>) {
    let rm = base & 7;
    if disp == 0 && rm != 5 && rm != 4 {
        // mod=00, rm=base: 无 disp. 注意 rm==5 (rbp/r13) 或 rm==4 (rsp/r12 SIB)
        // 在 mod=00 下不是「无 disp」, 分别要 disp32 / SIB. 调用方应保证不传这俩.
        (reg_field << 3 | rm, Vec::new())
    } else if (-128..=127).contains(&disp) {
        // mod=01, disp8
        (0x40 | (reg_field << 3) | rm, vec![disp as i8 as u8])
    } else {
        // mod=10, disp32
        (0x80 | (reg_field << 3) | rm, (disp as i32).to_le_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_tables_match_render_one() {
        // 这张表必须与 render_one.rs 内的字面量完全一致. 不对拍就是两份真相.
        assert_eq!(REG64[13], "%r13");
        assert_eq!(REG64[4], "%rsp");
        assert_eq!(REG32[0], "%eax");
        assert_eq!(REG8[6], "%sil");
        assert_eq!(XMM[0], "%xmm0");
    }

    #[test]
    fn mem_format() {
        assert_eq!(mem_disp(13, -64), "-64(%r13)");
        assert_eq!(mem_disp(13, 0), "(%r13)");
        assert_eq!(mem_disp(4, 8), "8(%rsp)");
    }

    #[test]
    fn imm_format() {
        assert_eq!(dec_imm(976), "$976");
        assert_eq!(dec_imm(-8), "$-8");
    }
}
