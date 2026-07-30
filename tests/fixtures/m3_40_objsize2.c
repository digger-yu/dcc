int main(void) {
	int x;
	char buf[16];
	if (__builtin_object_size(&x, 0) != 4) {
		return 1;
	}
	if (__builtin_object_size(buf, 0) != 16) {
		return 2;
	}
	return 0;
}
