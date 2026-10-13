/* P4 F7 caller (gcc 侧).
 *
 * 判据逐项返回不同非零码, 便于区分"哪一处错", 不用 sum-N。
 *
 * caller 侧给 f(3.25i, 2.5i) → cld 走 16 字节栈槽, 内容真 80-bit 编码。
 * callee 内部把 __imag__ b / __imag__ c 的整数部分回写到全局 A/B,
 * 并返回 __imag__ b + __imag__ c = 5.75 (在 st(0))。
 */
typedef _Complex long double cld;
extern long long A, B;
extern long double f(cld, cld);

int main(void) {
    cld b = 0.0L + 3.25i;
    cld c = 0.0L + 2.5i;
    long double r = f(b, c);
    long long ri = (long long) r;
    int frac = (int)(r * 100.0L) - (int)ri * 100;

    if (A != 3)
        return 1;   /* 第 1 个 cld 形参读错 */
    if (B != 2)
        return 2;   /* 第 2 个 cld 形参读错 */
    if (ri != 5)
        return 3;   /* 返回值整数部分错 → st(0) 返回约定不符 */
    if (frac != 75)
        return 4;   /* 返回值小数部分错 */
    return 0;
}
