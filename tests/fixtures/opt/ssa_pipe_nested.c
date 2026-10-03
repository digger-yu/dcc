/* 嵌套循环必须逐档与 -O0 同结果 —— 这条是 P16 落地「不得破坏外层结构」的护栏。
 *
 * 为什么单独立一条：内层循环被流水时曾出现两类实测缺陷（都被 `run_pipelining`
 * 的嵌套准入拦下，见 ssa::pipelining::is_nested_in_other_loop）：
 *   1. 外层携带值（这里两个循环共享 `sum`）在内层区域里读到的名字既不是内层 phi
 *      dest 也不在内层区域内定义 ⇒ 被当成循环不变量原样保留 ⇒ 累加丢失；
 *   2. 落地往内层的 preheader/exit 追加指令、摘除内层 latch，而这些块位于外层循环
 *      体内 ⇒ 被重写的正是外层的推进与判定链，产物直接死循环。
 * 下面每个形态都覆盖一条：不同层数、不同 trip 组合、体内共享累加器、内层独立累加器。
 *
 * 通过: 与 gcc 各档位输出逐字节一致
 */
#include <stdio.h>

/* 10×10 共享累加器：曾经挂死的最小形态 */
int nest_shared(void) {
    int sum = 0;
    for (int i = 0; i < 10; i++)
        for (int j = 0; j < 10; j++)
            sum += i * j;
    return sum;
}

/* 外层 trip=1、内层 trip=10：内层单独就能触发流水闸门 */
int nest_1x10(void) {
    int s = 0;
    for (int i = 0; i < 1; i++)
        for (int j = 0; j < 10; j++)
            s += 1;
    return s;
}

/* 内层自带独立累加器，外层读它 */
int nest_inner_acc(void) {
    int total = 0;
    for (int i = 0; i < 6; i++) {
        int s = 0;
        for (int j = 0; j < 8; j++) s += j;
        total += s;
    }
    return total;
}

/* 三层：最内层写数组、中间层累加。这里还另外钉一条历史缺陷：同一形态的
 * 「函数内 static 数组 + 同 TU 内还有其他函数」曾在原夹具里 -O2 得到 210（应为 36），
 * 而单函数独立成 TU 是对的 ⇒ 错的是跨函数/TU 级污染，不是循环嵌套本身。
 * 现第 20 轮复测：本函数与重建的 static 局部变体（带两个无关的旁函数）在
 * -O0/-O1/-O2/-O3 均为 36，与 gcc 一致 ⇒ 该缺陷已关（见
 * docs/ssa-pass-upgrade-plan.md §4 表与 §7.4 项 6）。保留在这里是因为它回归
 * 过一次，删掉就等于把防线拆掉。*/
int nest_three(void) {
    int buf[6] = {0, 0, 0, 0, 0, 0};
    int acc = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 4; j++)
            for (int k = 0; k < 2; k++)
                buf[j] += i + k;
    for (int j = 0; j < 6; j++) acc += buf[j];
    return acc;
}

/* 外层体内既有可流水的归约、又有嵌套：两条路径同时存在 */
int nest_mixed(void) {
    static int a[8];
    int s = 0;
    for (int i = 0; i < 8; i++) a[i] = i;
    for (int i = 0; i < 4; i++) {
        s += a[i];
        for (int j = 0; j < 8; j++) s += a[j] - i;
    }
    return s;
}

int main(void) {
    printf("nest_shared=%d\n", nest_shared());
    printf("nest_1x10=%d\n", nest_1x10());
    printf("nest_inner_acc=%d\n", nest_inner_acc());
    printf("nest_three=%d\n", nest_three());
    printf("nest_mixed=%d\n", nest_mixed());
    return 0;
}
