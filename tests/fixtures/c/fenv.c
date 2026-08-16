/* fenv.h 运行时调用 (Section 11.2)
 * 验证 feclearexcept / fegetround / fesetround / feraiseexcept
 * / fetestexcept 等被 dcc 内联为 MXCSR 操作 (不依赖 libm/libc 的
 * fe* 符号, host 链接无需 -lm). 编译运行后返回 0 表示语义正确. */
#include <fenv.h>

int test_fenv(void) {
    feclearexcept(FE_ALL_EXCEPT);
    int r = fegetround();
    fesetround(FE_TOWARDZERO);
    if (fegetround() != FE_TOWARDZERO) {
        return 1;
    }
    fesetround(r);
    feraiseexcept(FE_OVERFLOW);
    int e = fetestexcept(FE_OVERFLOW);
    feclearexcept(FE_ALL_EXCEPT);
    return e == FE_OVERFLOW ? 0 : 1;
}

int main(void) {
    return test_fenv();
}
