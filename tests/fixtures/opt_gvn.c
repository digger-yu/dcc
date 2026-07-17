int dup_add(int x, int y) {
	int a = x + y;
	int b = x + y;
	return a + b;
}

int main(void) {
	return dup_add(3, 4) == 14 ? 0 : 1;
}
