// C++20 features test
#include <span>
#include <concepts>
#include <coroutine>
#include <compare>

enum class Color { Red, Green, Blue };

int use_span(int* arr, size_t n) {
    std::span<int> s(arr, n);
    int sum = 0;
    for (size_t i = 0; i < s.size(); ++i) {
        sum += s[i];
    }
    return sum;
}

// C++20: using enum
using enum Color;

// C++20: consteval (compile-time function)
consteval int square(int x) {
    return x * x;
}

// C++20: constinit
constinit int global_val = 42;

int main() {
    int arr[] = {1, 2, 3, 4, 5};
    int result = use_span(arr, 5);

    // using enum brings values into scope
    Color c = Red;

    // constexpr/consteval
    constexpr int sq = square(5);

    // three-way comparison types
    std::strong_ordering ord = std::strong_ordering::less;

    return result + sq;
}
