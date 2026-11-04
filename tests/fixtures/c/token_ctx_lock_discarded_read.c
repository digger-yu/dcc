/* 「值被丢弃的纯读」必须一个字节都不发 —— 内核 context-analysis token 变量的形状。
 *
 * 缺陷现场（report/linux71-dcc-fulltree-build-2026-10-28.md §3.30）：
 * 内核 include/linux/compiler-context-analysis.h:242 的
 * `token_context_lock(name, ...)` 展开成
 *     context_lock_struct(__ctx_lock_##name, ##__VA_ARGS__) {};
 *     extern const struct __ctx_lock_##name *name;
 * 注释原话：declare an abstract global context lock instance ... **but not backed by a
 * real data structure (linker error if accidentally referenced)**。也就是说 `RCU` 这类
 * 名字**天生没有定义**，它只当类型令牌存在，谁真去读它谁就链接失败。
 *
 * 而 rcupdate.h:456 在关掉 CONFIG_PROVE_RCU 时给
 *     #define lockdep_assert_in_rcu_read_lock() __assume_shared_ctx_lock(RCU)
 * compiler-context-analysis.h:173 又把它定义成
 *     #define __assume_shared_ctx_lock(var)  do { (void)(var); } while (0)
 * —— 一条**只求值不取值**的读。gcc 对丢弃值的纯读不发任何指令，所以引用不存在、UND
 * 条目也不存在；dcc 以前在 `Expr::Cast` 的 void 臂里只筛掉了字面量，`Ident` 照常
 * 发码 ⇒ 产物里多出 `R_X86_64_PC32 RCU` 重定位与 `UND RCU`，最终链接报
 * `undefined reference to 'RCU'`（实测 io_uring/tw.o，`io_ctx_mark_taskrun()` 里）。
 *
 * 本夹具把「该删的」和「绝不能删的」钉在同一份产物上：
 *   * `token_only`         —— 陷阱本体，丢弃值的纯读：不得留任何引用（红→绿判据）
 *   * `live_call_kept`     —— `(void)f()`：call 是副作用，引用必须在
 *   * `volatile_read_kept` —— `(void)v`：volatile 读本身可观察，引用必须在
 *   * `addr_kept`          —— 真把 token 当值取用：引用必须在（反向对照，证明
 *                            修复不是「凡是 RCU 都删」）
 *
 * 为什么主判据在 -O2/-O3：规则住在 AST 层的 `opt.rs::prune_dead_stores`（`level >= 2`），
 * 因为只有那一层拿得到**声明**（`VarDecl::quals`）——「丢弃值的纯读能不能整条不发」必须
 * 与「这条读是不是 volatile」同处一层判，否则就会把后者误删（发射器侧试过，实测
 * `(void)(vglob)` 的引用从 1 条变成 0 条，已退回）。-O0 仍会发那一条，而内核不用 -O0 编；
 * 真树的 -O2 判据另拿 `io_uring/tw.o` 的单 TU 回放（见报告 §3.30）。
 */
struct __ctx_lock_RCU;
extern const struct __ctx_lock_RCU *RCU;
extern const struct __ctx_lock_RCU *rcu_token_used;

extern int trap_call(void);
volatile int vglob_token;

#define __assume_shared_ctx_lock(var) do { (void)(var); } while (0)

void token_only(void)
{
	__assume_shared_ctx_lock(RCU);
	__assume_shared_ctx_lock(RCU);
}

int live_call_kept(void)
{
	do { (void)trap_call(); } while (0);
	return 1;
}

int volatile_read_kept(void)
{
	/* 只留这一条 volatile 访问：产物里对 `vglob_token` 的引用数因此是个确定值，
	 * 能直接当作「丢弃值的 volatile 读有没有被顺手删掉」的尺子。 */
	do { (void)(vglob_token); } while (0);
	return 0;
}

const void *addr_kept(void)
{
	return (const void *)rcu_token_used;
}
