/* `sizeof`/`_Alignof` 的结果类型必须是 `size_t`（LP64 上 = `unsigned long`），不是 `int`。
 *
 * 为什么这条要单独钉：`Expr::ty()` 以前对这些节点写死 `Type::Int`，而 `typeof(表达式)` 与
 * `__auto_type` 的推导直接吃这个值 ⇒ 局部变量的**类型**推错。现场后果（全树未定义引用里
 * 最大的一族）：内核 `min()/max()` 的符号性检查
 *   `count = min(count, sizeof(buf) - 1);`
 * 里 `sizeof(buf) - 1` 被推成有符号 int，于是 `is_signed_type(typeof(...))` 选错臂，
 * `__compiletime_assert_NNN`（一个**故意不存在**的函数）留在产物里，链接期直接
 * `undefined reference`（实测 `lib/kstrtox.o` 11 条，修后 0 条）。
 *
 * 判据把「有符号?10:0 + sizeof」编成返回值，逐形对 **gcc -O2 实测值**（注释里标着）：
 * 只看 `sizeof` 的宽度看不出错——真正错的是符号性，两者一起编才分得开。
 * `main` 返回不匹配的条数（0 = 全对）。
 */
typedef unsigned long size_t;

#define is_signed_type(t) (((t)(-1)) < (t)1)
/* 值 = 有符号 ? 10 : 0 加上 sizeof，所以 8 = 无符号 8 字节，14 = 有符号 4 字节。 */
#define probe_of(t) (is_signed_type(t) * 10 + (int)sizeof(t))

/* 期望值全部来自 gcc -O2 实测（同一台机器、同一版本） */
int a1_auto_from_param(size_t c)
{
        __auto_type a = c; /* gcc: 8 */
        return probe_of(typeof(a));
}
int a2_typeof_param(size_t c)
{
        typeof(c) a = c; /* gcc: 8 */
        return probe_of(typeof(a));
}
int a3_auto_from_sizeof(size_t c)
{
        __auto_type b = (sizeof(char[8]) - 1); /* gcc: 8 —— 修前 dcc 给 14（有符号 int） */
        (void)c;
        return probe_of(typeof(b));
}
int a4_typeof_sizeof(size_t c)
{
        typeof(sizeof(char[8]) - 1) b = (sizeof(char[8]) - 1); /* gcc: 8 —— 修前 14 */
        (void)c;
        return probe_of(typeof(b));
}
int a5_spelled(size_t c)
{
        size_t b = c; /* gcc: 8 */
        return probe_of(typeof(b));
}
int a6_int_signed(size_t c)
{
        int b = (int)c; /* gcc: 14 —— 反向对照：有符号 int 就该是 14，证明判据不是「一律 8」 */
        return probe_of(typeof(b));
}
/* `sizeof(expr)`（表达式形式）与 `_Alignof` 也必须是 size_t */
int a7_sizeof_expr(long v)
{
        __auto_type b = sizeof(v); /* gcc: 8 */
        return probe_of(typeof(b));
}
int a8_alignof(long v)
{
        /* 只能走「先存进一个 `__auto_type` 局部再推回去」这条路：`is_signed_type` 的
         * 参数必须是一个**类型**，直接写 `_Alignof(long)` 会展开成
         * `((_Alignof(long))(-1)) < …` 这种非法强制（gcc 与 dcc 都报错，已实测）。 */
        (void)v;
        __auto_type al = _Alignof(long);
        return probe_of(typeof(al)); /* gcc: 8（size_t）*/
}

#define CK(idx, got, want)          \
        do {                        \
                if ((got) != (want)) { \
                        mismatches++; \
                        if (!first)   \
                                first = (idx); \
                }                   \
        } while (0)

static int mismatches;
static int first;

int main(void)
{
        size_t c = 3;
        long v = 7;
        CK(1, a1_auto_from_param(c), 8);
        CK(2, a2_typeof_param(c), 8);
        CK(3, a3_auto_from_sizeof(c), 8);
        CK(4, a4_typeof_sizeof(c), 8);
        CK(5, a5_spelled(c), 8);
        CK(6, a6_int_signed(c), 14);
        CK(7, a7_sizeof_expr(v), 8);
        CK(8, a8_alignof(v), 8);
        /* 退出码先给「不匹配条数」，再乘 16 留一条低位通道：第一条编号（1..8）。
         * 直接返回 first 更实用：判据能指着一条说「就是这一形错了」。 */
        return mismatches ? first : 0;
}
