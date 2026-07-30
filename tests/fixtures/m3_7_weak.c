/* m3_7: weak symbols - weak definition and weak reference */

/* weak definition: can be overridden by strong symbol */
int weak_def(void) __attribute__((weak));
int weak_def(void) { return 42; }

/* weak variable */
int weak_var __attribute__((weak)) = 100;

/* strong definition */
int strong_func(void) { return 99; }

/* weak variable with initializer */
int weak_val __attribute__((weak)) = 777;

int main(void) {
    int fail = 0;

    /* weak_def should return 42 (our own definition) */
    if (weak_def() != 42) fail |= 1;

    /* weak_var should be 100 */
    if (weak_var != 100) fail |= 2;

    /* strong_func should return 99 */
    if (strong_func() != 99) fail |= 4;

    /* weak val should be correct */
    if (weak_val != 777) fail |= 8;

    return fail;
}
