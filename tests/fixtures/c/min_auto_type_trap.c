/* 内核 `min()` 的符号性检查在四种「临时量声明写法」下的形状。
 *
 * 判据 = 产物里 `trap_v*` 的调用重定位条数，gcc -O2 实测**全 0**（两条 `__auto_type`/
 * `typeof(表达式)` 的形在修 `sizeof` 结果类型之前各留 1 条）。
 * 为什么留四条而不是只留坏的那条：v3（写类型名）与 v4（一半一半）在修前**本来就是绿的**，
 * 有它们在，「把判据改成只检 v1/v2」这种自欺就瞒不过去。
 */
typedef unsigned long size_t;
extern void trap_v1(void) __attribute__((noreturn));
extern void trap_v2(void) __attribute__((noreturn));
extern void trap_v3(void) __attribute__((noreturn));
extern void trap_v4(void) __attribute__((noreturn));

#define CTA(cond, name)                              \
        do {                                         \
                if (!(cond))                        \
                        name();                     \
        } while (0)
#define BBM(cond, name) CTA(!(cond), name)
#define is_signed_type(t) (((t)(-1)) < (t)1)
#define statically_true(x) (__builtin_constant_p(x) && (x))
#define __is_nonneg(ux) statically_true((long long)(ux) >= 0)
#define __sign_use(ux)                                                        \
        (is_signed_type(typeof(ux)) ? (2 + __is_nonneg(ux))                    \
                                    : (1 + 2 * (sizeof(ux) < 4)))
#define __types_ok(ux, uy) (__sign_use(ux) & __sign_use(uy))

/* v1：两个临时量都是 `__auto_type`（内核 7.1 的 `min()` 就是这个写法） */
int v1_auto_type(size_t count)
{
        size_t r = ({
                __auto_type a_ = (count);
                __auto_type b_ = (sizeof(char[8]) - 1);
                BBM(!__types_ok(a_, b_), trap_v1);
                a_ < b_ ? a_ : b_;
        });
        return (int)r;
}
/* v2：两个都用 `typeof(表达式)` 声明 */
int v2_typeof(size_t count)
{
        size_t r = ({
                typeof(count) a_ = (count);
                typeof(sizeof(char[8]) - 1) b_ = (sizeof(char[8]) - 1);
                BBM(!__types_ok(a_, b_), trap_v2);
                a_ < b_ ? a_ : b_;
        });
        return (int)r;
}
/* v3：两个都写类型名（修前就应当是绿的） */
int v3_spelled(size_t count)
{
        size_t r = ({
                size_t a_ = (count);
                size_t b_ = (sizeof(char[8]) - 1);
                BBM(!__types_ok(a_, b_), trap_v3);
                a_ < b_ ? a_ : b_;
        });
        return (int)r;
}
/* v4：只有一半用 `__auto_type`（修前也应是绿的，用来定位「错在哪个推导」） */
int v4_half(size_t count)
{
        size_t r = ({
                __auto_type a_ = (count);
                size_t b_ = (sizeof(char[8]) - 1);
                BBM(!__types_ok(a_, b_), trap_v4);
                a_ < b_ ? a_ : b_;
        });
        return (int)r;
}
