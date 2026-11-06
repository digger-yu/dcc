/* 逐函数 canary 判据 (GCC `-fstack-protector-strong` 语义) 的产物级锁:
 * 只有「有数组局部 / 取过局部地址 / VLA」的函数插桩, 纯标量局部不插。
 * 内核 native_write_cr4() 就是后者 —— 早期启动时 GS 基址为 0, 多插一条
 * `%gs:...` canary 装载就会 #PF -> #DF (PVH 启动 triple fault 现场)。 */
int g_counter;

int scalar_only(int x)
{
	int a = x + 1;

	return a * 3;
}

int with_array(int x)
{
	char buf[16];
	int i;

	for (i = 0; i < 16; i++)
		buf[i] = (char)(x + i);
	return buf[x & 15];
}

int with_addr(int *p)
{
	int v = g_counter;
	int *q = &v;

	*q += 1;
	return v + *p;
}

int plain(void)
{
	return g_counter;
}
