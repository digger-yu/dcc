// Comprehensive _Complex arithmetic test
int printf(const char *fmt, ...);

int main() {
    _Complex double a = 3.0 + 4.0i;
    _Complex double b = 1.0 + 2.0i;
    
    // Test 1: Multiplication with literal
    _Complex double c = a * 1.0i;
    double real = __real__ c;
    double imag = __imag__ c;
    printf("(3+4i) * 1.0i = %.1f + %.1fi (expected: -4.0 + 3.0i)\n", real, imag);
    if (real != -4.0 || imag != 3.0) {
        printf("FAIL: Test 1\n");
        return 1;
    }
    
    // Test 2: Multiplication of two complex numbers
    _Complex double d = a * b;
    real = __real__ d;
    imag = __imag__ d;
    printf("(3+4i) * (1+2i) = %.1f + %.1fi (expected: -5.0 + 10.0i)\n", real, imag);
    if (real != -5.0 || imag != 10.0) {
        printf("FAIL: Test 2\n");
        return 2;
    }
    
    // Test 3: Addition
    _Complex double e = a + b;
    real = __real__ e;
    imag = __imag__ e;
    printf("(3+4i) + (1+2i) = %.1f + %.1fi (expected: 4.0 + 6.0i)\n", real, imag);
    if (real != 4.0 || imag != 6.0) {
        printf("FAIL: Test 3\n");
        return 3;
    }
    
    // Test 4: Subtraction
    _Complex double f = a - b;
    real = __real__ f;
    imag = __imag__ f;
    printf("(3+4i) - (1+2i) = %.1f + %.1fi (expected: 2.0 + 2.0i)\n", real, imag);
    if (real != 2.0 || imag != 2.0) {
        printf("FAIL: Test 4\n");
        return 4;
    }
    
    // Test 5: Division
    _Complex double g = a / b;
    real = __real__ g;
    imag = __imag__ g;
    printf("(3+4i) / (1+2i) = %.1f + %.1fi (expected: 2.2 + -0.4i)\n", real, imag);
    // Allow small floating point errors
    if (real < 2.1 || real > 2.3 || imag > -0.3 || imag < -0.5) {
        printf("FAIL: Test 5\n");
        return 5;
    }
    
    // Test 6: Complex expression
    _Complex double h = (a + b) * (a - b);
    real = __real__ h;
    imag = __imag__ h;
    printf("((3+4i)+(1+2i)) * ((3+4i)-(1+2i)) = %.1f + %.1fi (expected: 12.0 + 16.0i)\n", real, imag);
    // (4+6i)(2+2i) = 8 + 8i + 12i + 12i^2 = 8 + 20i - 12 = -4 + 20i
    // Wait, let me recalculate: (4+6i)(2+2i) = 4*2 + 4*2i + 6i*2 + 6i*2i
    // = 8 + 8i + 12i + 12i^2 = 8 + 20i - 12 = -4 + 20i
    if (real != -4.0 || imag != 20.0) {
        printf("FAIL: Test 6\n");
        return 6;
    }
    
    printf("All tests PASSED!\n");
    return 0;
}
