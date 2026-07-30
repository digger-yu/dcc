/* m3_1: inline assembly - asm volatile, constraints, clobbers */
#include <stddef.h>

/* basic asm volatile with output/input register constraints */
static int add_asm(int a, int b) {
    int result;
    asm volatile (
        "addl %2, %0"
        : "=r" (result)
        : "0" (a), "r" (b)
    );
    return result;
}

/* asm with immediate constraint */
static int shift_left(int x) {
    int result;
    asm volatile (
        "shl $3, %0"
        : "=r" (result)
        : "0" (x)
    );
    return result;
}

/* memory clobber */
static void mem_barrier(void) {
    asm volatile ("" ::: "memory");
}

/* asm with multiple outputs - simple swap via registers */
static void swap_vals(int a, int b, int *out_a, int *out_b) {
    int ra, rb;
    asm volatile (
        "movl %2, %0\n\t"
        "movl %3, %1"
        : "=r" (ra), "=r" (rb)
        : "r" (b), "r" (a)
    );
    *out_a = ra;
    *out_b = rb;
}

/* asm with "i" immediate constraint */
static int rd_const(void) {
    int v;
    asm volatile (
        "movl $42, %0"
        : "=r" (v)
    );
    return v;
}

/* clobber list with named register */
static long clobber_test(void) {
    long v;
    asm volatile (
        "mov $99, %%rax"
        : "=a" (v)
        :
        : "cc"
    );
    return v;
}

int main(void) {
    int fail = 0;

    /* add_asm: 3 + 7 = 10 */
    if (add_asm(3, 7) != 10) fail |= 1;

    /* shift_left: 5 << 3 = 40 */
    if (shift_left(5) != 40) fail |= 2;

    /* memory barrier: just should not crash */
    mem_barrier();

    /* swap via asm: swap(3, 7) → (7, 3) */
    int sa, sb;
    swap_vals(3, 7, &sa, &sb);
    if (sa != 7 || sb != 3) fail |= 4;

    /* rd_const */
    if (rd_const() != 42) fail |= 8;

    /* clobber_test */
    if (clobber_test() != 99) fail |= 16;

    return fail;
}
