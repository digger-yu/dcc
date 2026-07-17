// 测试全部次要级特性

// 测试 __builtin_object_size
int test_object_size() {
    int x = 42;
    char buf[16];
    long sz = __builtin_object_size(&x, 0);
    if (sz != 4) return 1;
    if (__builtin_object_size(buf, 0) != 16) return 2;
    return 0;
}

// 测试 __builtin_expect_with_probability
int test_expect_with_prob() {
    int x = 5;
    int y = __builtin_expect_with_probability(x, 5, 1);
    if (y != 5) return 2;
    return 0;
}

// 测试 __builtin_memcpy 内联
int test_memcpy_inline() {
    char src[8];
    char dst[8];
    src[0] = 1; src[1] = 2; src[2] = 3; src[3] = 4;
    dst[0] = 0; dst[1] = 0; dst[2] = 0; dst[3] = 0;
    __builtin_memcpy(dst, src, 4);
    if (dst[0] != 1 || dst[1] != 2 || dst[2] != 3 || dst[3] != 4) return 3;
    return 0;
}

// 测试 __builtin_memset 内联
int test_memset_inline() {
    char buf[8];
    __builtin_memset(buf, 65, 8);
    int i = 0;
    while (i < 8) {
        if (buf[i] != 65) return 4;
        i = i + 1;
    }
    return 0;
}

// 测试 __builtin_memmove 内联 (重叠拷贝)
int test_memmove_inline() {
    char buf[16];
    buf[0] = 72; buf[1] = 101; buf[2] = 108; buf[3] = 108; buf[4] = 111;
    __builtin_memmove(buf + 2, buf, 5);
    if (buf[2] != 72 || buf[3] != 101 || buf[4] != 108 || buf[5] != 108 || buf[6] != 111) return 5;
    return 0;
}

// 测试 address_space(N) 静默跳过
int test_addr_space(int * __attribute__((address_space(0))) p) {
    return *p;
}

int main() {
    int r;
    r = test_object_size();
    if (r) return r;
    r = test_expect_with_prob();
    if (r) return r;
    r = test_memcpy_inline();
    if (r) return r;
    r = test_memset_inline();
    if (r) return r;
    r = test_memmove_inline();
    if (r) return r;
    int x = 42;
    r = test_addr_space(&x);
    if (r != 42) return 6;
    return 0;
}
