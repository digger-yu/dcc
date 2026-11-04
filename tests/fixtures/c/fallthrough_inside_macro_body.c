/* 内核 include/linux/signal.h 的真实形状：整个 switch 坐在**宏定义体**里
 * （_SIG_SET_OP），`fallthrough;` 由 compiler_attributes.h 展开成
 * `__attribute__((__fallthrough__))`。修前实测：localmodconfig 全树 W0501 的
 * 主体就是这里（每个含 <linux/signal.h> 的 TU 刷 5 条）。
 * 判据取 gcc 同旗标实测：整份夹具只该报 f_real_bug 那 1 条。 */
#define fallthrough __attribute__((__fallthrough__))

#define _sig_or(x, y) ((x) | (y))

#define _SIG_SET_OP(name, op)							\
static inline int name(int *sig, const int *set)					\
{										\
	switch (4) {								\
	case 4: *sig = op(set[3], 0);						\
		*sig += op(set[2], 0);						\
		fallthrough;							\
	case 3: *sig += op(set[1], 0);						\
		fallthrough;							\
	case 2: *sig += op(set[0], 0);						\
		break;								\
	default: return -1;							\
	}									\
	return 0;								\
}

_SIG_SET_OP(signotset_probe, _sig_or)

/* 同一形状但直接用双下划线属性名（不经宏），作对照 */
static inline int direct_form(int *x)
{
	switch (2) {
	case 2:
		*x = 1;
		__attribute__((__fallthrough__));
	case 1:
		*x += 1;
		break;
	default:
		return -1;
	}
	return 0;
}

/* 反向对照：真没标记的贯穿，gcc 与 dcc 都必须报 */
int f_real_bug(int x)
{
	switch (x) {
	case 1:
		x++;
	case 2:
		return x;
	default:
		return 0;
	}
}
