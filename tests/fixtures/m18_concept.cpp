// m18_concept.cpp - Concepts / requires 编译 smoke test
// 验证: `concept Name = ...;` 解析, `requires (expr)` 跳过.
// 简化: 不做实际 SFINAE 过滤, 仅语法层通过.

template <typename T>
concept Integral = sizeof(T) <= 8;

template <typename T>
    requires Integral<T>
T identity(T x) { return x; }

int test_concept() {
    int a = identity(42);
    if (a != 42) return 1;
    return 0;
}

int main() {
    return test_concept();
}
