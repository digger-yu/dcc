/* test_m12_builtins.c - 测试新增的 M12+ 内建函数 */
/* 提供本地 strcasecmp/strncasecmp/fma/fmaf 实现, 验证 codegen 走 libc 路径. */

struct packed_s { char a; int b; char c; } __attribute__((packed));

typedef int (*my_fn_ptr)(void);

int g_counter = 0;

int helper(int x) {
    g_counter++;
    return x * 2;
}

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

double fma(double a, double b, double c) { return a * b + c; }
float fmaf(float a, float b, float c) { return a * b + c; }

int test_func(int a) {
    /* __builtin_LINE / __builtin_FUNCTION / __builtin_FILE */
    int line = __builtin_LINE();
    const char *func = __builtin_FUNCTION();
    const char *file = __builtin_FILE();
    if (line <= 0) return 1;
    if (func == 0) return 2;
    if (file == 0) return 3;
    (void)file;

    /* __builtin_clrsb 系列 */
    int cl1 = __builtin_clrsb(0x00000001);
    int cl2 = __builtin_clrsb(0x80000000);
    int cl3 = __builtin_clrsb(0xFFFFFFFF);
    int cl4 = __builtin_clrsb(0x00000000);
    int cll = __builtin_clrsbll(0x100000000LL);
    if (cl1 != 30) return 11;
    if (cl2 != 0) return 12;
    if (cl3 != 31) return 13;
    if (cl4 != 31) return 14;
    if (cll != 30) return 15;

    /* __builtin_rotateleft/right 系列 */
    unsigned int r1 = __builtin_rotateleft32(0x80000001u, 1);
    unsigned int r2 = __builtin_rotateright32(0x00000003u, 1);
    unsigned long long rl1 = __builtin_rotateleft64(0x8000000000000001ull, 1);
    unsigned long long rl2 = __builtin_rotateright64(rl1, 1);
    if (r1 != 0x00000003u) return 21;
    if (r2 != 0x80000001u) return 22;
    if (rl1 != 0x0000000000000003LL) return 23;
    if (rl2 != 0x8000000000000001LL) return 24;

    /* __builtin_choose_expr / __builtin_types_compatible_p */
    int t1 = __builtin_choose_expr(1, 100, 200);
    int t2 = __builtin_choose_expr(0, 100, 200);
    int compat1 = __builtin_types_compatible_p(int, int);
    int compat2 = __builtin_types_compatible_p(int, long);
    int compat3 = __builtin_types_compatible_p(int, const int);
    if (t1 != 100) return 31;
    if (t2 != 200) return 32;
    if (compat1 != 1) return 33;
    if (compat2 != 0) return 34;
    if (compat3 != 1) return 35;

    /* __builtin_classify_type */
    int c1 = __builtin_classify_type(int);
    int c2 = __builtin_classify_type(int *);
    int c3 = __builtin_classify_type(double);
    int c4 = __builtin_classify_type(struct packed_s);
    int c5 = __builtin_classify_type(void);
    int c6 = __builtin_classify_type(my_fn_ptr);
    if (c1 != 2) return 41;
    if (c2 != 3) return 42;
    if (c3 != 4) return 43;
    if (c4 != 6) return 44;
    if (c5 != 1) return 45;
    if (c6 != 3) return 46;

    /* __builtin_strcasecmp / __builtin_strncasecmp */
    int sc1 = __builtin_strcasecmp("Hello", "HELLO");
    int sc2 = __builtin_strcasecmp("Hello", "World");
    int sc3 = __builtin_strncasecmp("Hello, World", "hello, foo", 5);
    if (sc1 != 0) return 51;
    if (sc2 == 0) return 52;
    if (sc3 != 0) return 53;

    /* __builtin_constant_p */
    int cpi1 = __builtin_constant_p(42);
    int cpi2 = __builtin_constant_p(a);
    if (cpi1 != 1) return 61;
    if (cpi2 != 0) return 62;

    /* __builtin_object_size (FORTIFY 完整版) */
    char buf[64];
    int obj1 = __builtin_object_size(buf, 0);
    int obj3 = __builtin_object_size("abcd", 0);
    int obj4 = __builtin_object_size("abcd", 1);
    int obj5 = __builtin_object_size("abcd", 2);
    int obj6 = __builtin_object_size("abcd", 3);
    if (obj1 != 64) return 71;
    if (obj3 != 5) return 72;
    if (obj4 != 1) return 73;
    if (obj5 != 5) return 74;
    if (obj6 != 1) return 75;

    /* __func__ / __FUNCTION__ / __PRETTY_FUNCTION__ */
    const char *f1 = __func__;
    const char *f2 = __FUNCTION__;
    const char *f3 = __PRETTY_FUNCTION__;
    if (f1 == 0) return 81;
    if (f2 == 0) return 82;
    if (f3 == 0) return 83;

    (void)helper;
    return 0;
}

int main(void) {
    return test_func(0);
}
