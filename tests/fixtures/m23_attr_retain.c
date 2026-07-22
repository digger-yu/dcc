// M23: __attribute__((retain)) —— 变量和函数均应被保留，gc-sections 不应移除
// 测试目标: 编译通过，变量可读，结果正确
__attribute__((retain)) int retain_var = 41;

__attribute__((retain)) int retain_func(int x) {
    return x + 1;
}

int main(void) {
    return retain_var + retain_func(0);  // 41 + 1 = 42
}
