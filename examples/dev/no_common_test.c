/* Test: -fno-common (tentative definitions) */

/* Tentative definition: with -fno-common, goes directly to .bss */
int global_var;

/* Another tentative definition */
int another_global;

/* Initialized global: goes to .data */
int initialized_global = 42;

int get_global(void) {
    return global_var;
}

void set_global(int x) {
    global_var = x;
}
