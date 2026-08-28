/* Test: TLS model codegen support
 * Verifies that __attribute__((tls_model("model"))) is respected
 * in code generation. The kernel uses tls_model("local-exec") for
 * per-cpu variables.
 */

/* Default TLS model (no attribute) - should use local-exec */
__thread int default_tls = 42;

/* Explicit local-exec model - kernel per-cpu style */
__thread int percpu_var __attribute__((tls_model("local-exec"))) = 100;

/* Initial-exec model */
__thread int ie_var __attribute__((tls_model("initial-exec"))) = 200;

__attribute__((noinline))
int read_default(void) {
    return default_tls;
}

__attribute__((noinline))
int read_percpu(void) {
    return percpu_var;
}

__attribute__((noinline))
int read_ie(void) {
    return ie_var;
}

int main(void) {
    if (read_default() != 42) return 1;
    if (read_percpu() != 100) return 2;
    if (read_ie() != 200) return 3;

    default_tls = 999;
    percpu_var = 888;
    ie_var = 777;

    if (read_default() != 999) return 4;
    if (read_percpu() != 888) return 5;
    if (read_ie() != 777) return 6;

    return 0;
}
