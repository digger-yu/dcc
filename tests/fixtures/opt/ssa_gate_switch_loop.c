/* SSA 准入闸门夹具：循环嵌在 switch / label / 语句表达式里。
 *
 * `run_ssa_passes` 用 `func_has_loop` 决定「本函数不进 SSA 管线」(rebuild_cycle 对
 * 循环 CFG 的重建尚不完全正确)。旧实现漏了 `Stmt::Switch` 分支 ⇒ switch 里的循环
 * 读不到，函数带着循环进了 SSA；而 `lower_switches_in_func` 是在闸门**之后**才把
 * switch 摊成 if-chain，循环因此在闸门读过旧形态之后才出现。
 *
 * 修复前的实测结果（`-O2`）：a(0) HANG、b(5) 错值、c(4) SEGV。
 * 通过: exit 0（期望值取自 gcc 参照）
 */
int a(int k) {
    int s = 0;
    switch (k) {
    case 0: {
        int n = 3;
        while (n > 0) {
            s += n;
            n--;
        }
        break;
    }
    default:
        s = 7;
    }
    return s;
}

int b(int k) {
    int s = 0;
    switch (k) {
    case 1:
        for (int i = 0; i < 4; i++)
            s += i;
        break;
    default:
        while (k-- > 0)
            s += k;
    }
    return s;
}

/* do-while + switch 的组合: 循环在 switch **之前**, switch 只读循环携带值。 */
int c(int k) {
    int s = 0;
    do {
        s += k;
        k--;
    } while (k > 0);
    switch (s) {
    case 10:
        return 1;
    default:
        return 2;
    }
}

/* 标签下的循环: `Stmt::Label` 同样必须被闸门遍历到。 */
int d(int k) {
    int s = 0;
    int n = k;
loop:
    if (n > 0) {
        s += n;
        n--;
        goto loop;
    }
    return s;
}

int main(void) {
    if (a(0) != 6) return 1;
    if (a(9) != 7) return 2;
    if (b(1) != 6) return 3;
    if (b(5) != 10) return 4;
    if (c(4) != 1) return 5;
    if (c(0) != 2) return 6;
    if (d(4) != 10) return 7;
    if (d(0) != 0) return 8;
    return 0;
}
