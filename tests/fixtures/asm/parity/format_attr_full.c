// __attribute__((format)) — comprehensive format string checking.
// Tests: printf/scanf format validation, type mismatch, arity,
//        length modifiers, width/precision stars, multiple formats.

// --- printf-style ---
__attribute__((format(printf, 1, 2)))
void my_log(const char *fmt, ...);

// --- scanf-style ---
__attribute__((format(scanf, 1, 2)))
int my_scanf(const char *fmt, ...);

// --- printf with __format__/__gnu_printf__ spelling ---
__attribute__((__format__(__gnu_printf__, 1, 2)))
void gnu_log(const char *fmt, ...);

int main(void) {
    // Correct printf usage — no warnings expected
    my_log("hello %d world %s\n", 42, "foo");
    my_log("float=%f char=%c\n", 3.14, 'A');
    my_log("hex=%x oct=%o unsigned=%u\n", 255, 8, 100U);
    my_log("long=%ld ulong=%lu ll=%lld\n", 100L, 200UL, 300LL);
    my_log("ptr=%p str=%s\n", (void*)0, "test");
    my_log("%%literal%%\n");

    // Width/precision with *
    my_log("%*.*f\n", 10, 3, 3.14);

    // Correct scanf usage (pointer args)
    int x;
    float f;
    my_scanf("%d %f", &x, &f);

    // gnu_printf spelling
    gnu_log("value=%d\n", 99);

    return 0;
}
