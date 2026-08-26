/* _Imaginary 类型基本测试 (C99) */

/* 测试 _Imaginary double 声明 + sizeof */
int main(void) {
    /* _Imaginary double 大小应为 8 (同 double) */
    _Imaginary double a = 4.0i;
    if (sizeof(a) != 8) return 1;

    /* __imag__ 提取虚部 */
    double im = __imag__ a;
    if (im != 4.0) return 2;

    /* __real__ 返回 0 */
    double re = __real__ a;
    if (re != 0.0) return 3;

    /* _Imaginary float */
    _Imaginary float b = 2.5if;
    if (sizeof(b) != 4) return 4;
    if (__imag__ b != 2.5f) return 5;

    /* _Imaginary + _Imaginary = _Imaginary */
    _Imaginary double c = 3.0i;
    _Imaginary double d = a + c;
    if (__imag__ d != 7.0) return 6;

    /* _Imaginary - _Imaginary = _Imaginary */
    _Imaginary double e = a - c;
    if (__imag__ e != 1.0) return 7;

    /* 取负 */
    _Imaginary double f = -a;
    if (__imag__ f != -4.0) return 8;

    return 0;
}
