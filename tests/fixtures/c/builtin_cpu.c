/* Test __builtin_cpu_supports and __builtin_cpu_init */

int main(void) {
    /* Initialize CPU detection */
    __builtin_cpu_init();
    
    /* Check for various CPU features */
    /* These should compile and link without error */
    /* For now, they return 0 (not supported) in our simplified implementation */
    int has_sse = __builtin_cpu_supports("sse");
    int has_sse2 = __builtin_cpu_supports("sse2");
    int has_sse4_2 = __builtin_cpu_supports("sse4.2");
    int has_avx = __builtin_cpu_supports("avx");
    
    (void)has_sse;
    (void)has_sse2;
    (void)has_sse4_2;
    (void)has_avx;
    
    return 0;
}
