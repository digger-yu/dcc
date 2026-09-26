/* a6: long double 的 80-bit **对象表示 + 算术 + 比较** 迁移判据。
 *
 * 为什么需要这个夹具（起点必须是红的）：
 * dcc 此前把 `long double` 的局部槽当成 "低 8 字节 = f64 位型、高 8 字节 = 0"
 * 来存，rvalue 值通道也是 xmm0-f64。于是 `1.0L / 3.0L` 的**商**只有 53 位尾数，
 * 而 gcc（x87 80-bit，64 位显式尾数）有 64 位。实测：
 *     gcc:  q=0.333333333333333333342368351
 *     dcc:  q=0.333333333333333314829616256   ← f64 的 1/3 再拓宽
 * 循环叠加会把差异放大：
 *     gcc:  s=3.333333333333333333044212754
 *     dcc:  s=3.333333333333333481363069950
 *
 * 判据是**双层**的（本仓库踩过"退出码说谎"与"只比退出码假绿"两个坑）：
 *   1. stdout 的 `%Lf` 文本必须与 gcc 逐字符一致（由 scripts/dcc_gcc_diff.sh 比对）；
 *   2. 退出码必须能独立表达失败 —— 这里把 gcc 实测的 **80-bit 对象低 10 字节**
 *      当作期望位型直接 memcmp。不用"与十进制字面量 == "做判据，因为字面量
 *      通道（`Expr::Float{v: f64}`）本轮不动，`0.4L` 这类不可精确表示的字面量
 *      在 dcc 里只有 f64 精度，拿它当右操作数比较会与 gcc 不同源（假红）。
 *      本夹具只与 **1.0L/2.0L/3.0L/0.5L/7.0L 等两种精度下都精确** 的字面量比较。
 *
 * 期望退出码 0。
 */
#include <stdio.h>

/* gcc -std=gnu17 -O0 下 `1.0L/3.0L` 与 `10 * (1.0L/3.0L)` 的对象位型
 * （x87 80-bit：前 8 字节尾数 + 第 9~10 字节符号/指数，小端）。
 * 字节 10..15 是 SysV 允许实现自定义的 padding，**不参与比对**。 */
static const unsigned char Q80[10] = {
    0xab, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xfd, 0x3f,
};
static const unsigned char S80[10] = {
    0x54, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0xd5, 0x00, 0x40,
};
/* 2 * (1.0L/3.0L)：指数 +1、尾数逐位不变，是"真 80-bit"最锋利的指纹
 * （f64 通道下 2*q 的低位与 80-bit 不同源） */
static const unsigned char TWOQ80[10] = {
    0xab, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xfe, 0x3f,
};
static const unsigned char PROD80[10] = {
    0x56, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x95, 0x00, 0x40,
};

static int fails = 0;

#define CHK(cond, msg) do { if (!(cond)) { printf("FAIL: %s\n", (msg)); fails++; } } while (0)

/* 逐字节比对 80-bit 对象的**有效 10 字节**（不依赖 padding，不依赖指针比较） */
static int obj10_eq(const void *v, const unsigned char *expect) {
    const unsigned char *p = (const unsigned char *)v;
    int i;
    for (i = 0; i < 10; i++) {
        if (p[i] != expect[i]) return 0;
    }
    return 1;
}

/* long double 返回值：SysV 规定标量 x87 返回值在 st(0)。
 * 必须用 volatile 局部做除法：`return 1.0L / 3.0L;` 会被**前端常量折叠**成
 * f64 商的位型（字面量通道 `Expr::Float{v: f64}` 本轮不动），于是这一行考的
 * 就不再是返回 ABI 而是折叠精度 —— 那属于本次范围外的已知差异。 */
static long double third(void)
{
    volatile long double one = 1.0L, three = 3.0L;
    return one / three;
}

/* long double 具名形参：SysV 规定 x87 参数走内存（16 字节栈槽，真 80-bit） */
static long double add_ld(long double x, long double y) { return x + y; }

int main(void) {
    long double a = 1.0L, b = 3.0L;
    long double q = a / b;
    printf("q=%.27Lf\n", q);

    long double s = 0.0L;
    int i;
    for (i = 0; i < 10; i++) s = s + q;
    printf("s=%.27Lf\n", s);

    /* ── 对象表示：必须是真 80-bit 位型，不是 f64 拓宽 ── */
    CHK(obj10_eq(&q, Q80), "q object is not 80-bit 1/3");
    CHK(obj10_eq(&s, S80), "s object is not 80-bit 10/3");

    /* ── 算术：精确可表示的组合仍须逐位一致 ── */
    long double sum = 2.0L + 3.0L;
    long double prod = q * 7.0L;
    long double diff = 5.0L - 2.0L;
    long double neg = -q;
    printf("sum=%.27Lf\n", sum);
    printf("prod=%.27Lf\n", prod);
    printf("diff=%.27Lf\n", diff);
    printf("neg=%.27Lf\n", neg);
    CHK(obj10_eq(&prod, PROD80), "prod 对象 == 80-bit 7/3");
    CHK(sum == 5.0L, "add");
    CHK(diff == 3.0L, "sub");
    CHK(neg + q == 0.0L, "neg/round-trip");
    CHK(q < 0.5L, "q < 0.5L");
    CHK(q > 0.0L, "q > 0.0L");
    CHK(!(q == 0.5L), "q != 0.5L");
    CHK(q != 0.0L, "q != 0.0");

    /* 1/3 * 3 在 80-bit 下回到原值；f64 通道下也恰好成立，
     * 但这里的除法必须经 x87 才不先把 q 舍成 f64。 */
    CHK(q * 3.0L == q + q + q, "q*3 == 3*q");

    /* 混合精度操作数：int / long double 与 long double / int */
    int n = 3;
    long double mi = (long double)1 / n;
    printf("mi=%.27Lf\n", mi);
    CHK(mi == q, "1/(int)3 == 1.0L/3.0L");
    long double im = n * q;
    printf("im=%.27Lf\n", im);
    CHK(im == q + q + q, "3*(int) * q");

    /* 跨界：st(0) 返回值直接参与运算 */
    long double cv = third() + 1.0L;
    printf("cv=%.27Lf\n", cv);
    CHK(cv > 1.3333333L, "third() + 1.0L 下界");
    CHK(cv < 1.3333334L, "third() + 1.0L 上界");

    /* 具名形参（真 80-bit 落槽，无 f64 中转）：加法须逐位相同 */
    CHK(add_ld(1.0L, 2.0L) == 3.0L, "add_ld(1,2)==3");
    CHK(add_ld(q, q) == q + q, "add_ld(q,q)==q+q");

    /* 数组下标 / 解引用位置的 ld 对象读写 */
    long double arr[3];
    arr[0] = 0.0L; arr[1] = q; arr[2] = q;
    long double *pp = &arr[1];
    *pp = *pp + *pp;
    printf("dbl=%.27Lf\n", arr[1]);
    CHK(obj10_eq(&arr[1], TWOQ80), "arr[1] = *pp + *pp 落槽为真 80-bit 2/3");

    /* 复合赋值 */
    long double w = 1.0L;
    w /= b;
    CHK(obj10_eq(&w, Q80), "w /= 3.0L 落槽为真 80-bit");

    if (fails == 0) printf("ALL OK\n");
    else printf("FAILURES: %d\n", fails);
    return fails != 0;
}
