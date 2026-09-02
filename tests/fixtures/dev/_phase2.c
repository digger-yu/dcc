__attribute__((section(".init.text"))) int init_func(void) { return 1; }
__attribute__((weak)) int weak_func(void) { return 2; }
__attribute__((aligned(32))) int aligned_var;
__attribute__((visibility("hidden"))) int hidden_var;
__attribute__((unused)) int unused_var;
int test_int128(void) { __int128 x = 100; __uint128_t y = 200; return (int)(x + y); }
int test_register(void) { register int r9 asm("r9") = 42; return r9; }
int main(void) { return init_func() + weak_func() + test_int128() + test_register(); }
