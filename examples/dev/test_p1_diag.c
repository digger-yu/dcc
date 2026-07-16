// P1 单项测试
int test_register_asm(void) {
    register int rx __asm__("r10");
    rx = 42;
    if (rx != 42) return 1;
    return 0;
}

int test_int128_add(void) {
    __int128 a = 1;
    __int128 b = 2;
    __int128 c = a + b;
    if (c != 3) return 2;
    return 0;
}

int test_int128_sub(void) {
    __int128 d = 100;
    __int128 e = 50;
    if (d - e != 50) return 3;
    return 0;
}

int test_aligned(void) {
    return 0;
}

int main(void) {
    int r = 0;
    r |= test_register_asm();
    r |= test_int128_add();
    r |= test_int128_sub();
    r |= test_aligned();
    return r;
}
