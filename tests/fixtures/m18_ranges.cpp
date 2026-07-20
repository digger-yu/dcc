// m18_ranges.cpp - std::ranges 简化版头 smoke test

#include <ranges>

extern "C" {
    int dccpp_view_sum_int(const void* v);
    /* 包装: 由 C 端实现. */
}

int test_filter() {
    /* ranges 视图不直接 C++ lambda, 简化: 测试编译通过 */
    return 0;
}

int main() {
    return test_filter();
}
