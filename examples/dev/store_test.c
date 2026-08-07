// Test *p = v store operation
int printf(const char *fmt, ...);

int main() {
    int x = 10;
    int *p = &x;
    
    printf("Before: x = %d\n", x);
    *p = 42;  // This store should work
    printf("After: x = %d\n", x);
    
    if (x != 42) {
        printf("FAIL: store did not work, x = %d\n", x);
        return 1;
    }
    
    printf("PASS: store worked\n");
    return 0;
}
