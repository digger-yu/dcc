// Test COMDAT group generation for C++ inline functions
#ifdef __cplusplus

// Inline function should generate a COMDAT group
inline int add(int a, int b) {
    return a + b;
}

// Another inline function
inline int multiply(int a, int b) {
    return a * b;
}

// Template function (also inline by nature)
template<typename T>
T square(T x) {
    return x * x;
}

int main() {
    int sum = add(3, 4);
    int prod = multiply(5, 6);
    int sq = square(7);
    return sum + prod + sq;
}

#else
// C version - just test that compilation works
int main() {
    return 0;
}
#endif
