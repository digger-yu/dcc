// tests/fixtures/c/comdat_weak.c
// COMDAT: weak functions should use STB_WEAK binding and COMDAT groups.

__attribute__((weak)) int weak_func(int x) {
    return x + 1;
}

// strong override should take precedence
int weak_func(int x);

int main(void) {
    int r = weak_func(41);
    // weak_func is defined above, should return 42
    if (r != 42) return 1;
    return 0;
}
