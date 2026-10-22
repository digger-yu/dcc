/* P13 阶段3 回归夹具: `__int128` 在**分支/真值语境**里必须按两半判。
**
** 本后端 16 字节整型的 rvalue 约定是「低半在 rax、高半在 rdx」一对，而消费 flags 的
** 那条兜底通道 `emit::expr::float::emit_truth_flags` 过去只按 `ty.size() > 4` 选
** `test %rax,%rax` —— 对 64 位以上正确，对 128 位**只测了低半**。
** 「低 64 位为 0、高 64 位非 0」于是被判成假，且只在条件被分解成「测值」时出现：
** 比较运算本身走 `gen_int128_binop`，两半都比，所以错值只从这三条入口漏进来：
**   1. `x != 0` / `x == 0` —— `gen_cond_jump_to` 消掉常量比较，直接落到测值；
**   2. `!x` —— 极性翻转后同样落到测值；
**   3. `if (x || y)` / `if (x && y)` —— 操作数本身就是 128 位值，没有比较可直生。
** 第 1 条在反汇编快照 `pr105613` 里现过形（`r[0] != 0` 被编成一条 `test %rax,%rax`），
** 但那个语料只覆盖 lo!=0 与全 0 两种数据，**恰好漏掉 hi-only**，所以它能一直绿。
**
** 数据一律取三组：`hi_only`（(u128)1<<64，lo==0 && hi!=0，最小反例）、`lo_only`、`zero`。
** 每段各占一个独立的 2^k，最后 `fail ? 1 : 0` 而不是 `return fail`：退出码只有 8 位，
** 直接返回累加值会把 k>=8 的那些段截掉（错得再狠也看不出来）。*/

typedef unsigned __int128 u128;
typedef __int128 i128;

/* 1. 常量比较被消成测值：三种真值走向。 */
__attribute__((noinline)) static int branch_ne_zero(u128 x) {
    if (x != 0)
        return 1;
    return 0;
}

__attribute__((noinline)) static int branch_eq_zero(u128 x) {
    if (x == 0)
        return 1;
    return 0;
}

/* 2. `!x` 走极性翻转。 */
__attribute__((noinline)) static int branch_not(u128 x) {
    if (!x)
        return 1;
    return 0;
}

/* 3. 操作数直接是 128 位值：`||` 的左/右臂与 `&&`。 */
__attribute__((noinline)) static int branch_lor(u128 x, u128 y) {
    if (x || y)
        return 1;
    return 0;
}

__attribute__((noinline)) static int branch_land(u128 x, u128 y) {
    if (x && y)
        return 1;
    return 0;
}

/* 4. 有符号 128 位的 -2^64：lo==0、hi==-1，与无符号 hi_only 同一形状。 */
__attribute__((noinline)) static int branch_signed(i128 x) {
    if (x != 0)
        return 1;
    return 0;
}

/* 5. 左真时右操作数的 store 一次都不许发生（钉住分解没把短路拆散）。 */
__attribute__((noinline)) static int branch_lor_store(u128 x, int *p) {
    if (x != 0 || (*p = 9) != 0)
        return 1;
    return 0;
}

/* 6. do-while 的「真则回头」语境（jump_if_true == true 那条臂）。 */
__attribute__((noinline)) static int loop_truth(u128 x, int n) {
    int steps = 0;
    do {
        x = x >> 1;
        steps++;
        if (steps > n + 4)
            break;
    } while (x != 0);
    return steps;
}

int main(void) {
    u128 hi_only = (u128)1 << 64;
    u128 lo_only = (u128)7;
    u128 zero = 0;
    int fail = 0;
    int v;

    /* 1. */
    fail += branch_ne_zero(hi_only) != 1 ? 1 : 0;
    fail += branch_ne_zero(zero) != 0 ? 2 : 0;
    fail += branch_ne_zero(lo_only) != 1 ? 4 : 0;
    fail += branch_eq_zero(hi_only) != 0 ? 8 : 0;
    fail += branch_eq_zero(zero) != 1 ? 16 : 0;

    /* 2. */
    fail += branch_not(hi_only) != 0 ? 32 : 0;
    fail += branch_not(zero) != 1 ? 64 : 0;

    /* 3. 左侧 hi-only 为真时右侧全不许碰；右侧为真时左侧必须假。 */
    fail += branch_lor(hi_only, zero) != 1 ? 128 : 0;
    fail += branch_lor(zero, hi_only) != 1 ? 256 : 0;
    fail += branch_lor(zero, zero) != 0 ? 512 : 0;
    fail += branch_land(hi_only, lo_only) != 1 ? 1024 : 0;
    fail += branch_land(hi_only, zero) != 0 ? 2048 : 0;
    fail += branch_land(zero, hi_only) != 0 ? 4096 : 0;

    /* 3b. 值语境：比较结果被当值用，两半都必须参与。 */
    fail += (hi_only != 0) != 1 ? 8192 : 0;
    fail += (zero != 0) != 0 ? 16384 : 0;

    /* 4. */
    fail += branch_signed(-((i128)1 << 64)) != 1 ? 32768 : 0;
    fail += branch_signed((i128)0) != 0 ? 65536 : 0;

    /* 5. x 只有高半非零 ⇒ 左侧为真，右操作数的 store 不得发生。 */
    v = -1;
    fail += branch_lor_store(hi_only, &v) != 1 ? 131072 : 0;
    fail += v != -1 ? 262144 : 0;
    v = -1;
    fail += branch_lor_store(zero, &v) != 1 ? 524288 : 0;
    fail += v != 9 ? 1048576 : 0;

    /* 6. do-while 至少走一趟：zero 进一次就出；lo_only=7 右移 3 次归零。 */
    fail += loop_truth(zero, 1) != 1 ? 2097152 : 0;
    fail += loop_truth(lo_only, 8) != 3 ? 4194304 : 0;

    return fail ? 1 : 0;
}
