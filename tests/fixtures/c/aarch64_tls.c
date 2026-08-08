/* AArch64 TLS test: Thread Local Storage with TLSDESC access sequence.
 * Tests that TLS variables are correctly accessed via TLSDESC relocations
 * on AArch64 target. */

__thread int tls_a = 42;
__thread long tls_b = 100L;
static _Thread_local int tls_c = 7;

int main(void) {
    int fail = 0;

    /* read initial values */
    if (tls_a != 42) fail |= 1;
    if (tls_b != 100L) fail |= 2;
    if (tls_c != 7) fail |= 4;

    /* modify and re-read */
    tls_a = 200;
    tls_b = 300L;
    tls_c = 99;

    if (tls_a != 200) fail |= 8;
    if (tls_b != 300L) fail |= 16;
    if (tls_c != 99) fail |= 32;

    /* address should be non-NULL */
    if (&tls_a == 0) fail |= 64;
    if (&tls_b == 0) fail |= 128;

    return fail;
}
