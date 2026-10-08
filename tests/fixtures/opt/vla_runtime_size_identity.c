#include <stdarg.h>
#include <stdio.h>
static int size;
static int probe(int n, ...) {
    va_list ap; __builtin_va_start(ap, n);
    struct { char data[size]; } d;
    d = __builtin_va_arg(ap, __typeof__(d));
    __builtin_va_end(ap);
    int s = 0;
    for (int i = 0; i < size; i++) s += (unsigned char)d.data[i];
    return s + (int)(sizeof d);
}
int main(void) {
    size = 5;
    struct { char data[size]; } a;
    for (int i = 0; i < size; i++) a.data[i] = (char)(i + 1);
    int local = (int)(sizeof a);
    int one = (int)a.data[4];
    int got = probe(1, a);
    int plain_n = 4;
    long arr[plain_n];
    for (int i = 0; i < plain_n; i++) arr[i] = i;
    arr[2] = 7;
    long psz = (long)(sizeof arr);
    long psum = 0;
    for (int i = 0; i < plain_n; i++) psum += arr[i];
    printf("l=%d o=%d g=%d sz=%ld sum=%ld\n", local, one, got, psz, psum);
    return (local == 5 && one == 5 && got == 20 && psz == 32 && psum == 11) ? 0 : 1;
}

/* 判据说明（为什么把 sizeof、元素访问、按值传参放在同一份源里）：
 * VLA/VM 对象的帧槽是**元数据槽**，槽内容才是动态数据区基址。旧形状把基址算成
 * `lea <槽>`（描述符自己），于是读写都落回固定帧区——两侧同错就**自洽**，
 * 「写完再读回」的夹具看不出来（`tests/fixtures/c/vla.c` 正是这种）。
 * 只有把 `sizeof a`（读槽里那个 size 字段）与元素写（同一区域）放在一起，
 * 或者把整个聚合作为变参实参传出去（调用侧要读槽内容当指针），错值才现形。
 *
 * 修前实测（同一份源，两个阶段分开记）：
 *   开工基线（四档均坑）：-O0/-O1/-O2/-O3 全部 **SIGSEGV(139)**；
 *   只修消费侧基址、还没让带 VLA/VM 聚合的函数退出 SSA 时：-O0/-O1 已经对
 *   （`l=5 o=5 g=20 sz=32 sum=11`），而 -O2/-O3 的 `sizeof a` 读回 0（同一形状在
 *   不带 `probe` 时给出 252 而非 255），带 `probe(1, a)` 的形状直接 139。
 * 修后四档与 gcc 逐字节同 stdout、同退出码 0。
 */
