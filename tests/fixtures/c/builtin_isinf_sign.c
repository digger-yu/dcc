/* 回归: `__builtin_isinf` 与 `__builtin_isinf_sign` 的全宽度语义。
 *
 * GCC 语义（x86-64, gcc 13 实测）:
 *   __builtin_isinf(x)      : ±Inf 一律返回 1
 *   __builtin_isinf_sign(x) : +Inf→1, -Inf→-1, 其余→0
 *
 * 曾出现的缺陷:
 *   - float 的 abs 掩码误用 0x7FFF_FFFF_FFFF_FFFF（低 dword 全 1，清不掉
 *     bit31 符号位）→ -Inf 的 isinf 判 0；
 *   - double 的符号位是 movmskps lane1 bit31，不右移时取成 +1；
 *   - `_Float64x` 的 inf/nan 常量按 8 字节 double 落 xmm0，赋给 x87 通道
 *     时 `fstpt` 弹出空栈 → indefinite NaN（值为 -nan、signbit=1）；
 *   - long double / _Float128 的普通 isinf 误返回 -1。
 *
 * 失败码逐个打印，便于定位是哪个宽度 / 哪种谓词。
 */
#include <stdio.h>

static int fails;

static void ck(int cond, int code) {
    if (!cond) {
        printf("FAIL code=%d\n", code);
        fails++;
    }
}

int main(void) {
    volatile float finf = __builtin_inff(), fninf = -__builtin_inff();
    volatile double dinf = __builtin_inf(), dninf = -__builtin_inf();
    volatile long double linf = __builtin_infl(), lninf = -__builtin_infl();
    volatile _Float16 hinf = __builtin_inff16(), hninf = -__builtin_inff16();
    volatile _Float32x yinf = __builtin_inff32x(), yninf = -__builtin_inff32x();
    volatile _Float64x xinf = __builtin_inff64x(), xninf = -__builtin_inff64x();
    volatile _Float128 qinf = __builtin_inff128(), qninf = -__builtin_inff128();
    volatile float fone = 1.0f;
    volatile double fnan = __builtin_nan("");

    /* isinf_sign: ±1 与 0 */
    ck(__builtin_isinf_sign(finf) == 1, 1);
    ck(__builtin_isinf_sign(fninf) == -1, 2);
    ck(__builtin_isinf_sign(dinf) == 1, 3);
    ck(__builtin_isinf_sign(dninf) == -1, 4);
    ck(__builtin_isinf_sign(linf) == 1, 5);
    ck(__builtin_isinf_sign(lninf) == -1, 6);
    ck(__builtin_isinf_sign(hinf) == 1, 7);
    ck(__builtin_isinf_sign(hninf) == -1, 8);
    ck(__builtin_isinf_sign(yinf) == 1, 9);
    ck(__builtin_isinf_sign(yninf) == -1, 10);
    ck(__builtin_isinf_sign(xinf) == 1, 11);
    ck(__builtin_isinf_sign(xninf) == -1, 12);
    ck(__builtin_isinf_sign(qinf) == 1, 13);
    ck(__builtin_isinf_sign(qninf) == -1, 14);
    ck(__builtin_isinf_sign(fone) == 0, 15);
    ck(__builtin_isinf_sign(fnan) == 0, 16);

    /* 普通 isinf: ±Inf 恒 1 */
    ck(__builtin_isinf(finf) == 1, 21);
    ck(__builtin_isinf(fninf) == 1, 22);
    ck(__builtin_isinf(dinf) == 1, 23);
    ck(__builtin_isinf(dninf) == 1, 24);
    ck(__builtin_isinf(linf) == 1, 25);
    ck(__builtin_isinf(lninf) == 1, 26);
    ck(__builtin_isinf(hinf) == 1, 27);
    ck(__builtin_isinf(hninf) == 1, 28);
    ck(__builtin_isinf(yinf) == 1, 29);
    ck(__builtin_isinf(yninf) == 1, 30);
    ck(__builtin_isinf(xinf) == 1, 31);
    ck(__builtin_isinf(xninf) == 1, 32);
    ck(__builtin_isinf(qinf) == 1, 33);
    ck(__builtin_isinf(qninf) == 1, 34);
    ck(__builtin_isinf(fone) == 0, 35);
    ck(__builtin_isinf(fnan) == 0, 36);

    if (fails == 0) {
        printf("ok\n");
    }
    return fails != 0;
}
