/* 同 TU 内「空体 void 函数」的调用必须被就地吸收（否则实参里那两个「只有声明、没有
 * 定义」的外部数组会留下链接期引用，见 docs/kernel_build_guide.md §5.0j 第 4 条）。
 * 同时钉住两条不许过头：
 *   1. 被调者有真实体时，调用必须留着；
 *   2. 实参自己有可观察副作用（这里是一次真函数调用）时，即使被调者是空体，
 *      整个调用也必须留着 —— 吸收它就把那次调用丢了。
 * 期望值取 gcc -O2 实测。 */
extern int probe_only_declared[], probe_only_declared_end[];

void empty_shell(int *a, int *b)
{
}

void __attribute__((noinline)) real_body(int *a, int *b)
{
	*a = *b + 1;
}

int * __attribute__((noinline)) make_arg(int *p)
{
	return p + 1;
}

int sink;
int counter;

/* 1) 该被吸收：两个实参都是纯读（外部数组名 = 地址常量） */
void absorbed(void)
{
	empty_shell(probe_only_declared, probe_only_declared_end);
}

/* 2) 不该被吸收：被调者有真实体 */
void kept_real_body(void)
{
	real_body(&sink, probe_only_declared);
}

/* 3) 不该被吸收：实参本身是一次真调用 */
void kept_side_effect_arg(void)
{
	empty_shell(make_arg(&counter), probe_only_declared);
}
