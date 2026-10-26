/* 回归：`i + p` 的缩放必须落在索引上，而不是指针上。
 *
 * 修复前 dcc 的 `imul` 恒作用于右操作数，`i + buf` 把 buf 乘了 16；
 * 修复方式是在 `Add` 且左非指针、右指针时对调操作数。
 */
#include <stdio.h>

typedef struct { long a, b; } V;

int main(void) {
    V buf[4];
    int i = 2;
    long k = 1;
    V *p = i + buf;                /* 指针在右 */
    V *q = buf + k;
    V *r = buf - k + 3;
    printf("%ld %ld %ld\n", (long)(p - buf), (long)(q - buf), (long)(r - buf));
    return (p - buf == 2 && q - buf == 1 && r - buf == 2) ? 0 : 1;
}
