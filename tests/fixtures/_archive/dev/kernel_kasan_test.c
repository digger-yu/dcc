// Test kernel-address sanitizer
int printf(const char *fmt, ...);

int main() {
    int x = 42;
    int *p = &x;
    
    // This should trigger KASAN check if shadow is poisoned
    int y = *p;
    printf("y = %d\n", y);
    
    return 0;
}
