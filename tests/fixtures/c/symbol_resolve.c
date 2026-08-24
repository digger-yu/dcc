/* symbol_resolve.c - Test undefined, weak, and duplicate symbol handling
 *
 * This file tests the linker's symbol resolution:
 * 1. Weak undefined function: resolves to NULL if not provided
 * 2. Weak undefined variable: resolves to NULL if not provided
 * 3. Strong function definition
 * 4. Strong variable definition
 */

/* Weak function declaration - may or may not be provided by another TU */
int optional_func(void) __attribute__((weak));

/* Weak variable - may or may not be provided */
int optional_var __attribute__((weak));

/* Strong definitions */
int required_func(void) { return 42; }
int required_var = 100;

int main(void) {
    int fail = 0;

    /* Strong symbols should work normally */
    if (required_func() != 42) fail |= 1;
    if (required_var != 100) fail |= 2;

    /* Weak undefined function: check address before calling */
    if (optional_func) {
        int r = optional_func();
        if (r != 0) fail |= 4;
    }

    /* Weak undefined variable: address should be NULL */
    int *p = &optional_var;
    if (p != 0 && *p != 0) fail |= 8;

    return fail;
}
