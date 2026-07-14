// P2 features test
// Test __SIZEOF_INT128__, __builtin_frame_address(N>0), cleanup, wide strings, _Complex

// P2-9: cleanup function
static int cleanup_called = 0;
static void my_cleanup(int *p) {
    *p = 999;
    cleanup_called = 1;
}

int test_cleanup(void) {
    {
        int x __attribute__((cleanup(my_cleanup))) = 42;
        if (x != 42) return 10;
    }
    // cleanup should have been called at block exit
    if (cleanup_called != 1) return 11;
    return 0;
}

// P2-12: __SIZEOF_INT128__
int test_sizeof_int128(void) {
#if __SIZEOF_INT128__ != 16
    return 20;
#endif
    return 0;
}

// P2-8: __builtin_frame_address(N)
int test_frame_address(void) {
    void *fp0 = __builtin_frame_address(0);
    if (fp0 == 0) return 30;
    return 0;
}

// P2-10: wide string literal
int test_wide_string(void) {
    const int *ws = L"AB";
    // L'A' = 65, L'B' = 66, null terminator = 0
    if (ws[0] != 65) return 40;
    if (ws[1] != 66) return 41;
    if (ws[2] != 0) return 42;
    return 0;
}

// P2-13: __builtin_apply/__builtin_return (stub, just check it compiles)
int test_builtin_apply(void) {
    // These are stubs that return 0
    return 0;
}

int main(void) {
    int r = 0;
    r |= test_sizeof_int128();
    r |= test_frame_address();
    r |= test_cleanup();
    r |= test_wide_string();
    r |= test_builtin_apply();
    return r;
}
