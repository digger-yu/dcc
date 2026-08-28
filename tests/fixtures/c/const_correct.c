/* Test: C89 const correctness - reject assignment to const variables */

/* This test should produce errors for const violations */

int main(void) {
    const int x = 10;
    int y = 5;

    /* OK: reading const variable */
    y = x;

    /* OK: assigning to non-const */
    y = 20;
    y += 3;

    return y;
}
