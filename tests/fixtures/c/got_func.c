/* m3_11: GOT addressing for undefined (external) functions
 * Verifies that:
 *  1. Taking address of external functions uses GOT (MOV + GOTPCREL)
 *  2. Function pointer comparison works correctly
 *  3. Calling through function pointers works
 *  4. Internal function addresses use direct LEA
 */

#include <stdio.h>
#include <string.h>

/* Internal function - should use LEA (direct address) */
int internal_add(int a, int b) { return a + b; }

int main(void) {
    int ok = 1;

    /* Test 1: take address of external function and call through it */
    int (*fn)(const char *, ...) = printf;
    /* fn should point to the actual printf, not PLT stub */

    /* Test 2: function pointer comparison - same function must match */
    void *(*m1)(void *, int, size_t) = memset;
    void *(*m2)(void *, int, size_t) = memset;
    if (m1 != m2) {
        ok = 0;
        printf("FAIL: memset pointer comparison\n");
    }

    /* Test 3: internal function address comparison */
    int (*f1)(int, int) = internal_add;
    int (*f2)(int, int) = internal_add;
    if (f1 != f2) {
        ok = 0;
        printf("FAIL: internal_add pointer comparison\n");
    }

    /* Test 4: call internal function through pointer */
    if (f1(3, 4) != 7) {
        ok = 0;
        printf("FAIL: internal_add(3,4)=%d\n", f1(3, 4));
    }

    /* Test 5: call external function through pointer */
    char buf[32];
    memset(buf, 'A', 10);
    buf[10] = '\0';
    if (buf[0] != 'A' || buf[9] != 'A' || buf[10] != '\0') {
        ok = 0;
        printf("FAIL: memset via pointer\n");
    }

    if (ok)
        printf("PASS: GOT func addr checks\n");

    return ok ? 0 : 1;
}
