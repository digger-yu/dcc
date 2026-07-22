// M23: __attribute__((flatten)) —— 所有调用都应当可内联
// 测试目标: 编译通过，结果正确
static int inner_add(int x) {
    return x + 1;
}

__attribute__((flatten)) int outer(int x) {
    return inner_add(x) * 2;
}

int main(void) {
    return outer(20);  // (20+1)*2 = 42
}
