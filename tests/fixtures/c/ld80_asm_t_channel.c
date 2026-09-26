/* a6 重点1：inline asm 的 x87 (`t`) 通道绑定 long double。
 *
 * 通道规则：SysV 上 long double 只能走 x87（st(0)），约束 `t` 对应 st(0)，
 * match-digit `"0"` 复用输出槽位但仍必须把输入值重新加载进 st(0)。
 * 全局输入走 RIP `fld tbyte`，局部输入与全局/局部写回走 16 字节栈槽 tbyte。
 *
 * 此前 dcc 在这条通道上踩过两个根因级编码 bug：
 *   1. 栈槽中转 SSE 指令把 REX 发射在 mandatory prefix (F2/F3) 之前，
 *      REX 失效，r13 被解码成 rbp（`41 f2 0f 10 84 25` → 必须 `f2 41 ...`）；
 *   2. 全局 ldouble 初始化器读侧曾按 movsd 只取低 8 字节。
 *
 * 判据：stdout 文本（%.27Lf）必须与 gcc 逐字符一致 + 退出码 0。
 * 注意：`0.1L` 字面量的最后一位精度由重点2（AST FloatConst 枚举化）解决，
 * 本夹具只验证通道（dcc 与 gcc 对同一来源各自一致即通道正确）。
 * 期望退出码 0。
 */
#include <stdio.h>

long double gv = 0.1L;
long double gout = 0.0L;

static int fails = 0;

#define CHK(cond, msg) do { if (!(cond)) { printf("FAIL: %s\n", (msg)); fails++; } } while (0)

int main(void) {
    long double lv = 0.1L;

    /* 全局 long double → t 通道（RIP fldt） */
    long double t = 0.0L;
    __asm__("" : "=t"(t) : "0"(gv));
    printf("t-global %.27Lf\n", t);
    CHK(t == gv, "t-global value equals source");

    /* 局部 long double → t 通道（栈槽 fldt） */
    long double t2 = 0.0L;
    __asm__("" : "=t"(t2) : "0"(lv));
    printf("t-local %.27Lf\n", t2);
    CHK(t2 == lv, "t-local value equals source");

    /* t 通道写回全局 long double（RIP fstpt） */
    __asm__("" : "=t"(gout) : "0"(gv));
    printf("gout %.27Lf\n", gout);
    CHK(gout == gv, "global writeback value equals source");

    /* t 通道写回局部 long double（栈槽 fstpt） */
    long double lout = 0.0L;
    __asm__("" : "=t"(lout) : "0"(lv));
    printf("lout %.27Lf\n", lout);
    CHK(lout == lv, "local writeback value equals source");

    /* 非零值也要过通道（防全零/常量巧合通过） */
    long double g25 = 2.5L;
    long double a = 0.0L;
    __asm__("" : "=t"(a) : "0"(g25));
    CHK(a == 2.5L, "2.5L through t channel");

    if (fails == 0) printf("ALL OK\n");
    else printf("FAILURES: %d\n", fails);
    return fails != 0;
}
