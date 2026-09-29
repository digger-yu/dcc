// x87 **80 位 `long double`** ([basic.fundamental]/3.3) 在 C++ native 后端的
// 端到端正确性。
//
// sema 用 `CxxType::Float{bits: 80}` 表示它, C 侧用 `Type::LongDouble`。早前
// `lower_ir::lower_type` 只认 32/64 位, 80 位直接报 `float with 80 bits` ——
// 任何用到 `long double` 的翻译单元 (printf 的 `%Lf`、`strtold`、结构体里的
// `long double` 成员) 都会在这里退回 legacy 文本降级。
//
// 交叉验证口径: 精度 (1/3 的 18 位十进制)、布局 (成员偏移与 sizeof 按 16 字节
// 对齐)、算术与比较结果, 全部与 g++ 逐行一致。
#include <cstdio>

struct Mixed {
    char c;
    long double ld;
    int i;
    long double ld2;
};

struct OnlyLd {
    long double a;
    long double b;
};

long double g_ld = 1.5L;

// long double 参与重载决议
double id(double x) { return x * 2.0; }
long double id(long double x) { return x * 3.0L; }
float id(float x) { return x * 5.0f; }

int main() {
    long double third = 1.0L / 3.0L;
    long double back = third * 3.0L;
    printf("third=%.18Lf back=%.18Lf\n", third, back);
    printf("global=%.6Lf\n", g_ld);

    Mixed m;
    m.c = 1;
    m.ld = 2.25L;
    m.i = 3;
    m.ld2 = 4.5L;
    printf("mixed size=%zu off=%td,%td,%td\n",
           sizeof(Mixed),
           (char*)&m.c - (char*)&m,
           (char*)&m.ld - (char*)&m,
           (char*)&m.ld2 - (char*)&m);
    printf("mixed vals=%.4Lf %d %.4Lf\n", m.ld, m.i, m.ld2);

    OnlyLd o;
    o.a = 0.5L;
    o.b = 1.5L;
    printf("onlyld size=%zu align_off=%td sum=%.4Lf\n",
           sizeof(OnlyLd), (char*)&o.b - (char*)&o, o.a + o.b);

    // 比较与三目: 16 字节的 long double 分支必须走控制流降级 (用 SSA `Select`
    // 承载不下寄存器化的 16 字节对象, 会得到 `nan`)
    printf("cmp=%d,%d,%d\n", (int)(third < 0.4L), (int)(third > 0.33L), (int)(m.ld == 2.25L));
    printf("ternary=%.4Lf,%.4Lf\n", (third < 0.4L) ? m.ld : m.ld2, (third > 0.4L) ? m.ld : m.ld2);

    // 循环累加 (长期稳定性)
    long double acc = 0.0L;
    for (int k = 0; k < 1000; k++) acc += 0.001L;
    printf("acc=%.6Lf\n", acc);

    // 与整型/浮点的隐式转换
    int i = 7;
    double d = 2.5;
    long double conv = (long double)i + (long double)d;
    printf("conv=%.4Lf i=%d d=%.4f\n", conv, i, (double)d);

    // 重载: 实参类型不需要同质提升时各自选到自己的版本
    printf("overload=%.4Lf,%.4Lf,%.4f\n", id(1.0L), (long double)id(1.0), (double)id(1.0f));

    // long double 精度的数值算法 (牛顿法求 sqrt(2)) —— 比直接调库函数更能
    // 体现 80 位中间值的精度是否真的保留了下来。
    long double r = 1.0L;
    for (int k = 0; k < 12; k++) r = (r + 2.0L / r) / 2.0L;
    printf("sqrt2=%.15Lf err=%.3Le\n", r, r * r - 2.0L);
    return (int)(back * 100.0L) - 100;
}
