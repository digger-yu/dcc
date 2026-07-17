typedef int v4si __attribute__((vector_size(16)));

int vec_add_loop(void) {
	v4si a = {1, 2, 3, 4};
	v4si b = {10, 20, 30, 40};
	v4si c = {0, 0, 0, 0};
	for (int i = 0; i < 4; i++)
		c[i] = a[i] + b[i];
	return c[0] == 11 && c[3] == 44 ? 0 : 1;
}

int main(void) {
	return vec_add_loop();
}
