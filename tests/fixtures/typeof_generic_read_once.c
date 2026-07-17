unsigned long read_once(const unsigned long *addr)
{
	return *(const volatile typeof(
		_Generic(*(unsigned long *)addr,
			char: (char)0,
			unsigned char: (unsigned char)0,
			unsigned long: (unsigned long)0,
			default: *(unsigned long *)addr)
	) *)&(*addr);
}

int main(void) {
	unsigned long x = 42;
	return read_once(&x) == 42 ? 0 : 1;
}
