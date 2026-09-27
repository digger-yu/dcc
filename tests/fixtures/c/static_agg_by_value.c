/* 缺陷 4 夹具: **static 存储期**的聚合按值传参被当成指针传.
 *
 * 现象 (修复前):
 *   struct S { char *p; int n; };
 *   static struct S big = { "abc", 7 };
 *   g(big, "z");            <- 被调方读出的是 big 的地址字节, 不是字段值
 *   同样的 struct 若是 auto 存储期就正确。
 * 根因: 调用方为按值聚合实参做「逐字拷贝到参数空间」时, 实参 lvalue 的地址
 * 求解走了 decay-to-pointer 分支 —— static/global lvalue 的聚合在前端被
 * 折叠成 `SymAddr`(标量类别), 于是把「聚合首地址」当「聚合内容」搬进寄存器,
 * auto 存储期走栈槽偏移的分支反而正确。
 *
 * 覆盖轴: static 文件作用域 / 函数内 static / global(外部链接) / auto (对照) /
 * const 限定的 static / 含数组成员 / 含浮点成员 (SSE 类) / 嵌套 struct /
 * >16 字节 (跨寄存器+栈) / union / static 数组的元素 (下标 lvalue) /
 * 返回值再传参。
 */
#include <stdio.h>
#include <string.h>

struct Pair {
    const char *p;
    int n;
};

struct WithArr {
    char tag[6];
    int len;
};

struct Wide {
    long a, b, c, d, e, f;
};

struct Fl {
    double d;
    float f;
    int n;
};

union U {
    int i;
    long l;
    char b[8];
};

struct Nested {
    struct Pair p;
    struct WithArr w;
};

static struct Pair sp = { "abc", 7 };
static const struct Pair csp = { "const", 11 };
struct Pair gp = { "global", 13 };
static struct WithArr sa = { "tag9", 4 };
static struct Wide sw = { 1, 2, 3, 4, 5, 6 };
static struct Fl sf = { 2.5, 1.5f, 42 };
static union U su = { .l = 0x1122334455667788L };
static struct Nested sn = { { "nest", 9 }, { "in", 2 } };
static struct Pair arr[3] = { { "e0", 100 }, { "e1", 200 }, { "e2", 300 } };

static int bad;

static void show_pair(const char *who, struct Pair x, const char *z)
{
    if (strcmp(x.p, z) == 0) {
        printf("FAIL %s: p 被换成实参 z 指针 (%s)\n", who, x.p);
        bad++;
        return;
    }
    printf("%-14s p=%s n=%d z=%s\n", who, x.p, x.n, z);
}

static void show_arr(const char *who, struct WithArr x)
{
    printf("%-14s tag=%.6s len=%d\n", who, x.tag, x.len);
}

static long wide_sum(struct Wide x)
{
    return x.a + x.b * 10 + x.c * 100 + x.d * 1000 + x.e * 10000 + x.f * 100000;
}

static double fl_mix(struct Fl x)
{
    return x.d * 2.0 + x.f * 10.0f + x.n;
}

static long u_pick(union U x, int which)
{
    return which ? x.l : (long)x.i;
}

static struct Pair mk_pair(const char *p, int n)
{
    struct Pair r;
    r.p = p;
    r.n = n;
    return r;
}

static void show_nested(struct Pair a, struct WithArr b, struct Pair c)
{
    printf("%-14s %s/%d %s/%d %s/%d\n", "nested-split", a.p, a.n, b.tag, b.len, c.p,
        c.n);
}

int main(void)
{
    /* 1. auto 存储期对照 */
    struct Pair ap = { "auto", 7 };
    show_pair("auto", ap, "z");
    /* 2. static 文件作用域 (缺陷现场) */
    show_pair("static-file", sp, "z");
    /* 3. static const */
    show_pair("static-const", csp, "z");
    /* 4. global 外部链接 */
    show_pair("global", gp, "z");
    /* 5. 函数内 static */
    {
        static struct Pair fs = { "fnstatic", 17 };
        show_pair("static-fn", fs, "z");
    }
    /* 6. 含 char 数组成员 (纯值语义, 没有指针可依赖) */
    show_arr("static-arrmem", sa);
    struct WithArr wa = { "w-auto", 6 };
    show_arr("auto-arrmem", wa);
    /* 7. 跨 6 个整型寄存器的大聚合 (>16 字节, 还会溢出到栈) */
    printf("wide-static=%ld\n", wide_sum(sw));
    struct Wide wv = { 1, 2, 3, 4, 5, 6 };
    printf("wide-auto  =%ld\n", wide_sum(wv));
    /* 8. SSE + GP 混合类 */
    printf("fl-static=%.1f\n", fl_mix(sf));
    /* 9. union */
    printf("u-static=%ldL u-lowbits=%ld\n", u_pick(su, 1), u_pick(su, 0));
    /* 10. static 数组元素 (下标 lvalue, 存储期仍是 static) */
    show_pair("static-idx0", arr[0], "z");
    show_pair("static-idx2", arr[2], "z");
    int k = 1;
    show_pair("static-idxk", arr[k], "z");
    /* 11. 嵌套 static 聚合拆成员传参 (每个成员仍是 static lvalue) */
    show_nested(sn.p, sn.w, sp);
    /* 12. 返回值 (auto) 与 static 混在同一调用里 */
    show_pair("ret-then-static", mk_pair("ret", 3), sp.p);
    /* 13. static 聚合赋值给 auto 再传 (读侧也覆盖) */
    struct Pair cp = sp;
    show_pair("copy-of-static", cp, "z");

    printf("bad=%d\n", bad);
    return bad;
}
