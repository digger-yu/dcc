// Test fixture for const/volatile/restrict optimization utilization
const int const_global = 42;
volatile int volatile_global = 0;

// This function attempts to modify a const global (would be caught by
// const-aware optimization pass if diagnostic infrastructure was integrated)
void attempt_const_modify(void) {
    // const_global = 100;  // This would be a constraint violation
}

// Volatile global access should not be optimized away
int read_volatile(void) {
    return volatile_global;
}

// Restrict qualifier on pointer parameters indicates non-aliasing
int add_with_restrict(int * restrict a, int * restrict b) {
    *a = 10;
    *b = 20;
    return *a + *b;  // Compiler can assume a and b don't alias
}
