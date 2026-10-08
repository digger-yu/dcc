#include <stdio.h>
static int val;
static int *impl(void) { return &val; }
static int takes(int a, int b) { val = a * 10 + b; return &val; }
/* 1) 返回函数指针的函数，自己带形参 */
int *(*f(int init))(void) { val = init; return impl; }
/* 2) 同上但两个形参 + 指针返回 */
int *(*g(int a, int b))(int, int) { return takes; }
/* 3) 只有一层参数表的老形状（回归对照） */
int *h(int init) { val = init * 2; return &val; }
int main(void) {
    int fail = 0;
    int *(*p)(void) = f(7);
    if (*p() != 7) fail |= 1;
    int *(*q)(int, int) = g(3, 4);
    if (*q(3, 4) != 34) fail |= 2;
    if (*h(5) != 10) fail |= 4;
    /* 形参遮蔽：内层同名声明不得复用形参的槽 */
    int *(*r)(void) = f(9);
    if (*r() != 9) fail |= 8;
    printf("fail=%d val=%d\n", fail, val);
    return fail;
}

/* 声明符读法判据（与 gcc 四档对拍）：`T *(*f(P1))(P2)` 里
 *   P1 = f 自己的形参，P2 = 被指向函数的形参（属于返回类型）。
 * 旧形状把两个列表用反：f 被当成收 P2（这里是 `(void)`），于是
 *   - 调用点传的参数没人接；
 *   - 体里读 `init` 只能靠发射期懒注册，拿到一个按 `Int` 猜的栈槽
 *     （任何非 int 形参因此被按错误宽度存取）。
 * 实测修前：本文件形状 1 在 gcc 是 rc=0，dcc（含开工前的二进制）是 rc=1。
 * 形状 3 是反向对照：只有一层参数表的 `T *h(int)` 一直是对的，改动不得弄坏它。
 */
