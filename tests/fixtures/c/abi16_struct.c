// 16 字节 struct 传值 ABI 测试
// struct {double,double} 应通过 xmm0:xmm1 传递
// struct {long,long} 应通过 rdi:rsi 传递
#include <stdio.h>

struct S_double {
    double x;
    double y;
};

struct S_long {
    long a;
    long b;
};

// 测试 1: double struct 传值
double sum_double(struct S_double s) {
    return s.x + s.y;
}

// 测试 2: long struct 传值
long sum_long(struct S_long s) {
    return s.a + s.b;
}

// 测试 3: double struct 返回值
struct S_double make_double(double x, double y) {
    struct S_double s;
    s.x = x;
    s.y = y;
    return s;
}

int main(void) {
    int ok = 1;

    // 测试 1
    struct S_double d = {3.0, 4.0};
    double r1 = sum_double(d);
    if (r1 != 7.0) {
        printf("FAIL: sum_double({3,4}) = %f, expected 7.0\n", r1);
        ok = 0;
    } else {
        printf("PASS: sum_double({3,4}) = 7.0\n");
    }

    // 测试 2
    struct S_long l = {100, 200};
    long r2 = sum_long(l);
    if (r2 != 300) {
        printf("FAIL: sum_long({100,200}) = %ld, expected 300\n", r2);
        ok = 0;
    } else {
        printf("PASS: sum_long({100,200}) = 300\n");
    }

    // 测试 3: 返回值
    struct S_double d2 = make_double(1.5, 2.5);
    if (d2.x != 1.5 || d2.y != 2.5) {
        printf("FAIL: make_double(1.5,2.5) = {%f,%f}, expected {1.5,2.5}\n", d2.x, d2.y);
        ok = 0;
    } else {
        printf("PASS: make_double(1.5,2.5) = {1.5,2.5}\n");
    }

    if (!ok) {
        printf("SOME TESTS FAILED\n");
        return 1;
    }
    printf("ALL TESTS PASSED\n");
    return 0;
}