/* switch 的第一个 case 之前的语句不得被顺序执行（只能被 fall-through 或标签命中）。
 *
 * 症状（实测旧 HEAD，-O2/-O3 与 gcc 不一致）：
 *   int s(int x){ switch (x) { side = 99; case 1: return 10; default: return 20; } }
 *   gcc -O2 与 dcc -O0 都是 `s(1)` 之后 side==0；而把 switch 降级成 if 链那条路把
 *   prelude **拼在链之前当顺序语句**发，于是 side 每次进入 switch 都被写成 99。
 *
 * 覆盖面：
 *   1. prelude 里有一条普通赋值（就是错的那一形）；
 *   2. prelude 里带标签，且那个标签被臂体里的 `goto` 命中 —— 这时它**必须**被执行
 *      （判据不能退化成「prelude 一律不发」）；
 *   3. prelude 出现在带 default 与不带 default 的两种 switch 里；
 *   4. 稠密到「能上跳表」的 switch 也带 prelude（表分派绕过 prelude，所以那一版
 *      直接不上表；这条盯住两个降级路径共用同一条判据）；
 *   5. 反向对照：prelude 为空时正常降级/正常执行，别把这条路整个关掉。
 *
 * 通过: exit 0（末行 `switch_prelude: OK`），四档与 gcc 输出一致。
 */

#include <stdio.h>

static int fails;
static volatile int side;

#define EXPECT(got, want, name)                                                                  \
    do {                                                                                         \
        long long got_ = (long long)(got);                                                       \
        long long want_ = (long long)(want);                                                     \
        if (got_ != want_) {                                                                     \
            printf("FAIL %s: got %lld want %lld\n", (name), got_, want_);                       \
            fails++;                                                                             \
        }                                                                                       \
    } while (0)

/* 1 + 3：prelude 只是一条赋值 */
static int pre_assign(int x) {
    switch (x) {
        side = 99;
    case 1:
        return 10;
    default:
        return 20;
    }
}

/* 2：prelude 里的标签被臂体的 goto 命中 ⇒ 那条语句必须执行 */
static int pre_labelled(int x) {
    switch (x) {
    fill:
        side = 7;
        return 70;
    case 1:
        goto fill;
    case 2:
        return 20;
    default:
        return 30;
    }
}

/* 4：稠密到能上跳表，但带 prelude ⇒ 只能保持不降级 */
static int pre_dense(int x) {
    switch (x) {
        side = 55;
    case 0:
        return 100;
    case 1:
        return 101;
    case 2:
        return 102;
    case 3:
        return 103;
    case 4:
        return 104;
    case 5:
        return 105;
    case 6:
        return 106;
    case 7:
        return 107;
    default:
        return 199;
    }
}

/* 5：反向对照，prelude 为空 */
static int no_prelude(int x) {
    switch (x) {
    case 0:
        return 1;
    case 1:
        return 2;
    case 2:
        return 3;
    case 3:
        return 4;
    case 4:
        return 5;
    case 5:
        return 6;
    case 6:
        return 7;
    case 7:
        return 8;
    default:
        return 9;
    }
}

int main(void) {
    side = 0;
    EXPECT(pre_assign(1), 10, "pre_assign(1) returns");
    EXPECT(side, 0, "pre_assign(1) leaves side untouched");
    EXPECT(pre_assign(5), 20, "pre_assign(5) returns");
    EXPECT(side, 0, "pre_assign(5) leaves side untouched");

    side = 0;
    EXPECT(pre_labelled(1), 70, "pre_labelled(1) reaches the prelude label");
    EXPECT(side, 7, "pre_labelled(1) executes the labelled prelude statement");
    EXPECT(pre_labelled(2), 20, "pre_labelled(2) returns");
    EXPECT(side, 7, "pre_labelled(2) still does not execute the prelude");

    side = 0;
    EXPECT(pre_dense(3), 103, "pre_dense(3) returns");
    EXPECT(side, 0, "pre_dense(3) leaves side untouched");
    EXPECT(pre_dense(70), 199, "pre_dense(70) hits default");
    EXPECT(side, 0, "pre_dense(70) leaves side untouched");

    EXPECT(no_prelude(0), 1, "no_prelude(0)");
    EXPECT(no_prelude(7), 8, "no_prelude(7)");
    EXPECT(no_prelude(70), 9, "no_prelude(default)");

    if (fails) {
        printf("switch_prelude: %d FAIL\n", fails);
        return 1;
    }
    printf("switch_prelude: OK\n");
    return 0;
}
