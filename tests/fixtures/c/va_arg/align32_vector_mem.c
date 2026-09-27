/* 对齐 **大于 16 字节** 的变参对象必须整体走内存 (SysV ABI 3.2.3p2 + GCC 4.6
 * 起的 ABI 变更, `classify_argument` 里的 `if (align > 128) return CLASS_MEMORY`)。
 *
 * 产物: `__attribute__((vector_size(32)))` 的 v4df / v8sf 这类 AVX 向量, 以及
 * 显式 `__attribute__((aligned(32)))` 的结构。若把它们按 16 字节对齐对象分类成
 * `[Sse, SseUp...]`, 调用方只会存低 16 字节、`va_arg` 也只消耗一个 XMM 槽,
 * 高 16 字节永久丢失。gcc -O0 实测: 每个此类参数在 overflow_arg_area 上先
 * `addq $31; andq $-32` 对齐, 再占 32 字节。
 *
 * 同时验证 XMM 寄存器计数不被这些"看似 SSE"的对象污染: 中间的 v2df/struct sd
 * 仍按 [Sse,SseUp]/[Sse,Sse] 正常用槽。
 *
 * 退出码 = 首个读错项的序号 (0 = 全对)。
 */
#include <stdarg.h>
#include <stdio.h>

typedef double v2df __attribute__((vector_size(16)));
typedef double v4df __attribute__((vector_size(32)));
typedef float v8sf __attribute__((vector_size(32)));
struct al32 { long long a; double b; } __attribute__((aligned(32)));
struct sd { double a; double b; };

static int probe(int n, ...) {
    va_list ap;
    int k = 0;
    va_start(ap, n);
    for (int i = 0; i < 3; i++) {
        v4df v = va_arg(ap, v4df); k++;
        printf("v4df %d %.17g %.17g %.17g %.17g\n", i,
               (double)v[0], (double)v[1], (double)v[2], (double)v[3]);
        if (v[0] != (double)(i * 4 + 1) || v[1] != (double)(i * 4 + 2)
            || v[2] != (double)(i * 4 + 3) || v[3] != (double)(i * 4 + 4)) return k;
        struct sd s = va_arg(ap, struct sd); k++;
        printf("sd %d %.17g %.17g\n", i, s.a, s.b);
        if (s.a != (double)(10 + i * 2) || s.b != (double)(11 + i * 2)) return k;
    }
    for (int i = 0; i < 2; i++) {
        v8sf f = va_arg(ap, v8sf); k++;
        printf("v8sf %d %.7g %.7g\n", i, (double)f[0], (double)f[7]);
        if (f[0] != (float)(20 + i * 8) || f[7] != (float)(27 + i * 8)) return k;
        struct al32 a = va_arg(ap, struct al32); k++;
        printf("al32 %d %lld %.17g\n", i, a.a, a.b);
        if (a.a != 30 + i * 2 || a.b != (double)(31 + i * 2)) return k;
    }
    for (int i = 0; i < 2; i++) { double d = va_arg(ap, double); k++;
        printf("d %d %.17g\n", i, d); if (d != (double)(40 + i)) return k; }
    va_end(ap);
    return 0;
}

int main(void) {
    v4df q0 = {1, 2, 3, 4}, q1 = {5, 6, 7, 8}, q2 = {9, 10, 11, 12};
    v8sf f0 = {20, 21, 22, 23, 24, 25, 26, 27};
    v8sf f1 = {28, 29, 30, 31, 32, 33, 34, 35};
    struct sd s0 = {10, 11}, s1 = {12, 13}, s2 = {14, 15};
    struct al32 a0; a0.a = 30; a0.b = 31.0;
    struct al32 a1; a1.a = 32; a1.b = 33.0;
    int r = probe(0, q0, s0, q1, s1, q2, s2, f0, a0, f1, a1, 40.0, 41.0);
    printf("rc=%d\n", r);
    return r;
}
