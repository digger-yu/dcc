/* a6 重点1：inline asm 的 SSE (`x`) 通道绑定 float/double。
 *
 * 通道规则：`x`/`Y` 约束绑定 xmm 寄存器，match-digit `"0"` 复用输出槽位
 * 但仍须把输入值重新加载进该 xmm。全局输入走 RIP movsd/movss，
 * 局部输入走 [r13+disp]，写回全局/局部同理。
 *
 * 本夹具覆盖此前 REX 前缀顺序 bug（REX 被发在 mandatory prefix F2/F3
 * 之前而失效，r13 解码成 rbp）的全部五个触发形态：
 * 全局 double 输入、局部 double 输入、写回全局 double、写回局部 double、
 * 全局 float 输入。修复后 dcc 五项输出与 gcc 逐字符一致。
 *
 * long double 走 `x` 是**非法**约束（gcc: inconsistent operand constraints），
 * 另有 ld80_asm_x_rejects_long_double 专测诊断。
 *
 * 期望退出码 0。
 */
#include <stdio.h>

double gvd = 0.1;
double goutd = 0.0;
float gvf = 0.25f;

static int fails = 0;

#define CHK(cond, msg) do { if (!(cond)) { printf("FAIL: %s\n", (msg)); fails++; } } while (0)

int main(void) {
    double lvd = 0.1;

    double x1 = 0.0;
    __asm__("" : "=x"(x1) : "0"(gvd));
    printf("x-gdouble %.17g\n", x1);
    CHK(x1 == gvd, "global double through x");

    double x2 = 0.0;
    __asm__("" : "=x"(x2) : "0"(lvd));
    printf("x-ldouble %.17g\n", x2);
    CHK(x2 == lvd, "local double through x");

    __asm__("" : "=x"(goutd) : "0"(gvd));
    printf("goutd %.17g\n", goutd);
    CHK(goutd == gvd, "writeback global double through x");

    double loutd = 0.0;
    __asm__("" : "=x"(loutd) : "0"(lvd));
    printf("loutd %.17g\n", loutd);
    CHK(loutd == lvd, "writeback local double through x");

    float x3 = 0.0f;
    __asm__("" : "=x"(x3) : "0"(gvf));
    printf("x-gfloat %.9g\n", x3);
    CHK(x3 == gvf, "global float through x");

    /* 局部 float 与写回局部 float */
    float lvf = 0.5f;
    float xf = 0.0f;
    __asm__("" : "=x"(xf) : "0"(lvf));
    CHK(xf == 0.5f, "local float through x");
    float loutf = 0.0f;
    __asm__("" : "=x"(loutf) : "0"(lvf));
    CHK(loutf == lvf, "writeback local float through x");

    if (fails == 0) printf("ALL OK\n");
    else printf("FAILURES: %d\n", fails);
    return fails != 0;
}
