/* asm 模板修饰符 %P / %a 对立即数的文本替换验证
 * 编译为汇编后检查输出文本中不含多余 $ 前缀
 */

/* %P 修饰符: 立即数输出纯数字 (无 $) */
void test_P_imm_text(void) {
    asm(".byte %P0"
        :
        : "i"(42));
    /* 期望替换为: .byte 42 (非 .byte $42) */
}

/* %a 修饰符: 立即数输出纯数字 (无 $) */
void test_a_imm_text(void) {
    asm(".byte %a0"
        :
        : "i"(99));
    /* 期望替换为: .byte 99 (非 .byte $99) */
}

/* %P 命名操作数: 立即数输出纯数字 */
void test_P_named_imm(void) {
    asm(".byte %P[val]"
        :
        : [val] "i"(7));
    /* 期望替换为: .byte 7 */
}

/* %a 命名操作数: 立即数输出纯数字 */
void test_a_named_imm(void) {
    asm(".byte %a[val]"
        :
        : [val] "i"(11));
    /* 期望替换为: .byte 11 */
}
