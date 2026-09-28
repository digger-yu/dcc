typedef struct { int counter; } atomic_t;

static inline int arch_atomic_read(atomic_t *v) { return v->counter; }

static inline bool arch_atomic_try_cmpxchg(atomic_t *v, int *old, int new)
{
    return ({
        bool success;
        volatile int *__ptr = &v->counter;
        int __old = *old;
        int __new = new;
        asm volatile("" "cmpxchgl %[new], %[ptr]"
                     "\n\tset" "z" " %[_cc_z]\n"
                     : [_cc_z] "=qm" (success),
                       [ptr] "+m" (*__ptr),
                       [old] "+a" (__old)
                     : [new] "r" (__new)
                     : "memory");
        if (!success)
            *old = __old;
        success;
    });
}

int main(void) { return 0; }
