/* Test: per_cpu_offset() compiler-level support
 *
 * per_cpu_offset(cpu) is provided by the kernel (asm/percpu.h).
 * The compiler only needs to:
 * 1. Support the per_cpu(var, cpu) macro which calls per_cpu_offset(cpu)
 * 2. Support __seg_gs pointer dereference for per-CPU access
 * 3. Not interfere with the kernel's per_cpu_offset() definition
 *
 * This test verifies that the compiler correctly handles the
 * per_cpu_offset() usage pattern without requiring any special
 * compiler-side implementation.
 */

/* Simulate kernel-provided per_cpu_offset() */
extern unsigned long __per_cpu_offset[4];
unsigned long per_cpu_offset(unsigned int cpu) {
    return __per_cpu_offset[cpu];
}
unsigned long __per_cpu_offset[4] = {0, 0x40, 0x80, 0xC0};

/* Per-CPU variables */
DEFINE_PER_CPU(int, pcpu_val);
DEFINE_PER_CPU(unsigned long, pcpu_count);

/* Test: direct per_cpu_offset() usage */
unsigned long test_offset(void) {
    return per_cpu_offset(0);
}

/* Test: per_cpu() macro uses per_cpu_offset() internally */
int test_per_cpu_via_offset(void) {
    return per_cpu(pcpu_val, 1);
}

/* Test: write through per_cpu() with offset calculation */
void test_per_cpu_write(unsigned int cpu) {
    per_cpu(pcpu_count, cpu) = 42;
}

/* Test: multiple per_cpu_offset() calls in expression */
unsigned long test_offset_diff(void) {
    return per_cpu_offset(2) - per_cpu_offset(1);
}

int main(void) {
    unsigned long off0 = test_offset();
    int val = test_per_cpu_via_offset();
    (void)val;
    unsigned long diff = test_offset_diff();
    return (off0 == 0 && diff == 0x40) ? 0 : 1;
}
