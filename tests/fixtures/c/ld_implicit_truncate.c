/* 缺陷 3 夹具: long double -> 整型的**隐式**转换被丢掉.
 *
 * 现象 (修复前): `long double d; int r = 2030 + 12.0L;` 之类"常见算术转换把
 * 结果提升成 long double, 再由初始化/赋值隐式截断回整型"的表达式, dcc 直接把
 * x87 栈顶的 80-bit 值按整型存进目标 (得 0 或垃圾), 而显式 `(int)` 转型是对的
 * —— 因为显式路径才会发 cvttsd2si/fistp。根因: 隐式转换只在整型秩之间补,
 * 浮点->整型的隐式收敛转换在 C 前端与 C++ lower 两处都缺。
 *
 * 覆盖轴: 初始化 / 赋值 / 复合赋值 / 实参 / 返回值 / 条件表达式 / 显式转型
 * (对照) / 负值向零截断 / 窄化到 short,char / 无符号目标 (含 [2^31,2^32) 与
 * [2^63,2^64) 两个旧实现会饱和到 INT_MAX 的区间)。
 */
#include <stdio.h>

static int bad;

#define CHK(label, got, want)                                                   \
    do {                                                                        \
        long long g_ = (long long)(got);                                        \
        long long w_ = (long long)(want);                                       \
        if (g_ != w_) {                                                         \
            printf("FAIL %-14s got=%lld want=%lld\n", (label), g_, w_);         \
            bad++;                                                              \
        } else {                                                                \
            printf("ok   %-14s = %lld\n", (label), g_);                         \
        }                                                                       \
    } while (0)

/* 无符号目标用单独的宏: 值域跨到 [2^31, 2^64), 按有符号报会看错 */
#define CHKU(label, got, want)                                                  \
    do {                                                                        \
        unsigned long long g_ = (unsigned long long)(got);                      \
        unsigned long long w_ = (unsigned long long)(want);                     \
        if (g_ != w_) {                                                         \
            printf("FAIL %-14s got=%llu want=%llu\n", (label), g_, w_);         \
            bad++;                                                              \
        } else {                                                                \
            printf("ok   %-14s = %llu\n", (label), g_);                         \
        }                                                                       \
    } while (0)

static long double gd = 7.5L;

static int ret_ld(void)
{
    long double v = 5.5L;
    return v; /* 返回值的隐式截断 */
}

static void take(int i, long l, unsigned u)
{
    CHK("arg-int", i, 7);
    CHK("arg-long", l, -12);
    CHKU("arg-uint", u, 4000000400ULL);
}

int main(void)
{
    /* 1. 常见算术转换的结果被初始化器隐式截断 */
    int r = 2030 + 12.0L;
    CHK("arith-init", r, 2042);

    /* 2. long double 变量直接初始化 int */
    long double e = 1.0L;
    int q = e;
    CHK("var-init", q, 1);

    /* 3. -> long / -> long long */
    long double f = 12.0L;
    long g = f;
    CHK("to-long", g, 12);
    long long ll = 1234567890123.0L;
    CHK("to-llong", ll, 1234567890123LL);
    long long ll2 = 1234567890123.75L;
    CHK("to-llong-trunc", ll2, 1234567890123LL);

    /* 4. 向零截断 (含负值) */
    int n = -3.7L;
    CHK("neg-trunc", n, -3);
    int n2 = 0.9L;
    CHK("frac-to-zero", n2, 0);
    long ngs = -9876543.21L;
    CHK("neg-long", ngs, -9876543L);

    /* 5. 窄化到 short / char (整型转换取模, 与 gcc 一致) */
    short s = 300.7L;
    CHK("to-short", s, 300);
    signed char ch = -100.9L;
    CHK("to-schar", ch, -100);

    /* 6. unsigned 目标 —— [2^31, 2^32) 区间旧实现会被有符号 cvtt 夹成 INT_MAX */
    long double ud = 4294967295.0L;
    unsigned u = ud;
    CHKU("to-uint", u, 4294967295ULL);
    unsigned u2 = 4000000000.5L;
    CHKU("to-uint2", u2, 4000000000ULL);
    /* 64 位无符号目标: 源值过 INT64_MAX 需要 2^64 校正轴。
     * 只取 double 能精确表示的量 (本后端 long double 的整数化通道是
     * `st(0) → f64 → cvtt`, 需要 >53 位有效位且落在 [2^63,2^64) 的值会丢
     * 末位 —— 见报告的残留项) */
    unsigned long ul = 18000000000000000000.0L;
    CHKU("to-ulong", ul, 18000000000000000000ULL);
    unsigned long long ull = 10000000000000000000.0L;
    CHKU("to-ullong", ull, 10000000000000000000ULL);

    /* 7. 赋值 (非初始化) 与复合赋值 */
    int a = 1;
    a = 9.99L;
    CHK("assign", a, 9);
    int b = 1;
    b += 2.7L;
    CHK("plus-assign", b, 3);
    long double inc = 4.5L;
    int b2 = 0;
    b2 = inc + inc;
    CHK("ld-plus-ld", b2, 9);

    /* 8. 全局 long double 的隐式截断 */
    int gg = gd;
    CHK("global-ld", gg, 7);

    /* 9. 实参位置的隐式截断 */
    take(7.9L, -12.99L, 4000000400.0L);

    /* 10. 返回值 */
    CHK("ret-conv", ret_ld(), 5);

    /* 11. 数组下标 (C 要求整型, 这里对照显式截断) */
    int arr[4] = { 10, 20, 30, 40 };
    long double idx = 2.9L;
    CHK("array-index", arr[(int)idx], 30);

    /* 12. 条件表达式的共同类型是 long double, 结果再隐式截断 */
    int cond = 1;
    int t = cond ? 1.5L : 2.5L;
    CHK("cond-expr", t, 1);
    int t2 = !cond ? 1.5L : 8.25L;
    CHK("cond-expr2", t2, 8);

    /* 13. 显式转型对照 (修复前后都必须正确) */
    int k = (int)(3.5L + 1.0L);
    CHK("explicit-cast", k, 4);
    unsigned uk = (unsigned)(1.0L + 4294967294.0L);
    CHKU("explicit-uint", uk, 4294967295ULL);

    /* 14. 与整型混合的比较: 值语义走 long double, 截断只发生在目标对象 */
    long double cmpv = 10.0L;
    CHK("cmp-mixed", (cmpv == 10) + (cmpv > 9.5L), 2);

    printf("bad=%d\n", bad);
    return bad;
}
