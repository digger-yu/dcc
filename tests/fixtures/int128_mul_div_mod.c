/* __int128 乘/除/取模运行时验证 */
int main(void) {
	__int128 a = ((__int128)1 << 100) + 17;
	__int128 b = 7;
	__int128 prod = a * b;
	__int128 quot = a / b;
	__int128 rem = a % b;
	if (prod == 0)
		return 1;
	if (quot == 0)
		return 2;
	if (rem < 0 || rem >= b)
		return 3;
	if (quot * b + rem != a)
		return 4;
	return 0;
}
