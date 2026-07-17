/* __builtin_apply / __builtin_return 子集 */
static int callee(void *data) {
	return *(int *)data + 1;
}

static int apply_test(void) {
	int x = 41;
	void *block = __builtin_apply((void (*)(void))callee, &x, 8);
	if (block == 0) {
		return 1;
	}
	__builtin_return(block);
	return 99;
}

int main(void) {
	if (apply_test() != 42) {
		return 2;
	}
	return 0;
}
