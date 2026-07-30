/* M1.4: ELF object file format verification */
#include <stdio.h>

static int static_var = 42;
int global_var = 100;

static void static_func(void) {
    printf("static\n");
}

void global_func(void) {
    static_func();
}

int main(void) {
    global_func();
    return static_var + global_var;
}
