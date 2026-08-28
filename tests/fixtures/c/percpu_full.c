/* Test: Per-CPU variable address resolution - complete support
 *
 * This test verifies:
 * 1. __seg_gs pointer type in cast expressions (parser propagation)
 * 2. __seg_gs pointer dereference generates %gs: segment prefix (codegen)
 * 3. __get_cpu_var macro expands to %gs:-prefixed access
 * 4. per_cpu(var, cpu) macro uses cpu parameter via per_cpu_offset()
 * 5. __this_cpu_read/write/add builtin functions
 * 6. __this_cpu_inc/dec builtin patterns
 *
 * Encode-only tests: verify compilation succeeds.
 * Runtime tests: verify non-percpu logic works (segment prefix
 * execution requires kernel %gs setup).
 */

/* === Per-CPU variable declarations === */
DEFINE_PER_CPU(int, pcpu_counter);
DEFINE_PER_CPU(long, pcpu_data);
DECLARE_PER_CPU(char, pcpu_flag);

/* === Test 1: __seg_gs pointer type in cast === */
/* Verify that (__seg_gs type *) creates a GS-qualified pointer */
int test_seg_gs_cast(void) {
    int x = 42;
    /* Cast address to __seg_gs pointer - parser must propagate
     * __seg_gs into Ptr QualType addr_space */
    int __seg_gs *gp = (int __seg_gs *)&x;
    (void)gp;
    return 1;
}

/* === Test 2: __seg_gs pointer dereference (encode-only) === */
/* Dereferencing a __seg_gs pointer must emit %gs: prefix */
int encode_seg_gs_deref_load(void) {
    int __seg_gs *gp = (int __seg_gs *)&pcpu_counter;
    /* This load should generate: mov %gs:(%rax), %eax */
    int val = *gp;
    return val;
}

int encode_seg_gs_deref_store(void) {
    int __seg_gs *gp = (int __seg_gs *)&pcpu_counter;
    /* This store should generate: mov %eax, %gs:(%rax) */
    *gp = 99;
    return 1;
}

/* === Test 3: __get_cpu_var macro === */
/* Should expand to cast through __seg_gs pointer and dereference */
int encode_get_cpu_var(void) {
    /* __get_cpu_var(pcpu_counter) should expand to:
     * (*((__typeof__(pcpu_counter) __seg_gs *)(&(pcpu_counter))))
     * which generates a %gs:-prefixed load */
    int val = __get_cpu_var(pcpu_counter);
    return val;
}

int encode_get_cpu_var_write(void) {
    __get_cpu_var(pcpu_counter) = 42;
    return 1;
}

/* === Test 4: per_cpu(var, cpu) macro === */
/* Should use per_cpu_offset(cpu) to access correct CPU's copy */
/* Stub: in the real kernel, per_cpu_offset is provided by
 * arch/x86/include/asm/percpu.h. For test purposes, we provide
 * a minimal implementation. */
extern unsigned long __per_cpu_offset_arr[4];
unsigned long per_cpu_offset(unsigned int cpu) {
    return __per_cpu_offset_arr[cpu];
}
unsigned long __per_cpu_offset_arr[4] = {0, 64, 128, 192};

int encode_per_cpu(void) {
    /* per_cpu(pcpu_counter, 1) should expand to use per_cpu_offset(1) */
    int val = per_cpu(pcpu_counter, 1);
    return val;
}

int encode_per_cpu_write(void) {
    per_cpu(pcpu_counter, 2) = 100;
    return 1;
}

/* === Test 5: __this_cpu_read/write/add builtins === */
int encode_builtin_this_cpu_read(void) {
    int val = __builtin_dcc_this_cpu_read(pcpu_counter);
    return val;
}

int encode_builtin_this_cpu_write(void) {
    __builtin_dcc_this_cpu_write(pcpu_counter, 42);
    return 1;
}

int encode_builtin_this_cpu_add(void) {
    __builtin_dcc_this_cpu_add(pcpu_counter, 1);
    return 1;
}

/* === Test 6: __this_cpu_inc/dec macros (inline asm) === */
int encode_this_cpu_inc(void) {
    __this_cpu_inc(pcpu_counter);
    return 1;
}

int encode_this_cpu_dec(void) {
    __this_cpu_dec(pcpu_counter);
    return 1;
}

/* === Test 7: __seg_fs pointer (TLS) === */
int encode_seg_fs_deref(void) {
    int __seg_fs *fp = (int __seg_fs *)0;
    int val = *fp;
    return val;
}

/* === Test 8: Multiple types === */
DEFINE_PER_CPU(unsigned long, pcpu_ulong);
DEFINE_PER_CPU(short, pcpu_short);
DEFINE_PER_CPU(char, pcpu_char);

int encode_multi_type_read(void) {
    unsigned long a = __get_cpu_var(pcpu_ulong);
    short b = __get_cpu_var(pcpu_short);
    char c = __get_cpu_var(pcpu_char);
    return (int)(a + b + c);
}

int main(void) {
    int ok = 1;

    ok &= test_seg_gs_cast();

    /* Encode-only functions: just verify they compile */
    (void)encode_seg_gs_deref_load;
    (void)encode_seg_gs_deref_store;
    (void)encode_get_cpu_var;
    (void)encode_get_cpu_var_write;
    (void)encode_per_cpu;
    (void)encode_per_cpu_write;
    (void)encode_builtin_this_cpu_read;
    (void)encode_builtin_this_cpu_write;
    (void)encode_builtin_this_cpu_add;
    (void)encode_this_cpu_inc;
    (void)encode_this_cpu_dec;
    (void)encode_seg_fs_deref;
    (void)encode_multi_type_read;

    return ok ? 0 : 1;
}
