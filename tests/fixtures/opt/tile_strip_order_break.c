/* -O3 的循环分段（P10 strip mining, `src/loop_opt.rs`）不得重排循环体内**可观察副作用
 * 的执行顺序**，也不得让体内 `break` 在分段后多跑迭代。
 *
 * 旧形态是 ts×ts 的 blocking（`ti/tj/i/j` 四层），它覆盖的迭代空间与原循环**逐点同集合
 * 同重数**，但顺序按块重排。C 的可观察行为包含体内副作用的顺序，于是它是一台静默误
 * 编译器：实测 `for(i<7)for(k<7)printf("%d", f(xs[i],xs[k]));` 在 -O3 打印的**数字集合
 * 与 -O0/gcc 完全相同、顺序不同** —— 只看多重集的判据会把这条缺陷写成「预期行为」，
 * 所以本夹具用两条顺序敏感的探针：
 *
 *   ① printf 逐条打印（顺序 = 输出字节序，与 gcc 逐字节对拍）；
 *   ② 十进制进位累加 `acc = acc * 33 + (i * 10 + k)`（把顺序压进一个整数并打进
 *      stdout，任何一次颠倒都会留下痕迹）。
 *
 * 现形态只对最内层维做**保序**分段（外层一个字节都不动），故 (i,k) 序列逐个相同，
 * 正确性不再依赖「体内无副作用」这类没实现的依赖分析。
 *
 * 另两条同 pass 的判据：
 *   ③ `break` 闸门：体内有**未被更内层循环屏蔽**的 `break` 时不得分段。分段把 `for(k…)`
 *      拆成「块计数器 + 子循环」两层，未屏蔽的 `break` 原本跳出内层循环后直接进下一轮
 *      外层，现在却还要把块计数器往上推 ⇒ 多跑迭代（实测去掉闸门后 -O3 的 `f()` 从
 *      12 变 18）。被更内层循环接住的 `break` 仍允许分段（`g()` 四档与 gcc 一致），
 *      所以这条不是一刀切拒分。
 *   ④ `do/while` 循环不属于本 pass 的改写范围，留在本夹具里作反向对照（旧缺陷里它们
 *      恰好是对的，说明错的只有分块那一条通道）。
 *
 * 判据: gcc -O0 与 dcc -O0/-O1/-O2/-O3 的 stdout 逐字节一致、退出码为 0。顺序敏感的
 * 量全部进 stdout: 逐条 printf 的输出字节序就是 (i,k) 序列本身, 进位累加值则把整段
 * 顺序压成一个整数 (顺序一变它就变)。
 */
#include <limits.h>
#include <stdio.h>

static int j_signed(int a, int b) {
    if (a < b) return 1;
    if (a >= b) return 2;
    return 3;
}

static unsigned j_unsigned(unsigned a, unsigned b) {
    if (a > b) return 1;
    if (a <= b) return 2;
    return 3;
}

static int j_ternary(int a, int b) {
    return a < b ? 11 : 22;
}

static int j_dowhile(int n) {
    int s = 0;
    do {
        s += n;
        n--;
    } while (n > 0);
    return s;
}

static int j_while(int n) {
    int s = 0;
    while (n > 0) {
        s += n;
        n--;
    }
    return s;
}

/* 体内只有「写一个外层局部」的 6×5 完美嵌套 —— 正是分块会接住的形态。
 * 十进制进位把 (i,k) 的执行顺序压进一个数: 顺序一变, 值必变。 */
static int order_hash(void) {
    int acc = 0;
    for (int i = 0; i < 6; i++)
        for (int k = 0; k < 5; k++) acc = acc * 33 + (i * 10 + k);
    return acc;
}

/* 同上但只打印首尾若干位, 用来确认分段确实发生 (块推进步长在产物里可见) 且顺序未变。 */
static void order_head(void) {
    int acc = 0;
    int first = -1;
    int last = -1;
    int n = 0;
    for (int i = 0; i < 6; i++)
        for (int k = 0; k < 5; k++) {
            int v = i * 10 + k;
            if (n == 0) first = v;
            last = v;
            n++;
            acc = acc * 7 + v;
        }
    printf("head first=%d last=%d n=%d tail=%d\n", first, last, n, acc);
}

/* 未被屏蔽的 break: 分段会多跑迭代 ⇒ 必须拒绝分段。 */
static int break_unshielded(void) {
    int acc = 0;
    for (int i = 0; i < 6; i++)
        for (int j = 0; j < 5; j++) {
            if (j == 2) break;
            acc++;
        }
    return acc;
}

/* 被更内层循环接住的 break: 允许分段, 值必须仍是 30。 */
static int break_shielded(void) {
    int acc = 0;
    for (int i = 0; i < 6; i++)
        for (int j = 0; j < 5; j++) {
            for (int t = 0; t < 2; t++) break;
            acc++;
        }
    return acc;
}

/* break 之后还有体内写入 (分段若不拒绝就会把这条写挪到不同的轮次上)。 */
static int break_then_write(void) {
    int acc = 0;
    int flag = 0;
    for (int i = 0; i < 5; i++)
        for (int j = 0; j < 4; j++) {
            if (i == 2 && j == 1) {
                flag = 1;
                break;
            }
            acc += 1;
        }
    return acc * 10 + flag;
}

int main(void) {
    const int xs[] = {INT_MIN, -5, -1, 0, 1, 5, INT_MAX};
    printf("signed:");
    for (int i = 0; i < 7; i++)
        for (int k = 0; k < 7; k++) printf("%d", j_signed(xs[i], xs[k]));
    printf("\nunsigned:");
    for (int i = 0; i < 7; i++)
        for (int k = 0; k < 7; k++) printf("%d", (int)j_unsigned((unsigned)xs[i], (unsigned)xs[k]));
    printf("\nternary:");
    for (int i = 0; i < 7; i++)
        for (int k = 0; k < 7; k++) printf("%d", j_ternary(xs[i], xs[k]));
    printf("\ndowhile:");
    for (int n = -3; n < 5; n++) printf("%d", j_dowhile(n));
    printf("\nwhile:");
    for (int n = -3; n < 5; n++) printf("%d", j_while(n));
    printf("\n");
    order_head();
    printf("order_hash=%d\n", order_hash());
    printf("break unshielded=%d shielded=%d then_write=%d\n",
           break_unshielded(), break_shielded(), break_then_write());
    return 0;
}
