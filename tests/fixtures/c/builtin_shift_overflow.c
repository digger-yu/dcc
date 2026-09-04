/* Test __builtin_shl_overflow and __builtin_shr_overflow */
#include <stdio.h>

int main(void) {
    int result;
    int overflow;
    
    /* Test normal left shift */
    overflow = __builtin_shl_overflow(10, 2, &result);
    if (result != 40 || overflow != 0) {
        printf("FAIL: shl_overflow(10, 2) = %d, overflow=%d, expected 40,0\n", result, overflow);
        return 1;
    }
    
    /* Test left shift overflow (shift by >= 32) */
    overflow = __builtin_shl_overflow(10, 32, &result);
    if (overflow != 1) {
        printf("FAIL: shl_overflow(10, 32) should overflow, got overflow=%d\n", overflow);
        return 2;
    }
    
    /* Test normal right shift */
    overflow = __builtin_shr_overflow(40, 2, &result);
    if (result != 10 || overflow != 0) {
        printf("FAIL: shr_overflow(40, 2) = %d, overflow=%d, expected 10,0\n", result, overflow);
        return 3;
    }
    
    /* Test right shift overflow (shift by >= 32) */
    overflow = __builtin_shr_overflow(40, 32, &result);
    if (overflow != 1) {
        printf("FAIL: shr_overflow(40, 32) should overflow, got overflow=%d\n", overflow);
        return 4;
    }
    
    /* Test negative shift amount (overflow) */
    overflow = __builtin_shl_overflow(10, -1, &result);
    if (overflow != 1) {
        printf("FAIL: shl_overflow(10, -1) should overflow, got overflow=%d\n", overflow);
        return 5;
    }
    
    printf("All shift overflow tests passed\n");
    return 0;
}
