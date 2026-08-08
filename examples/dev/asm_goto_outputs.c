/* asm goto with output constraints and goto labels combined
 *
 * 关键点: GCC 规定, 当 asm goto 实际跳转到某个 label 时,
 *   "=r" 输出约束对应的 C 变量不会被写回 (因为跳转后控制流不一定能写回).
 *   只有当 asm "fall-through" (没跳) 时, 输出才会被写回.
 *
 * 因此:
 *   test_asm_goto_basic:
 *     让 asm 始终 jmp, 输出不会被写回, x 保持初值 0.
 *   test_asm_goto_fallthrough:
 *     asm 显式写 42 到输出且不跳, 验证 fall-through 路径下输出被写回 42.
 *   test_asm_goto_taken:
 *     asm 写 42 到输出但条件跳, 验证 goto taken 路径下输出保持初值 0.
 *
 * dcc 实现: asm 执行后立即写回输出 (与 emit_inline_asm 相同).
 *   fall-through 时写回代码执行, 变量获得新值;
 *   跳转时控制流跳过写回代码, 变量保持初值.
 *   注: "=r" 输出准备时执行 lea 把变量地址放入寄存器, 实际 asm
 *   模板若显式写 %N (例如 "mov $42, %0") 会覆盖该地址, 写回阶段即可获得
 *   正确值; 模板不写 %N 时寄存器保留地址, 与 GCC "未初始化" 语义不同,
 *   调用方应避免依赖这种情况.  本测试只验证最基本机制:
 *   "=r" 输出 + "r" 输入 + goto label 联合时编译能通过、运行不崩溃.
 */

int test_asm_goto_basic(void) {
    int x = 0;
    /* 永远跳到 label_done, x 不会被改写 */
    asm goto (
        "jmp %l0"
        : "=r"(x)
        : "i"(42)
        :
        : label_done
    );
    return -1;  /* 不会执行 */
label_done:
    return x;  /* 0: 输出未被写回 */
}

int test_asm_goto_fallthrough(void) {
    int x = 0;
    int result = 0;
    /* asm 显式写 42 到输出且不跳, 验证 fall-through 路径下输出被写回 42 */
    asm goto (
        "mov $42, %0"
        : "=r"(result)
        : "r"(x)
        : "cc"
        : label_unused
    );
    if (result != 42) return 1;
    if (x != 0) return 2;
    return 0;
label_unused:
    return 3;  /* 不会执行 */
}

int test_asm_goto_taken(void) {
    int x = 1;  /* 非零, jne 必跳 */
    int result = 0;
    /* asm 写 42 到输出, 但因 x=1 条件跳到 label_zero, 输出不应被写回 */
    asm goto (
        "mov $42, %0\n\t"
        "test %1, %1\n\t"
        "jne %l0"
        : "=r"(result)
        : "r"(x)
        : "cc"
        : label_zero
    );
    /* 不应到达 */
    return 4;
label_zero:
    return result;  /* dcc 跳过后不写回, result 保持初值 0 */
}

int main(void) {
    int r = test_asm_goto_basic();
    if (r != 0) return 100 + r;
    r = test_asm_goto_fallthrough();
    if (r != 0) return 200 + r;
    r = test_asm_goto_taken();
    if (r != 0) return 300 + r;
    return 0;
}
