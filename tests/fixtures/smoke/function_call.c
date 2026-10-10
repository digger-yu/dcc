// tests/fixtures/smoke/function_call.c - 测试函数调用
int add(int a, int b) {
    return a + b;
}
int main() {
    return add(20, 22);  // 期望 42
}
