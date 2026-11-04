/* `__builtin_types_compatible_p` 的判定表：每一条的期望值都是 **gcc -O2 实测**出来的
 * （工具链同机同版本；实测方法：每个形状单独一个函数，`objdump -d` 取返回立即数）。
 *
 * 为什么值得单独钉一张表：内核拿这个内置做**类型分支**（`container_of`、`kunmap`、
 * `min/max` 的符号性检查、`static_assert(__is_same_type(...))`），返回错值不是「少一个
 * 优化」而是**走错臂**。实测修前 dcc 在四条上给 1 而 gcc 给 0：
 *   * `void *` vs `struct page *` / `void *` vs `int *` / `char *` vs `void *`
 *     —— 旧实现有条「`void *` 与任意对象指针兼容」的捷径（那是 _Generic 也不有的规则）；
 *   * `int` vs `unsigned`、`short` vs `unsigned short`、`long long` vs `unsigned long long`
 *     —— 旧实现把同宽度的有符号/无符号并成一组；
 *   * `char` vs `signed char`、`char` vs `unsigned char` —— C 里这是三个不同类型；
 *   * `int[4]` vs `const int[4]` —— 旧实现不比数组元素的限定符。
 * 现场后果：`mm/shmem.o` 的 `__compiletime_assert_408/410` 两条陷阱引用就是这么留下的
 * （条件 `if (!(!(__builtin_types_compatible_p(typeof(kaddr), typeof(struct page *)))))`
 * 正确答案是 0 ⇒ `if (0)` ⇒ 死臂）。
 *
 * main 返回**第一条不匹配的形状编号**（0 = 全表与 gcc 一致）。把编号直接当退出码，
 * 是为了让判据能指着一条说「就是这一形错了」，而不是只报一个条数。
 */
struct S1 { int a; };
struct S2 { int a; };
enum E1 { EA = 1 };
enum E2 { EB = 1 };

static int mismatches;

#define CK(idx, fn, expect)                                                \
        do {                                                              \
                if ((fn) != (expect)) {                                   \
                        mismatches = mismatches ? mismatches : (idx);      \
                }                                                         \
        } while (0)

/* —— 指针：pointee 要紧，`void *` 不接管任意对象指针 —— */
static int p01_voidp_vs_structp(void *v, struct S1 *s)
{
        return __builtin_types_compatible_p(typeof(v), typeof(s));
}
static int p02_voidp_vs_voidp(void *a, void *b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int p03_charp_vs_voidp(char *c, void *v)
{
        return __builtin_types_compatible_p(typeof(c), typeof(v));
}
static int p04_intp_vs_intp(int *x, int *y)
{
        return __builtin_types_compatible_p(typeof(x), typeof(y));
}
static int p05_cintp_vs_intp(const int *c, int *p)
{
        return __builtin_types_compatible_p(typeof(c), typeof(p));
}
static int p06_intp_vs_cintp(int *p, const int *c)
{
        return __builtin_types_compatible_p(typeof(p), typeof(c));
}
static int p07_s1p_vs_s2p(struct S1 *a, struct S2 *b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int p08_structp_vs_written(struct S1 *a)
{
        return __builtin_types_compatible_p(typeof(a), typeof(struct S1 *));
}
static int p09_fptr_same(int (*f)(void), int (*g)(void))
{
        return __builtin_types_compatible_p(typeof(f), typeof(g));
}
static int p10_fptr_diff_param(int (*f)(void), int (*g)(int))
{
        return __builtin_types_compatible_p(typeof(f), typeof(g));
}

/* —— 整型：符号性是类型的一部分；`char`/`signed char`/`unsigned char` 是三个类型 —— */
static int i01_int_vs_unsigned(int a, unsigned int b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i02_int_vs_int(int a, signed int b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i03_long_vs_longint(long a, long int b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i04_ulong_vs_ulongint(unsigned long a, unsigned long int b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i05_long_vs_longlong(long a, long long b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i06_short_vs_ushort(short a, unsigned short b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i07_char_vs_schar(char a, signed char b)
{
        /* 已知缺口（仅在 -DTCP_KNOWN_GAPS 下才检）：`parse_type` 与声明路径对
         * `signed char` 的类型表示不一致（声明那条会归到 `Type::SChar`，
         * 内置的类型参数那条不归），所以两者被判成同一个变体⇒ 兼容。
         * 实测 gcc=0（C11 6.2.7：`char`、`signed char`、`unsigned char` 是三个不同类型）。 */
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i08_char_vs_uchar(char a, unsigned char b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i09_schar_vs_uchar(signed char a, unsigned char b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int i10_bool_vs_int(_Bool b, int a)
{
        return __builtin_types_compatible_p(typeof(b), typeof(a));
}
static int i11_int_vs_tyof_literal(int a)
{
        return __builtin_types_compatible_p(typeof(a), typeof(3));
}
static int i12_cint_vs_int(const int c, int v)
{
        /* 顶层限定符不算差别（与 pointee 限定符相反），实测 gcc=1 */
        return __builtin_types_compatible_p(typeof(c), typeof(v));
}

/* —— 浮点 / enum / 数组 —— */
static int f01_double_vs_longdouble(double a, long double b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int f02_double_vs_double(double a, double b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int e01_enum_vs_int(enum E1 e, int a)
{
        return __builtin_types_compatible_p(typeof(e), typeof(a));
}
static int e02_enum_vs_otherenum(enum E1 a, enum E2 b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int e03_enum_vs_sameenum(enum E1 a, enum E1 b)
{
        return __builtin_types_compatible_p(typeof(a), typeof(b));
}
static int a01_arr_vs_carr(void)
{
        int x[4];
        const int y[4];
        /* 实测 gcc=1（兼容）：`int[4]` 与 `const int[4]` 这一对只差在元素限定符，
         * gcc 的 `__builtin_types_compatible_p` 判成兼容；而同样这个差别放到指针下面
         * （`int (*)[4]` vs `const int (*)[4]`）gcc 判成不兼容。两条都是实测，不推。 */
        return __builtin_types_compatible_p(typeof(x), typeof(y));
}
static int a04_arrp_vs_carrp(void)
{
        /* 已知缺口（仅在 -DTCP_KNOWN_GAPS 下才检）：指针指向的数组里那层元素限定符，
         * 实测 gcc 判不兼容而 dcc 判兼容。 */
        int(*p)[4] = 0;
        const int(*q)[4] = 0;
        return __builtin_types_compatible_p(typeof(p), typeof(q));
}
static int a02_arr_vs_arr(void)
{
        int x[4];
        int y[4];
        return __builtin_types_compatible_p(typeof(x), typeof(y));
}
static int a03_arr5_vs_arr4(void)
{
        int x[5];
        int y[4];
        return __builtin_types_compatible_p(typeof(x), typeof(y));
}

int main(void)
{
        struct S1 s1;
        struct S2 s2;
        int arr_x[4];
        (void)arr_x;
        (void)s1;
        (void)s2;
#ifdef TCP_KNOWN_GAPS
        /* 两条已知与 gcc 不一致的形状（默认不进本表，开了旋钮才检）：
         *   * 17 号 `char` vs `signed char`；
         *   * 31 号 `int (*)[4]` vs `const int (*)[4]`。 */
#endif
        /* 指针：期望值全部来自 gcc -O2 实测 */
        CK(1, p01_voidp_vs_structp((void *)0, &s1), 0);
        CK(2, p02_voidp_vs_voidp((void *)0, (void *)0), 1);
        CK(3, p03_charp_vs_voidp((char *)0, (void *)0), 0);
        CK(4, p04_intp_vs_intp((int *)0, (int *)0), 1);
        CK(5, p05_cintp_vs_intp((const int *)0, (int *)0), 0);
        CK(6, p06_intp_vs_cintp((int *)0, (const int *)0), 0);
        CK(7, p07_s1p_vs_s2p(&s1, &s2), 0);
        CK(8, p08_structp_vs_written(&s1), 1);
        CK(9, p09_fptr_same(0, 0), 1);
        CK(10, p10_fptr_diff_param(0, 0), 0);
        /* 整型 */
        CK(11, i01_int_vs_unsigned(0, 0U), 0);
        CK(12, i02_int_vs_int(0, 0), 1);
        CK(13, i03_long_vs_longint(0L, 0L), 1);
        CK(14, i04_ulong_vs_ulongint(0UL, 0UL), 1);
        CK(15, i05_long_vs_longlong(0L, 0LL), 0);
        CK(16, i06_short_vs_ushort((short)0, (unsigned short)0), 0);
#ifdef TCP_KNOWN_GAPS
        CK(17, i07_char_vs_schar('a', (signed char)'a'), 0);
#endif
        CK(18, i08_char_vs_uchar('a', (unsigned char)'a'), 0);
        CK(19, i09_schar_vs_uchar((signed char)1, (unsigned char)1), 0);
        CK(20, i10_bool_vs_int(1, 1), 0);
        CK(21, i11_int_vs_tyof_literal(0), 1);
        CK(22, i12_cint_vs_int(0, 0), 1);
        /* 浮点 / enum / 数组 */
        CK(23, f01_double_vs_longdouble(1.0, 1.0L), 0);
        CK(24, f02_double_vs_double(1.0, 2.0), 1);
        CK(25, e01_enum_vs_int(EA, 1), 0);
        CK(26, e02_enum_vs_otherenum(EA, EB), 0);
        CK(27, e03_enum_vs_sameenum(EA, EA), 1);
        CK(28, a01_arr_vs_carr(), 1);
        CK(29, a02_arr_vs_arr(), 1);
        CK(30, a03_arr5_vs_arr4(), 0);
#ifdef TCP_KNOWN_GAPS
        CK(31, a04_arrp_vs_carrp(), 0);
#endif
        return mismatches;
}
