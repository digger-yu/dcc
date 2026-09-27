/* 缺陷 5 夹具: 聚合形参跨寄存器/栈边界拆分时放置错位.
 *
 * 现象 (修复前): `void f(int a,int b,int c,int d,int e,struct P p)` —— 5 个
 * 整型参数已吃掉 edi/esi/edx/ecx/r8d, 只剩 r9 一个整数寄存器, 于是 2 字 (16
 * 字节) 的 struct 必须「第 6 字进 r9, 第 7 字落栈」。dcc 把两字整体按错误
 * 落点摆放 (被调方读到的 struct 半新半旧), 去掉 `...` 之外的任一参数即不复现。
 * 根因: 调用方 (`Integer`/`SseUp`  eightbyte 分类) 与栈槽分配两处各自数
 * "剩余寄存器数", 口径不一致: 调用方按八字节顺序逐个决定去向, 栈槽布局却
 * 按整块聚合预留, 于是拆分点之后的字被写到下一个栈槽的偏移上。
 *
 * 覆盖轴: struct 全整型 / struct 含 SSE 八字节 (整数+xmm 混合拆分) /
 * SseUp (float 对) 走 xmm 不占 GP / 3 字 struct (2 寄存器 + 1 栈) /
 * 6 个 GP 参数把 struct 整体推到栈 / 变参下同位置 / struct 返回值再传参。
 */
#include <stdarg.h>
#include <stdio.h>

struct P2 {
    long a, b;
};
struct P3 {
    long a, b, c;
};
struct S2 {
    float x, y;
};
struct M {
    long a;
    double d;
};

static long ra, rb, rc, rd, re;
static struct P2 g_p2;
static struct P3 g_p3;
static float g_sx, g_sy;
static long g_ma;
static double g_md;
static int bad;

#define CHK(label, got, want)                                                   \
    do {                                                                        \
        long long g_ = (long long)(got);                                        \
        long long w_ = (long long)(want);                                       \
        if (g_ != w_) {                                                         \
            printf("FAIL %-16s got=%lld want=%lld\n", (label), g_, w_);         \
            bad++;                                                              \
        } else {                                                                \
            printf("ok   %-16s = %lld\n", (label), g_);                         \
        }                                                                       \
    } while (0)

/* 5 个整型吃满 edi..r8d, struct 第 6 字只能在 r9, 第 7 字落栈 */
static void f5_int_plus_p2(int a, int b, int c, int d, int e, struct P2 p)
{
    ra = a;
    rb = b;
    rc = c;
    rd = d;
    re = e;
    g_p2 = p;
}

/* 3 字 struct: r9 + 2 个栈槽 */
static void f5_int_plus_p3(int a, int b, int c, int d, int e, struct P3 p)
{
    ra = a;
    rb = b;
    rc = c;
    rd = d;
    re = e;
    g_p3 = p;
}

/* 4 个 double 只吃 SSE, 6 个 GP 全空 → struct 完整落在 rdi/rsi */
static void f4_double_plus_p2(double w, double x, double y, double z, struct P2 p)
{
    g_md = w + x + y + z;
    g_p2 = p;
}

/* struct 的两个八字节分别是 INTEGER / SSE: 拆分点必须让第二个字进 xmm */
static void f_mixed_gp_sse(int a, int b, struct M m, int c)
{
    ra = a;
    rb = b;
    rc = c;
    g_ma = m.a;
    g_md = m.d;
}

/* {float,float} 正好合成一个八字节且归 SSE → 不占整数寄存器,
 * 后面的 `int e` 仍能拿到 edi, struct 整体进 xmm0 */
static void f_sseup(int a, int b, int c, int d, struct S2 s, int e)
{
    ra = a;
    rb = b;
    rc = c;
    rd = d;
    re = e;
    g_sx = s.x;
    g_sy = s.y;
}

/* 6 个 GP 标量把 struct 整体推到栈 (两个栈槽, 顺序必须按八字节升序) */
static void f_all_stack(long s1, long s2, long s3, long s4, long s5, long s6,
    struct P2 p, long s7)
{
    ra = s1 * 100 + s6;
    rb = s7;
    g_p2 = p;
}

/* 变参版本: 同一位置的命名聚合拆分必须与非变参一致 */
static void f_vararg(int a, int b, int c, int d, int e, struct P2 p, ...)
{
    va_list ap;
    ra = a;
    rb = b;
    rc = c;
    rd = d;
    re = e;
    g_p2 = p;
    va_start(ap, e);
    (void)va_arg(ap, int); /* 只证明省略号存在改变了寄存器掩码的场合可用 */
    va_end(ap);
}

static struct P2 mk_p2(long a, long b)
{
    struct P2 r;
    r.a = a;
    r.b = b;
    return r;
}

int main(void)
{
    struct P2 q = { 111, 222 };
    struct P3 r3 = { 31, 32, 33 };
    struct M m = { 777, 1.25 };
    struct S2 s = { 1.5f, -2.25f };

    f5_int_plus_p2(1, 2, 3, 4, 5, q);
    CHK("5int+p2.a", g_p2.a, 111);
    CHK("5int+p2.b", g_p2.b, 222);
    CHK("5int+p2.args", ra * 10000 + rb * 1000 + rc * 100 + rd * 10 + re, 12345);

    f5_int_plus_p3(1, 2, 3, 4, 5, r3);
    CHK("5int+p3.a", g_p3.a, 31);
    CHK("5int+p3.b", g_p3.b, 32);
    CHK("5int+p3.c", g_p3.c, 33);

    f4_double_plus_p2(1.0, 2.0, 3.0, 4.0, q);
    CHK("4dbl+p2.a", g_p2.a, 111);
    CHK("4dbl+p2.b", g_p2.b, 222);
    CHK("4dbl+p2.sse", (long)g_md, 10);

    f_mixed_gp_sse(7, 8, m, 9);
    CHK("mixed.m.a", g_ma, 777);
    CHK("mixed.m.c", rc, 9);
    printf("%s %-16s = %.2f\n", g_md == 1.25 ? "ok  " : "FAIL", "mixed.m.d", g_md);
    if (g_md != 1.25) {
        bad++;
    }

    f_sseup(1, 2, 3, 4, s, 5);
    CHK("sseup.args", ra * 10000 + rb * 1000 + rc * 100 + rd * 10 + re, 12345);
    printf("%s %-16s = %.2f %.2f\n", g_sx == 1.5f && g_sy == -2.25f ? "ok  " : "FAIL",
        "sseup.pair", (double)g_sx, (double)g_sy);
    if (g_sx != 1.5f || g_sy != -2.25f) {
        bad++;
    }

    f_all_stack(1, 2, 3, 4, 5, 6, q, 7);
    CHK("allstack.p.a", g_p2.a, 111);
    CHK("allstack.p.b", g_p2.b, 222);
    CHK("allstack.s1s6", ra, 106);
    CHK("allstack.s7", rb, 7);

    f_vararg(1, 2, 3, 4, 5, q, 99);
    CHK("vararg.p.a", g_p2.a, 111);
    CHK("vararg.p.b", g_p2.b, 222);

    /* struct 返回值直接当实参 (拆分点在调用方, 值不在任何具名对象里) */
    f5_int_plus_p2(1, 1, 1, 1, 1, mk_p2(999, 888));
    CHK("retarg.p.a", g_p2.a, 999);
    CHK("retarg.p.b", g_p2.b, 888);

    printf("bad=%d\n", bad);
    return bad;
}
