/* _Complex 字面量 (4.0i) + __real__/__imag__ 关键字 (Section 9) */
#include <complex.h>

int main(void) {
    /* 虚数字面量初始化: 实部 0, 虚部 4 */
    double _Complex z = 4.0i;
    if (__real__ z != 0.0) return 1;
    if (__imag__ z != 4.0) return 2;
    /* 标量 __real__: 返回自身; __imag__ 标量: 0 */
    if (__real__ 3.5 != 3.5) return 3;
    if (__imag__ 2.0 != 0.0) return 4;
    /* _Complex_I 宏 (complex.h): 1.0iF → _Complex float */
    float _Complex w = _Complex_I;
    if (__real__ w != 0.0f) return 5;
    if (__imag__ w != 1.0f) return 6;
    /* 虚数字面量参与算术: (0+4i)*(0+1i) = -4+0i */
    double _Complex p = z * 1.0i;
    if (__real__ p != -4.0) return 7;
    if (__imag__ p != 0.0) return 8;
    /* 一元负: -(0+4i) = 0-4i */
    double _Complex n = -z;
    if (__real__ n != 0.0) return 9;
    if (__imag__ n != -4.0) return 10;
    /* 整数虚数: 3j → _Complex int */
    _Complex int ci = 3j;
    if (__real__ ci != 0) return 11;
    if (__imag__ ci != 3) return 12;
    /* __imag__ 作用于非 lvalue 表达式 */
    if (__imag__ (z + z) != 8.0) return 13;
    if (__real__ (z * 2.0) != 0.0) return 14;
    /* 赋值路径 (16 字节与 8 字节 _Complex) */
    double _Complex d2;
    d2 = 4.0i;
    if (__imag__ d2 != 4.0) return 15;
    float _Complex f2;
    f2 = 2.5if;
    if (__real__ f2 != 0.0f) return 16;
    if (__imag__ f2 != 2.5f) return 17;
    /* 标量初始化 complex 变量: 虚部为 0 */
    double _Complex s = 7.0;
    if (__real__ s != 7.0) return 18;
    if (__imag__ s != 0.0) return 19;
    return 0;
}
