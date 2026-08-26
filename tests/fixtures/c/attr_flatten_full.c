// __attribute__((flatten)) — all calls within the function should
// be inlined regardless of optimization level or function size.
// Test: compile at various -O levels, verify correct result.

static int add_one(int x) {
    return x + 1;
}

static int double_it(int x) {
    return x * 2;
}

static int compute(int a, int b) {
    return add_one(a) + double_it(b);
}

// flatten: all calls (compute, add_one) should be inlined
__attribute__((flatten))
int outer(int x, int y) {
    return compute(x, y) + add_one(y);
}

int main(void) {
    int r1 = outer(10, 5);    // compute(10,5) + add_one(5) = (11+10) + 6 = 27
    if (r1 != 27) return 1;
    return 0;
}
