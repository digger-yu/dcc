/* `#if` 的**短路求值**与内核常用内建的诊断静默。两条都拿 gcc 实测当判据：
 *
 * 1) -Wundef 只该警告「真正被读到」的未定义宏。内核这两行在 gcc 下一条警告都没有：
 *      #if defined(__KERNEL__) || !defined(__GLIBC__) || (__GLIBC__ < 2)
 *        （include/uapi/linux/stat.h:7）
 *      #if !defined(_UQUAD_T_) && (!defined(__GLIBC2__) || __GLIBC2__ < 2)
 *        （include/uapi/linux/coda.h:100）
 *    而 dcc 原先先把整行里的未知标识符一律换成 0 并逐个警告 ⇒ 全树 2 536 条 W0202。
 *    顶层 `?:` 也是同一个坑，而且形状更常见（内核那一族字节序判据，实测全树 656 条
 *    W0202，gcc 一条不报）：cond 为假时 `==` 那一支根本不该被读到。
 *    反向对照：真正会被读到的裸标识符（最后一行）gcc 与 dcc 都必须报。
 *
 * 2) `__builtin_unreachable()` / `__builtin_prefetch()` 是编译器内建，**没有声明可写**，
 *    后端本来按名字拦截；不该报「返回类型未知，假定为 int」（全树 39 235 条 W1015）。
 *
 * 判据同时检查**分支选择没被改坏**：短路是求值策略，不是语义。 */
#if defined(__HAS_NOT_THIS__) || !defined(__GLIBC__) || (__GLIBC__ < 2)
int short_circuit_true;
#else
int short_circuit_false;
#endif

#if !defined(_UQUAD_T_) && (!defined(__GLIBC2__) || __GLIBC2__ < 2)
int paren_short_true;
#else
int paren_short_false;
#endif

/* 反向对照：这个裸标识符真的被读到，gcc -Wundef 也报 */
#if READING_UNDEF_MACRO
int read_undef;
#endif

/* 顶层 `?:` 的短路：判据取 gcc -Wundef -E 实测，三行一条警告都没有，
 * 且分支选择分别是 be_not_taken / one_taken / three_taken。 */
#if defined(__BYTE_ORDER) ? __BYTE_ORDER == __BIG_ENDIAN : defined(__BIG_ENDIAN)
int endian_be_taken;
#else
int endian_be_not_taken;
#endif

#define ONE_IS_ONE 1
#if defined(ONE_IS_ONE) ? 1 : UNREAD_IN_ELSE
int cond_true_taken;
#else
int cond_true_not_taken;
#endif

/* cond 为假时反过来：then 支里的裸标识符不该被读到，但 else 的 `1` 要成立 */
#if 0 ? UNREAD_IN_THEN : 1
int cond_false_taken;
#else
int cond_false_not_taken;
#endif

void kernel_builtins(int *p, int cond)
{
	__builtin_prefetch(p);
	if (cond)
		__builtin_unreachable();
}
