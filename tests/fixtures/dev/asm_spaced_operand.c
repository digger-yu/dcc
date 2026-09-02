typedef struct { int counter; } atomic_t;

static inline _Bool arch_atomic_try_cmpxchg(atomic_t *v, int *old, int new)
{
	return ({
		_Bool success;
		switch (4) {
		case 4: {
			volatile int *__ptr = (volatile int *)(((&v->counter)));
			asm volatile("" "cmpxchgl %[new], %[ptr]"
				     : [ _cc_z ] "=qm" (success),
				       [ptr] "+m" (*__ptr)
				     : [new] "r" (new)
				     : "memory");
			break;
		}
		default:
			break;
		}
		success;
	});
}

int main(void) { return 0; }
