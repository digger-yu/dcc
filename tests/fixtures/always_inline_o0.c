static int __attribute__((always_inline)) double_it(int x) { return x + x; }

int main(void) {
	return double_it(21) == 42 ? 0 : 1;
}
