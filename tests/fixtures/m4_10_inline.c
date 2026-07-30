static int __attribute__((always_inline)) sq(int x) { return x * x; }

static int __attribute__((noinline)) add100(int x) { return x + 100; }

int main(void) {
	if (sq(6) != 36)
		return 1;
	if (add100(8) != 108)
		return 2;
	return 0;
}
