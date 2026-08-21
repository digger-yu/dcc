/* m27_lowlevel_builtins.c - low-level builtins 汇编/运行时断言测试 */
/* 覆盖: trap, debugtrap, unreachable, assume_aligned, alloca, */
/*        frame_address, return_address */

#include <stddef.h>

/* ============== __builtin_assume_aligned ============== */
/* 应原样返回指针参数 */
int test_assume_aligned(void) {
    int arr[16] __attribute__((aligned(16)));
    int *p = (int *)__builtin_assume_aligned(arr, 16);
    p[0] = 42;
    return arr[0]; /* 应返回 42 */
}

/* ============== __builtin_alloca ============== */
/* 应动态分配栈空间并返回指针 */
int test_alloca(void) {
    int *p = (int *)__builtin_alloca(4 * sizeof(int));
    p[0] = 10;
    p[1] = 11;
    p[2] = 12;
    p[3] = 9; /* sum = 42 */
    return p[0] + p[1] + p[2] + p[3];
}

/* ============== __builtin_frame_address ============== */
/* 应返回当前帧指针 (非 NULL) */
int test_frame_address(void) {
    void *fp = __builtin_frame_address(0);
    return fp != NULL ? 42 : 0;
}

/* ============== __builtin_return_address ============== */
/* 应返回返回地址 (非 NULL) */
int test_return_address(void) {
    void *ra = __builtin_return_address(0);
    return ra != NULL ? 42 : 0;
}

/* ============== 综合运行时测试 ============== */
int main(void) {
    int sum = 0;

    /* assume_aligned + alloca 应正常工作 */
    sum += test_assume_aligned(); /* 42 */
    sum += test_alloca();          /* 42 */
    sum += test_frame_address();   /* 42 */
    sum += test_return_address();  /* 42 */

    /* 总和应为 168 (4*42) */
    return sum == 168 ? 0 : 1;
}