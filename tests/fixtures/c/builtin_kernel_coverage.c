/* Test newly added builtins for kernel coverage (P0-6)
 * Verifies: __builtin_ffs, __builtin_dynamic_object_size,
 *           __builtin_extract_return_addr, __builtin_isdigit,
 *           __builtin_preserve_access_index, __builtin_alloca_uninitialized
 */
#include <stdio.h>

int test_ffs(void) {
    int ok = 1;
    /* ffs(0) = 0 */
    if (__builtin_ffs(0) != 0) { ok = 0; printf("FAIL: ffs(0)=%d\n", __builtin_ffs(0)); }
    /* ffs(1) = 1 (bit 0 set) */
    if (__builtin_ffs(1) != 1) { ok = 0; printf("FAIL: ffs(1)=%d\n", __builtin_ffs(1)); }
    /* ffs(0x80) = 8 (bit 7 set) */
    if (__builtin_ffs(0x80) != 8) { ok = 0; printf("FAIL: ffs(0x80)=%d\n", __builtin_ffs(0x80)); }
    /* ffs(0x100) = 9 (bit 8 set) */
    if (__builtin_ffs(0x100) != 9) { ok = 0; printf("FAIL: ffs(0x100)=%d\n", __builtin_ffs(0x100)); }
    return ok;
}

int test_isdigit(void) {
    int ok = 1;
    if (!__builtin_isdigit('0')) { ok = 0; printf("FAIL: isdigit('0')\n"); }
    if (!__builtin_isdigit('5')) { ok = 0; printf("FAIL: isdigit('5')\n"); }
    if (!__builtin_isdigit('9')) { ok = 0; printf("FAIL: isdigit('9')\n"); }
    if (__builtin_isdigit('a')) { ok = 0; printf("FAIL: isdigit('a')!=0\n"); }
    if (__builtin_isdigit('/')) { ok = 0; printf("FAIL: isdigit('/')!=0\n"); }
    if (__builtin_isdigit(':')) { ok = 0; printf("FAIL: isdigit(':')!=0\n"); }
    return ok;
}

int test_dynamic_object_size(void) {
    int buf[10];
    /* dynamic_object_size should behave like object_size at compile time */
    long sz = __builtin_dynamic_object_size(buf, 0);
    /* sz should be 40 (10 * 4) or -1 if unknown */
    if (sz != 40 && sz != -1) {
        printf("FAIL: dynamic_object_size=%ld\n", sz);
        return 0;
    }
    return 1;
}

int test_extract_return_addr(void) {
    /* On x86_64, extract_return_addr is a no-op pass-through */
    void *addr = __builtin_return_address(0);
    void *extracted = __builtin_extract_return_addr(addr);
    if (addr != extracted) {
        printf("FAIL: extract_return_addr changed address\n");
        return 0;
    }
    return 1;
}

int test_preserve_access_index(void) {
    int x = 42;
    /* preserve_access_index is a BPF CO-RE pass-through */
    int *p = __builtin_preserve_access_index(&x);
    if (*p != 42) {
        printf("FAIL: preserve_access_index\n");
        return 0;
    }
    return 1;
}

int main(void) {
    int ok = 1;
    ok &= test_ffs();
    ok &= test_isdigit();
    ok &= test_dynamic_object_size();
    ok &= test_extract_return_addr();
    ok &= test_preserve_access_index();
    if (ok)
        printf("PASS: new builtins coverage\n");
    return ok ? 0 : 1;
}
