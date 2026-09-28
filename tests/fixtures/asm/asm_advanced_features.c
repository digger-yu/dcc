/* GCC asm 高级特性测试
 *
 * 覆盖以下特性:
 *   .rept/.endr       - 重复块
 *   .irp/.endr        - 迭代值列表
 *   .irpc/.endr       - 迭代字符
 *   .macro/.endm      - 宏定义与调用
 *   .if/.else/.endif  - 条件汇编
 *   .ifdef/.ifndef    - 符号条件 (始终 false/true)
 *   .intel_syntax/.att_syntax - 语法切换 (透传)
 *   .altmacro/.noaltmacro - 交替宏模式 (透传)
 *   asm_output 属性   - GCC15 提案
 */

/* 1. .rept 重复块: 重复 3 次 nop */
int test_rept(void) {
    asm volatile(
        ".rept 3\n\t"
        "nop\n\t"
        ".endr"
    );
    return 0;
}

/* 2. .irp 迭代值列表 */
int test_irp(void) {
    int x = 0;
    asm volatile(
        ".irp val, 1, 2, 3\n\t"
        "add $\\val, %0\n\t"
        ".endr"
        : "+r"(x)
    );
    /* x = 0 + 1 + 2 + 3 = 6 */
    return x == 6 ? 0 : 2;
}

/* 3. .irpc 迭代字符 */
int test_irpc(void) {
    /* 简单测试: 迭代字符生成 nop */
    asm volatile(
        ".irpc c, abc\n\t"
        "nop\n\t"
        ".endr"
    );
    return 0;
}

/* 4. .macro 定义与调用 */
int test_macro(void) {
    int x = 0;
    asm volatile(
        ".macro my_add dst, src\n\t"
        "add \\src, \\dst\n\t"
        ".endm\n\t"
        "my_add %0, $10\n\t"
        "my_add %0, $20"
        : "+r"(x)
    );
    /* x = 0 + 10 + 20 = 30 */
    return x == 30 ? 0 : 4;
}

/* 5. .if 条件汇编 (真) */
int test_if_true(void) {
    int x = 0;
    asm volatile(
        ".if 1\n\t"
        "mov $42, %0\n\t"
        ".endif"
        : "=r"(x)
    );
    return x == 42 ? 0 : 5;
}

/* 6. .if/.else 条件汇编 (假, 走 else) */
int test_if_else(void) {
    int x = 0;
    asm volatile(
        ".if 0\n\t"
        "mov $1, %0\n\t"
        ".else\n\t"
        "mov $99, %0\n\t"
        ".endif"
        : "=r"(x)
    );
    return x == 99 ? 0 : 6;
}

/* 7. .ifdef (始终 false) / .ifndef (始终 true) */
int test_ifdef_ifndef(void) {
    int x = 0;
    asm volatile(
        ".ifdef SOME_UNDEFINED_SYMBOL\n\t"
        "mov $1, %0\n\t"
        ".endif\n\t"
        ".ifndef SOME_UNDEFINED_SYMBOL\n\t"
        "mov $77, %0\n\t"
        ".endif"
        : "=r"(x)
    );
    return x == 77 ? 0 : 7;
}

/* 8. .intel_syntax / .att_syntax 语法切换 (透传至 -S 输出) */
int test_syntax_switch(void) {
    asm volatile(
        ".intel_syntax noprefix\n\t"
        "nop\n\t"
        ".att_syntax prefix\n\t"
        "nop"
    );
    return 0;
}

/* 9. .altmacro / .noaltmacro 模式切换 (透传) */
int test_altmacro(void) {
    asm volatile(
        ".altmacro\n\t"
        "nop\n\t"
        ".noaltmacro\n\t"
        "nop"
    );
    return 0;
}

/* 10. asm_output 属性 (GCC15 提案, 仅解析) */
__attribute__((asm_output("mov %0, %%rax")))
int test_asm_output_attr(void) {
    return 0;
}

int main(void) {
    int r;
    r = test_rept();           if (r) return r;
    r = test_irp();            if (r) return r;
    r = test_irpc();           if (r) return r;
    r = test_macro();          if (r) return r;
    r = test_if_true();        if (r) return r;
    r = test_if_else();        if (r) return r;
    r = test_ifdef_ifndef();   if (r) return r;
    r = test_syntax_switch();  if (r) return r;
    r = test_altmacro();       if (r) return r;
    r = test_asm_output_attr(); if (r) return r;
    return 0;
}
