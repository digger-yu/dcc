/* C11 _Noreturn semantics test */
#include <stdio.h>
#include <stdlib.h>

_Noreturn void die(const char *msg) {
    fprintf(stderr, "FATAL: %s\n", msg);
    exit(1);
}

_Noreturn void infinite_loop(void) {
    while (1) {
        /* never returns */
    }
}

int main(void) {
    printf("Testing _Noreturn...\n");
    
    /* This should compile without warnings about missing return */
    if (0) {
        die("should not happen");
    }
    
    printf("PASS\n");
    return 0;
}
