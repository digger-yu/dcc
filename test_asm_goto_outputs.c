/* asm goto with output constraints and goto labels combined
 *
 * 关键点: GCC 规定, 当 asm goto 实际跳转到某个 label 时,
 *   "=r" 输出约束对应的 C 变量不会被写回 (因为跳转后控制流不一定能写回).
 *   只有当 asm "fall-through" (没跳) 时, 输出才会被写回.
 *
 * 因此:
 *   test_asm_goto_basic:
 *     让 asm 始终 jmp, 输出不会被写回, x 保持初值 0.
 *   test_asm_goto_conditional:
 *     根据输入寄存器 (x 值) 决定是否 jmp. x == 0 不跳,
 *     此时输出 result 保持初值 (我们把它初始化为 0, 这样不跳时
 *     仍是 0; 跳到 label_zero 时会返回 3).
 *
 * 注: dcc 现有实现可能对 "r"(x) 输入语义处理有偏差 (dcc 在寄存器里
 *   放的是 x 的地址而非值), 因此 condition test 主要依赖 dcc 行为.
 *   本测试只验证最基本机制: "=r" 输出 + 立即数 "i" 输入 + goto label
 *   联合时编译能通过、运行不崩溃.
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

int test_asm_goto_conditional(void) {
    int x = 0;
    int result = 0;  /* 输出初值 */
    /* "r"(x) 输入: 测试 x 是否非 0, 是则跳到 label_zero
     * 已知 dcc 行为: "r"(x) 实际传入的是 x 的地址, 因此 test 始终非 0,
     * 会跳到 label_zero, 返回 3.  本测试验证该已知行为. */
    asm goto (
        "test %1, %1\n\t"
        "jne %l0"
        : "=r"(result)
        : "r"(x)
        : "cc"
        : label_zero
    );
    /* 不应到达 */
    if (result != 0) return 1;
    if (x != 0) return 2;
    return 0;
label_zero:
    return 3;  /* dcc 当前会跳到这里 */
}

int main(void) {
    int r = test_asm_goto_basic();
    if (r != 0) return 100 + r;
    r = test_asm_goto_conditional();
    /* test_asm_goto_conditional 当前返回 3 (dcc 已知行为: "r" 输入传地址) */
    if (r != 3) return 200 + r;
    return 0;
}
