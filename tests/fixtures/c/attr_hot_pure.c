/* Test: __attribute__((hot/pure/const/returns_twice/no_instrument_function))
 * GCC extension: optimization hint attributes.
 * dcc should parse and accept these without error. */

__attribute__((hot))
int hot_func(int x) {
    return x + 1;
}

__attribute__((pure))
int pure_func(int x) {
    return x * 2;
}

__attribute__((const))
int const_func(int x) {
    return x * x;
}

__attribute__((returns_twice))
int returns_twice_func(void) {
    return 0;
}

__attribute__((no_instrument_function))
void no_instr_func(void) {
}

/* Multiple attributes combined */
__attribute__((hot, pure))
int hot_pure_func(int x) {
    return x + 2;
}

/* noclone / noipa / nocf_check: consumed but no codegen effect */
__attribute__((noclone))
void noclone_func(void) {
}

__attribute__((noipa))
void noipa_func(void) {
}

__attribute__((nocf_check))
void nocf_func(void) {
}

int main(void) {
    int a = hot_func(1);
    int b = pure_func(a);
    int c = const_func(b);
    (void)c;
    (void)returns_twice_func;
    no_instr_func();
    (void)hot_pure_func(0);
    noclone_func();
    noipa_func();
    nocf_func();
    return 0;
}
