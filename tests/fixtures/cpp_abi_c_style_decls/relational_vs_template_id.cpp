// [temp.names]: **`<` 的消歧** —— 比较运算符 vs 模板实参列表开端。
//
// `if (a < b && c > d)` 里扫描 `<` 之后能否闭合 `>` 时, 只看尖括号平衡是不够的:
// 闭合 `>` 后面紧跟的是一个**裸标识符** (`d`), 表达式语法里「值后面紧跟标识符」
// 永远不合法, 所以那个 `<` 只能是比较。早前 dcc 的启发式只看「能否闭合」, 于是
// `a < b && c > d` 被当成 `a < (b && c) > d` 的 template-id, 整条语句降级成
// OpaqueStmt, 整个 TU 退回 legacy 文本降级 —— 而 `x < y && w > z` /
// `f(a<b, c>d)` 是最基础的 C++ 写法。
//
// 交叉验证口径: 每个比较的真假值与算术结果都要与 g++ 逐行一致。
#include <cstdio>

struct Box {
    int v;
    Box(int x) : v(x) {}
};

// 真正的模板, 必须仍然被识别为 template-id (不能被消歧规则误伤)
template <typename T>
struct Wrap {
    T x;
    T get() { return x; }
};

int g(int a, int b) { return a - b; }

int cmp_all(int a, int b, int c, int d) {
    int n = 0;
    if (a < b && c > d) n += 1;
    if (a > b && c < d) n += 2;
    if (a < b || c > d) n += 4;
    if ((a < b) == (c > d)) n += 8;
    n += g(a < b, c > d);
    n += (a < b) + (c > d) * 10;
    // 逗号表达式两侧都是比较
    n += (a < b, c > d) ? 100 : 0;
    return n;
}

int main() {
    printf("1 %d\n", cmp_all(1, 2, 3, 4));
    printf("2 %d\n", cmp_all(4, 3, 2, 1));
    printf("3 %d\n", cmp_all(1, 2, 1, 2));
    printf("4 %d\n", cmp_all(2, 1, 4, 3));

    // 链式比较与模板调用混在同一个表达式里
    int lo = 1, hi = 10, mid = 5;
    printf("in_range=%d\n", (lo < mid) && (mid < hi));
    printf("outside=%d\n", (mid < lo) || (hi < mid));

    // 模板侧: `Wrap<int>` 在类型位置 (后跟标识符) 与表达式位置 (后跟 `(`)
    Wrap<int> w;
    w.x = 7;
    printf("wrap=%d,%d\n", w.x, w.get());
    printf("templated_call=%d\n", g(w.get() < 10, w.get() > 3));

    Box bx(3);
    Box by(4);
    printf("boxes=%d\n", (bx.v < by.v) && (by.v - bx.v > 0));
    return cmp_all(1, 2, 3, 4) + (bx.v < by.v);
}
