// alias_template_expansion.cpp —— **别名模板**展开 ([temp.alias])
//
// `using X = ...` 带模板形参时 (`template <class T> using value_of = ...`) 就是别名
// 模板: 使用点 `value_of<int>` 必须在解析期就地展开成目标类型, 下游 (实例化工作表
// / sema / 布局) 不该再看到 `value_of` 这个名字. 覆盖三类使用位置:
//   A) 全局函数返回类型 (非限定 / `std::` 限定) —— 旧行为: 限定名分支不认 `<`,
//      整条声明被顶层容错降级成 `CppDecl::Opaque` ("unsupported opaque declaration");
//   B) 函数模板返回类型是别名模板, 且非类型实参是常量表达式 `sizeof(int) == 4`;
//   C) 局部变量的类型是 `std::conditional_t` / 命名空间内的别名模板.
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照).
#include <type_traits>

template <typename T>
struct holder {
    typedef T type;
};

// 用户自己的别名模板, 目标是另一个模板的成员类型 (别名链)
template <typename T>
using value_of = typename holder<T>::type;

value_of<int> bump(int v) { return v + 1; }

// `std::` 限定的别名模板出现在**返回类型**位置
std::enable_if_t<true, int> twice(int v) { return v * 2; }

// 函数模板: 返回类型是别名模板, 非类型实参是常量表达式
template <typename T>
std::enable_if_t<(sizeof(int) == 4), T> as_is(T v) { return v; }

namespace ns {
template <typename T>
using plain_t = T;
}

ns::plain_t<int> ident(int v) { return v; }

int main() {
    if (bump(3) != 4) return 1;
    if (twice(4) != 8) return 2;
    if (as_is(5) != 5) return 3;
    if (ident(6) != 6) return 4;
    std::conditional_t<true, int, double> sel = 7;
    if (sel != 7) return 5;
    std::enable_if_t<true, int> e = 8;
    if (e != 8) return 6;
    ns::plain_t<int> p = 9;
    if (p != 9) return 10;
    return 0;
}
