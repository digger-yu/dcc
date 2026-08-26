// 测试 address_space 静默跳过 (以及 __seg_fs/__seg_gs 限定符)
int test_addr_space(int * __attribute__((address_space(0))) p) {
    return *p;
}

int test_seg_gs(int * __seg_gs p) {
    return *p;
}

int test_seg_fs(int __seg_fs * p) {
    return *p;
}

int main() {
    int x = 42;
    int r = test_addr_space(&x);
    if (r != 42) return 1;
    r = test_seg_gs(&x);
    if (r != 42) return 1;
    r = test_seg_fs(&x);
    if (r != 42) return 1;
    return 0;
}
