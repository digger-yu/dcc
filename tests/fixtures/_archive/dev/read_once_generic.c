typedef struct { long long counter; } atomic64_t;

static inline long long arch_atomic64_read(const atomic64_t *v)
{
	return (*(const volatile typeof(
		_Generic(((v)->counter),
			 char: (char)0,
			 unsigned char: (unsigned char)0,
			 signed char: (signed char)0,
			 unsigned short: (unsigned short)0,
			 signed short: (signed short)0,
			 unsigned int: (unsigned int)0,
			 signed int: (signed int)0,
			 unsigned long: (unsigned long)0,
			 signed long: (signed long)0,
			 unsigned long long: (unsigned long long)0,
			 signed long long: (signed long long)0,
			 default: ((v)->counter))) *)&((v)->counter));
}

static inline void arch_atomic64_set(atomic64_t *v, long long i)
{
	do {
		*(volatile typeof(v->counter) *)&(v->counter) = (i);
	} while (0);
}

int main(void) { return 0; }
