int sum4(const int *p) {
	int s = 0;
	for (int i = 0; i < 4; i++)
		s += p[i];
	return s;
}

int main(void) {
	int a[4] = {1, 2, 3, 4};
	return sum4(a) == 10 ? 0 : 1;
}
