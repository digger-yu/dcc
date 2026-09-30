/* LICM 根因夹具 ②：do-while 同形复现（体至少执行一次，cond 在体后）。
 *
 * 与 ① 同一失效路径：`Stmt::Do` 分支过去也传 `loop_var = None`，于是整个体被
 * 当成不变式搬空。do-while 的额外风险点在**栅栏的作用域**：cond 读的名字由体内
 * 写入推进，若把这些写入搬出体外，循环推进量就不再每轮生效（根因 3/5）。
 */
int sum_down(int n) {
    int s = 0;
    do {
        s += n;
        n--;
    } while (n > 0);
    return s;
}

/* 体内有真正不变式声明 + 循环携带写：只准搬不变式。 */
int with_invariant(int n) {
    int s = 0;
    do {
        int t = 5;
        s += t + n;
        n--;
    } while (n > 0);
    return s;
}

/* 名字在**体内**被写（k = 0）⇒ 依赖它的声明不是不变式；按名字逐个判定，
   不能因为"看起来像初始化"就放行。 */
int body_written_bound(int n) {
    int s = 0;
    int k = 2;
    do {
        int t = k + 1;
        s += t + n;
        n--;
        k = 0;
    } while (n > 0);
    return s;
}

int main(void) {
    if (sum_down(3) != 6) return 1;
    if (sum_down(1) != 1) return 2;
    /* do-while 至少执行一次：n = 0 时仍累加一个 0。 */
    if (sum_down(0) != 0) return 3;
    if (with_invariant(2) != 13) return 4;
    if (with_invariant(0) != 5) return 5;
    if (body_written_bound(2) != 7) return 6;
    if (body_written_bound(0) != 3) return 7;
    return 0;
}
