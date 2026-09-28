/* m27_lowlevel_simple.c - low-level builtins 简化测试 */
int test_assume_aligned(void) {
    int arr[16] __attribute__((aligned(16)));
    int *p = (int *)__builtin_assume_aligned(arr, 16);
    p[0] = 42;
    return arr[0];
}

int test_alloca(void) {
    int *p = (int *)__builtin_alloca(4 * sizeof(int));
    p[0] = 10;
    p[1] = 11;
    p[2] = 12;
    p[3] = 9;
    return p[0] + p[1] + p[2] + p[3];
}

int test_frame_address(void) {
    void *fp = __builtin_frame_address(0);
    return fp != NULL ? 42 : 0;
}

int test_return_address(void) {
    void *ra = __builtin_return_address(0);
    return ra != NULL ? 42 : 0;
}

int main(void) {
    int sum = 0;
    sum += test_assume_aligned();
    sum += test_alloca();
    sum += test_frame_address();
    sum += test_return_address();
    return sum == 168 ? 0 : 1; /* 4*42 = 168 */
}