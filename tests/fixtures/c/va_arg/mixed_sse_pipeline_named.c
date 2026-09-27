/* 命名 va_list 上长串混合 SSE 类聚合的读取流水线: double / struct{double,double}
 * / __m128d 向量 / v4sf / struct{int,double} / struct{double,double,double}
 * / long double / int 交替, 逐值打印并与 gcc -O0 的 stdout 逐字节对照.
 *
 * 覆盖根因轴: "分类计数 → 槽位分配 → reg_save_area 写入 → va_arg 读取" 四条轴
 * 必须共用同一个分类真源。任一侧按"书写形态"(是否 struct/是否向量) 而非
 * eightbyte 分类决定槽数, 就会在某一个类型上错位, 而单一类型的夹具看不出来
 * (游标偏移错误只在混合流里累积暴露)。
 *
 * 关键分类差异 (gcc -O0 实测):
 *   struct sd {double a; double b;} → [Sse, Sse]   占 2 个 XMM 寄存器 / 2 个栈槽
 *   v2df = __m128d                 → [Sse, SseUp] 占 1 个 XMM 寄存器
 *   struct s3 {double a,b,c;}      → [Sse, Sse, Sse] 占 3 个
 *   struct sid {int i; double d;}  → [Integer, Sse] GP/XMM 各推进一格
 *   long double                    → MEMORY, 只经 overflow_arg_area
 * 退出码 = 首个读错位置的累计读取序号(0 = 全对)。
 */
#include <stdarg.h>
#include <stdio.h>

typedef double v2df __attribute__((vector_size(16)));
typedef float v4sf __attribute__((vector_size(16)));
struct sd { double a; double b; };
struct sid { int i; double d; };
struct s3 { double a, b, c; };

static int probe(int n, ...) {
    va_list ap;
    int k = 0;
    va_start(ap, n);
    for (int i = 0; i < 6; i++) { double d = va_arg(ap, double); k++;
        printf("d %d %.17g\n", i, d); if (d != (double)(i + 1)) return k; }
    for (int i = 0; i < 3; i++) { struct sd s = va_arg(ap, struct sd); k++;
        printf("sd %d %.17g %.17g\n", i, s.a, s.b);
        if (s.a != (double)(10 + i * 2) || s.b != (double)(11 + i * 2)) return k; }
    for (int i = 0; i < 4; i++) { v2df v = va_arg(ap, v2df); k++;
        printf("v2 %d %.17g %.17g\n", i, (double)v[0], (double)v[1]);
        if (v[0] != (double)(20 + i * 2) || v[1] != (double)(21 + i * 2)) return k; }
    for (int i = 0; i < 2; i++) { v4sf f = va_arg(ap, v4sf); k++;
        printf("vf %d %.7g %.7g %.7g %.7g\n", i, (double)f[0], (double)f[1], (double)f[2], (double)f[3]);
        if (f[0] != (float)(30 + i * 4) || f[3] != (float)(33 + i * 4)) return k; }
    for (int i = 0; i < 2; i++) { struct sid s = va_arg(ap, struct sid); k++;
        printf("sid %d %d %.17g\n", i, s.i, s.d);
        if (s.i != 40 + i * 2 || s.d != (double)(41 + i * 2)) return k; }
    for (int i = 0; i < 2; i++) { struct s3 s = va_arg(ap, struct s3); k++;
        printf("s3 %d %.17g %.17g %.17g\n", i, s.a, s.b, s.c);
        if (s.a != (double)(50 + i * 3) || s.c != (double)(52 + i * 3)) return k; }
    for (int i = 0; i < 3; i++) { long double l = va_arg(ap, long double); k++;
        printf("ld %d %.21Lg\n", i, l); if (l != (long double)(60 + i)) return k; }
    for (int i = 0; i < 4; i++) { int q = va_arg(ap, int); k++;
        printf("q %d %d\n", i, q); if (q != 70 + i) return k; }
    va_end(ap);
    return 0;
}

int main(void) {
    struct sd s0 = {10, 11}, s1 = {12, 13}, s2 = {14, 15};
    struct sid i0 = {40, 41.0}, i1 = {42, 43.0};
    struct s3 t0 = {50, 51, 52}, t1 = {53, 54, 55};
    v2df v0 = {20, 21}, v1 = {22, 23}, v2 = {24, 25}, v3 = {26, 27};
    v4sf f0 = {30, 31, 32, 33}, f1 = {34, 35, 36, 37};
    int r = probe(0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, s0, s1, s2,
                  v0, v1, v2, v3, f0, f1, i0, i1, t0, t1,
                  (long double)60, (long double)61, (long double)62,
                  70, 71, 72, 73);
    printf("rc=%d\n", r);
    return r;
}
