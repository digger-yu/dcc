/* 回归：`ptr - ptr` 的定型必须是整数（C11 6.5.6p9）。
 *
 * 修复前 dcc 把 `top - old` 记成 `Ptr`，于是 `(top - old) + nw` 又按
 * 元素大小（16B）缩放一次，`r` 落到 buf + 5 而不是 buf + 3。
 */
#include <stdio.h>

typedef struct { long a, b; } V;   /* sizeof = 16 */

int main(void) {
    V buf[4];
    V *top = buf + 2, *old = buf, *nw = buf + 1;
    V *r = top - old + nw;         /* 必须是 buf + 3 */
    long idx = (long)(r - buf);
    printf("%ld\n", idx);
    return idx == 3 ? 0 : 1;
}
