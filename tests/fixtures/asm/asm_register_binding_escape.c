/* `register asm` 变量的地址被取走时，它必须留在内存里。
 *
 * 为什么这份夹具**不能**与 gcc 对照：gcc 直接拒绝编译这种写法（实测
 * `error: address of register variable 'v' requested`）。dcc 选择接受它，并
 * 按“一旦取址就放弃寄存器绑定、走内存”的语义实现，于是 `v` 与 `*p` 必然一致。
 *
 * 旧实现无条件绑定，实测同一份代码打印 `42 0` 并返回 1 —— 绑定时那个栈槽从来没被
 * 写过，`&v` 的读者拿到的是栈上的残留。逸出判据与 GVN 共用同一个真源
 * (`ast_gvn::addr_escaped_names`)，因此内联汇编的内存约束 (`"m"` 操作数) 同样算逸出。
 *
 * 反向对照 `bound_ok()` 不取址，绑定路径必须仍然给出正确值 —— 判据不能退化成
 * 「把整个特性关掉」。
 */
#include <stdio.h>

static int fail;

/* 地址逸出：必须留在内存里，两条读法看到同一份值。 */
long escaped(void) {
    register long v asm("r15") = 0;
    long *p = &v;
    v = 42;
    if (v != 42 || *p != 42)
        fail = 1;
    return v + *p;
}

/* 内联汇编的内存约束也是逸出：asm 拿的是地址，绑在寄存器里它就读不到值。 */
long asm_memory_operand(void) {
    register long m asm("r14") = 0;
    long out = 0;
    m = 5;
    __asm__ volatile("movq %1, %0" : "=r"(out) : "m"(m));
    if (out != 5 || m != 5)
        fail = 2;
    return out;
}

/* 没有逸出：绑定路径仍须正确。 */
long bound_ok(void) {
    register long v asm("r14") = 0;
    v = 7;
    v += 5;
    if (v != 12)
        fail = 3;
    return v;
}

int main(void) {
    if (escaped() != 84)
        return 10;
    if (asm_memory_operand() != 5)
        return 11;
    if (bound_ok() != 12)
        return 12;
    return fail;
}
