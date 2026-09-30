/* LICM 根因夹具 ①：while 循环体内含循环携带写（原始复现形态）。
 *
 * 修复前 `-O2/-O3`：`Stmt::While` 分支把 loop_var 传成 `None`，
 * `expr_mentions(e, None)` 恒为 false ⇒ 体内**每一条**语句都被判成不变式，
 * 整个循环体被搬空成 `while (n > 0) {}` 忙等（根因 1/2/3）。
 * 因此本夹具在带缺陷的编译器下不会返回错误值，而是**永不退出** ——
 * 测试侧运行 dcc 产物时必须带超时（见 tests/integration/opt_licm.rs）。
 *
 * 期望退出码取自 gcc 参照，逐档位（-O0/-O1/-O2/-O3）对照。
 */
int sum_down(int n) {
    int s = 0;
    while (n > 0) {
        s += n;
        n--;
    }
    return s;
}

/* 体内既有循环携带写、又有真正的不变式声明：只有后者允许外提。 */
int sum_down_step2(int n) {
    int s = 0;
    while (n > 0) {
        int step = 2;
        s += step * n;
        n--;
    }
    return s;
}

/* cond 终止所依赖的写入全在体内 ⇒ 退化护栏必须放弃本次外提（根因 3/5）。 */
int countdown(int n) {
    int hits = 0;
    while (n > 0) {
        int keep = 1;
        hits += keep;
        n = n - keep;
    }
    return hits;
}

int main(void) {
    if (sum_down(0) != 0) return 1;
    if (sum_down(3) != 6) return 2;
    if (sum_down(10) != 55) return 3;
    if (sum_down_step2(3) != 12) return 4;
    if (sum_down_step2(0) != 0) return 5;
    if (countdown(5) != 5) return 6;
    if (countdown(0) != 0) return 7;
    return 0;
}
