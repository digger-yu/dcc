/* m3_9: Thread Local Storage (__thread, _Thread_local)
 * Note: stack canary (-fstack-protector) is not supported
 * by dcc, so only TLS is tested here.
 * Known limitation: .tdata + .tbss mixing has relocation
 * issues, so all TLS vars here are initialized (.tdata). */

/* __thread TLS variable */
static __thread int tls_var = 42;

/* _Thread_local TLS variable */
static _Thread_local long tls_long = 100L;

/* TLS global variable */
__thread int tls_global = 999;

/* another TLS int */
static __thread int tls_extra = 55;

static void modify_tls(void) {
    tls_var = 200;
    tls_long = 300L;
    tls_extra = 77;
}

int main(void) {
    int fail = 0;

    /* initial values */
    if (tls_var != 42) fail |= 1;
    if (tls_long != 100L) fail |= 2;
    if (tls_global != 999) fail |= 4;
    if (tls_extra != 55) fail |= 8;

    /* modify and re-read */
    modify_tls();
    if (tls_var != 200) fail |= 16;
    if (tls_long != 300L) fail |= 32;
    if (tls_extra != 77) fail |= 64;

    /* address should be non-NULL */
    if (&tls_var == 0) fail |= 128;
    if (&tls_global == 0) fail |= 256;

    return fail;
}
