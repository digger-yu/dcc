/* 多出口循环的结构化重建判据：循环体里的提前 return / break 与循环的正常落出是**两个
 * 不同的循环外后继**，而 `while` 只能表达一个落出口。
 *
 * 缺陷起点（-O2/-O3 对照 -O0 与 gcc）：
 *
 *     static int a(int n){ for (int i=0;i<n;i++) if (i==2) return 7; return 1; }
 *     a(0)/a(1)/a(2): gcc 1，dcc -O2 **7/7/7**（a(4) 两边都是 7）
 *
 * 循环体其实只正确跑了 2 轮 —— 错在**出口接线**：`rebuild_cycle` 把节点头的分支提升成
 * `while` 条件后，`break` 与「循环落出」都接到侧出口 `if.then` 的 `return 7` 上，
 * `for.end` 的 `return 1` 再也到不了。反向补丁（改接 `for.end`）会把 a(0) 变成 0
 * （落出路径后面空无一物，函数直接落到序言），见 `has_multi_exit_cycle` 的注释。
 *
 * 所有期望值都取自 `gcc -O0` 的实测输出（本文件初稿有 3 处是手算的，与 gcc 不符，
 * 包括一个「break 在 i==1 先触发、return 999 永远到不了」的自相矛盾设计）。
 *
 * 判据是双层的（本仓库踩过「退出码说谎」与「只比退出码」两个坑）：
 *   1. 每行打印结果，失败原因在 stdout 里直接可读；
 *   2. 退出码独立表达失败（0 = 全对）。
 */
#include <stdio.h>

static int fails = 0;

#define CHK(got, want, tag)                        \
    do {                                           \
        int g_ = (got), w_ = (want);                \
        if (g_ != w_) {                            \
            printf("FAIL %s: got %d want %d\n", (tag), g_, w_); \
            fails++;                               \
        }                                          \
    } while (0)

/* 1 —— 最小形态：侧出口 return 与循环落出是两个出口（三个入参都要，n=0/1/2 都曾各错一次） */
static int early_return(int n)
{
    for (int i = 0; i < n; i++) {
        if (i == 2) {
            return 7;
        }
    }
    return 1;
}

/* 2 —— while 形（条件在头、自增在体里） */
static int early_return_while(int n)
{
    int i = 0;
    while (i < n) {
        i++;
        if (i == 3) {
            return 42;
        }
    }
    return -1;
}

/* 3 —— 侧出口自己带语句：只发 break 会把这些语句丢掉（同一判据的另一半） */
static int side_effect;
static int early_return_with_stmt(int n)
{
    side_effect = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            side_effect = 100 + i;
            return 5;
        }
    }
    return 6;
}

/* 4 —— return 与 break 同时存在：谁先触发取决于下标，两个方向都要测 */
static int return_first(int n)
{
    int sum = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            return 999;
        }
        if (i == 3) {
            break;
        }
        sum += i;
    }
    return sum;
}

static int break_first(int n)
{
    int sum = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            break;
        }
        if (i == 3) {
            return 999;
        }
        sum += i;
    }
    return sum;
}

/* 5 —— 反向对照：没有提前 return 的累加器形状（一直是好的，用来证明判据不是空的） */
static int accumulate(int n)
{
    int r = 1;
    for (int i = 0; i < n; i++) {
        if (i == 2) {
            r = 0;
        }
    }
    return r;
}

/* 6 —— 嵌套：内层提前 return，外层还要继续跑完/落出 */
static int nested_inner(int m)
{
    for (int j = 0; j < m; j++) {
        if (j == 1) {
            return 77;
        }
    }
    return 78;
}

static int nested_outer(int n)
{
    int acc = 0;
    for (int i = 0; i < n; i++) {
        acc += nested_inner(i);
        if (i == 2) {
            return acc + 1;
        }
    }
    return acc;
}

/* 7 —— 循环里同时有 return、break、continue 三种出边 */
static int all_three(int n)
{
    int sum = 0;
    for (int i = 0; i < n; i++) {
        if (i == 3) {
            return 7000;
        }
        if (i == 5) {
            break;
        }
        if (i % 2 == 0) {
            continue;
        }
        sum += i;
    }
    return sum;
}

int main(void)
{
    CHK(early_return(0), 1, "n=0 一次都不跑");
    CHK(early_return(1), 1, "n=1 不该走到 return 7");
    CHK(early_return(2), 1, "n=2 不该走到 return 7");
    CHK(early_return(5), 7, "n=5 该走到 return 7");

    CHK(early_return_while(2), -1, "while 未命中要落出");
    CHK(early_return_while(3), 42, "while 命中 return");
    CHK(early_return_while(4), 42, "while 命中 return（多一圈）");

    CHK(early_return_with_stmt(1), 6, "未命中：return 6");
    CHK(side_effect, 0, "未命中时侧出口的语句不该执行");
    CHK(early_return_with_stmt(2), 5, "命中：return 5");
    CHK(side_effect, 101, "命中时侧出口的语句必须执行");

    CHK(return_first(1), 0, "return 与 break 都没触发");
    CHK(return_first(2), 999, "return 先触发");
    CHK(return_first(4), 999, "return 仍先触发");
    CHK(break_first(1), 0, "break 先触发（sum=0）");
    CHK(break_first(2), 0, "break 后 return 不可达");
    CHK(break_first(4), 0, "同上，多几圈");

    CHK(accumulate(2), 1, "累加器形状未命中");
    CHK(accumulate(3), 0, "累加器形状命中");

    CHK(nested_inner(0), 78, "内层 n=0 直接落出");
    CHK(nested_inner(2), 77, "内层命中 return");
    CHK(nested_outer(1), 78, "外层 1 圈");
    /* nested_inner(1) 落在 78（j==1 需要 m>1 才命中），所以两圈是 78+78 */
    CHK(nested_outer(2), 156, "外层 2 圈后落出");
    CHK(nested_outer(3), 234, "外层第 3 圈命中 return");
    CHK(nested_outer(4), 234, "外层多几圈不变");

    /* continue 只跳过偶数下标，所以 sum = 1 + …；return 在 break 之前 */
    CHK(all_three(3), 1, "continue 跳偶数、只加到 1");
    CHK(all_three(4), 7000, "命中 return");
    CHK(all_three(9), 7000, "return 先于 break，多几圈也一样");

    printf("%s\n", fails == 0 ? "ALL OK" : "FAILED");
    return fails == 0 ? 0 : 1;
}
