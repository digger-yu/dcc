int counter;

int main(void) {
	int old = 0;
	int newv = 1;
	int out;
	__asm__ volatile(
		"lock; cmpxchgl %2, %1\n"
		: "=a"(out), "+m"(counter)
		: "r"(newv), "a"(old)
		: "memory", "cc");
	return (out == 0 && counter == 1) ? 0 : 1;
}
