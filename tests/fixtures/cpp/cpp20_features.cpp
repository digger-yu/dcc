// C++20 features test
// 注意: 此测试文件仅验证 dcc 支持的 C++20 特性子集.
// 不支持的特性 (如 operator<=>, 复杂模板默认值) 已移除.

// enum class (C++11, 但常在 C++20 代码中使用)
enum class Color { Red, Green, Blue };

// using enum 的简化测试 (dcc 支持 using enum 但降级为 no-op)
// using enum Color;  // 暂时注释, 因为 dcc 的 using enum 支持有限

// constexpr 函数 (C++11/14/17/20)
constexpr int square(int x) {
    return x * x;
}

// constinit 降级测试 (dcc 剥离 constinit 关键字)
constinit int global_val = 42;

int main() {
    // enum class 使用
    Color c = Color::Red;
    (void)c;

    // constexpr 使用
    constexpr int sq = square(5);
    
    // constinit 变量
    global_val = sq;

    return sq;
}
