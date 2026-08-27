// Struct ABI 综合测试: 覆盖 SysV AMD64 ABI 各种结构体传值分类
//
// 1. 8 字节 SSE 结构体 (struct {double}) - 单 XMM 寄存器传递
// 2. 16 字节 SSE 结构体 (struct {double,double}) - 双 XMM 寄存器传递
// 3. 16 字节整数结构体 (struct {long,long}) - 双 GP 寄存器传递
// 4. 24 字节结构体 (stack_t 布局) - 栈上传递 (Memory 类)
// 5. 结构体返回值 (sret / 寄存器返回)
#include <stdio.h>
#include <stddef.h>

// --- 8 字节 SSE 结构体 ---
struct S1d { double x; };

double get_1d(struct S1d s) { return s.x; }

struct S1d make_1d(double v) {
    struct S1d s; s.x = v; return s;
}

// --- 16 字节 SSE 结构体 ---
struct S2d { double x; double y; };

double sum_2d(struct S2d s) { return s.x + s.y; }

struct S2d make_2d(double a, double b) {
    struct S2d s; s.x = a; s.y = b; return s;
}

// --- 16 字节整数结构体 ---
struct S2l { long a; long b; };

long sum_2l(struct S2l s) { return s.a + s.b; }

// --- 24 字节结构体 (stack_t 布局) ---
struct stack_like {
    void *ss_sp;
    int ss_flags;
    size_t ss_size;
};

void check_stack(struct stack_like s) {
    if (s.ss_sp != (void*)0x1000) { printf("FAIL: ss_sp\n"); return; }
    if (s.ss_flags != 7) { printf("FAIL: ss_flags\n"); return; }
    if (s.ss_size != 4096) { printf("FAIL: ss_size\n"); return; }
    printf("PASS: check_stack\n");
}

// --- 混合参数: 结构体 + 标量 ---
double mix_call(struct S1d s, double extra) {
    return s.x + extra;
}

double mix_call2(double extra, struct S1d s) {
    return s.x + extra;
}

int main(void) {
    int ok = 1;

    // Test 1: 8 字节 SSE 结构体传值
    {
        struct S1d s = {3.14};
        double r = get_1d(s);
        if (r != 3.14) {
            printf("FAIL: get_1d = %f, expected 3.14\n", r);
            ok = 0;
        } else {
            printf("PASS: get_1d({3.14}) = 3.14\n");
        }
    }

    // Test 2: 8 字节 SSE 结构体返回值
    {
        struct S1d s = make_1d(2.718);
        if (s.x != 2.718) {
            printf("FAIL: make_1d = %f, expected 2.718\n", s.x);
            ok = 0;
        } else {
            printf("PASS: make_1d(2.718) = {2.718}\n");
        }
    }

    // Test 3: 16 字节 SSE 结构体传值
    {
        struct S2d s = {1.5, 2.5};
        double r = sum_2d(s);
        if (r != 4.0) {
            printf("FAIL: sum_2d = %f, expected 4.0\n", r);
            ok = 0;
        } else {
            printf("PASS: sum_2d({1.5,2.5}) = 4.0\n");
        }
    }

    // Test 4: 16 字节 SSE 结构体返回值
    {
        struct S2d s = make_2d(10.0, 20.0);
        if (s.x != 10.0 || s.y != 20.0) {
            printf("FAIL: make_2d = {%f,%f}\n", s.x, s.y);
            ok = 0;
        } else {
            printf("PASS: make_2d(10,20) = {10,20}\n");
        }
    }

    // Test 5: 16 字节整数结构体传值
    {
        struct S2l s = {100, 200};
        long r = sum_2l(s);
        if (r != 300) {
            printf("FAIL: sum_2l = %ld, expected 300\n", r);
            ok = 0;
        } else {
            printf("PASS: sum_2l({100,200}) = 300\n");
        }
    }

    // Test 6: 24 字节结构体 (Memory 类) 传值
    {
        struct stack_like s = {(void*)0x1000, 7, 4096};
        check_stack(s);
    }

    // Test 7: 混合参数 (结构体 + 标量)
    {
        struct S1d s = {1.0};
        double r = mix_call(s, 0.5);
        if (r != 1.5) {
            printf("FAIL: mix_call = %f, expected 1.5\n", r);
            ok = 0;
        } else {
            printf("PASS: mix_call({1.0}, 0.5) = 1.5\n");
        }
    }

    // Test 8: 混合参数 (标量 + 结构体)
    {
        struct S1d s = {2.0};
        double r = mix_call2(0.5, s);
        if (r != 2.5) {
            printf("FAIL: mix_call2 = %f, expected 2.5\n", r);
            ok = 0;
        } else {
            printf("PASS: mix_call2(0.5, {2.0}) = 2.5\n");
        }
    }

    // Test 9: 结构体字段偏移验证
    {
        if (offsetof(struct stack_like, ss_sp) != 0) {
            printf("FAIL: offsetof(ss_sp) = %zu\n", offsetof(struct stack_like, ss_sp));
            ok = 0;
        }
        if (offsetof(struct stack_like, ss_flags) != 8) {
            printf("FAIL: offsetof(ss_flags) = %zu\n", offsetof(struct stack_like, ss_flags));
            ok = 0;
        }
        if (offsetof(struct stack_like, ss_size) != 16) {
            printf("FAIL: offsetof(ss_size) = %zu\n", offsetof(struct stack_like, ss_size));
            ok = 0;
        }
        if (sizeof(struct stack_like) != 24) {
            printf("FAIL: sizeof(stack_like) = %zu\n", sizeof(struct stack_like));
            ok = 0;
        }
        printf("PASS: struct layout offsets correct\n");
    }

    if (!ok) {
        printf("SOME TESTS FAILED\n");
        return 1;
    }
    printf("ALL TESTS PASSED\n");
    return 0;
}
