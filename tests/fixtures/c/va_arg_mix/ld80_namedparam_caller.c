/* ld80_namedparam_callee.c 的调用方（gcc 侧）。
 *
 * 判据逐项返回不同非零码，便于区分"哪一处错"，不用 sum-N
 * （mod-256 会别名成 0）。
 *
 * ⚠ 三项判据缺一不可。上一轮版本只查全局 A/B、**从不检查返回值**，于是
 * 删掉 callee 里那条错误的隐藏指针 sret 存储之后四个方向全部为 0，看起来
 * 像修好了，实际 `long double` 返回约定（SysV 要求 st(0)）仍然错着。
 * 现在 3/4 两项专门盯返回值的整数部分与小数部分。
 */
extern long long A, B;
extern long double f(long double, long double);

int main(void) {
    long double r = f(3.25L, 2.5L);
    /* 3.25 + 2.5 = 5.75 */
    long long ri = (long long) r;
    int frac = (int) (r * 100.0L) - (int) ri * 100;

    if (A != 3)
        return 1; /* 第 1 个具名形参读错 */
    if (B != 2)
        return 2; /* 第 2 个具名形参读错 */
    if (ri != 5)
        return 3; /* 返回值的整数部分错 → st(0) 返回约定不符 */
    if (frac != 75)
        return 4; /* 返回值小数部分错 */
    return 0;
}
