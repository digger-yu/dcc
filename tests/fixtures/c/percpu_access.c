/* Test: Per-CPU variable support via GS segment register
 *
 * This test verifies:
 * 1. __percpu type qualifier parsing
 * 2. DEFINE_PER_CPU / DECLARE_PER_CPU macros
 * 3. __seg_gs/__seg_fs pointer type parsing
 * 4. Inline asm with %%gs: segment prefix (encode-only)
 */

/* Test DEFINE_PER_CPU macro expansion */
DEFINE_PER_CPU(int, percpu_counter);
DEFINE_PER_CPU_SHARED_ALIGNED(long, percpu_data);

/* Test DECLARE_PER_CPU macro expansion */
DECLARE_PER_CPU(char, percpu_flag);

/* Test __percpu qualifier */
int __percpu my_percpu_var;

/* Test __seg_gs pointer type */
int test_seg_gs_pointer(void) {
    int __seg_gs *p;
    p = (int __seg_gs *)0;
    return p == 0;
}

/* Test __seg_fs pointer type (for TLS) */
int test_seg_fs_pointer(void) {
    int __seg_fs *p;
    p = (int __seg_fs *)0;
    return p == 0;
}

/* Test per-cpu variable address taking */
int test_percpu_addr(void) {
    void *addr = (void *)&percpu_counter;
    return addr != 0;
}

/* Test multiple per-cpu variables */
DEFINE_PER_CPU(unsigned long, percpu_a);
DEFINE_PER_CPU(unsigned long, percpu_b);
DEFINE_PER_CPU(unsigned long, percpu_c);

int test_multiple_percpu(void) {
    unsigned long *a = &percpu_a;
    unsigned long *b = &percpu_b;
    unsigned long *c = &percpu_c;
    return (a != b) && (b != c) && (a != c);
}

/* Encode-only test for __this_cpu_read pattern.
 * Real execution requires %gs to be set up by kernel. */
void encode_this_cpu_read(void) {
    int val;
    asm volatile("mov %%gs:%1, %0" : "=r"(val) : "m"(percpu_counter));
}

/* Encode-only test for __this_cpu_write pattern */
void encode_this_cpu_write(void) {
    int new_val = 42;
    asm volatile("mov %0, %%gs:%1" : : "r"(new_val), "m"(percpu_counter) : "memory");
}

/* Encode-only test for __this_cpu_add pattern */
void encode_this_cpu_add(void) {
    int inc = 1;
    asm volatile("add %0, %%gs:%1" : : "r"(inc), "m"(percpu_counter) : "memory");
}

/* Encode-only test for __this_cpu_inc pattern */
void encode_this_cpu_inc(void) {
    asm volatile("incl %%gs:%0" : : "m"(percpu_counter) : "memory");
}

int main(void) {
    int ok = 1;
    
    ok &= test_seg_gs_pointer();
    ok &= test_seg_fs_pointer();
    ok &= test_percpu_addr();
    ok &= test_multiple_percpu();
    
    /* Reference encode-only functions to prevent optimization */
    encode_this_cpu_read();
    encode_this_cpu_write();
    encode_this_cpu_add();
    encode_this_cpu_inc();
    
    return ok ? 0 : 1;
}
