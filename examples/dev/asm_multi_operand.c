int main(void) {
    int success;
    int __ptr;
    int __old;
    int __new;
    asm volatile("" "x"
                 : [cc] "=qm" (success),
                   [ptr] "+m" (__ptr),
                   [old] "+a" (__old)
                 : [new] "r" (__new)
                 : "memory");
    return success;
}
