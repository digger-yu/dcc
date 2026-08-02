// m26_gcc_compat.cpp - GCC ABI 兼容运行时 smoke test
// 覆盖: 静态局部变量 (__cxa_guard_acquire/release) / typeid 静态路径
// (__dcc_typeinfo_t + __dcc_ti_<T>) / 类自动 typeinfo 定义.

class Widget {
public:
    int id;
};

int counter() {
    static int n = 5;
    return n;
}

int main() {
    int got = counter();
    return (typeid(Widget) == typeid(Widget) && got == 5) ? 0 : 1;
}
