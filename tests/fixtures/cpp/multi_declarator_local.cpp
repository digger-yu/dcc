// multi_declarator_local.cpp —— 一条局部声明里的多个 init-declarator ([dcl.dcls]/[stmt.pre])
//
// C++ 文法: `declaration -> decl-specifier-seq init-declarator-list_opt ;`，
// 逗号分隔的是**同一声明**里的多个 declarator，不是两条声明。
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) 带初始化的 `int a = 3, b = 4;` 两者都得各自初值;
//   2) 无初始化的 `int c, d;` 两个名字都必须存在 (旧行为: 整条降级 OpaqueStmt);
//   3) **指针/引用层级属于 declarator** ([dcl.name]): `int *p, q = 6;` 里
//      q 的类型是 int (sizeof == 4), p 的类型是 int* (sizeof == 8);
//   4) `p = &q` 后 q 的写必须透过 p 可见 (两者指向同一对象, 未被误并成一个变量);
//   5) 多 declarator 的初值可调用函数 (`int r = f(2), s = f(5);`).
static int twice(int v) { return v + v; }

int probe(int x) {
    int a = 3, b = 4;
    if (a + b != 7) return 1;
    int c, d;
    c = 5;
    d = c + a;
    if (d != 8) return 2;
    int *p, q = 6;
    p = &q;
    if (sizeof(q) != sizeof(int)) return 3;
    if (sizeof(p) != sizeof(int *)) return 4;
    if (*p != 6) return 5;
    q = 7;
    if (*p != 7) return 6;
    int r = twice(2), s = twice(5);
    if (r + s != 14) return 7;
    if (x + a + b + c + d != x + 20) return 8;
    return 0;
}

int main() { return probe(1); }
