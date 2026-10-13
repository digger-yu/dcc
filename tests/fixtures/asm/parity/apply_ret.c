/* __builtin_apply / __builtin_return 子集 (GCC 语义) */
static int callee(int v) {
	return v + 1;
}

static int apply_test(int x) {
	void *block = __builtin_apply((void (*)(void))callee, __builtin_apply_args(), 8);
	__builtin_return(block);
	return 99;
}

int main(void) {
	if (apply_test(41) != 42) {
		return 2;
	}
	return 0;
}
