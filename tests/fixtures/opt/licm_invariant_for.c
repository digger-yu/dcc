/* LICM 正面夹具 ③：for 体内读"真正的循环不变量"（全局 + 参数）。
 *
 * 修复不是把外提能力一刀切废掉：`gv` 与参数 `k` 在循环里都没被写、cond/step 也不
 * 碰它们 ⇒ 它们落在名字栅栏之外，`int t = gv * k + 918273;` **应当**被外提到
 * `for` 之前，且结果分毫不变。
 *
 * 魔数 918273 只用于 `-S` 结构断言：`tri` 的迭代次数是编译期常量 3，而
 * run_with_target 先跑 LICM 再跑 unroll ⇒ 若不变式真被外提，-O3 展开后该魔数在
 * 汇编里只出现 1 次；若 LICM 什么都没搬，展开会把它复制 3 份。
 */
int gv = 6;

int tri(int k) {
    int s = 0;
    for (int i = 0; i < 3; i++) {
        int t = gv * k + 918273;
        s += t + i;
    }
    return s;
}

/* 同一名字在体内被写 ⇒ 读它的声明立刻不再是不变式（与 ③ 的上半部分成对）。 */
int walking(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        gv = gv + 1;
        int t = gv * 2;
        s += t;
    }
    return s;
}

/* 归纳变量 i 自身被 step 写 ⇒ 提到 i 的表达式不可外提；下标为不变量的数组元素
   读则是允许的（`a[1]`）。 */
int indexed(int n) {
    int a[4];
    a[0] = 1;
    a[1] = 20;
    a[2] = 300;
    a[3] = 4000;
    int s = 0;
    for (int i = 0; i < n; i++) {
        int t = a[1] + a[2];
        s += t + i;
    }
    return s;
}

int main(void) {
    if (tri(3) != 2754876) return 1;
    if (tri(0) != 2754822) return 2;
    if (walking(3) != 48) return 3;
    if (gv != 9) return 4;
    if (indexed(3) != 963) return 5;
    if (indexed(0) != 0) return 6;
    return 0;
}
