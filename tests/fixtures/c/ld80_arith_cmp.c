/*
 * 80-bit long double 算术/比较回归夹具。
 *
 * 判据的设计要点：**只有真 80-bit 运算才可能全部成立**。
 * `1.0L / 3.0L` 的两个操作数在 f64 与 80-bit 下都精确，但**商本身需要
 * 超过 53 位**才能表示，所以它直接区分"x87 80-bit"与"SSE f64 再拓宽"：
 *   - 真 80-bit 商 = 0.3333333333333333333363...（%.27Lf 打 333342368351）
 *   - f64 商       = 0.3333333333333333148296...
 * 二者可比较：80-bit 商严格大于 f64 商。
 *
 * 每项占 1 位，退出码把具体哪一项失败编码进去（不用布尔/sum-N，便于定位）。
 */
extern int printf(const char *, ...);

int main(void)
{
    long double one = 1.0L;
    long double three = 3.0L;
    long double q = one / three;
    /* f64 的 1/3：常量表达式里 (double) 运算保证舍到 53 位，再拓宽回 80-bit。
       两侧编译器对这一项的取值一致，因此它是干净的对照基准。 */
    double third64 = 1.0 / 3.0;
    long double third_widened = (long double)third64;

    long double s = 0.0L;
    int i;
    for (i = 0; i < 10; i++)
        s = s + q;

    long double m = q * q;

    /* 只依赖 80-bit 位宽的判据：significand 有 64 位，故 1 + 2^-63 可精确
       表示；f64 只有 53 位，1 + 2^-63 必舍回 1.0。2^63 是 2 的幂，两侧
       字面量都精确，因而不依赖十进制字面量解析精度。 */
    long double pow2_63 = 9223372036854775808.0L;   /* 2^63, 精确 */
    long double eps63 = one / pow2_63;              /* 2^-63, 精确 */
    long double sum63 = one + eps63;

    int r = 0;
    /* bit0: 80-bit 商严格大于 f64 商（f64 实现里两者相等 → 此位为 0） */
    if (q > third_widened) r |= 1;
    /* bit1: 1 + 2^-63 严格大于 1 —— 只有 64 位 significand 才成立 */
    if (sum63 > one) r |= 2;
    /* bit2: 累加保住了 80-bit 的低位（f64 累加结果不同） */
    if (s > 10.0L * third_widened) r |= 4;
    /* bit3: 自洽性护栏。注意这一项**不区分精度**（f64 下也为真），
       它只防“低位被整段丢失/写成 0”这类粗错，不作为 80-bit 证据。 */
    if (m > 0.0L && m < q) r |= 8;

    printf("r=%d q=%.27Lf s=%.27Lf m=%.27Lf sum63=%.27Lf\n",
           r, q, s, m, sum63);
    /* 期望 r=15；且各行数字与 gcc 逐字符一致（由差分 harness 比 stdout）。
       bit0/1/2 已单独验证有鉴别力：先把 q 过一遍 double 再比，对应位会变 0。 */
    return r == 15 ? 0 : 1;
}
