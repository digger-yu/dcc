/* builtin_strcasecmp.c - 验证 __builtin_strcasecmp / __builtin_strncasecmp */
/* 提供本地 strcasecmp / strncasecmp 实现, 使 codegen 的 libc 调用解析到本地函数. */

static int my_tolower(int c) {
    if (c >= 'A' && c <= 'Z') return c - 'A' + 'a';
    return c;
}

int strcasecmp(const char *a, const char *b) {
    while (*a && my_tolower((unsigned char)*a) == my_tolower((unsigned char)*b)) {
        a++; b++;
    }
    return my_tolower((unsigned char)*a) - my_tolower((unsigned char)*b);
}

int strncasecmp(const char *a, const char *b, int n) {
    int i = 0;
    while (i < n && *a && my_tolower((unsigned char)*a) == my_tolower((unsigned char)*b)) {
        a++; b++; i++;
    }
    if (i == n) return 0;
    return my_tolower((unsigned char)*a) - my_tolower((unsigned char)*b);
}

int main(void) {
    int a1 = __builtin_strcasecmp("Hello", "HELLO");
    int a2 = __builtin_strcasecmp("Hello", "World");
    int a3 = __builtin_strcasecmp("ABC", "abc");

    int b1 = __builtin_strncasecmp("Hello, World", "hello, foo", 5);
    int b2 = __builtin_strncasecmp("Hello, World", "hello, foo", 7);
    int b3 = __builtin_strncasecmp("Hello, World", "hello, foo", 9);

    if (a1 != 0) return 1;
    if (a2 == 0) return 2;
    if (a3 != 0) return 3;
    /* b1: 前 5 字符 "Hello" vs "hello" 相同 (case-ins) */
    if (b1 != 0) return 4;
    /* b2: 前 7 字符 "Hello, " vs "hello, " 相同 (case-ins, 含空格) */
    if (b2 != 0) return 5;
    /* b3: 9 字符 "Hello, W" vs "hello, f" 在第 8 位 'W'/'w' vs 'f'/'F' 不同 -> 非 0 */
    if (b3 == 0) return 6;
    return 0;
}
