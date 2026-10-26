/* 回归：`#if` 按 `intmax_t`/`uintmax_t` 求值（C11 6.10.1p4）。
 *
 * 修复前 dcc 整条表达式链走 `i64`，`ULONG_MAX` 左移 31 再左移 31 溢出成 0，
 * `((ULONG_MAX >> 31) >> 31) >= 3` 判假，`lmathlib.c` 因此把 `Rand64` 定成
 * `unsigned long long` 而不是 `unsigned long`，`math.random` 序列与 gcc 不同。
 */
#include <limits.h>
#include <stdio.h>

int main(void) {
#if ((ULONG_MAX >> 31) >> 31) >= 3
    printf("branch1 (Rand64 = unsigned long)\n");
#elif !defined(LUA_USE_C89) && defined(LLONG_MAX)
    printf("branch2 (Rand64 = unsigned long long)\n");
#else
    printf("branch3\n");
#endif
#if 0xffffffffffffffffu > 0
    printf("mask64 gt0: yes\n");
#else
    printf("mask64 gt0: no\n");
#endif
    return 0;
}
