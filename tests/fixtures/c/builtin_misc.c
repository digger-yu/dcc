/* Test: other missing __builtin_* functions */

#include <stddef.h>

/* __builtin_setjmp / __builtin_longjmp */
void test_jmp(void) {
    void *buf[8];
    if (__builtin_setjmp(buf) == 0) {
        /* first time */
        __builtin_longjmp(buf, 1);
    }
    /* resumed here */
}

/* __builtin_speculation_safe_value */
void test_speculation_safe(void) {
    int x = 42;
    int y = __builtin_speculation_safe_value(x);
    (void)y;
}

/* __builtin_addc / __builtin_subc */
void test_addc_subc(void) {
    unsigned long a = 100, b = 200;
    unsigned long result;
    unsigned char carry;

    result = __builtin_addc(a, b, 0, &carry);
    (void)result;
    (void)carry;

    result = __builtin_subc(a, b, 0, &carry);
    (void)result;
    (void)carry;
}

/* __builtin_is_constant_evaluated */
void test_is_constant_evaluated(void) {
    int x = __builtin_is_constant_evaluated();
    (void)x;
}

/* __builtin_memcpy_inline / __builtin_memset_inline */
void test_memcpy_memset_inline(void) {
    char src[10] = "hello";
    char dst[10];

    __builtin_memcpy_inline(dst, src, 6);
    __builtin_memset_inline(dst, 0, 10);
}

/* __builtin_unwind_init */
void test_unwind_init(void) {
    __builtin_unwind_init();
}

int main(void) {
    test_jmp();
    test_speculation_safe();
    test_addc_subc();
    test_is_constant_evaluated();
    test_memcpy_memset_inline();
    test_unwind_init();
    return 0;
}
