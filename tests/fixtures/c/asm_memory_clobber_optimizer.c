/* Test: memory clobber prevents optimizer reordering
 * Verifies that under -O2, memory clobber acts as a compiler barrier.
 */

static volatile int shared_var = 0;

__attribute__((noinline))
int test_memory_clobber_barrier(void) {
    /* Write to shared_var */
    shared_var = 10;

    /* Memory clobber: compiler barrier */
    asm volatile (
        "nop"
        :
        :
        : "memory"
    );

    /* Read shared_var after barrier */
    int c = shared_var;

    /* c must be 10 */
    return c;
}

__attribute__((noinline))
int test_memory_clobber_nop(void) {
    /* Simple test: just verify asm volatile with memory clobber compiles and runs */
    int x = 42;
    asm volatile ("nop" ::: "memory");
    return x;  /* should be 42 */
}

int main(void) {
    int r1 = test_memory_clobber_barrier();
    if (r1 != 10) return 1;

    int r2 = test_memory_clobber_nop();
    if (r2 != 42) return 2;

    return 0;
}
