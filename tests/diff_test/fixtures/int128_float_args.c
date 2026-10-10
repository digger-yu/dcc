typedef __int128 int128_t;
typedef unsigned __int128 uint128_t;

// Test complex parameter passing with int128 and intermediate float args
int128_t test_with_floats(int128_t a, float b, double c, int128_t d) {
    return a + d;
}

int128_t nested_call(float base) {
    int128_t x = (int128_t)(base * 100);
    int128_t y = (int128_t)(base * 200);
    return test_with_floats(x, base, base, y);
}

int main() {
    int128_t result = nested_call(1.0f);
    
    // Expected: 100 + 200 = 300
    if (result != 300) {
        return 1;
    }
    
    // Additional test: [float, int128, int128, float]
    int128_t another(int128_t a, float f1, int128_t b, float f2) {
        return a + b;
    }
    
    int128_t r2 = another(50, 1.0f, 75, 2.0f);
    if (r2 != 125) {
        return 2;
    }
    
    return 0;
}
