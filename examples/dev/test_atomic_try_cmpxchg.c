typedef struct { int counter; } atomic_t;

static inline _Bool arch_atomic_try_cmpxchg(atomic_t *v, int *old, int new)
{
    return ({
        _Bool success;
        __typeof__(((&v->counter))) _old = (__typeof__(((&v->counter))))(((old)));
        __typeof__(*(((&v->counter)))) __old = *_old;
        __typeof__(*(((&v->counter)))) __new = ((((new))));
        switch ((sizeof(*(&v->counter)))) {
        case 4: {
            volatile int *__ptr = (volatile int *)(((&v->counter)));
            asm volatile("" "cmpxchgl %[new], %[ptr]"
                         "\n\tset" "z" " %[_cc_z]\n"
                         : [_cc_z] "=qm" (success),
                           [ptr] "+m" (*__ptr),
                           [old] "+a" (__old)
                         : [new] "r" (__new)
                         : "memory");
            break;
        }
        case -1: {
            volatile long long *__ptr = (volatile long long *)(((&v->counter)));
            asm volatile("" "cmpxchgq %[new], %[ptr]"
                         : [_cc_z] "=qm" (success),
                           [ptr] "+m" (*__ptr),
                           [old] "+a" (__old)
                         : [new] "r" (__new)
                         : "memory");
            break;
        }
        default:
            break;
        }
        if (!success)
            *_old = __old;
        __builtin_expect(!!(success), 1);
    });
}

int main(void) { return 0; }
