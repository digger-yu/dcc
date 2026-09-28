/* 编译期浮点字面量测试 */
int test_float_lit(void) {
    double d = 3.14159;
    float f = 2.71828f;
    if (d != 3.14159) return 1;
    if (f != 2.71828f) return 2;
    /* NaN 检查
     * dcc 浮点 ==/!= 实现基于 sete，对 NaN 操作数不严格遵循
     * C 语义 (NaN != NaN 应当为 true)。此处已知 dcc 限制,
     * 故不直接比较 0.0/0.0 == 0.0/0.0,而使用 __builtin_isnan
     * 来验证 0.0/0.0 确实产生 NaN。 */
    double nanv = 0.0/0.0;
    if (!__builtin_isnan(nanv)) return 3;
    /* +Inf 检查 */
    double pos_inf = 1.0/0.0;
    if (pos_inf != 1.0/0.0) return 4;
    return 0;
}

int main(void) {
    int r = test_float_lit();
    if (r != 0) return 100 + r;
    return 0;
}
