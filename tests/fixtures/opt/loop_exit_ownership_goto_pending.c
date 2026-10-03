/* G2已修：`goto` 跳到循环后第二个标签的旧行为在 -O2/-O3 静默错值（gcc `7 7 -3 -3 -3 -3 -3`
 * → dcc `7 7 -8 -8 -8 -8 -8`）。与多出口循环门无关，也不在 SSA 一侧。
 *
 * 历史实测归因（保留在本文件头）：`DCC_OPT_SKIP=run_ssa_passes`（整条 SSA 往返关掉）
 * 仍错 ⇒ 病灶在 AST 层。含 `goto` 的函数本来就被 `is_structured` 挡在 SSA 外，
 * 与 SSA 侧任何改动无关。具体到 pass：`DCC_OPT_SKIP=eliminate_unreachable` 后四档
 * 全对 ⇒ `src/opt.rs::dce_block`（`-O2` 的 `eliminate_unreachable` 内部）。
 *
 * 根因：`Stmt::Label { stmt }` 只把紧随标签的**一条语句**纳入自己。本夹具里
 * block-level items 是 `[VarDecl, For, s+=7, Return, Label(out){s-=3}, Return,
 * Label(late){s-=5}, Return]`。旧 `dce_block` 在顶层 Return 后 stopped=true，
 * 只保留后续 Label —— `Label(out){...}` 之后的 `return s;` 被当成不可达清掉。
 * 于是 `goto out` 从 `out: s-=3;` 直落进 `late: s-=5;`，再从函数尾 fall-off，
 * `%eax` = `-8`（`s -= 3 - 5`）而不是 `out:` 单独少 3 的 `-3`。
 *
 * 修法（见 `src/opt.rs::dce_block`）：Label 是跳入入口，遇到它就把 stopped 复位。
 * 四档与 gcc 逐字节一致后，本夹具从「pending不入语料门」改为「进门禁」。
 *
 * 参：docs/wp_goto_pending.md（G2 定位与修复报告）。
 */
#include <stdio.h>

/* A2 goto 跳出循环，且循环后面还有分支代码（自然落出与 goto 目标是两个块） */
static int a_goto_out2(int n)
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
int main(void)
{
    for (int k = 0; k <= 6; k++) {
        printf("goto_out2 %d\n", a_goto_out2(k));
    }
    return 0;
}
