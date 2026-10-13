/* P4 F8: V5 防回归夹具 —— `conjl` / `_Complex long double` 取负与共轭
 * 是纯 x87 路径 (builtins.rs:1563, dispatch.rs 取负共轭分支)。
 * 本夹具在 O0/O1/O2/O3 四档下与 gcc 逐字节比对运行结果。
 *
 * 判据: 计算若干 cld/ild 组合的取负、共轭、四则、conjl,
 * 把虚/实部按整数化后逐位打印, 退出码 0 表示全对。
 */
#include <complex.h>
typedef __extension__ _Complex long double cld;
typedef __extension__ _Imaginary long double ild;

int main(void) {
    cld a = 3.25L + 4.5i;
    cld b = 1.25L + 2.5i;

    cld na = -a;
    if ((long long)__real__ na != -3) return 1;
    if ((long long)__imag__ na != -4) return 2;

    cld ca = conjl(a);
    if ((long long)__real__ ca != 3) return 3;
    if ((long long)__imag__ ca != -4) return 4;

    cld sum = a + b;
    if ((long long)__real__ sum != 4) return 5;
    if ((long long)__imag__ sum != 7) return 6;

    ild i = 2.5i;
    ild ni = -i;
    if ((long long)__imag__ ni != -2) return 7;

    return 0;
}
