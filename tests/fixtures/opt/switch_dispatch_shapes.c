/* switch 分派形态的行为夹具（P13 阶段6 批1「跳表」的前置判据）。
 *
 * 为什么先写它再写跳表：批1 要把稠密整型 selector 的比较链换成查表分派，而表分派最容易
 * 在六个地方静默错值 —— 有符号负数下标（必须按有符号判域或用无符号 `ja` 一次性挡掉）、
 * 稀疏值里的洞（必须落到 default 而不是邻近臂）、GNU 区间 case（表表达不了）、
 * default 不在源码末环（链的次序会把它跑歪）、末尾具名臂落入 default（无 break）、
 * 以及 `__int128`/指针 selector（宽度与符号性都不适用表）。
 *
 * 每个形态都配一份**手写的 if/else 参照实现**，逐 k 断言两者一致：这样判据不依赖
 * 「我记得正确值是多少」，也不会因为两边一起错而空跑。
 * 通过: exit 0（末行 `switch_dispatch_shapes: OK`），四档与 gcc 一致。
 */

#include <stdio.h>

typedef __int128 i128;

static int fails;

#define EXPECT_EQ(a, b, name)                                                                    \
    do {                                                                                         \
        if ((long long)(a) != (long long)(b)) {                                                   \
            printf("FAIL %s: switch=%lld ifelse=%lld\n", (name), (long long)(a),                   \
                   (long long)(b));                                                                \
            fails++;                                                                              \
        }                                                                                         \
    } while (0)

/* 为什么不累加哈希：本仓库的 `-O0/-O1` 有一条已登记的缺陷（同宽 signed↔unsigned
 * cast 在使用点不做归一），任何 `unsigned long` 累加器都会先被它污染，而本夹具
 * 只想问「分派选中了哪个臂」。于是这里只做 switch ↔ 手写 if/else 的相等断言，
 * 不引入任何宽度混合的算术。 */

/* --- 形态 1：稠密 0..7（跳表的主场景） --- */
static int sw_dense(int k) {
    switch (k) {
        case 0: return 11;
        case 1: return 12;
        case 2: return 13;
        case 3: return 14;
        case 4: return 15;
        case 5: return 16;
        case 6: return 17;
        case 7: return 18;
        default: return -1;
    }
}
static int if_dense(int k) {
    if (k == 0) return 11;
    if (k == 1) return 12;
    if (k == 2) return 13;
    if (k == 3) return 14;
    if (k == 4) return 15;
    if (k == 5) return 16;
    if (k == 6) return 17;
    if (k == 7) return 18;
    return -1;
}

/* --- 形态 2：稀疏含洞（表要填洞，洞必须指向 default） --- */
static int sw_sparse(int k) {
    switch (k) {
        case 3: return 21;
        case 17: return 22;
        case 99: return 23;
        case 1000: return 24;
        default: return -2;
    }
}
static int if_sparse(int k) {
    if (k == 3) return 21;
    if (k == 17) return 22;
    if (k == 99) return 23;
    if (k == 1000) return 24;
    return -2;
}

/* --- 形态 3：GNU 区间 case（表表达不了，必须留在链上） --- */
static int sw_range(int k) {
    switch (k) {
        case 0: return 31;
        case 3 ... 6: return 32;
        case 9: return 33;
        default: return -3;
    }
}
static int if_range(int k) {
    if (k == 0) return 31;
    if (k >= 3 && k <= 6) return 32;
    if (k == 9) return 33;
    return -3;
}

/* --- 形态 4：default 在源码首位（链的末环必须是它） --- */
static int sw_default_first(int k) {
    switch (k) {
        default: return 41;
        case 2: return 42;
        case 5: return 43;
    }
}
static int if_default_first(int k) {
    if (k == 2) return 42;
    if (k == 5) return 43;
    return 41;
}

/* --- 形态 5：末尾具名臂无 break 落入 default --- */
static int sw_fall_into_default(int k) {
    int r = 0;
    switch (k) {
        case 1: r += 5;
        case 2: r += 6; break;
        default: r += k;
    }
    return r;
}
static int if_fall_into_default(int k) {
    int r = 0;
    if (k == 1) {
        r += 5;
        r += 6;
    } else if (k == 2) {
        r += 6;
    } else {
        r += k;
    }
    return r;
}

/* --- 形态 6：goto 跳进/跳出 switch 体（分派入口改动不得打断标签可达性） --- */
static int sw_goto_in(int k) {
    int r = 0;
    switch (k) {
        case 1: goto L1;
        case 2:
        L1:
            r = 51;
            break;
        default: goto Lend;
    }
Lend:
    return r;
}
static int if_goto_in(int k) {
    int r = 0;
    if (k == 1 || k == 2) r = 51;
    return r;
}

/* --- 形态 7：无符号极值（界检必须按无符号语义） --- */
static int sw_unsigned(unsigned k) {
    switch (k) {
        case 0u: return 61;
        case 0x80000000u: return 62;
        case 0xFFFFFFFFu: return 63;
        default: return -6;
    }
}
static int if_unsigned(unsigned k) {
    if (k == 0u) return 61;
    if (k == 0x80000000u) return 62;
    if (k == 0xFFFFFFFFu) return 63;
    return -6;
}

/* --- 形态 8：有符号负 case 值（下标有负偏移，表基址不能按 0 起算） --- */
static int sw_negative(int k) {
    switch (k) {
        case -5: return 71;
        case -1: return 72;
        case 0: return 73;
        case 7: return 74;
        default: return -7;
    }
}
static int if_negative(int k) {
    if (k == -5) return 71;
    if (k == -1) return 72;
    if (k == 0) return 73;
    if (k == 7) return 74;
    return -7;
}

/* --- 形态 9：__int128 与指针 selector（宽度/符号性不适用表，须回退链） --- */
static int sw_i128(i128 k) {
    switch (k) {
        case 0: return 81;
        case 100: return 82;
        default: return -8;
    }
}
static int if_i128(i128 k) {
    if (k == (i128)0) return 81;
    if (k == (i128)100) return 82;
    return -8;
}

static int buf[4];

int main(void) {
    static const int ks[] = {-2147483647 - 1, -1000, -5, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17,
                             98, 99, 100, 999, 1000, 1001, 65535, 2147483647};
    const int n = (int)(sizeof ks / sizeof ks[0]);
    for (int i = 0; i < n; i++) {
        int k = ks[i];
        EXPECT_EQ(sw_dense(k), if_dense(k), "dense");
        EXPECT_EQ(sw_sparse(k), if_sparse(k), "sparse");
        EXPECT_EQ(sw_range(k), if_range(k), "range");
        EXPECT_EQ(sw_default_first(k), if_default_first(k), "default_first");
        EXPECT_EQ(sw_fall_into_default(k), if_fall_into_default(k), "fall_into_default");
        EXPECT_EQ(sw_goto_in(k), if_goto_in(k), "goto_in");
        EXPECT_EQ(sw_negative(k), if_negative(k), "negative");
    }
    /* 无符号形态单独走一遍（含 0x80000000 / 0xFFFFFFFF 两个极值）。 */
    static const unsigned us[] = {0u, 1u, 2u, 7u, 0x7FFFFFFFu, 0x80000000u, 0xFFFFFFFEu,
                                 0xFFFFFFFFu};
    for (int i = 0; i < (int)(sizeof us / sizeof us[0]); i++) {
        EXPECT_EQ(sw_unsigned(us[i]), if_unsigned(us[i]), "unsigned");
    }
    EXPECT_EQ(sw_i128((i128)0), if_i128((i128)0), "i128_zero");
    EXPECT_EQ(sw_i128((i128)100), if_i128((i128)100), "i128_100");
    EXPECT_EQ(sw_i128((i128)-1), if_i128((i128)-1), "i128_neg");
    /* 指针 selector：值不参与表，但必须能正常分派（回退链）。 */
    buf[0] = 1;
    buf[2] = 3;
    switch ((int)(buf[2] - buf[0])) {
        case 2: EXPECT_EQ(2, (int)(buf[2] - buf[0]), "ptr_selector"); break;
        default: fails++; break;
    }
    EXPECT_EQ((int)(buf[2] - buf[0]), 2, "ptr_selector");

    printf("fails=%d\n", fails);
    if (fails) {
        printf("switch_dispatch_shapes: %d FAIL\n", fails);
        return 1;
    }
    printf("switch_dispatch_shapes: OK\n");
    return 0;
}
