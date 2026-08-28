/* Test: __builtin_expect_with_probability and __builtin_nonzero_p
 *
 * Verify that these GCC builtins are correctly handled by the compiler:
 * 1. __builtin_expect_with_probability(expr, val, prob) - returns expr
 * 2. __builtin_nonzero_p(x) - returns 1 if x is known non-zero constant
 * 3. __builtin_expect(expr, val) - returns expr (branch hint)
 * 4. __builtin_constant_p(x) - returns 1 if x is compile-time constant
 * 5. __builtin_object_size(ptr, type) - returns (size_t)-1 when unknown
 */

int test_expect_with_prob(int x) {
    /* Should return x (first argument), probability is ignored */
    return __builtin_expect_with_probability(x, 1, 0.9);
}

int test_expect(int x) {
    /* Should return x (first argument) */
    return __builtin_expect(x, 0);
}

int test_nonzero_p_const(void) {
    /* 42 is known non-zero at compile time -> returns 1 */
    return __builtin_nonzero_p(42);
}

int test_nonzero_p_zero(void) {
    /* 0 is known zero -> returns 0 */
    return __builtin_nonzero_p(0);
}

int test_nonzero_p_var(int x) {
    /* Variable: not known at compile time -> returns 0 (conservative) */
    return __builtin_nonzero_p(x);
}

int test_constant_p_literal(void) {
    /* 42 is a compile-time constant -> returns 1 */
    return __builtin_constant_p(42);
}

int test_constant_p_var(int x) {
    /* Variable is not a compile-time constant -> returns 0 */
    return __builtin_constant_p(x);
}

int test_object_size(void) {
    /* Unknown object size -> returns (size_t)-1 */
    int x;
    return __builtin_object_size(&x, 0);
}

int main(void) {
    int ok = 1;
    ok &= (test_expect_with_prob(5) == 5);
    ok &= (test_expect(7) == 7);
    ok &= (test_nonzero_p_const() == 1);
    ok &= (test_nonzero_p_zero() == 0);
    ok &= (test_constant_p_literal() == 1);
    return ok ? 0 : 1;
}
