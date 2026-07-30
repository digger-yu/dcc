/* m3_10: stack canary (-fstack-protector) basic test
 * Verifies that:
 *  1. Functions compiled with -fstack-protector execute correctly
 *     when the stack is not corrupted (canary matches on return).
 *  2. Multiple nested calls with canary protection work.
 *  3. Local array variables (typical canary targets) work properly.
 */

#include <stdio.h>

/* Function with local buffer - typical canary target */
int process_buffer(int n) {
    char buf[64];
    for (int i = 0; i < 64 && i < n; i++)
        buf[i] = (char)(i & 0xff);
    return buf[0] + buf[1];
}

/* Nested calls with stack frames */
int inner(int x) {
    int arr[16];
    for (int i = 0; i < 16; i++)
        arr[i] = x + i;
    return arr[15];
}

int outer(int x) {
    int arr[8];
    for (int i = 0; i < 8; i++)
        arr[i] = inner(x + i);
    return arr[7];
}

/* Recursive function with canary */
int fib_guarded(int n) {
    int local_buf[4];
    local_buf[0] = n;
    if (n <= 1) return n;
    return fib_guarded(n - 1) + fib_guarded(n - 2);
}

int main(void) {
    int ok = 1;

    /* Test 1: basic buffer function */
    int r1 = process_buffer(10);
    if (r1 != 1) { ok = 0; printf("FAIL: process_buffer=%d\n", r1); }

    /* Test 2: nested calls */
    int r2 = outer(10);
    if (r2 != 32) { ok = 0; printf("FAIL: outer=%d\n", r2); }

    /* Test 3: recursive with canary */
    int r3 = fib_guarded(10);
    if (r3 != 55) { ok = 0; printf("FAIL: fib_guarded=%d\n", r3); }

    if (ok)
        printf("PASS: stack canary basic checks\n");

    return ok ? 0 : 1;
}
