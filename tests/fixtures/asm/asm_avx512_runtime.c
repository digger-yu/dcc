/* Test: SIMD/SSE + AVX-512 EVEX instruction end-to-end runtime
 * verification.
 *
 * SSE scalar section verifies addsd/mulsd/subsd/divsd execution.
 * EVEX section verifies that dcc's simplified EVEX encodings
 * (vaddps_z / vmulps_z / vmovdqu32_z) execute correctly across a
 * 16-element array. Results are checked via integer exit codes to
 * avoid float varargs promotion differences.
 */

/* ---- SSE scalar baseline ---- */

__attribute__((noinline))
double test_sse_addsd(double a, double b) {
    double result = a;
    asm volatile (
        "addsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

__attribute__((noinline))
double test_sse_mulsd(double a, double b) {
    double result = a;
    asm volatile (
        "mulsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

__attribute__((noinline))
double test_sse_subsd(double a, double b) {
    double result = a;
    asm volatile (
        "subsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

__attribute__((noinline))
double test_sse_divsd(double a, double b) {
    double result = a;
    asm volatile (
        "divsd %1, %0"
        : "+x" (result)
        : "x" (b)
    );
    return result;
}

/* ---- AVX-512 EVEX masked instructions ---- */

/* 16-element SIMD add via vaddps_z (512-bit EVEX 3-operand) */
__attribute__((noinline))
int test_evex_vaddps(const float *a, const float *b, float *out, int n) {
    for (int i = 0; i < n; i++) {
        float ai = a[i], bi = b[i];
        float r = ai;
        asm volatile (
            "vaddps_z %2, %0, %0\n\t"
            : "+x" (r)
            : "x" (ai), "x" (bi)
            : "zmm0"
        );
        out[i] = r;
    }
    return 0;
}

/* 16-element SIMD multiply via vmulps_z (512-bit EVEX 3-operand) */
__attribute__((noinline))
int test_evex_vmulps(const float *a, const float *b, float *out, int n) {
    for (int i = 0; i < n; i++) {
        float ai = a[i], bi = b[i];
        float r = ai;
        asm volatile (
            "vmulps_z %2, %0, %0\n\t"
            : "+x" (r)
            : "x" (ai), "x" (bi)
            : "zmm0"
        );
        out[i] = r;
    }
    return 0;
}

/* Chained EVEX ops in one asm block: t = (a + b) * c */
__attribute__((noinline))
float test_evex_chain(float a, float b, float c) {
    float t = a;
    asm volatile (
        "vaddps_z %2, %0, %0\n\t"
        "vmulps_z %3, %0, %0\n\t"
        : "+x" (t)
        : "x" (a), "x" (b), "x" (c)
        : "zmm0"
    );
    return t;
}

/* EVEX 2-operand register move: r = s via vmovdqu32_z */
__attribute__((noinline))
float test_evex_vmovdqu32(float s) {
    float r = 0.0f;
    asm volatile (
        "vmovdqu32_z %1, %0\n\t"
        : "+x" (r)
        : "x" (s)
        : "zmm0"
    );
    return r;
}

/* ---- CPU feature detection ---- */

/* Check if AVX-512F is supported via CPUID */
static int has_avx512f(void) {
    unsigned int eax, ebx, ecx, edx;
    /* CPUID leaf 7, sub-leaf 0: EBX bit 16 = AVX-512F */
    __asm__ volatile ("cpuid"
        : "=a"(eax), "=b"(ebx), "=c"(ecx), "=d"(edx)
        : "a"(7), "c"(0));
    return (ebx >> 16) & 1;
}

int main(void) {
    /* Skip gracefully if CPU lacks AVX-512 */
    if (!has_avx512f()) return 0;

    /* SSE scalar tests */
    if (test_sse_addsd(3.0, 4.0) != 7.0) return 1;
    if (test_sse_mulsd(3.0, 4.0) != 12.0) return 2;
    if (test_sse_subsd(10.0, 3.5) != 6.5) return 3;
    if (test_sse_divsd(12.0, 4.0) != 3.0) return 4;

    /* EVEX 16-element batch add: out[i] = a[i] + b[i] */
    static float a[16] = {1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16};
    static float b[16] = {1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16};
    static float out[16];
    int ok = 1;

    if (test_evex_vaddps(a, b, out, 16) != 0) return 5;
    for (int i = 0; i < 16; i++) {
        float expect = a[i] + b[i];
        if (out[i] != expect) { ok = 0; break; }
    }
    if (!ok) return 6;

    /* EVEX 16-element batch multiply: out[i] = a[i] * b[i] */
    if (test_evex_vmulps(a, b, out, 16) != 0) return 7;
    for (int i = 0; i < 16; i++) {
        float expect = a[i] * b[i];
        if (out[i] != expect) { ok = 0; break; }
    }
    if (!ok) return 8;

    /* EVEX chained ops: (2 + 3) * 4 = 20 */
    if (test_evex_chain(2.0f, 3.0f, 4.0f) != 20.0f) return 9;

    /* EVEX 2-operand move: 42.0 */
    if (test_evex_vmovdqu32(42.0f) != 42.0f) return 10;

    return 0;
}
