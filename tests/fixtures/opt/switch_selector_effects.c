/* switch selector 的副作用只求值一次（P13 阶段6 批0.5 的行为夹具）。
 *
 * 为什么要单独成一条：`ssa::structure::lower_switch_stmt` 把 switch 降级成 if-else 链时，
 * 对**每个臂**都 `expr.clone()` 一份比较（那里只挡了区间 case、fallthrough、default 位次，
 * 没有纯度判据）。于是链的短路次序会把「求值 N 次」的差别藏起来 —— 命中第一个臂时
 * 一切看起来都对，命中中间或末尾臂时才露：selector 被求值了「臂位 + 1」次。
 * 实测本夹具在 -O2 上 `sw_iadd` 得 `pick=0 i=5`（gcc 是 `pick=300 i=3`），
 * `sw_side` 得 `calls=2`（gcc 1 次）—— 静默错值，且 -O0 是对的。
 *
 * 每段都同时钉两面：选中的臂（错臂会露）+ 副作用次数（重复求值会露）。
 * 通过: exit 0（末行 `switch_selector_effects: OK`），四档与 gcc 一致。
 */

#include <stdio.h>

static int fails;

#define EXPECT(expr, want, name)                                                                 \
    do {                                                                                         \
        if ((expr) != (want)) {                                                                  \
            printf("FAIL %s: got %d want %d\n", (name), (int)(expr), (int)(want));               \
            fails++;                                                                              \
        }                                                                                        \
    } while (0)

static int pick, incs, calls, adv;
static int arr[3];
static int *pp;
static int counter;

/* selector 是 i++：值 2 → 必须走 case 2，而 i 只能自增一次。 */
static void sw_iadd(void) {
    int i = 2;
    incs = 0;
    switch (i++) {
        case 0: pick = 100; break;
        case 1: pick = 200; break;
        case 2: pick = 300; break;
        default: pick = -1; break;
    }
    incs = i;
}

/* selector 是函数调用：返回值 1 → case 1，而函数必须只被调一次。 */
static int side(void) {
    calls++;
    return 1;
}
static void sw_side(void) {
    calls = 0;
    switch (side()) {
        case 0: pick = 11; break;
        case 1: pick = 22; break;
        case 2: pick = 33; break;
    }
}

/* selector 是 *p++：命中第一个臂（链只走一步），指针只能前进一次。
 * 注：本段的臂位是故意选在链首的 —— 降级成链时「首环命中」会掩盖重复求值，
 * 真正能把问题暴露出来的是 sw_lastarm（值落在链末环）。 */
static void sw_padd(void) {
    arr[0] = 2;
    arr[1] = 5;
    arr[2] = 7;
    pp = arr;
    adv = 0;
    switch (*pp++) {
        case 2: pick = 42; break;
        case 5: pick = 55; break;
        case 7: pick = 77; break;
        default: pick = -1; break;
    }
    adv = (int)(pp - arr);
}

/* selector 自增且值域落在链的最后一环：降级成链时求值次数最藏不住。 */
static void sw_lastarm(void) {
    counter = 9;
    switch (counter += 4) { /* 13 → 最后一个具名臂 */
        case 1: pick = 111; break;
        case 5: pick = 112; break;
        case 9: pick = 113; break;
        case 13: pick = 114; break;
        default: pick = -1; break;
    }
}

int main(void) {
    sw_iadd();
    EXPECT(pick, 300, "sw_iadd.pick");
    EXPECT(incs, 3, "sw_iadd.i");

    sw_side();
    EXPECT(pick, 22, "sw_side.pick");
    EXPECT(calls, 1, "sw_side.calls");

    sw_padd();
    EXPECT(pick, 42, "sw_padd.pick");
    EXPECT(adv, 1, "sw_padd.adv");

    sw_lastarm();
    EXPECT(pick, 114, "sw_lastarm.pick");
    EXPECT(counter, 13, "sw_lastarm.counter");

    if (fails) {
        printf("switch_selector_effects: %d FAIL\n", fails);
        return 1;
    }
    printf("switch_selector_effects: OK\n");
    return 0;
}
