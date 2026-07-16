int main(void) {
    return ({
        int success;
        int __ptr;
        int __old;
        int __new;
        switch (4) {
        case 4: {
            asm volatile("" "cmpxchgl %[new], %[ptr]"
                         "\n\tset" "z" " %[_cc_z]\n"
                         : [_cc_z] "=qm" (success),
                           [ptr] "+m" (__ptr),
                           [old] "+a" (__old)
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
