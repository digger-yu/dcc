// 测试 __builtin_choose_expr
int main() {
    int x = __builtin_choose_expr(1, 42, 99);  // 应为 42
    int y = __builtin_choose_expr(0, 42, 99);  // 应为 99
    return x + y;  // 期望: 42 + 99 = 141
}
