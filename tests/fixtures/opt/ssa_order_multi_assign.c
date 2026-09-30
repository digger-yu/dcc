/* SSA 重排类 pass 的「同基名多次赋值」顺序夹具。
 *
 * 本 IR 的 SSA→AST 重建按**基名塌缩**：`y_1/y_2/y_3` 是三个互不相关的 SSA 名，
 * 重建后都是同一个变量 `y`。因此任何块内重排（投机上提、list scheduling）只要
 * 跨过一次对 `y` 的写入，就会颠倒对同一变量的两次赋值 —— 结果是**丢掉后续写入**。
 *
 * 这一条是本轮自己引入又被抓出的回归：投机执行改成「按依赖闭包上提」后，
 * `y = 20; y += 3;` 被提到 `y = 5;` 之前，`b()` 从 23 变成 5。
 *
 * 通过: 返回 0（各函数结果与 -O0 / gcc 一致）
 */
int b_seq(void) {
    int y = 5;
    y = 20;
    y += 3;
    return y; /* 23 */
}

const int cx = 10;

int a_const(void) {
    int y = 5;
    y = cx;
    y = 20;
    y += 3;
    return y; /* 23 */
}

/* 同一基名反复赋值 + 中途读取: 上提必须完全放弃。 */
int c_chain(int n) {
    int s = 0;
    s = n;
    int t = s;
    s = t * 2;
    s = s + 1;
    return s; /* n*2+1 */
}

/* 两个不同基名交替写: 顺序仍不可颠倒（s 依赖 t 的当前值）。
   t=n+1; s=t*3; t+=100; s+=t  ⇒  n=5 时 18 + 106 = 124。 */
int d_interleave(int n) {
    int t = 0;
    int s = 0;
    t = n + 1;
    s = t * 3;
    t = t + 100;
    s = s + t;
    return s; /* (n+1)*3 + (n+101) */
}

int main(void) {
    if (b_seq() != 23) return 1;
    if (a_const() != 23) return 2;
    if (c_chain(4) != 9) return 3;
    if (c_chain(0) != 1) return 4;
    if (d_interleave(5) != 124) return 5;
    if (d_interleave(0) != 104) return 6;
    return 0;
}
