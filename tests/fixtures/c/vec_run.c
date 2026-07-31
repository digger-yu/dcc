typedef int v4si __attribute__((vector_size(16)));

v4si g_vec = {1, 2, 3, 4};
v4si inc = {10, 20, 30, 40};

int use_global(void) {
	v4si a = g_vec;
	v4si b = a + inc;
	int *p = (int *)&b;
	return p[0] + p[1] + p[2] + p[3];
}

int main(void) { return use_global() == 110 ? 0 : 1; }
