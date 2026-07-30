/* m3_8: visibility control - hidden/default visibility */

/* hidden function: not exported to shared library */
int hidden_fn(void) __attribute__((visibility("hidden")));
int hidden_fn(void) { return 42; }

/* default visibility function */
int default_fn(void) __attribute__((visibility("default")));
int default_fn(void) { return 99; }

/* hidden variable */
int hidden_var __attribute__((visibility("hidden"))) = 77;

/* default variable */
int default_var __attribute__((visibility("default"))) = 55;

/* protected visibility (less common) */
int protected_fn(void) __attribute__((visibility("protected")));
int protected_fn(void) { return 33; }

int main(void) {
    int fail = 0;

    /* all functions should be callable within same TU */
    if (hidden_fn() != 42) fail |= 1;
    if (default_fn() != 99) fail |= 2;
    if (hidden_var != 77) fail |= 4;
    if (default_var != 55) fail |= 8;
    if (protected_fn() != 33) fail |= 16;

    return fail;
}
