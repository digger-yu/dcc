/* SSA→AST 重建的内联窗口：把临时值内联到使用点，不得跨过对同基名的写入。
 *
 * 重建把 `n_1/n_2/...` 塌缩回同一个变量 `n`，于是「内联一条指令」在真 SSA 里
 * 无害（读的是不可变名字），在塌缩后的 AST 里却等于把那次读**搬到使用点重算**。
 * 调度器把 `_tbin = 2*n` 和它的唯一使用 `s = _tbin + s` 拉开、中间夹一条 `n` 的
 * 赋值时，内联结果就成了 `s = (2*n) + s` —— 乘的是已自减后的 `n`。实测 `a(4)`
 * 在 -O3 返回 6 而不是 20（只有 -O3 触发，因为只有 level>=3 跑调度）。
 *
 * 同一条根因还有第二种形态：`while (k-- > 0)` 的**自减写在节点头块**，而
 * `rebuild_cycle` 在能提出循环条件时会整体丢弃节点头块的普通指令，于是 `k`
 * 永不递减（忙等）。两条都由 `plan_inlined_temps` 的窗口判定 + `header_insts_droppable`
 * 兜住。
 *
 * 通过: exit 0（期望值取自 gcc -O0 参照）
 */

/* 体内不变量 × 循环携带变量，夹一次 `n--` */
int step_mul(int n) {
    int s = 0;
    while (n > 0) {
        int step = 2;
        s += step * n;
        n--;
    }
    return s;
}

/* 条件里带自减：节点头必须保留 `k` 的写，且比较必须用自减**前**的值 */
int post_cond(int k) {
    int s = 0;
    while (k-- > 0)
        s += k;
    return s;
}

int post_cond_zero(int k) {
    int s = 5;
    while (k-- > 0)
        s += k;
    return s;
}

int post_cond_one(int k) {
    int s = 5;
    while (k-- > 0)
        s += k;
    return s;
}

/* 循环携带的乘数：`m` 在体内自增，`s += m*i` 必须用当轮的 m */
int mul_add(int n) {
    int s = 0;
    int m = 3;
    for (int i = 0; i < n; i++) {
        s += m * i;
        m = m + 1;
    }
    return s;
}

/* do-while 同形 */
int do_mul(int n) {
    int s = 0;
    int k = n;
    do {
        s += 2 * k;
        k--;
    } while (k > 0);
    return s;
}

/* 体内读「刚被同一个表达式写过」的累加器 */
int shift_acc(int n) {
    int s = 0;
    while (n > 0) {
        int w = s;
        s += w + n;
        n--;
    }
    return s;
}

/* for 的步长在体内被读两次，中间夹一次改写 */
int twice_step(int n) {
    int s = 0;
    for (int i = 0; i < n; i = i + 1) {
        int a = i + i;
        i = i + 0;
        s = s + a;
    }
    return s;
}

int main(void) {
    if (step_mul(4) != 20)
        return 1;
    if (step_mul(0) != 0)
        return 2;
    if (post_cond(4) != 6)
        return 3;
    if (post_cond(0) != 0)
        return 4;
    if (post_cond_zero(0) != 5)
        return 5;
    if (post_cond_one(1) != 5)
        return 6;
    if (mul_add(4) != 32)
        return 7;
    if (do_mul(4) != 20)
        return 8;
    if (shift_acc(3) != 17)
        return 9;
    if (twice_step(4) != 12)
        return 10;
    return 0;
}
