/* 回归：整数字面量按「后缀定起点、大小定落点」定型（C11 6.4.4.1）。
 *
 * 修复前 `0xffffffffffffffffu` 只按 `u` 后缀定成 `unsigned int`（4 字节），
 * `trim64(x)` 于是只剩低 32 位：`b` 变成 0 而不是 8，`rotl` 结果错。
 */
#include <stdio.h>

#define trim64(x) ((x) & 0xffffffffffffffffu)

int main(void) {
    unsigned long long x = 0x1122334455667788ULL;
    unsigned long long a = trim64(x);
    unsigned long long b = trim64(x) >> 57;
    unsigned long long c = x << 7;
    unsigned long long d = c | b;
    printf("x=%016llx a=%016llx b=%016llx c=%016llx d=%016llx\n", x, a, b, c, d);
    unsigned long long u = 0xffffffffffffffffu;
    unsigned long long e = x & u;
    printf("u=%016llx e=%016llx sizeof=%zu\n", u, e, sizeof(0xffffffffffffffffu));
    return (a == x && b == 8 && e == x && sizeof(0xffffffffffffffffu) == 8) ? 0 : 1;
}
