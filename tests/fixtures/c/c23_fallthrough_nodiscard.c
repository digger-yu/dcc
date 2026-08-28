/* Test: C23 [[fallthrough]] and [[nodiscard]] compile-time diagnostics
 *
 * 1. [[fallthrough]]: parsed and accepted in switch case bodies
 * 2. [[nodiscard]]: function return value discard emits W0401 warning
 * 3. [[nodiscard]] with attribute syntax also recognized
 */

/* [[nodiscard]] function: discarding result should warn */
[[nodiscard]] int compute_value(void) {
    return 42;
}

/* Normal function: no warning */
int normal_func(void) {
    return 1;
}

/* [[fallthrough]] in switch: should be accepted */
int test_fallthrough(int x) {
    int result = 0;
    switch (x) {
    case 1:
        result = 10;
        [[fallthrough]];
    case 2:
        result += 20;
        break;
    case 3:
        result = 30;
        [[fallthrough]];
    default:
        result += 1;
        break;
    }
    return result;
}

/* Using compute_value result: no warning */
int test_nodiscard_used(void) {
    int val = compute_value();
    return val;
}

int main(void) {
    int ok = 1;
    ok &= (test_fallthrough(1) == 30);
    ok &= (test_fallthrough(2) == 20);
    ok &= (test_fallthrough(3) == 31);
    ok &= (test_nodiscard_used() == 42);
    /* Intentionally discard nodiscard return: should emit W0401 */
    /* compute_value(); -- uncomment to test warning */
    /* normal_func();  -- no warning for non-nodiscard */
    return ok ? 0 : 1;
}
