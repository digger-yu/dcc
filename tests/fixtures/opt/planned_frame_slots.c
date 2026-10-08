/* 帧内槽必须全部在**规划期**登记：发射期 `var_offset` 的兜底只能按 `Type::Int`
 * 猜一个槽（非 int 的对象因此被按错宽度存取），并且让「晚分配带」的容量无法预知。
 * 本夹具把历史上会走兜底的形状各放一个，判据是「与 gcc 四档同结果」——
 * 宽度猜错时这些读法立刻错。
 *
 *   1) `for` 的 init 里的声明（`for (int i = 0; …)`）—— 规划期曾看不见；
 *   2) 初始化式里夹着的 `({...})` 块声明（内核 min()/max() 形状）；
 *   3) 带标签的块里的声明（`out: long v = 1;`）；
 *   4) 文件作用域 `register asm` 变量（每个引用它的函数都要一个帧内槽，且宽度是
 *      `unsigned long`，按 Int 猜就会截断）；
 *   5) 非 int 的隐式类型对象（`unsigned long` 全局经 percpu/段路径取用时也同类）。
 */
#include <stdio.h>

register unsigned long slotvar asm("r14");

static int loop_init(int n) {
    long sum = 0;
    for (int i = 0; i < n; i++) sum += i;
    return (int)sum;
}

static int macro_shape(int a, int b) {
    int r = ({ __typeof__(a) _a = (a); __typeof__(b) _b = (b); _a > _b ? _a : _b; });
    return r;
}

static int labeled_decl(int k) {
    if (k > 0) goto out;
    return -1;
out:
    long v = k * 3;
    return (int)v;
}

static int wide_slot(void) {
    slotvar = 0x100000040UL;      /* 需要 8 字节宽度的槽才存得下 */
    return (int)(slotvar & 0xff);
}

int main(void) {
    int fail = 0;
    if (loop_init(5) != 10) fail |= 1;
    if (macro_shape(3, 9) != 9) fail |= 2;
    if (macro_shape(11, 4) != 11) fail |= 4;
    if (labeled_decl(4) != 12) fail |= 8;
    if (wide_slot() != 64) fail |= 16;
    if (loop_init(0) != 0) fail |= 32;
    printf("fail=%d\n", fail);
    return fail;
}
