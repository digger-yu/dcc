/* Test: C89 const correctness - assignment to const should be rejected */
int main(void) {
    const int x = 10;
    x = 20;
    return 0;
}
