/* P4 F6: `_Imaginary long double` 二元算术全程走 x87,
 * 不得用 SSE addsd/mulsd 截断 80-bit tbyte。
 * dcc 旧实现: gen_imaginary_binop 无 x87 臂, ild 运算全部
 * 落到 addsd/mulsd + movsd, fldt 压进 st(0) 后被当 f64 读,
 * x87 栈也漏一对未配平的 fldt。
 */
typedef long double ld;

ld add_ild(_Imaginary long double b, _Imaginary long double c) {
    return b + c;
}

ld sub_ild(_Imaginary long double b, _Imaginary long double c) {
    return b - c;
}

ld mul_ild(_Imaginary long double b, _Imaginary long double c) {
    return b * c;
}

ld div_ild(_Imaginary long double b, _Imaginary long double c) {
    return b / c;
}
