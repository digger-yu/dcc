// 简单测试 __builtin_types_compatible_p
int main() {
    int a = __builtin_types_compatible_p(int, int);
    int b = __builtin_types_compatible_p(int, long);
    return a * 10 + b;  // 期望: 1*10 + 0 = 10
}
