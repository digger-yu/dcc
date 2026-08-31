// C++23 features test
#include <expected>
#include <flat_map>
#include <print>
#include <stdfloat>

int main() {
    // std::expected<T, E>
    std::expected<int, int> e1(42);
    int v = e1.value();

    std::expected<int, int> e2 = std::make_unexpected(1);
    int has = e2.has_value();

    // value_or
    int fallback = e2.value_or(99);

    // std::flat_map<K, V> (basic usage with fixed arrays)
    // Note: full template usage requires template instantiation support

    // std::float types
    std::float32_t f32 = 3.14f;
    std::float64_t f64 = 2.718281828;

    // std::print (degrades to printf)
    std::print("Hello %s %d\n", "C++23", 23);

    return v + has + fallback + (int)f32 + (int)f64;
}
