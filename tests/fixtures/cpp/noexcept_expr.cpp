// m26_noexcept_expr.cpp - noexcept(expr) 编译期谓词
// 验证: 表达式抛异常可能性分析 (不再恒为 true).
// 运行返回 0 表示所有断言通过.

int maybe_throw(int x);
int safe_fn(int x) noexcept { return x; }

class Widget {
public:
    Widget() {}
    int safe() noexcept { return 1; }
    int risky();
};

int main() {
    int x = 1;
    Widget w;
    int r = 0;
    if (!noexcept(x)) r |= 1;           // 变量访问不抛
    if (!noexcept(1 + 2)) r |= 2;       // 内建运算不抛
    if (!noexcept(safe_fn(1))) r |= 4;  // noexcept 函数不抛
    if (noexcept(maybe_throw(1))) r |= 8;   // 普通函数可能抛
    if (noexcept(new int)) r |= 16;     // new 可能抛 bad_alloc
    if (!noexcept(w.safe())) r |= 32;   // noexcept 成员不抛
    if (noexcept(w.risky())) r |= 64;   // 普通成员可能抛
    if (!noexcept((int)1.5)) r |= 128;  // cast 不抛
    return r;
}
