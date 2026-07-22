// M23: __attribute__((used)) —— 即使静态函数未被引用也应保留
// 测试目标: 编译通过
__attribute__((used)) static int used_helper(int x) {
    return x * 2;
}

int main(void) {
    return used_helper(21);  // 42
}
