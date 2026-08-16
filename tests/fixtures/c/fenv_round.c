/* FENV_ACCESS 语义 (Section 11.2)
 * #pragma STDC FENV_ACCESS ON 时, 浮点运算必须在运行时按当前舍入模式
 * 求值 (opt 阶段不得编译期折叠), 且 fesetround 等调用不被 -O2 丢弃.
 * 返回 0 表示向下/向上舍入的 1.0/3.0 结果不同 (舍入模式生效). */
#pragma STDC FENV_ACCESS ON
#include <fenv.h>

int main(void) {
    fesetround(FE_DOWNWARD);
    double down = 1.0 / 3.0;
    fesetround(FE_UPWARD);
    double up = 1.0 / 3.0;
    fesetround(FE_TONEAREST);
    return down != up ? 0 : 1;
}
