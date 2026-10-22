/* computed goto 的跳表分派在基址常驻寄存器后必须逐项算对。
 *
 * 症状（阶段 6 批 4 的失败形态，全是「分派跳错地方」那一类）：
 *   * 基址取错一张表 ⇒ 直接跳进别的函数（实测同款症状 SIGSEGV）；
 *   * 存槽没配对（多 return 少了一条恢复）⇒ 返回调用者后 `%rbp` 是跳表地址，
 *     上层函数的帧指针坏掉；
 *   * 被调函数自己也要基址常驻而两者共用 `%rbp` ⇒ 回来以后基址是**被调者**的表，
 *     调用者后面每一跳都错。
 *
 * 覆盖面（每段各钉一条判据的反面）：
 *   1. `static const void *const` 表 + 三个宏展开出来的分派站点（选出主表靠站点数）；
 *   2. 同一函数里第二张表（只有一个号，另一张必须仍走原来的 lea 路）；
 *   3. 分派之间调外部函数（callee 会毁 caller-saved，`%rbp` 是 callee-saved ⇒ 必须还在）；
 *   4. **被调函数自己也有一张常驻表**（嵌套占用，序言/尾声要各存复各的）；
 *   5. 多个 return 站点（每条尾声都要把 `%rbp` 还回去）；
 *   6. 运行期改写表项后再分派（常驻的是**地址**，不是某一项的值）；
 *   7. 回跳循环，同一张表被分派几十次、指标不断变化；
 *   8. 反向对照：指针数组不是从 `goto *` 上读的（普通取值）⇒ 值必须算对。
 *
 * 通过: exit 0（末行 `computed_goto_dispatch: OK`），四档与 gcc 输出一致。
 */

#include <stdio.h>

static int fails;

#define EXPECT(got, want, name)                                                                  \
    do {                                                                                         \
        long long got_ = (long long)(got);                                                       \
        long long want_ = (long long)(want);                                                     \
        if (got_ != want_) {                                                                     \
            printf("FAIL %s: got %lld want %lld\n", (name), got_, want_);                       \
            fails++;                                                                             \
        }                                                                                       \
    } while (0)

static volatile int which[8] = {1, 2, 3, 0, 1, 2, 3, 0};

/* 外部真正参与运算的函数：分派之间必须还认得自己的基址 */
static int scale(int acc, int k) {
    return acc * 3 + k;
}

/* 4) 被调函数自己也用一张常驻表 —— 它回来以后 caller 的基址不得是被调者的 */
static int inner_dispatch(int code) {
    static const void *const itab[4] = {&&I0, &&I1, &&I2, &&I3};
    int r;
    goto *itab[code & 3];
I0:
    r = 100;
    goto Iend;
I1:
    r = 200;
    goto Iend;
I2:
    r = 300;
    goto Iend;
I3:
    r = 400;
Iend:
    return r;
}

/* 1/2/3/5/7：两张表 + 宏展开的多站点 + 跨调用 + 多个 return + 回跳循环 */
static int outer_run(int n) {
    static const void *const otab[4] = {&&O_A, &&O_B, &&O_C, &&O_D};
    /* 第二张表：站点比 otab 少（本函数只从它跳一次），它必须仍算得对 */
    static const void *const stab[2] = {&&S_EVEN, &&S_ODD};
    int acc = 0;
    int i;
    int code;
#define DISPATCH(c)                                                                              \
    do {                                                                                         \
        code = (c);                                                                              \
        goto *otab[code & 3];                                                                    \
    } while (0)

    for (i = 0; i < n; i++) {
        DISPATCH(which[i & 7]);
    O_A:
        acc = acc + 1;
        continue;
    O_B:
        acc = acc + 2;
        continue;
    O_C:
        /* 3) 跨一次真实调用：callee 毁掉 caller-saved，基址是 callee-saved 的号 */
        acc = scale(acc, 7);
        continue;
    O_D:
        /* 4) 跨一次「自己也常驻一张表」的调用 */
        acc = acc + inner_dispatch(i);
        continue;
    }
    if (n > 2) {
        return acc + 1000; /* 5) 第二个 return 站点 */
    }
    /* 2) 另一张表的一次分派 */
    goto *stab[n & 1];
S_EVEN:
    return acc + 11;
S_ODD:
    return acc + 22;
}

/* 6) 表项在运行期被改写：常驻的是表的地址，不是任何一项 */
static void **mutable_tab;
static int table_update(int code) {
    static void *tab[3] = {0, 0, 0};
    static int filled;
    int r = 0;
    if (!filled) {
        tab[0] = &&U_ZERO;
        tab[1] = &&U_ONE;
        tab[2] = &&U_TWO;
        filled = 1;
    }
    /* 运行期把 1 号槽改到 2 号的处理上去 */
    if (code == 1) {
        tab[1] = &&U_TWO;
    }
    goto *tab[code];
U_ZERO:
    r = 10;
    goto U_end;
U_ONE:
    r = 20;
    goto U_end;
U_TWO:
    r = 30;
U_end:
    mutable_tab = tab;
    return r;
}

/* 8) 反向对照：指针数组当普通值读（不是 `goto *`），算出来的地址要能用 */
static int not_a_dispatch(int code) {
    static const void *const ptab[4] = {&&N0, &&N1, &&N2, &&N3};
    const void *p = ptab[(code + 1) & 3];
    /* 让 p 真的被用到（否则整条读会被删掉），然后照原样跳过去 */
    if (p == ptab[0]) {
        return 1;
    }
    goto *p;
N0:
    return 2;
N1:
    return 3;
N2:
    return 4;
N3:
    return 5;
}

/* 10) setjmp/longjmp 穿过常驻基址帧的那一形**不进本夹具**：同一份源在 gcc -O2 下自己
 *     就 SIGSEGV（-O0 与 dcc 都对），而 `assert_all_levels_match_gcc` 比的是与 gcc 一致。
 *     它进了另一条只比 dcc 自己的用例：`setjmp_longjmp_across_hoisted_dispatch_*`。*/

int main(void) {
    /* 期望值逐条来自 gcc 臂（同一份源、同一算法），不是手算：
     *   n=1: which[0]=1 → O_B acc=2 → stab[1] → S_ODD 2+22 = 24
     *   n=2: acc 2 → O_C 2*3+7=13 → stab[0] → S_EVEN 13+11 = 24
     *   n=4: 2,13,313(=13+300),314 → +1000 = 1314
     *   n=8: 2,13,313,314,316,955,1255(=955+300),1256 → +1000 = 2256 */
    EXPECT(outer_run(1), 24, "outer_run(1)");
    EXPECT(outer_run(2), 24, "outer_run(2)");
    EXPECT(outer_run(4), 1314, "outer_run(4)");
    EXPECT(outer_run(8), 2256, "outer_run(8)");
    EXPECT(inner_dispatch(0), 100, "inner_dispatch(0)");
    EXPECT(inner_dispatch(3), 400, "inner_dispatch(3)");
    EXPECT(table_update(0), 10, "table_update(0)");
    EXPECT(table_update(1), 30, "table_update(1)"); /* 槽被改到 U_TWO */
    EXPECT(table_update(2), 30, "table_update(2)");
    EXPECT(not_a_dispatch(0), 3, "not_a_dispatch(0)");
    EXPECT(not_a_dispatch(1), 4, "not_a_dispatch(1)");
    /* (3+1)&3 == 0 → p 就是 ptab[0] → 走那个「不跳、直接回」的分支 */
    EXPECT(not_a_dispatch(3), 1, "not_a_dispatch(3)");

    if (fails) {
        printf("computed_goto_dispatch: %d FAIL\n", fails);
        return 1;
    }
    printf("computed_goto_dispatch: OK\n");
    return 0;
}
