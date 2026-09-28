/* Test: segment register prefix (%gs:/%fs:) in inline asm
 * + asm operand modifiers (%b/%w/%k/%q/%h).
 *
 * Segment prefix tests verify correct encoding (compile-only).
 * Modifier tests verify runtime correctness.
 */
#include <stdio.h>

/* Global variables for segment prefix encoding tests */
static long gs_val = 0x100;
static long fs_val = 0x200;
static int gs_counter = 10;

/* === Segment prefix encode-only tests === */
int encode_gs_mov_load(void) {
    long r = 0;
    asm volatile("mov %%gs:%1, %0" : "=r"(r) : "m"(gs_val));
    return 1;
}
int encode_fs_mov_store(void) {
    long r = 42;
    asm volatile("mov %0, %%fs:%1" : : "r"(r), "m"(fs_val) : "memory");
    return 1;
}
int encode_gs_alu_add(void) {
    long r = 5;
    asm volatile("add %%gs:%1, %0" : "+r"(r) : "m"(gs_val));
    return 1;
}
int encode_gs_alu_or(void) {
    long r = 0;
    asm volatile("or %%gs:%1, %0" : "+r"(r) : "m"(gs_val));
    return 1;
}
int encode_gs_alu_xor(void) {
    long r = 0xFF;
    asm volatile("xor %%gs:%1, %0" : "+r"(r) : "m"(gs_val));
    return 1;
}
int encode_gs_alu_and(void) {
    long r = 0xFF;
    asm volatile("and %%gs:%1, %0" : "+r"(r) : "m"(gs_val));
    return 1;
}
int encode_gs_lock_add(void) {
    asm volatile("lock addl $1, %%gs:%0" : "+m"(gs_counter) : : "memory");
    return 1;
}
int encode_gs_lock_inc(void) {
    asm volatile("lock incl %%gs:%0" : "+m"(gs_counter) : : "memory");
    return 1;
}
int encode_gs_lock_cmpxchg(void) {
    int old = 10, new_val = 20;
    asm volatile("lock cmpxchgl %2, %%gs:%1"
        : "=a"(old), "+m"(gs_counter)
        : "r"(new_val), "0"(old) : "memory");
    return 1;
}
int encode_multi_seg_ops(void) {
    long r1 = 0, r2 = 0;
    asm volatile(
        "mov %%gs:%2, %0\n\t"
        "mov %%fs:%3, %1\n\t"
        "add %1, %0"
        : "=r"(r1), "=r"(r2)
        : "m"(gs_val), "m"(fs_val));
    return 1;
}

/* === Runtime modifier tests === */

/* %b modifier: 8-bit sub-register */
int test_b_modifier(void) {
    int val = 42, result = 0;
    asm("mov %b1, %b0" : "=r"(result) : "r"(val));
    return (result & 0xFF) == 42 ? 1 : 0;
}

/* %q modifier forcing 64-bit register */
int test_q_modifier(void) {
    long val = 0x123456789ABCDEF0L, result = 0;
    asm("mov %q1, %q0" : "=r"(result) : "r"(val), "0"(result));
    return result == val ? 1 : 0;
}

/* %w modifier with 16-bit arithmetic */
int test_w_arith(void) {
    int a = 0x100, b = 0x200;
    asm("add %w2, %w0" : "+r"(a) : "0"(a), "r"(b));
    return (a & 0xFFFF) == 0x300 ? 1 : 0;
}

/* %k modifier with 32-bit move */
int test_k_modifier(void) {
    int val = 99, result = 0;
    asm("mov %k1, %k0" : "=r"(result) : "r"(val), "0"(result));
    return result == 99 ? 1 : 0;
}

int main(void) {
    int ok = 1;
    ok &= test_b_modifier();
    ok &= test_q_modifier();
    ok &= test_w_arith();
    ok &= test_k_modifier();
    /* Reference encode-only functions to ensure compilation */
    (void)encode_gs_mov_load;
    (void)encode_fs_mov_store;
    (void)encode_gs_alu_add;
    (void)encode_gs_alu_or;
    (void)encode_gs_alu_xor;
    (void)encode_gs_alu_and;
    (void)encode_gs_lock_add;
    (void)encode_gs_lock_inc;
    (void)encode_gs_lock_cmpxchg;
    (void)encode_multi_seg_ops;
    if (ok)
        printf("PASS: seg prefix + asm modifiers\n");
    return ok ? 0 : 1;
}
