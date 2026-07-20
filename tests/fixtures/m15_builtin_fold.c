// __builtin_* 编译期折叠 (M15)
int main(void) {
    int n = __builtin_strlen("hello");   // 5
    int a = __builtin_abs(-42);          // 42
    unsigned int v = __builtin_bswap32(0x12345678);  // 0x78563412
    if (n != 5) return 1;
    if (a != 42) return 2;
    if (v != 0x78563412) return 3;
    return 0;
}
