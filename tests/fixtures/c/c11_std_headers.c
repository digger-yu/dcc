/* Test: <stdnoreturn.h> noreturn macro and <stdalign.h> macros */

/* noreturn should expand to _Noreturn */
noreturn void foo(void) { while(1) {} }

/* alignas/alignof should work via macro in C11 mode */
#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112
/* alignof test */
int a = _Alignof(int);

/* alignas test */
alignas(16) int x;
#endif

int main(void) { return 0; }
