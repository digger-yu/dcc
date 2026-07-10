// 测试: 函数调用结果用于 += (期望 20)
int foo(void) { return 10; }
int main(void) {
    int r = 0;
    r += foo();
    r += foo();
    return r;
}
