/* Test: SIMD/SSE instruction end-to-end runtime verification
 * Verifies that SSE scalar instructions produce correct results when executed.
 * Uses scalar double-precision SSE2 operations which are universally supported.
 */

__attribute__((noinline))
double test_sse_addsd(double a, double b) {
    double result = a;
    asm volatile (
        "addsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

__attribute__((noinline))
double test_sse_mulsd(double a, double b) {
    double result = a;
    asm volatile (
        "mulsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

__attribute__((noinline))
double test_sse_subsd(double a, double b) {
    double result = a;
    asm volatile (
        "subsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

__attribute__((noinline))
double test_sse_divsd(double a, double b) {
    double result = a;
    asm volatile (
        "divsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

int main(void) {
    /* Test addsd: 3.0 + 4.0 = 7.0 */
    double r1 = test_sse_addsd(3.0, 4.0);
    if (r1 != 7.0) return 1;

    /* Test mulsd: 3.0 * 4.0 = 12.0 */
    double r2 = test_sse_mulsd(3.0, 4.0);
    if (r2 != 12.0) return 2;

    /* Test subsd: 10.0 - 3.5 = 6.5 */
    double r3 = test_sse_subsd(10.0, 3.5);
    if (r3 != 6.5) return 3;

    /* Test divsd: 12.0 / 4.0 = 3.0 */
    double r4 = test_sse_divsd(12.0, 4.0);
    if (r4 != 3.0) return 4;

    return 0;
}
