/* Test: struct field aligned attribute codegen
 * Verifies that __attribute__((aligned(N))) on struct fields
 * correctly affects struct layout and memory access.
 */

struct aligned_fields {
    char a;                         /* offset 0 */
    int b __attribute__((aligned(8))); /* offset 8 (not 4) */
    char c;                         /* offset 12 */
    long d __attribute__((aligned(16))); /* offset 16 */
};

/* Verify layout at compile time via offset checks */
_Static_assert(sizeof(struct aligned_fields) >= 32,
    "struct with aligned fields should be at least 32 bytes");

int main(void) {
    struct aligned_fields s = {0};

    s.a = 1;
    s.b = 42;
    s.c = 3;
    s.d = 100;

    /* Verify values are stored correctly */
    if (s.a != 1) return 1;
    if (s.b != 42) return 2;
    if (s.c != 3) return 3;
    if (s.d != 100) return 4;

    /* Verify alignment via pointer check */
    if ((unsigned long)&s.b % 8 != 0) return 5;
    if ((unsigned long)&s.d % 16 != 0) return 6;

    return 0;
}
