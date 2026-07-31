/* fenv.h 函数 pass-through (Section 11.2)
 * 验证 feclearexcept / fegetround / fesetround / feraiseexcept
 * / fetestexcept 等被 dcc 发射为外部符号 (走 libm / libc).
 * 编译为 -c 后, 用 nm 检查符号表中包含 fenv 函数名. */
#include <fenv.h>

int test_fenv(void) {
    feclearexcept(FE_ALL_EXCEPT);
    int r = fegetround();
    fesetround(FE_TOWARDZERO);
    fesetround(r);
    feraiseexcept(FE_OVERFLOW);
    int e = fetestexcept(FE_OVERFLOW);
    feclearexcept(FE_ALL_EXCEPT);
    return e;
}
