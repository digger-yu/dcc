/* __int128 SysV 传参/返回值：16 字节占两个 GP 寄存器 */
static __int128 passthrough(__int128 v) { return v; }

static __int128 add128(__int128 a, __int128 b) { return a + b; }

static __int128 mul128(__int128 a, __int128 b) { return a * b; }

int main(void) {
	__int128 a = ((__int128)10 << 64) | 3;
	__int128 b = passthrough(a);
	if (b != a)
		return 1;
	__int128 c = add128(a, ((__int128)1 << 64) | 2);
	if (c != (((__int128)11 << 64) | 5))
		return 2;
	__int128 m = mul128((__int128)100, 7);
	if (m != 700)
		return 3;
	return 0;
}
