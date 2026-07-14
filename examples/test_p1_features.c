// P1 严重问题测试: __int128, register asm, 全局符号引用, aligned 属性

// === P1-7: aligned 属性 ===
static int __attribute__((aligned(64))) aligned_var = 42;

// === P1-6: 全局符号引用 ===
int global_a = 100;
int *global_ptr = &global_a;

// === P1-5: register asm 变量 ===
int test_register_asm(void) {
    register int x __asm__("r10");
    x = 77;
    return x;  // 应从 r10 返回
}

// === P1-4: __int128 运算 ===
int test_int128_add(void) {
    __int128 a = 1;
    __int128 b = 2;
    __int128 c = a + b;
    return (int)(c == 3 ? 0 : 1);
}

int test_int128_sub(void) {
    __int128 a = 100;
    __int128 b = 50;
    __int128 c = a - b;
    return (int)(c == 50 ? 0 : 2);
}

int test_int128_and(void) {
    __int128 a = 0xFF00;
    __int128 b = 0x0FF0;
    __int128 c = a & b;
    return (int)(c == 0x0F00 ? 0 : 3);
}

int test_int128_or(void) {
    __int128 a = 0xFF00;
    __int128 b = 0x00FF;
    __int128 c = a | b;
    return (int)(c == 0xFFFF ? 0 : 4);
}

int test_int128_xor(void) {
    __int128 a = 0xFFFF;
    __int128 b = 0x0FF0;
    __int128 c = a ^ b;
    return (int)(c == 0xF00F ? 0 : 5);
}

int test_int128_eq(void) {
    __int128 a = 42;
    __int128 b = 42;
    __int128 c = 43;
    int r = 0;
    if (a != b) r |= 6;
    if (a == c) r |= 6;
    return r;
}

int test_int128_cmp(void) {
    __int128 a = 10;
    __int128 b = 20;
    int r = 0;
    if (!(a < b)) r |= 7;
    if (a > b) r |= 7;
    if (!(b > a)) r |= 7;
    if (a >= b) r |= 7;
    return r;
}

int test_int128_shl(void) {
    __int128 a = 1;
    __int128 b = a << 4;
    return (int)(b == 16 ? 0 : 8);
}

int test_int128_shr(void) {
    __int128 a = 256;
    __int128 b = a >> 4;
    return (int)(b == 16 ? 0 : 9);
}

// === P1-7: 检查对齐 ===
int test_aligned(void) {
    unsigned long addr = (unsigned long)&aligned_var;
    return (int)(addr % 64 == 0 ? 0 : 10);
}

// === P1-6: 检查符号引用 ===
int test_symbol_ref(void) {
    return (*global_ptr == 100 ? 0 : 11);
}

int main(void) {
    int r = 0;
    r |= test_register_asm();
    r |= test_int128_add();
    r |= test_int128_sub();
    r |= test_int128_and();
    r |= test_int128_or();
    r |= test_int128_xor();
    r |= test_int128_eq();
    r |= test_int128_cmp();
    r |= test_int128_shl();
    r |= test_int128_shr();
    r |= test_aligned();
    r |= test_symbol_ref();
    return r;
}
