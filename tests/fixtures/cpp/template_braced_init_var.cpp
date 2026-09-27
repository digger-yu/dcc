// template_braced_init_var.cpp —— 类模板对象的**花括号初始化** ([dcl.init.list]/[basic.type.ident])
//
// `wrap<int> x{5};` 里两处消歧都必须靠"已知类型名集合"这一真源:
//   - `wrap` 后的 `<` 是模板实参起点, 不是小于号 (否则整句被读成
//     `(wrap<int>) x < 5 ...` 比较表达式);
//   - `{5}` 是初始化式, 不是复合语句块.
// 同时锁住反向情形: 未知类型名开头的 `a < b`、`if (n < 4) {}` 仍是表达式语句.
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照).
//
// 已知缺口 (不在本夹具覆盖范围): **两个及以上**初始化式的花括号初始化列表在
// C++ native 管线上还没有正确降级, 与模板无关 (以下三条在接手前的基线上同样
// 复现, 属 lower_ir 对象模型 / 数组降级归属域):
//   1. 聚合体 `struct P{int a;int b;}; P d = {3, 4};` 与 `box<int> c{7, 8};` 都
//      SIGSEGV —— `lower_init_list` 对非 Array 目标取 `elem_ty = 整个结构体类型`
//      再按 `i * sizeof(struct)` 发 IndexStore, 第二个元素越界写;
//   2. 局部数组 `int a[4] = {1, 2, 3, 4};` 能编译但 `a[0] != 1`;
//   3. 全局数组 `int g[4] = {1, 2};` 取下标报 "subscripted value is not an array
//      or pointer".
// 因此本夹具只覆盖**单元**花括号初始化与 `<` 消歧这两条能力线。
template <typename T>
struct wrap {
    T v;
    wrap(T x) : v(x) {}
};

// 对照: 参数名 `l`/`r` 不是类型名, 这里的 `<` 必须仍是小于号
int cmp(int l, int r) { return l < r; }

int main() {
    wrap<int> a{5};
    if (a.v != 5) return 1;
    wrap<int> b(6);
    if (b.v != 6) return 2;
    // 初始化式本身是个成员访问表达式, 消歧后仍要正确求值
    wrap<int> d{a.v};
    if (d.v != 5) return 3;
    // 另一个类型实参的实例化走同一消歧路径
    wrap<short> q{3};
    if (q.v != 3) return 4;
    if (cmp(1, 2) != 1) return 5;
    if (cmp(2, 1) != 0) return 6;
    int n = 3;
    if (n < 4) {
        n = n + 1;
    }
    if (n != 4) return 7;
    return 0;
}
