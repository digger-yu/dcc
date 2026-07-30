int main(void) {
	_Complex float a = 3.0f;
	_Complex float b = 1.0f;
	/* (3+0i)/(1+0i) == 3 */
	return (int)(a / b) == 3 ? 0 : 1;
}
