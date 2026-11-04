/* 内核 `kernel/sched/fair.c:537-554` 的 `vruntime_cmp()`：整族死臂押在
 * `__builtin_strcmp(字面量, 字面量)` 能不能折成常量上。
 *
 * `__BUILD_BUG_vruntime_cmp` 是**故意不存在**的函数（源码注释原话
 * “Unknown operator throws linker error”）。不折那次 strcmp，`else` 臂就留下引用，
 * 最终链接就是 `undefined reference to '__BUILD_BUG_vruntime_cmp'`（实测
 * `kernel/sched/fair.o` 7 条：min_vruntime / entity_before / entity_key /
 * vruntime_eligible / __min_vruntime_update / protect_slice / update_deadline）。
 *
 * 两层判据同一个夹具：
 *   * 产物级：`trap_vruntime_cmp` 的调用重定位必须为 0（gcc -O2 实测 0）；
 *   * 运行级：`main` 返回不匹配的形状编号（0 = 全对），逐条对 **gcc -O2 实测值**。
 */
typedef unsigned long u64;
typedef long long s64;

extern void trap_vruntime_cmp(void) __attribute__((noreturn));

#define vruntime_cmp(A, CMP_STR, B) ({              \
        int __res = 0;                              \
        if (!__builtin_strcmp(CMP_STR, "<")) {       \
                __res = ((s64)((A) - (B)) < 0);     \
        } else if (!__builtin_strcmp(CMP_STR, "<=")) { \
                __res = ((s64)((A) - (B)) <= 0);    \
        } else if (!__builtin_strcmp(CMP_STR, ">")) {  \
                __res = ((s64)((A) - (B)) > 0);     \
        } else if (!__builtin_strcmp(CMP_STR, ">=")) { \
                __res = ((s64)((A) - (B)) >= 0);    \
        } else {                                    \
                trap_vruntime_cmp();                \
        }                                           \
        __res;                                      \
})

/* 四条真实用法：任何一条留下引用都算红 */
int use_lt(u64 a, u64 b) { return vruntime_cmp(a, "<", b); }
int use_le(u64 a, u64 b) { return vruntime_cmp(a, "<=", b); }
int use_gt(u64 a, u64 b) { return vruntime_cmp(a, ">", b); }
int use_ge(u64 a, u64 b) { return vruntime_cmp(a, ">=", b); }

/* 折叠后的**符号**也要对。注意 C 只规定 `strcmp` 返回值的**符号**，不规定大小：
 * 同一对字面量，gcc 常量折叠给 ±1（实测），glibc 运行时的 `strcmp` 给 ±99
 *（差值 'c'-'\0'）。所以下面这三对只断言符号，不断言大小——否则判据就依赖
 * 「拿哪个实现的大小」，而那在 C 里本来就没有定义。 */
int v_eq(void) { return __builtin_strcmp("abc", "abc"); }        /* 符号 0 */
int v_lt(void) { return __builtin_strcmp("abc", "abd"); }        /* 负 */
int v_gt(void) { return __builtin_strcmp("abd", "abc"); }        /* 正 */
int v_prefix(void) { return __builtin_strcmp("abc", "ab"); }     /* 正 */
int v_short(void) { return __builtin_strcmp("ab", "abc"); }      /* 负 */
int v_ncmp0(void) { return __builtin_strncmp("abc", "abd", 0); } /* 0（一个字节也不比）*/
int v_ncmp1(void) { return __builtin_strncmp("abc", "abd", 1); } /* 0 */
int v_ncmp3(void) { return __builtin_strncmp("abc", "abd", 3); } /* 负 */

/* 把符号编成 1/0/-1 三态，避开「返回值大小算不算对」这个无定义问题 */
#define SIGN(x) (((x) > 0) - ((x) < 0))

#define CK(idx, got, want) \
        do {               \
                if ((got) != (want)) \
                        return (idx); \
        } while (0)

int main(void)
{
        u64 a = 5, b = 9;
        /* 语义本身也得对（引用消失不等于算对） */
        CK(1, use_lt(a, b), 1);
        CK(2, use_lt(b, a), 0);
        CK(3, use_gt(b, a), 1);
        CK(4, use_ge(a, a), 1);
        /* 折叠结果的符号（期望值来自主机 glibc 与 gcc 常量折叠的**一致部分**：符号）*/
        CK(5, SIGN(v_eq()), 0);
        CK(6, SIGN(v_lt()), -1);
        CK(7, SIGN(v_gt()), 1);
        CK(8, SIGN(v_prefix()), 1);
        CK(9, SIGN(v_short()), -1);
        CK(10, SIGN(v_ncmp0()), 0);
        CK(11, SIGN(v_ncmp1()), 0);
        CK(12, SIGN(v_ncmp3()), -1);
        return 0;
}
