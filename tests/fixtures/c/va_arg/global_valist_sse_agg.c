/* 全局 va_list + 混合 SSE 类聚合: va_start 在 callee 里写真全局内存, va_arg 在
 * **另一个函数**里读, 且读的是 [Sse,Sse] / [Sse,SseUp] / [Integer,Sse] /
 * MEMORY(long double) 四类混合。
 *
 * 覆盖两种漂移:
 *   1. 落点漂移 —— 全局 va_list 的四字段基址是 rip 相对全局量 (r10), 命名版是
 *      [r13+槽]; 曾出现 va_start 写真全局、va_arg 读函数私有懒分配栈槽。
 *   2. 副本漂移 —— 全局版与命名版各持一份溢出/寄存器判定与装载代码, 单点修复
 *      会在另一份里静默失效; 现两处共用 emit_va_arg_agg_body。
 * 另外本夹具的读取跨越 XMM 保存区边界 (8 个槽用完后转入 overflow_arg_area),
 * 因而同时钉住"按 XMM 寄存器序号计数"的判据。
 *
 * 退出码 = 首个读错位置的累计读取序号 (0 = 全对)。
 */
#include <stdarg.h>
#include <stdio.h>

typedef double v2df __attribute__((vector_size(16)));
struct sd { double a; double b; };
struct sid { int i; double d; };

static va_list gap;

static int reader(void) {
    int k = 0;
    for (int i = 0; i < 3; i++) { struct sd s = va_arg(gap, struct sd); k++;
        printf("sd %d %.17g %.17g\n", i, s.a, s.b);
        if (s.a != (double)(10 + i * 2) || s.b != (double)(11 + i * 2)) return k; }
    for (int i = 0; i < 5; i++) { v2df v = va_arg(gap, v2df); k++;
        printf("v2 %d %.17g %.17g\n", i, (double)v[0], (double)v[1]);
        if (v[0] != (double)(20 + i * 2) || v[1] != (double)(21 + i * 2)) return k; }
    for (int i = 0; i < 2; i++) { struct sid s = va_arg(gap, struct sid); k++;
        printf("sid %d %d %.17g\n", i, s.i, s.d);
        if (s.i != 30 + i * 2 || s.d != (double)(31 + i * 2)) return k; }
    for (int i = 0; i < 4; i++) { double d = va_arg(gap, double); k++;
        printf("d %d %.17g\n", i, d); if (d != (double)(40 + i)) return k; }
    for (int i = 0; i < 2; i++) { long double l = va_arg(gap, long double); k++;
        printf("ld %d %.21Lg\n", i, l); if (l != (long double)(50 + i)) return k; }
    for (int i = 0; i < 2; i++) { struct sd s = va_arg(gap, struct sd); k++;
        printf("sd2 %d %.17g %.17g\n", i, s.a, s.b);
        if (s.a != (double)(60 + i * 2) || s.b != (double)(61 + i * 2)) return k; }
    return 0;
}

static int callee(int n, ...) {
    va_start(gap, n);
    int r = reader();
    va_end(gap);
    return r;
}

int main(void) {
    struct sd s0 = {10, 11}, s1 = {12, 13}, s2 = {14, 15};
    struct sd t0 = {60, 61}, t1 = {62, 63};
    struct sid i0 = {30, 31.0}, i1 = {32, 33.0};
    v2df v0 = {20, 21}, v1 = {22, 23}, v2 = {24, 25}, v3 = {26, 27}, v4 = {28, 29};
    int r = callee(0, s0, s1, s2, v0, v1, v2, v3, v4, i0, i1,
                   40.0, 41.0, 42.0, 43.0,
                   (long double)50, (long double)51, t0, t1);
    printf("rc=%d\n", r);
    return r;
}
