int main(void) {
	int x = 1;
	if (__builtin_constant_p(42) != 1)
		return 1;
	if (__builtin_constant_p(x) != 0)
		return 2;
	if (__builtin_constant_p(((__int128)1 << 100) / 3) != 1)
		return 3;
	return 0;
}
