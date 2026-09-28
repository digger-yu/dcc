/* asm 模板修饰符测试: %a / %P / %l 完整语义 + 立即数优先级
 *
 * 注意: %P / %a 对立即数/寄存器输出无 AT&T 前缀的 raw 形式,
 * 在 mini-asm 中通常不能直接作为指令操作数。
 * 这些测试验证: (1) 编译器不崩溃, (2) 模板替换产生正确文本。
 * 对可执行测试使用标准 %N 形式和 mini-asm 支持的指令。
 */

/* 基本 %N 替换: 寄存器约束 */
int test_basic_reg_subst(void) {
    int val = 7, result;
    asm("mov %1, %0"
        : "=r"(result)
        : "r"(val));
    return result;  /* 期望: 7 */
}

/* 立即数约束: const_eval 折叠后输出 $N */
int test_imm_consteval(void) {
    int result;
    asm("mov %1, %0"
        : "=r"(result)
        : "i"(5 + 3));
    return result;  /* 期望: 8 */
}

/* %b 修饰符: 8 位子寄存器 (通过 mov 验证替换) */
int test_b_modifier(void) {
    int val = 42, result;
    /* %b1 产生 8 位寄存器名; mov 在同寄存器不同宽度间有效 */
    asm("mov %b1, %b0"
        : "=r"(result)
        : "r"(val));
    return result & 0xFF;  /* 期望: 42 */
}

/* %w 修饰符: 16 位子寄存器 */
int test_w_modifier(void) {
    int val = 0x1234, result;
    asm("mov %w1, %w0"
        : "=r"(result)
        : "r"(val));
    return result & 0xFFFF;  /* 期望: 0x1234 */
}

/* %k 修饰符: 32 位子寄存器 */
int test_k_modifier(void) {
    int val = 99, result;
    asm("mov %k1, %k0"
        : "=r"(result)
        : "r"(val));
    return result;  /* 期望: 99 */
}

/* %q 修饰符: 64 位全寄存器 */
int test_q_modifier(void) {
    int val = 42, result;
    asm("mov %q1, %q0"
        : "=r"(result)
        : "r"(val));
    return result;  /* 期望: 42 */
}

/* %a 修饰符: 对寄存器与无修饰符一致 (同名) */
int test_a_modifier_reg(void) {
    int val = 7, result;
    /* %a1 对寄存器: 保持同名, 等价于 %1 */
    asm("mov %a1, %0"
        : "=r"(result)
        : "r"(val));
    return result;  /* 期望: 7 */
}

/* 命名操作数: 基本替换 */
int test_named_operands(void) {
    int src = 99, r1, r2;
    asm("mov %[src], %[out1]\n\t"
        "mov %[src], %[out2]"
        : [out1] "=r"(r1), [out2] "=r"(r2)
        : [src] "r"(src));
    return r1 == 99 && r2 == 99 ? 0 : 1;
}

/* 命名操作数: %k[name] 修饰符 */
int test_named_k_modifier(void) {
    int val = 55, result;
    asm("mov %k[src], %k0"
        : "=r"(result)
        : [src] "r"(val));
    return result;  /* 期望: 55 */
}

/* asm goto %l 修饰符 */
int test_l_modifier(void) {
    int x = 0;
    asm goto(
        "jmp %l0"
        :
        :
        :
        : label_done
    );
    return -1;
label_done:
    return x;  /* 期望: 0 */
}

/* asm goto %l[name] 修饰符 */
int test_l_named_modifier(void) {
    int x = 0;
    asm goto(
        "jmp %l[target]"
        :
        :
        :
        : target
    );
    return -1;
target:
    return x;  /* 期望: 0 */
}

/* 立即数优先级: const_eval 折叠复合表达式 */
int test_imm_complex_expr(void) {
    int result;
    /* (10 * 5) + 2 = 52; const_eval 应折叠为单值 */
    asm("mov %1, %0"
        : "=r"(result)
        : "i"(10 * 5 + 2));
    return result;  /* 期望: 52 */
}

int main(void) {
    if (test_basic_reg_subst() != 7) return 1;
    if (test_imm_consteval() != 8) return 2;
    if (test_b_modifier() != 42) return 3;
    if (test_w_modifier() != 0x1234) return 4;
    if (test_k_modifier() != 99) return 5;
    if (test_q_modifier() != 42) return 6;
    if (test_a_modifier_reg() != 7) return 7;
    if (test_named_operands() != 0) return 8;
    if (test_named_k_modifier() != 55) return 9;
    if (test_l_modifier() != 0) return 10;
    if (test_l_named_modifier() != 0) return 11;
    if (test_imm_complex_expr() != 52) return 12;
    return 0;
}
