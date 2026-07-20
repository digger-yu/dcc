// __builtin_* 编译期折叠 (M15)
extern int printf(const char *, ...);

int main(void) {
    // 字符串字面量编译期求值: __builtin_strlen 折叠
    int n = __builtin_strlen("hello");
    printf("%d\n", n);
    // __builtin_abs
    int a = __builtin_abs(-42);
    printf("%d\n", a);
    // __builtin_bswap32
    unsigned int v = __builtin_bswap32(0x12345678);
    printf("0x%x\n", v);
    return 0;
}
