/* tests/fixtures/c/global_reg_call_clobbered.c
 *
 * 文件作用域 `register asm` 绑到**会被调用毁掉**的寄存器上：本夹具钉的是「这句话必须
 * 说，而且只在 gcc 说的地方说」（`docs/p12-physreg-spill-design.md` §11.29，Phase 5.1
 * 前置三的第一块）。
 *
 * 语义形状与 gcc 逐字对齐：`lost` 绑在 `%r11`（SysV caller-saved）上，两侧都**不**为它
 * 插保存/恢复 —— 调用确实会毁掉它，这是这个语言扩展本身的行为，不是编译器的错。差别只能
 * 在诊断上看：gcc 报
 *     warning: call-clobbered register used for global register variable
 * 而修这里的 dcc 一声不响。一个「看起来应该住在某处」的全局对象被放进跨不过调用的寄存器，
 * 编译器知道而用户不知道 —— 那正是本仓一直在消除的那类「事实只住在编译器里」。
 *
 * 两条与 gcc 共有的注意点，写清楚免得后人以为这里是「忘了初始化」的 bug：
 *   1. 全局寄存器变量**没有初值**（它就是这个寄存器在函数入口时的内容，通常是链接器/运行时
 *      留下的指针位型），所以本夹具一律只断言**相对量**（`k0 + 1`），不断言 `keep == 0`；
 *   2. `lost` 只在**没有调用**的区间里写后读（`leaf_use` 内部 `lost = x; ... lost`），
 *      否则两侧都会读到被调用毁掉的垃圾 —— 那不再是「与 gcc 一致」的判据。
 * 同一份源在 gcc 与 dcc 下都必须返回 0；两侧唯一的差别就是那句警告。
 */

register unsigned long keep asm("r15"); /* callee-saved：两侧都不警告 */
register unsigned long lost asm("r11"); /* call-clobbered：两侧都警告 */

unsigned long bump(void) {
    keep += 1;
    return keep;
}

/* 叶子路径：`lost = x` 与读 `lost` 之间没有任何调用 ⇒ 值确定（x + 当前 keep） */
unsigned long leaf_use(unsigned long x) {
    lost = x;
    return lost + keep;
}

int main(void) {
    unsigned long k0 = keep;
    unsigned long a = bump();
    unsigned long b = leaf_use(40);
    /* keep 绑 callee-saved：跨 bump() 的调用仍然存活（序言会保存它） */
    return (a == k0 + 1 && b == 40 + k0 + 1) ? 0 : 1;
}
