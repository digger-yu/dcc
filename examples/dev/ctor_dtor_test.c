// Test __attribute__((constructor/destructor)) and __attribute__((alias))
static int ctor_called = 0;
static int dtor_called = 0;

static void __attribute__((constructor)) my_init(void) {
    ctor_called = 1;
}

static void __attribute__((destructor)) my_fini(void) {
    dtor_called = 1;
}

// alias test: real_func is the real function, alias_func is an alias
int real_func(void) {
    return 42;
}

int alias_func(void) __attribute__((alias("real_func")));

int printf(const char *fmt, ...);

int main(void) {
    printf("ctor_called=%d\n", ctor_called);
    int r = alias_func();
    printf("alias_func()=%d\n", r);
    printf("real_func()=%d\n", real_func());
    return 0;
}
