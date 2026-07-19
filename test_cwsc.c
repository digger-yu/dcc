/* __builtin_call_with_static_chain 测试: 静态链调用约定
 *
 * 该 builtin 用法: __builtin_call_with_static_chain(call, chain, args...)
 *   1. 把 chain 装入 r10 (静态链寄存器)
 *   2. 按 SysV ABI 准备 call 的 args... 参数
 *   3. call call
 *
 * 在嵌套函数 (nested function) 场景下, chain 指向父函数栈帧,
 * 子函数通过 r10 + upvar_offset 访问外层变量.
 *
 * dcc 限制: 嵌套函数的"取地址" (nested function → function pointer)
 *   暂未生成 trampoline, 因此不能写 `(void *)inner` 来取嵌套函数地址.
 *   本测试改用普通函数 + 在函数内显式读 r10 模拟 nested 场景.
 *   r10 指向 chain (一个 int 变量的地址), 函数体内通过 r10 取值.
 */

int fake_nested(int a, int b) {
    /* 模拟 nested 访问: 读取 r10 指向的 int 作为"外层变量 x" */
    int x;
    asm volatile ("movl (%%r10), %0" : "=r"(x) : : "memory");
    return a + b + x;
}

int fake_helper(int v) {
    int x;
    asm volatile ("movl (%%r10), %0" : "=r"(x) : : "memory");
    return v * 2 + x;
}

int test_via_chain(void) {
    /* 父帧: x = 42, 通过 chain (即 &x) 把 x 地址传给 fake_nested via r10 */
    int x = 42;
    int r = __builtin_call_with_static_chain(fake_nested, &x, 10, 20);
    if (r != 72) return 1;  /* 10+20+42 */
    if (x != 42) return 2;
    return 0;
}

int test_plain_chain(void) {
    int x = 100;
    int r = __builtin_call_with_static_chain(fake_helper, &x, 5);
    if (r != 110) return 1;  /* 5*2 + 100 */
    return 0;
}

int main(void) {
    int r = test_via_chain();
    if (r != 0) return 100 + r;
    r = test_plain_chain();
    if (r != 0) return 200 + r;
    return 0;
}
