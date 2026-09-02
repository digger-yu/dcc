// examples/dev/btf_test.c
// Test BTF generation: float types, global variables, DATASEC
// Compile with -g to emit .BTF and .BTF.ext sections

float pi = 3.14159f;
double e = 2.71828;
int counter;
static int internal_var = 42;

struct point {
    int x;
    int y;
};

struct point origin = {0, 0};

int add(int a, int b) {
    return a + b;
}

int main(void) {
    float area = pi * 2.0f;
    counter = add(counter, 1);
    return (int)area + internal_var;
}
