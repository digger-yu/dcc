// m18_multi_index.cpp - C++23 多维下标 operator[] smoke test
// 验证 parser 把 `obj[i, j]` 解析为 MultiIndex, lower 改写为
// __dcc_multi_index_2(obj, i, j).
//
// 注: dcc C++ 前端暂不支持函数内 `int arr[N];` 数组声明,
// 因此本测试只验证 lower 阶段把 `p[i, j, ...]` 改写为
// `__dcc_multi_index_N(p, i, j, ...)` 辅助函数调用, 不
// 真的去解引用. 由于 p 是 NULL, 运行时如果真的解引用会
// 段错误, 但 lower 阶段只关心源码结构, 我们只用
// `-fsyntax-only` 或检查 .c 输出即可.

int test_2d(int* p) {
    /* 应改写为 __dcc_multi_index_2(p, 1, 2) */
    return p[1, 2];
}

int test_3d(int* p) {
    return p[0, 1, 2];
}

int main() {
    int* p = 0;
    /* 仅作语法检查, 不真调用 */
    return 0;
}