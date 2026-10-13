/* P4 F5: `_Imaginary long double` 取负必须走 x87 `fchs`,
 * 而非 SSE `xorpd`(会截断 80-bit tbyte + 留未配平的 fldt)。
 * dcc 旧实现: `_Imaginary` 的取负在 dispatch.rs 被 `!matches!(e_ty,
 * Type::Imaginary(_))` 排除在 st(0) 通道之外, 走 xorpd on stale xmm0。
 */
typedef long double ld;

ld neg_ild(_Imaginary long double b) {
    return -b;
}

ld neg_ild2(_Imaginary long double b, _Imaginary long double c) {
    return -(b + c);
}
