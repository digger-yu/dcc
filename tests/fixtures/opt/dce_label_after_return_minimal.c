/* G2 最小形状：`-O2 eliminate_unreachable` (`src/opt.rs::dce_block`) 的
 * 「Label 后紧跟的顶层 item 也算不可达」误判。
 *
 * 结构事实：C 语法里 `out: s -= 3;` 是一条 *labeled statement*，只把紧随标签的
 * **一条语句**打包进 `Stmt::Label { stmt }`。函数体的 block-level items 序列因此
 * 长这样：
 *     [Goto, Return, Label(out){s-=3}, Return, Label(late){s-=5}, Return]
 *
 * 旧 `dce_block` 在遇到顶层 `Return` 后把 stopped 置 true，此后再碰到任何非
 * `Label` 的 item 一律 retain=false —— 于是 `Label(out){...}` 之后那条 `return s;`
 * 被当成不可达清掉；同理 `Label(late){...}` 之后的 `return s;` 也被清掉。
 * `goto out` 从 `out:` 顺落到 `late:`，再从函数尾 fall-off，`%eax` 只剩最后一条
 * 对 `s` 的写：gcc `-3` 变 dcc `-8`（`s -= 3; s -= 5;` 连做两次）。
 *
 * 修法（`src/opt.rs::dce_block`）：Label 是跳入入口，遇到它就把 stopped 复位；
 * 之后的 item 保留到本 block 末尾或下一个顶层 Return（再触发 stopped）。
 * 这样本夹具在四档 (0/1/2/3) 都与 gcc 逐字节一致。
 *
 * F3 保留：`return` 之后**没有标签**的真不可达仍需被清掉 —— 防止修 G2 时矫枉
 * 过正、把 dce 修成完全不删。它的 stdout 与 gcc 一致（都是 `1 2`）。
 */
#include <stdio.h>

/* F1: 双标签 + 顶层 Return —— 触发旧 dce_block 误清的最小形状 */
static int f_goto_two_labels(int n)
{
    int s = n;
    goto out;
    return 999;
out:
    s -= 3;
    return s;
late:
    s -= 5;
    return s;
}

/* F2: 循环 + 双标签 —— 与 loop_exit_ownership_goto_pending.c 同族，
 * 保留在此是为了把这条形状同时钉进「最小夹具」侧，不受 loop 相关 pass 影响。 */
static int f_loop_goto_second_label(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            goto out;
        }
        if (i == 3) {
            goto late;
        }
        s += i;
    }
    s += 7;
    return s;
out:
    s -= 3;
    return s;
late:
    s -= 5;
    return s;
}

/* F3: return 之后是**没有标签**的真不可达 —— 必须仍被 dce 掉。
 * 修 G2 后这条不受影响（stopped 只在遇到 Label 时复位，此处没有 Label）。 */
static int f_real_dead_after_return(int n)
{
    if (n == 0) {
        return 1;
        printf("SHOULD_NEVER_PRINT\n");
    }
    return 2;
}

int main(void)
{
    for (int k = 0; k < 3; k++) {
        printf("F1 %d %d\n", k, f_goto_two_labels(k));
    }
    for (int k = 0; k <= 6; k++) {
        printf("F2 %d %d\n", k, f_loop_goto_second_label(k));
    }
    printf("F3 %d %d\n", f_real_dead_after_return(0), f_real_dead_after_return(1));
    return 0;
}
