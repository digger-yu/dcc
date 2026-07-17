typedef int v4si __attribute__((vector_size(16)));

int use_rvalue(void) {
	v4si a = {1, 2, 3, 4};
	v4si b = a + (v4si){10, 20, 30, 40};
	int *p = (int *)&b;
	return p[0] + p[1] + p[2] + p[3];
}

int main(void) { return use_rvalue() == 110 ? 0 : 1; }
