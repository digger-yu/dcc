// LTO test: file 1 - defines a function
int add(int a, int b) {
    return a + b;
}

static int helper(int x) {
    return x * 2;
}

int multiply(int a, int b) {
    return helper(a) * b;
}
