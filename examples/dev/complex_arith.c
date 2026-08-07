// Test _Complex arithmetic operations
int printf(const char *fmt, ...);

int main() {
    _Complex double a = 3.0 + 4.0i;
    _Complex double b = 1.0 + 2.0i;
    
    // Test multiplication
    _Complex double c = a * b;
    double real = __real__ c;
    double imag = __imag__ c;
    printf("a * b = %.1f + %.1fi\n", real, imag);
    // Expected: (3+4i)(1+2i) = 3 + 6i + 4i + 8i^2 = 3 + 10i - 8 = -5 + 10i
    
    // Test addition
    _Complex double d = a + b;
    real = __real__ d;
    imag = __imag__ d;
    printf("a + b = %.1f + %.1fi\n", real, imag);
    // Expected: (3+4i) + (1+2i) = 4 + 6i
    
    // Test subtraction
    _Complex double e = a - b;
    real = __real__ e;
    imag = __imag__ e;
    printf("a - b = %.1f + %.1fi\n", real, imag);
    // Expected: (3+4i) - (1+2i) = 2 + 2i
    
    // Test division
    _Complex double f = a / b;
    real = __real__ f;
    imag = __imag__ f;
    printf("a / b = %.1f + %.1fi\n", real, imag);
    // Expected: (3+4i)/(1+2i) = (3+4i)(1-2i)/((1+2i)(1-2i)) = (3-6i+4i-8i^2)/(1+4) = (11-2i)/5 = 2.2 - 0.4i
    
    return 0;
}
