// Test -mno-sse mode: kernel interrupt context must not use XMM registers
// This file tests that floating-point operations use x87 FPU instead of SSE

// Simple float arithmetic
double add_double(double a, double b) {
    return a + b;
}

double sub_double(double a, double b) {
    return a - b;
}

double mul_double(double a, double b) {
    return a * b;
}

double div_double(double a, double b) {
    return a / b;
}

// Float versions
float add_float(float a, float b) {
    return a + b;
}

float sub_float(float a, float b) {
    return a - b;
}

// Float comparison
int compare_double(double a, double b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return 2;
}

// Float literal
double get_pi(void) {
    return 3.14159265358979;
}

// Float variable
double compute_area(double radius) {
    double pi = 3.14159265358979;
    return pi * radius * radius;
}

int main(void) {
    double x = 10.0;
    double y = 3.0;
    
    double sum = add_double(x, y);
    double diff = sub_double(x, y);
    double prod = mul_double(x, y);
    double quot = div_double(x, y);
    
    int cmp = compare_double(x, y);
    double area = compute_area(5.0);
    
    return (int)sum + (int)diff + (int)prod + (int)quot + cmp + (int)area;
}
