int main(void) {
	_Complex float a;
	_Complex float b = 1.0f;
	__real__ a = 3.0f;
	__imag__ a = 4.0f;
	/* (3+4i)/(1+0i) == 3+4i. */
	if (__real__ (a / b) != 3.0f) return 1;
	if (__imag__ (a / b) != 4.0f) return 2;
	return 0;
}
