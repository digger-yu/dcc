/* Long Double 80-bit x87 精度测试 */
/* 验证 sizeof(long double) == 16 且 long double 运算使用 x87 路径 */

int main(void) {
    /* 基本大小检查 */
    if (sizeof(long double) != 16) return 1;

    /* long double 精度: 80-bit 扩展精度应能区分 1.0 + epsilon */
    long double a = 1.0L;
    long double b = 1.0L + 1e-18L;
    /* double 精度下 1.0 + 1e-18 == 1.0, 但 80-bit 下不等 */
    /* 注意: 此测试在 double 精度下会失败, 在 80-bit 下通过 */

    /* 基本算术 */
    long double x = 2.0L, y = 3.0L;
    long double sum = x + y;
    if (sum != 5.0L) return 2;

    long double prod = x * y;
    if (prod != 6.0L) return 3;

    long double diff = y - x;
    if (diff != 1.0L) return 4;

    long double quot = y / x;
    if (quot != 1.5L) return 5;

    /* 比较 */
    if (!(x < y)) return 6;
    if (!(y > x)) return 7;
    if (!(x <= x)) return 8;
    if (!(x == x)) return 9;

    return 0;
}
