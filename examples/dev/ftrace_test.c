/* Test: ftrace instrumentation (-pg/-mfentry/-mrecord-mcount) */

int simple_func(int x) {
    return x + 1;
}

int another_func(void) {
    return 42;
}

void void_func(int a, int b) {
    (void)a;
    (void)b;
}
