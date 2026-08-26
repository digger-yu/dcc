/* Test: noreturn functions emit trap instruction at the end.
 *
 * C11 _Noreturn and GCC __attribute__((noreturn)) should both:
 * 1. Mark the function so codegen knows it never returns
 * 2. Emit ud2 (undefined instruction) before the ret instruction
 *    so that if the function does fall through (panic/abort/longjmp
 *    failure), execution traps instead of continuing into caller code.
 */

#include <stdnoreturn.h>

/* 1. C11 _Noreturn keyword */
_Noreturn void c11_noreturn_exit(int code) {
    /* empty body - should still emit ud2 before ret */
    for (;;) { /* infinite loop */ }
}

/* 2. C11 _Noreturn with leading attribute style */
__attribute__((noreturn)) void gcc_noreturn_abort(void) {
    for (;;) { /* infinite loop */ }
}

/* 3. noreturn with __noreturn__ alias */
__attribute__((__noreturn__)) void gcc_noreturn_alias(void) {
    for (;;) { /* infinite loop */ }
}

/* 4. noreturn in stdnoreturn.h (uses _Noreturn internally) */
noreturn void std_noreturn_loop(int n) {
    for (;;) { /* infinite loop */ }
}

/* 5. Regular function (control case - should NOT emit ud2) */
int regular_add(int a, int b) {
    return a + b;
}

/* 6. noreturn in decl-spec position with __declspec syntax (MS extension) */
#if defined(__GNUC__) || defined(__clang__)
__declspec(noreturn) void declspec_noreturn_handler(void) {
    for (;;) { /* infinite loop */ }
}
#endif
