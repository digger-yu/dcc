int add(int a, int b) {
    return a + b;
}

int sub(int a, int b) {
    return a - b;
}

int mul(int a, int b) {
    return a * b;
}

int global_var = 42;

int get_global(void) {
    return global_var;
}

int call_external(int x);

int use_external(int x) {
    return call_external(x) + 1;
}
