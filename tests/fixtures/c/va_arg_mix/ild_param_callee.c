/* P4 F7 (V4): 具名 `_Complex long double` 形参走 ABI X87 类,
 * 与 long double 相同: 每参 16 字节栈槽、真 80-bit 编码、返回值在 st(0)。
 * 历史缺陷 (V4): `abi/tyclass.rs` 无 `Type::Imaginary` 臂, 把 ild 按 8B GP
 * 传入。gcc 不认 `_Imaginary` 关键字 (仅 _Complex), 这里用 cld 虚部非零、
 * 实部为 0 来表达"等价于 ild"的传参几何 —— ABI 归类与槽布局完全同型。
 */
typedef _Complex long double cld;

long long A, B;

/* caller 传 b=3.25i, c=2.5i (实部 0); 返回 __imag__ b + __imag__ c = 5.75 */
long double f(cld b, cld c) {
    A = (long long) __imag__ b;
    B = (long long) __imag__ c;
    return __imag__ b + __imag__ c;
}
