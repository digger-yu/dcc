/* 浮点值当条件用（`if (double)`）的真值判据夹具（P13 阶段3 的直接后果）
 *
 * `emit_truth_flags` 曾没有 SSE 浮点那一臂：`if (d)` 只把 d 装进 xmm0，然后
 * `test rax,rax` —— 读的是**上上次求值留在 rax 里的残留**。HEAD 与 gcc 一致纯属
 * 巧合（它的整数比较会把 0/1 留在 rax）；阶段3 把 `cmp+setcc+movzx+test` 换成
 * `cmp+jcc` 之后残留不再是 0，同一个形状立刻暴露成误编译
 * （`gcc.dg/tree-ssa/scev-2.c` 当场 abort）。
 *
 * 修复形态：`xmm1 ← 0` 后复用 `gen_float_cmp(Ne)`，所以 NaN 也必须判真
 * （`ucomisd` 无序时 ZF=1，只看 ZF 会把 `if (nan)` 判成假）。
 *
 * 本夹具覆盖：if / while / for / do-while / 三元 五种条件语境 × 零 / 非零 / 负值 /
 * 运行时 NaN / float 与 double 两种宽度；每处都同时问「真值」与「走没走 else」。
 * 值语境下的 `!d`、`d && x` 与 `long double`（x87 通道）/`__float128` 条件
 * **不在本夹具里**——同族缺陷仍未修，记录在
 * `.trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md` §9.3b。
 *
 * 通过: exit 0（末行 `float_truth_condition: OK`），四档与 gcc 一致。
 */

#include <stdio.h>

static int fails;

#define CHK(cond, want, name)                                                                    \
    do {                                                                                         \
        int got_ = (cond) ? 1 : 0;                                                               \
        if (got_ != (want)) {                                                                    \
            fails++;                                                                             \
            printf("FAIL %s: got %d want %d\n", (name), got_, (want));                           \
        }                                                                                        \
    } while (0)

/* 五种条件语境，各自只依赖浮点真值判据 */

static int if_else_d(double d) {
    if (d) return 11;
    return 22;
}

static int while_d(double d) {
    int n = 0;
    while (d > 0 ? d != 0 : 0) { /* 前置：仅浮点真值参与的部分在下面 */
        n++;
        d -= 1.0;
        if (n > 3) break;
    }
    return n;
}

static int while_truth(double d) {
    int n = 0;
    while (d) { /* 真值直接是浮点 */
        n++;
        if (n >= 3) d = 0;
    }
    return n;
}

static int for_truth(double d) {
    int n = 0;
    for (; d; ++n) { /* 条件的「继续」判据是浮点真值 */
        if (n >= 2) d = 0;
    }
    return n;
}

static int do_truth(double d) {
    int n = 0;
    do {
        ++n;
        d = 0; /* 第二次判假 */
    } while (d);
    return n;
}

static int ternary_truth(double d) { return d ? 7 : 9; }

static int while_truth_f(float f) {
    int n = 0;
    while (f) {
        n++;
        if (n >= 2) f = 0;
    }
    return n;
}

int main(void) {
    volatile double zero = 0.0;
    volatile double three = 3.0;
    volatile double neg = -2.5;
    volatile double z2 = 0.0;
    double nanv = z2 / z2; /* 运行时 NaN，不被编译期折叠 */
    volatile float fzero = 0.0f;
    volatile float fone = 1.5f;

    CHK(if_else_d(zero) == 22, 1, "if_else_zero_takes_else");
    CHK(if_else_d(three) == 11, 1, "if_else_three_takes_then");
    CHK(if_else_d(neg) == 11, 1, "if_else_negative_takes_then");
    CHK(if_else_d(nanv) == 11, 1, "if_else_NaN_takes_then");

    CHK(ternary_truth(zero) == 9, 1, "ternary_zero_takes_else");
    CHK(ternary_truth(nanv) == 7, 1, "ternary_NaN_takes_then");

    CHK(while_truth(zero), 0, "while_zero_no_iterations");
    CHK(while_truth(three) == 3, 1, "while_three_three_iterations");
    CHK(while_truth(nanv) == 3, 1, "while_NaN_exits_via_assignment");
    CHK(for_truth(zero), 0, "for_zero_no_iterations");
    CHK(for_truth(three) == 3, 1, "for_three_three_iterations");
    CHK(do_truth(zero) == 1, 1, "dowhile_zero_one_iteration");
    CHK(do_truth(three) == 1, 1, "dowhile_three_exits_in_body");
    CHK(while_truth_f(fzero), 0, "while_float_zero_no_iterations");
    CHK(while_truth_f(fone) == 2, 1, "while_float_two_iterations");

    /* 与 scev-2 同形：整数循环 + 浮点累减，最后用浮点真值收尾 */
    double a = 20;
    for (int i = 0; i < 10; ++i)
        a -= 2;
    CHK(if_else_d(a) == 22, 1, "scev2_shape_zero_takes_else");
    double b = 20;
    for (int i = 0; i < 9; ++i)
        b -= 2;
    CHK(if_else_d(b) == 11, 1, "scev2_shape_nonzero_takes_then");

    /* 循环节律本身（while_d 用了比较而非纯真值，作为对照必须同样正确） */
    CHK(while_d(three) == 3, 1, "while_float_compare_three");
    CHK(while_d(zero), 0, "while_float_compare_zero");

    if (fails == 0) printf("float_truth_condition: OK\n");
    return fails;
}
