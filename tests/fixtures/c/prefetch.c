/* __builtin_prefetch 应为 void no-op，可编译链接 */
int g;

int main(void) {
	__builtin_prefetch(&g, 0, 3);
	__builtin_prefetch(&g, 1, 2);
	return g;
}
