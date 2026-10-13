/* __builtin_object_size FORTIFY 模式：数组/结构体成员 */
struct pkt {
	char data[8];
	int len;
};

int main(void) {
	struct pkt p;
	char buf[16];
	if (__builtin_object_size(buf, 1) != 1) {
		return 1;
	}
	if (__builtin_object_size(buf, 0) != 16) {
		return 2;
	}
	if (__builtin_object_size(p.data, 0) != 8) {
		return 3;
	}
	if (__builtin_object_size(&p.len, 0) != 4) {
		return 4;
	}
	return 0;
}
