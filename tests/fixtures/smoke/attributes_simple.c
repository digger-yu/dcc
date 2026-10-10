// 简化测试: __attribute__ 剥离 + 函数调用
__attribute__((unused)) int foo(void) {
    return 10;
}

int main(void) {
    return foo();
}
