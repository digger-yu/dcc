int main(void) {
    int *old;
    int *v;
    return ({
        int success;
        __typeof__(((&v->counter))) _old = (__typeof__(((&v->counter))))(((old)));
        int __old = *_old;
        int __new = 0;
        volatile int *__ptr = (volatile int *)0;
        switch ((sizeof(*(&v->counter)))) {
        case 4: {
            asm volatile("" "cmpxchgl %[new], %[ptr]"
                         : [ptr] "+m" (*__ptr)
                         : [new] "r" (__new)
                         : "memory");
            break;
        }
        case -1: {
            break;
        }
        default:
            break;
        }
        success;
    });
}
