// Test _Complex literal multiplication
int printf(const char *fmt, ...);

int main() {
    _Complex double imag = 0.0 + 1.0i;
    
    // Test imag * 1.0i
    _Complex double result = imag * 1.0i;
    double real = __real__ result;
    double imag_part = __imag__ result;
    printf("imag * 1.0i = %.1f + %.1fi\n", real, imag_part);
    // Expected: (0+1i)(0+1i) = 0 + 0i + 0i + 1i^2 = -1 + 0i
    
    // Test with variable
    _Complex double a = 2.0 + 3.0i;
    _Complex double b = a * 1.0i;
    real = __real__ b;
    imag_part = __imag__ b;
    printf("(2+3i) * 1.0i = %.1f + %.1fi\n", real, imag_part);
    // Expected: (2+3i)(0+1i) = 0 + 2i + 0i + 3i^2 = -3 + 2i
    
    return 0;
}
