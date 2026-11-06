/* 调用结果直接当全局数组下标（等价于内核的
 * `per_cpu_ptr(&cpu_stopper, task_cpu(task))` 形状）。
 *
 * 判据只看运行值：调用返回在 %rax，而 SIB 变址要求索引在 %rcx。若是
 * `-mno-sse` 下把整数类型的返回值误判成「交付在 st(0)」，那条
 * `mov %rcx, %rax` 会被跳过，下标取到调用前的垃圾。
 *
 * 两个入口都试：直接调用（noinline 保住 call 形状）与 volatile 函数指针
 * （任何人都没法内联），两者都走同一段调用结果物化代码。
 */

static int table[8] = { 10, 11, 12, 13, 14, 15, 16, 17 };

__attribute__((noinline)) static unsigned int pick_index(void)
{
	return 3;
}

__attribute__((noinline)) static unsigned int pick_one(void)
{
	return 1;
}

static unsigned int (*volatile pick_indirect)(void) = pick_one;

int main(void)
{
	if (table[pick_index()] != 13)
		return 1;
	if (table[pick_indirect()] != 11)
		return 2;
	return 0;
}
