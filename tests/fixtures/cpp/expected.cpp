// m18_expected.cpp - std::expected 头 smoke test (C++23)

#include <expected>

int test_expected_int() {
    std::expected_int ok(42);
    if (!ok.has_value()) return 1;
    if (ok.value() != 42) return 2;

    std::expected_int err = std::expected_int::error(7);
    if (err.has_value()) return 3;
    if (err.error() != 7) return 4;
    return 0;
}

int test_expected_str() {
    std::expected_str ok("hello");
    if (!ok.has_value()) return 1;

    std::expected_str err = std::expected_str::error(-1);
    if (err.has_value()) return 2;
    if (err.error() != -1) return 3;
    return 0;
}

int main() {
    int r1 = test_expected_int();
    int r2 = test_expected_str();
    return r1 + r2;
}
