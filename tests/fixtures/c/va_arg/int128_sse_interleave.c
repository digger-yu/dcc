/* `__int128`（2 个 GP eightbyte）与 SSE 类聚合（1~2 个 XMM 槽）**交替**读取:
 * GP 游标与 XMM 游标必须各自独立推进，且 `__int128` 装不进剩余 GP 保存区时
 * "整体上栈、gp_offset 不推进" 的规则不得因为中间插了 SSE 读取而被破坏。
 *
 * 单类型夹具看不到这类交互: 若把 XMM 的推进量误加到 gp_offset 上（或反之）,
 * 只有混合流才会暴露。布局（callee 有 1 个命名 GP 参数 → gp_offset 起点 8,
 * fp_offset 起点 48）:
 *   a  8..24  寄存器保存区   | s0 用 xmm0,xmm1
 *   b 24..40  寄存器保存区   | v0 用 xmm2
 *   c 40..    gp_offset=40 只剩 8 字节 → 整体上栈, gp_offset 仍为 40
 *   s1 用 xmm3,xmm4          | d 40.. 同样装不下 → 也整体上栈
 *   v1 用 xmm5               | e 紧随其后从 overflow_arg_area 读
 * 溢出区内 c/d/e 依次各占 16 字节（16 字节对齐, 无需额外 round-up）。
 *
 * 退出码 = 首个读错项的序号（0 = 全对）; 同时逐值打印供与 gcc 逐字节对照。
 */
#include <stdarg.h>
#include <stdio.h>

typedef double v2df __attribute__((vector_size(16)));
struct sd { double a; double b; };

static __int128 mk(long long lo, long long hi) {
    return ((__int128)hi << 64) | (unsigned long long)lo;
}
static long long lo_of(__int128 v) { return (long long)v; }
static long long hi_of(__int128 v) { return (long long)(v >> 64); }

static int probe(int n, ...) {
    va_list ap;
    int k = 0;
    va_start(ap, n);
    __int128 a = va_arg(ap, __int128); k++;
    printf("a %llx %llx\n", (unsigned long long)lo_of(a), (unsigned long long)hi_of(a));
    if (lo_of(a) != 0x111111111111LL || hi_of(a) != 0x222222222222LL) return k;
    struct sd s0 = va_arg(ap, struct sd); k++;
    printf("s0 %.17g %.17g\n", s0.a, s0.b);
    if (s0.a != 10.5 || s0.b != 11.25) return k;
    __int128 b = va_arg(ap, __int128); k++;
    printf("b %llx %llx\n", (unsigned long long)lo_of(b), (unsigned long long)hi_of(b));
    if (lo_of(b) != 0x333333333333LL || hi_of(b) != 0x444444444444LL) return k;
    v2df v0 = va_arg(ap, v2df); k++;
    printf("v0 %.17g %.17g\n", (double)v0[0], (double)v0[1]);
    if (v0[0] != 20.5 || v0[1] != 21.25) return k;
    /* 以下三个 128 位量都已越过 GP 保存区末端, 必须整体上栈且游标不双推进。 */
    __int128 c = va_arg(ap, __int128); k++;
    printf("c %llx %llx\n", (unsigned long long)lo_of(c), (unsigned long long)hi_of(c));
    if (lo_of(c) != 0x555555555555LL || hi_of(c) != (long long)0xCCCCCCCCCCCCCCCLL) return k;
    struct sd s1 = va_arg(ap, struct sd); k++;
    printf("s1 %.17g %.17g\n", s1.a, s1.b);
    if (s1.a != 12.5 || s1.b != 13.25) return k;
    __int128 d = va_arg(ap, __int128); k++;
    printf("d %llx %llx\n", (unsigned long long)lo_of(d), (unsigned long long)hi_of(d));
    if (lo_of(d) != 0x777777777777LL || hi_of(d) != (long long)0xDEADBEEFDEADBEEFULL) return k;
    v2df v1 = va_arg(ap, v2df); k++;
    printf("v1 %.17g %.17g\n", (double)v1[0], (double)v1[1]);
    if (v1[0] != 22.5 || v1[1] != 23.25) return k;
    __int128 e = va_arg(ap, __int128); k++;
    printf("e %llx %llx\n", (unsigned long long)lo_of(e), (unsigned long long)hi_of(e));
    if (lo_of(e) != 0x999999999999LL || hi_of(e) != (long long)0x0123456789ABCDEFLL) return k;
    va_end(ap);
    return 0;
}

int main(void) {
    struct sd s0 = {10.5, 11.25}, s1 = {12.5, 13.25};
    v2df v0 = {20.5, 21.25}, v1 = {22.5, 23.25};
    int r = probe(0,
                  mk(0x111111111111LL, 0x222222222222LL), s0,
                  mk(0x333333333333LL, 0x444444444444LL), v0,
                  mk(0x555555555555LL, (long long)0xCCCCCCCCCCCCCCCLL), s1,
                  mk(0x777777777777LL, (long long)0xDEADBEEFDEADBEEFULL), v1,
                  mk(0x999999999999LL, 0x0123456789ABCDEFLL));
    printf("rc=%d\n", r);
    return r;
}
