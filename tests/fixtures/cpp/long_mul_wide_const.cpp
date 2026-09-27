/*
 * C++ native 路径下 64 位 `long` 乘加跨 2^31/2^32 静默算错。
 *
 * 根因: C++ 前端 (Sema → lower_ir → SSA→AST 重建) 在「常见算术转换」里把乘数/
 * 加数的类型推成 32 位 `int`, 或把超出 32 位范围的常量物化时走 sext/immediate
 * 的 32 位通道 —— 于是 `fp * 100 + -10` 的高位丢失, 结果被截到低 32 位。同一段
 * 代码走 C 路径 (直接建 AST) 是对的, 说明缺陷只在 C++ 那条类型推导轴上。
 *
 * 本夹具把宽常量乘法、宽立即数、跨 2^31 的 int→long 混合加法、移位、循环累加、
 * 宽值除法与按值传参全部逐个打印: 任何一次高位截断都会同时改变 stdout 与退出码。
 * 退出码 = 与 g++ 真值不符的断言条数。
 */
#include <cstdio>

static long g_acc = 0L;

static long scale(long base, long factor, long delta) {
    return base * factor + delta;
}

static long take_by_value(long v) {
    return v + 1L;
}

int main() {
    int bad = 0;

    /* 移交者的原始复现 */
    long fp = 103060990L;
    fp = fp * 100 + -10;
    printf("A %ld\n", fp);
    if (fp != 10306098990L) bad++;

    /* 宽立即数物化 (常数本身 > 2^31) */
    long wide = 3000000000L;
    wide = wide + 1L;
    printf("B %ld\n", wide);
    if (wide != 3000000001L) bad++;

    /* 跨 2^32 的乘法 */
    long m = 1234567L * 1234567L;
    printf("C %ld\n", m);
    if (m != 1524155677489L) bad++;

    /* int 操作数参与 long 运算: 结果类型必须是 long, 不能在 int 里折叠 */
    int a = 2000000000;
    long s = (long)a + a;
    printf("D %ld\n", s);
    if (s != 4000000000L) bad++;

    /* 移位产生宽值 */
    long sh = 1L << 40;
    printf("E %ld\n", sh);
    if (sh != 1099511627776L) bad++;

    /* 宽值除法 (只错高位的值在这里也会露出来) */
    long dv = 10306098990L / 100L;
    printf("F %ld\n", dv);
    if (dv != 103060989L) bad++;

    /* 循环累加: 每一步都是 int 常量 * long 变量 */
    long acc = 0L;
    for (int i = 0; i < 1000; i++) {
        acc += 10000000L;
    }
    printf("G %ld\n", acc);
    if (acc != 10000000000L) bad++;

    /* 跨函数调用的宽值传参 (按值, 不是指针) */
    long c1 = scale(103060990L, 100L, -10L);
    long c2 = scale(2000000000L, 4L, 5L);
    printf("H %ld %ld\n", c1, c2);
    if (c1 != 10306098990L) bad++;
    if (c2 != 8000000005L) bad++;

    /* 全局 long 的宽值读写 */
    g_acc = 9999999999L;
    g_acc = g_acc * 2L;
    printf("I %ld\n", g_acc);
    if (g_acc != 19999999998L) bad++;

    /* 按值传参 + 返回宽值 */
    long tv = take_by_value(4294967296L);
    printf("J %ld\n", tv);
    if (tv != 4294967297L) bad++;

    /* 宽值与 int 比较: 关系运算也要按 64 位做 */
    int cmp = (fp > 2000000000L) ? 1 : 0;
    printf("K %d\n", cmp);
    if (cmp != 1) bad++;

    printf("bad=%d\n", bad);
    return bad;
}
