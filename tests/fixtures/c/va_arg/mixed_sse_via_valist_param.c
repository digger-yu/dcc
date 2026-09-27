/* 与 mixed_sse_pipeline_named.c 同一批混合 SSE 类对象, 但 va_list 经**形参**
 * 传入 reader(va_list ap), 即已退化为指针的落点 (rcx 基址)。
 *
 * 为什么必须单独一份: va_list 有三种落点 (命名局部栈槽 / 全局内存 / 已退化为
 * 指针), 历史上它们各持一份 ABI 判定与装载体的逐字副本, 于是"修好 named 却
 * 让 ptr 静默失效"。现在三处共用 emit_va_arg_agg_body, 本夹具就是这条收敛的
 * 回归护栏: 同一串变参在两种落点下的读出结果必须与 gcc 逐一相同。
 *
 * 退出码 = 首个读错位置的累计读取序号 (0 = 全对); stdout 逐值打印供与 gcc
 * 逐字节对照。
 */
#include <stdarg.h>
#include <stdio.h>

typedef double v2df __attribute__((vector_size(16)));
struct sd { double a; double b; };
struct sid { int i; double d; };

static int reader(va_list ap) {
    int k = 0;
    for (int i = 0; i < 5; i++) { double d = va_arg(ap, double); k++;
        printf("d %d %.17g\n", i, d); if (d != (double)(i + 1)) return k; }
    for (int i = 0; i < 2; i++) { struct sd s = va_arg(ap, struct sd); k++;
        printf("sd %d %.17g %.17g\n", i, s.a, s.b);
        if (s.a != (double)(10 + i * 2) || s.b != (double)(11 + i * 2)) return k; }
    for (int i = 0; i < 3; i++) { v2df v = va_arg(ap, v2df); k++;
        printf("v2 %d %.17g %.17g\n", i, (double)v[0], (double)v[1]);
        if (v[0] != (double)(20 + i * 2) || v[1] != (double)(21 + i * 2)) return k; }
    for (int i = 0; i < 2; i++) { struct sid s = va_arg(ap, struct sid); k++;
        printf("sid %d %d %.17g\n", i, s.i, s.d);
        if (s.i != 30 + i * 2 || s.d != (double)(31 + i * 2)) return k; }
    for (int i = 0; i < 2; i++) { long double l = va_arg(ap, long double); k++;
        printf("ld %d %.21Lg\n", i, l); if (l != (long double)(40 + i)) return k; }
    return 0;
}

static int probe(int n, ...) {
    va_list ap;
    va_start(ap, n);
    int r = reader(ap);
    va_end(ap);
    return r;
}

int main(void) {
    struct sd s0 = {10, 11}, s1 = {12, 13};
    struct sid i0 = {30, 31.0}, i1 = {32, 33.0};
    v2df v0 = {20, 21}, v1 = {22, 23}, v2 = {24, 25};
    int r = probe(0, 1.0, 2.0, 3.0, 4.0, 5.0, s0, s1, v0, v1, v2, i0, i1,
                  (long double)40, (long double)41);
    printf("rc=%d\n", r);
    return r;
}
