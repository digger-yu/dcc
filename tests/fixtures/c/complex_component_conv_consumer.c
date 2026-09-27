/* _Complex float / _Complex double 的跨 TU 互操作夹具: 消费者侧。
 *
 * 与 complex_component_conv_provider.c 配对: 每个分量都乘以 8 (全部
 * 取值都是二进制可精确表示的), 于是打印出来的整数就是分量真值的指纹,
 * 不需要 `%f` 格式化, 也不依赖 libc 的复数函数。
 * 打印逐字节对照 gcc 产物 + 退出码 = 失败断言计数。
 */
#include <stdio.h>

_Complex float cf_id(_Complex float z);
_Complex double cd_id(_Complex double z);
_Complex double cf_to_cd(_Complex float z);
_Complex float cd_to_cf(_Complex double z);
_Complex double int_to_cd(int n);
_Complex float int_to_cf(int n);
_Complex float cf_scale(_Complex float z, float k);
_Complex double cd_add(_Complex double a, _Complex double b);
_Complex double cd_after_seven_ints(int a, int b, int c, int d,
                                    int e, int f, int g,
                                    _Complex double z);
long cd_mixed(_Complex double z, double k, long tag);
_Complex double cd_spill_readback(_Complex double z);
_Complex float cf_spill_readback(_Complex float z);
extern _Complex double g_cd[4];
extern _Complex float g_cf[3];
typedef _Complex double (*cd_fn)(double _Complex);
cd_fn cd_selector(void);

static long r8(float _Complex z) {
    return (long)(__real__ z * 8.0f);
}

static long i8(float _Complex z) {
    return (long)(__imag__ z * 8.0f);
}

static long d8(double _Complex z) {
    return (long)(__real__ z * 8.0);
}

static long e8(double _Complex z) {
    return (long)(__imag__ z * 8.0);
}

int main(void) {
    int bad = 0;
    _Complex float fa;
    _Complex double da;
    __real__ fa = 1.25f;
    __imag__ fa = -2.5f;
    __real__ da = 3.5;
    __imag__ da = -4.25;

    /* 恒等搬运 */
    _Complex float fb = cf_id(fa);
    _Complex double db = cd_id(da);
    if (__real__ fb != 1.25f || __imag__ fb != -2.5f) bad++;
    if (__real__ db != 3.5 || __imag__ db != -4.25) bad++;
    printf("id %ld %ld %ld %ld\n", r8(fb), i8(fb), d8(db), e8(db));

    /* 分量转换 float -> double (实 *2, 虚 *3) */
    _Complex double up = cf_to_cd(fa);
    if (__real__ up != 2.5 || __imag__ up != -7.5) bad++;
    printf("cf_to_cd %ld %ld\n", d8(up), e8(up));

    /* 分量转换 double -> float (实 = re+im, 虚 = re-im) */
    _Complex float dn = cd_to_cf(da);
    if (__real__ dn != -0.75f || __imag__ dn != 7.75f) bad++;
    printf("cd_to_cf %ld %ld\n", r8(dn), i8(dn));

    /* 整型 -> 复数: 虚部必须为 0 */
    _Complex double from_i = int_to_cd(-7);
    _Complex float from_f = int_to_cf(5);
    if (__imag__ from_i != 0.0 || __real__ from_i != -7.0) bad++;
    if (__imag__ from_f != 0.0f || __real__ from_f != 5.0f) bad++;
    printf("int_to %ld %ld %ld %ld\n", d8(from_i), e8(from_i), r8(from_f),
           i8(from_f));

    /* 复数算术 */
    _Complex float sc = cf_scale(fa, 2.0f);
    _Complex double ad = cd_add(da, 1.5 + 2.25i);
    if (__real__ sc != 2.5f || __imag__ sc != -5.0f) bad++;
    if (__real__ ad != 5.0 || __imag__ ad != -2.0) bad++;
    printf("arith %ld %ld %ld %ld\n", r8(sc), i8(sc), d8(ad), e8(ad));

    /* 7 个整型形参之后的 SSE 对 */
    int s = 1 + 2 + 3 + 4 + 5 + 6 + 7;
    _Complex double after = cd_after_seven_ints(1, 2, 3, 4, 5, 6, 7, da);
    if (__real__ after != 3.5 + s || __imag__ after != -4.25 - s) bad++;
    printf("after_ints %ld %ld\n", d8(after), e8(after));

    /* 混合类别形参: (long)(3.5*2 + (-4.25)*2) = (long)(-1.5) = -1 (向零截断) */
    long m = cd_mixed(da, 2.0, 100);
    if (m != 99) bad++;
    printf("mixed %ld\n", m);

    /* callee 内 spill 后按分量回读 */
    _Complex double sp = cd_spill_readback(da);
    _Complex float spf = cf_spill_readback(fa);
    if (__real__ sp != 7.0 || __imag__ sp != -8.5) bad++;
    if (__real__ spf != -1.25f || __imag__ spf != 3.75f) bad++;
    printf("spill %ld %ld %ld %ld\n", d8(sp), e8(sp), r8(spf), i8(spf));

    /* 跨 TU 全局复数数组 */
    for (int i = 0; i < 4; i++) {
        printf("g_cd%d %ld %ld\n", i, d8(g_cd[i]), e8(g_cd[i]));
    }
    if (__real__ g_cd[2] != -5.5 || __imag__ g_cd[3] != -8.25) bad++;
    for (int i = 0; i < 3; i++) {
        printf("g_cf%d %ld %ld\n", i, r8(g_cf[i]), i8(g_cf[i]));
    }
    if (__real__ g_cf[1] != 2.5f || __imag__ g_cf[2] != 5.75f) bad++;

    /* 经函数指针返回复数 */
    cd_fn fn = cd_selector();
    _Complex double via = fn(da);
    if (__real__ via != 3.5 || __imag__ via != -4.25) bad++;
    printf("via_fp %ld %ld\n", d8(via), e8(via));

    /* 复数局部量整体赋值后再按分量读 (聚合拷贝路径) */
    _Complex double mv = via;
    _Complex double *mp = &mv;
    __imag__ *mp = __real__ *mp + 1.0;
    if (__imag__ mv != 4.5) bad++;
    printf("move %ld %ld\n", d8(mv), e8(mv));

    printf("bad=%d\n", bad);
    return bad;
}
