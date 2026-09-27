/* va_copy 之后两份游标必须各自独立推进, 且副本对 SSE 类聚合的读取与原件逐值
 * 相同。
 *
 * 判据: SysV 的 va_list 是含 gp_offset/fp_offset/overflow_arg_area/reg_save_area
 * 的结构体, va_copy 是**整结构体赋值**。若只拷指针不拷游标, 或把 16 字节
 * [Sse,SseUp] 对象按 8 字节拷, cp 与 ap 的读出就会分叉。本夹具先让 cp 把全部
 * 参数读一遍, 再让 ap 从同一位置把同样的参数读第二遍, 两侧结果都必须等于
 * gcc -O0 的输出。
 *
 * 退出码 = 首个读错位置的累计读取序号 (低 8 位: cp 侧; 高位 ×100: ap 侧)。
 */
#include <stdarg.h>
#include <stdio.h>

typedef double v2df __attribute__((vector_size(16)));
struct sd { double a; double b; };
struct sid { int i; double d; };

static int scan(va_list ap, const char *tag, int base) {
    int k = 0;
    for (int i = 0; i < 3; i++) { struct sd s = va_arg(ap, struct sd); k++;
        printf("%s sd %d %.17g %.17g\n", tag, i, s.a, s.b);
        if (s.a != (double)(10 + i * 2) || s.b != (double)(11 + i * 2)) return base + k; }
    for (int i = 0; i < 4; i++) { v2df v = va_arg(ap, v2df); k++;
        printf("%s v2 %d %.17g %.17g\n", tag, i, (double)v[0], (double)v[1]);
        if (v[0] != (double)(20 + i * 2) || v[1] != (double)(21 + i * 2)) return base + k; }
    for (int i = 0; i < 2; i++) { struct sid s = va_arg(ap, struct sid); k++;
        printf("%s sid %d %d %.17g\n", tag, i, s.i, s.d);
        if (s.i != 30 + i * 2 || s.d != (double)(31 + i * 2)) return base + k; }
    for (int i = 0; i < 2; i++) { long double l = va_arg(ap, long double); k++;
        printf("%s ld %d %.21Lg\n", tag, i, l);
        if (l != (long double)(40 + i)) return base + k; }
    for (int i = 0; i < 3; i++) { int q = va_arg(ap, int); k++;
        printf("%s q %d %d\n", tag, i, q);
        if (q != 50 + i) return base + k; }
    return 0;
}

static int probe(int n, ...) {
    va_list ap, cp;
    int r;
    va_start(ap, n);
    va_copy(cp, ap);
    r = scan(cp, "cp", 0);
    if (r) { va_end(cp); va_end(ap); return r; }
    r = scan(ap, "ap", 100);
    va_end(cp);
    va_end(ap);
    return r;
}

int main(void) {
    struct sd s0 = {10, 11}, s1 = {12, 13}, s2 = {14, 15};
    struct sid i0 = {30, 31.0}, i1 = {32, 33.0};
    v2df v0 = {20, 21}, v1 = {22, 23}, v2 = {24, 25}, v3 = {26, 27};
    int r = probe(0, s0, s1, s2, v0, v1, v2, v3, i0, i1,
                  (long double)40, (long double)41, 50, 51, 52);
    printf("rc=%d\n", r);
    return r;
}
