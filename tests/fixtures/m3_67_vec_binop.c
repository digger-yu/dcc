typedef int v4si __attribute__((vector_size(16)));

int main(void) {
	v4si a = {1, 2, 3, 4};
	v4si b = {10, 20, 30, 40};
	v4si c = {5, 5, 5, 5};
	v4si d = (a + b) + c;
	v4si e = a;
	e = e + b;
	int *p = (int *)&d;
	int *q = (int *)&e;
	if (p[0] != 16 || p[1] != 27 || p[2] != 38 || p[3] != 49) {
		return 1;
	}
	if (q[0] != 11 || q[1] != 22 || q[2] != 33 || q[3] != 44) {
		return 2;
	}
	return 0;
}
