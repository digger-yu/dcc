// 测试阻塞级 builtin 特性

// 测试 __builtin_types_compatible_p
int test_types_compat() {
    int a = __builtin_types_compatible_p(int, int);       // 应为 1
    int b = __builtin_types_compatible_p(int, long);      // 应为 0
    int c = __builtin_types_compatible_p(char, char);     // 应为 1
    return a * 100 + b * 10 + c;  // 期望: 1*100 + 0*10 + 1 = 101
}

// 测试 __builtin_choose_expr
int test_choose() {
    // 条件为真时选择第一个表达式
    int x = __builtin_choose_expr(1, 42, 99);  // 应为 42
    // 条件为假时选择第二个表达式
    int y = __builtin_choose_expr(0, 42, 99);  // 应为 99
    // 条件为 sizeof 比较
    int z = __builtin_choose_expr(sizeof(int) == 4, 100, 200);  // 应为 100
    return x + y + z;  // 期望: 42 + 99 + 100 = 241
}

// 测试 __builtin_add_overflow
int test_add_overflow() {
    int result;
    int overflow;
    
    // 正常加法
    overflow = __builtin_add_overflow(10, 20, &result);
    if (result != 30 || overflow != 0) return 1;
    
    // 溢出加法 (大数相加)
    overflow = __builtin_add_overflow(2000000000, 2000000000, &result);
    // 结果会溢出，overflow 应为 1
    if (overflow != 1) return 2;
    
    return 0;  // 成功
}

// 测试 __builtin_sub_overflow
int test_sub_overflow() {
    int result;
    int overflow;
    
    // 正常减法
    overflow = __builtin_sub_overflow(50, 20, &result);
    if (result != 30 || overflow != 0) return 1;
    
    // 负数减法
    overflow = __builtin_sub_overflow(-2000000000, 2000000000, &result);
    // 结果会溢出，overflow 应为 1
    if (overflow != 1) return 2;
    
    return 0;  // 成功
}

// 测试 __builtin_mul_overflow
int test_mul_overflow() {
    int result;
    int overflow;
    
    // 正常乘法
    overflow = __builtin_mul_overflow(6, 7, &result);
    if (result != 42 || overflow != 0) return 1;
    
    // 溢出乘法
    overflow = __builtin_mul_overflow(1000000, 1000000, &result);
    // 结果会溢出，overflow 应为 1
    if (overflow != 1) return 2;
    
    return 0;  // 成功
}

// 测试组合使用
int test_combined() {
    // 使用 types_compatible_p 和 choose_expr 组合
    int x = __builtin_choose_expr(
        __builtin_types_compatible_p(int, int),
        42,
        0
    );
    return x;  // 期望: 42
}

int main() {
    int r1 = test_types_compat();
    int r2 = test_choose();
    int r3 = test_add_overflow();
    int r4 = test_sub_overflow();
    int r5 = test_mul_overflow();
    int r6 = test_combined();
    
    // 输出结果
    if (r1 != 101) return 10;
    if (r2 != 241) return 20;
    if (r3 != 0) return 30 + r3;
    if (r4 != 0) return 40 + r4;
    if (r5 != 0) return 50 + r5;
    if (r6 != 42) return 60;
    
    return 0;  // 全部通过
}
