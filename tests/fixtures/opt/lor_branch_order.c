/* P13 阶段3 回归夹具: 分支语境的 `A || B` 必须保持短路顺序。
**
** 犯案的判据在 `emit::expr::utils::gen_cond_jump_to` 的分解表里。If/While/For 走的是
** 「假则跳出」（jump_if_true == false）语境，那条臂曾把 `&&` 与 `||` 当成同一件事：
**   * `!(l && r)` = 「任一侧为假就跳」—— 拆成两条同向跳到出口，正确；
**   * `!(l || r)` = 「两侧都假才跳」—— 左真时**不能**跳，必须落到右操作数。
** 用 `&&` 的拆法编 `if (a || f())`：左侧为假就提前跳出，右操作数一次都不求值；
** 左侧为真时反过来去求值右操作数。`short_circuit_rhs_impure` 从 SSA 闸门侧接住了
** 它（那里的右值带 load，急切求值会打空），本夹具从**分支语境**直接接：既钉「该跳的
** 跳了」，也钉「不该发生的写发生了」，还钉「`||` 里嵌 `&&` 时递归分解没走偏」。
**
** 五段形状各有分工：
**   `lor_side_effect`：左真 ⇒ 右值的 store 一次都不许发生；
**   `lor_take_branch`：左假、右值为真 ⇒ 必须走进 then（错拆时落进 else）；
**   `lor_pure_both`：两侧都是纯值 ⇒ 四种走向的返回布尔都要对（值语境与分支语境分开钉）；
**   `lor_nested_and`：`a || (b && c)` ⇒ 递归分解的四种走向；
**   `lor_while`：`while (a || b)` 是 jump_if_true == true 那条臂，与上面互为反向。
**
** main 里的判断一律只用单侧比较与 `&&`：被测的正是 `||`，若再用 `||` 来判断结果，
** 分解错时连「报错」这一步都会一起错掉（自举盲区）。另外最后返回
** `mask ? 1 : 0` 而不是 `mask` 本身：退出码只有 8 位，直接返回会把 bit>=8 的段截掉。*/

static int sink;

/* 左真：右操作数整个不许发生，*p 必须保持初值。 */
__attribute__((noinline)) static int lor_side_effect(int a, int *p) {
    if (a != 0 || (*p = 5) != 0)
        return 1;
    return 0;
}

/* 左假、右值为真：必须走进 then。 */
__attribute__((noinline)) static int lor_take_branch(int a, int b) {
    if (a != 0 || b > 100)
        return 1;
    return 0;
}

/* 两侧都是纯比较：`||` 的**值**语境（右值还要多读一次 sink，钉住它没被整条丢掉）。 */
__attribute__((noinline)) static int lor_pure_both(int a, int b) {
    int r = (a > 0 || b > 0);
    sink = a + b;
    return r;
}

/* `a || (b && c)`：混嵌一层，判据必须递归而不是只拆最外层。 */
__attribute__((noinline)) static int lor_nested_and(int a, int b, int c) {
    if (a || (b && c))
        return 1;
    return 0;
}

/* `while (i > 0 || j > 0)`：真则回头那条臂（jump_if_true == true）。 */
__attribute__((noinline)) static int lor_while(int i, int j) {
    int steps = 0;
    while (i > 0 || j > 0) {
        if (i > 0)
            i -= 2;
        else
            j -= 3;
        steps++;
        if (steps > 20)
            break;
    }
    return steps;
}

int main(void) {
    int mask = 0;
    int v;

    /* 1. 左真 ⇒ 右值的 store 不得发生。 */
    v = -1;
    if (lor_side_effect(1, &v) != 1)
        mask |= 1;
    if (v != -1)
        mask |= 2;
    /* 左假 ⇒ 右值必须发生（写 5 后为真，走 then）。 */
    v = -1;
    if (lor_side_effect(0, &v) != 1)
        mask |= 4;
    if (v != 5)
        mask |= 8;

    /* 2. 左假、右值为真时必须走 then；右值为假时走 else。 */
    if (lor_take_branch(0, 200) != 1)
        mask |= 16;
    if (lor_take_branch(0, 5) != 0)
        mask |= 32;
    if (lor_take_branch(1, 200) != 1)
        mask |= 64;
    if (lor_take_branch(7, 0) != 1)
        mask |= 128;

    /* 3. 纯值的四种走向。 */
    if (lor_pure_both(1, 0) != 1)
        mask |= 256;
    if (lor_pure_both(0, 1) != 1)
        mask |= 512;
    if (lor_pure_both(0, 0) != 0)
        mask |= 1024;
    if (lor_pure_both(-1, -1) != 0)
        mask |= 2048;

    /* 4. `a || (b && c)` 的四种走向。 */
    if (lor_nested_and(1, 0, 0) != 1)
        mask |= 4096;
    if (lor_nested_and(0, 1, 1) != 1)
        mask |= 8192;
    if (lor_nested_and(0, 1, 0) != 0)
        mask |= 16384;
    if (lor_nested_and(0, 0, 1) != 0)
        mask |= 32768;

    /* 5. 回头语境：(4,0) 走 i 臂 2 步，(0,6) 走 j 臂 2 步，(0,0) 一步不走。 */
    if (lor_while(4, 0) != 2)
        mask |= 65536;
    if (lor_while(0, 6) != 2)
        mask |= 131072;
    if (lor_while(0, 0) != 0)
        mask |= 262144;
    /* 一臂耗尽后换另一臂继续：(1,1) 先把 i 减到 -1，再把 j 减到 -2 ⇒ 2 步。 */
    if (lor_while(1, 1) != 2)
        mask |= 524288;

    return mask ? 1 : 0;
}
