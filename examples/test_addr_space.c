// 测试 address_space 静默跳过
int test_addr_space(int * __attribute__((address_space(0))) p) {
    return *p;
}

int main() {
    int x = 42;
    int r = test_addr_space(&x);
    if (r != 42) return 1;
    return 0;
}
