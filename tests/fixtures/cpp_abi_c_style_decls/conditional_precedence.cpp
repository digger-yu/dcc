// [expr.cond] + [expr.ass]: **三元的优先级高于赋值**, 也高于逗号运算符。
//
// 早前 `parse_expr` 先做 `parse_assignment` 再看 `?`, 于是
//     t = a > 3 ? 100 : 200;
// 被解析成
//     (t = (a > 3)) ? 100 : 200;     // t 得到 1 而不是 100
// 这是赋值 + 三元的**最常见**写法 (条件赋值), 一旦 precedence 错位, 程序不是
// 编不过而是**静默算错** —— 比编译失败危险得多。
//
// 交叉验证口径: 每个变量的最终值都逐行打印, 与 g++ 比较。
//
// 本夹具同时守住两件容易只修一半的事:
//  - **三元只求值被选中的分支** ([expr.cond]/1): 「假分支带副作用」的写法必须
//    看到 `w2` 保持 0 (用 SSA `Select` 降级会把两个分支都执行);
//  - **嵌套分支的块重建不得丢代码**: `else` 里再嵌 `if` / 三元分支里放
//    `&&` 短路链, 早期实现会让 `If` 之后的所有语句只在一条路径上执行。
#include <cstdio>

int pick(int a) { return a > 0 ? a * 2 : a * 3; }

int main() {
    // 1. 赋值右操作数是三元
    int a = 5;
    int t = a > 3 ? 100 : 200;
    printf("t=%d\n", t);

    // 2. 复合赋值右操作数是三元
    int u = 0;
    u += a > 10 ? 7 : 9;
    printf("u=%d\n", u);

    // 3. 三元嵌套 (右结合): cond ? x : (cond ? y : z)
    int v = 0;
    v = a > 10 ? 1 : a > 3 ? 2 : 3;
    printf("v=%d\n", v);

    // 4. 三元的中间操作数可以再是赋值; 未选中的分支不得执行
    int w1 = 0, w2 = 0;
    int w = a > 3 ? (w1 = 11) : (w2 = 22);
    printf("w=%d w1=%d w2=%d\n", w, w1, w2);
    int x1 = 0, x2 = 0;
    int x = a > 99 ? (x1 = 111) : (x2 = 222);
    printf("x=%d x1=%d x2=%d\n", x, x1, x2);

    // 5. 逗号表达式作为整体出现在括号里
    int c1 = 0;
    int cz = (c1 = a, c1 + 1);
    printf("cz=%d c1=%d\n", cz, c1);

    // 6. 三元与逻辑运算的结合: 条件是 logical-OR 表达式
    int flag = 0;
    int f = a > 1 || a > 3 ? 5 : 6;
    f = a > 1 && a > 9 ? 5 : 6;
    flag = (a == 5 ? 1 : 0) + (a > 0 ? 1 : 0);
    printf("f=%d flag=%d\n", f, flag);

    // 7. 函数返回值来自三元
    printf("pick=%d,%d\n", pick(4), pick(-4));

    // 8. 三元的两个分支是不同侧类的指针/引用场景 (常见 cond ? x : y 值合并)
    int s = (a > 0) + (a > 3) * 10;
    printf("s=%d\n", s);

    // 9. return 语句里赋值与三元并存 (整条语句都要重新检查 precedence)
    int acc = 0;
    acc = a > 4 ? acc + 1 : acc + 2;
    acc = a > 9 ? acc + 100 : acc + 200;
    printf("acc=%d\n", acc);

    // 10. `&&` 短路链坐在三元的分支里 (与嵌套 if 同一个块重建形状)
    int p = 0;
    int q = a > 10 ? 1 : (a > 3 && a < 9);
    p = (a > 1 && a < 9) ? 7 : 8;
    printf("q=%d p=%d\n", q, p);
    return t == 100 && u == 9 && v == 2 && acc == 201 ? 0 : 1;
}
