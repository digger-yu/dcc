/* Test: C89 void* implicit conversion to/from any pointer type */
/* C89 3.3.16, C99 6.3.2.3: void* is compatible with any object pointer */

void *my_alloc(unsigned long n);
void my_free(void *p);

/* void* -> int* implicit conversion */
int *test_void_to_int(void *p) {
    int *ip = p;
    return ip;
}

/* any pointer -> void* implicit conversion */
void *test_int_to_void(int *p) {
    void *v = p;
    return v;
}

/* void* in function arguments */
void *test_func_arg(void) {
    int x = 42;
    int *ip = &x;
    my_free(ip);  /* int* -> void* parameter */
    return my_alloc(100);  /* void* return -> void* */
}

/* void* assignment both directions */
int test_assignments(void) {
    void *vp;
    int *ip;
    char *cp;
    struct { int a; } *sp;

    vp = ip;  /* int* -> void* */
    vp = cp;  /* char* -> void* */
    ip = vp;  /* void* -> int* */
    cp = vp;  /* void* -> char* */
    sp = vp;  /* void* -> struct ptr */
    return 0;
}

int main(void) {
    return 0;
}
