/* F2: bitfield RMW temp slots must not overwrite the stack canary.
 *
 * Regression for H1/H2 under -fstack-protector: the prologue stores
 * the canary at -8(%r13). The old bitfield RMW write path also
 * spilled its container address at -8(%r13), so the very first
 * bitfield write in a protected function silently clobbered the
 * canary and __stack_chk_fail fired on return.
 *
 * This fixture is built with -fstack-protector so every function
 * gets a canary; if the bitfield write still lands at -8(%r13)
 * the process aborts here.
 */
#include <stdio.h>

struct S {
    unsigned a : 4;
    unsigned b : 4;
};

unsigned accumulate(int n) {
    /* Do a bitfield write in a function that also has several local
     * variables, so the prologue allocates a non-trivial frame and
     * installs a canary. */
    struct S s;
    unsigned acc = 0;
    for (int i = 0; i < n; i++) {
        s.a = (unsigned)(i & 0x7);
        s.b = (unsigned)((i >> 3) & 0x7);
        acc += s.a + s.b;
    }
    return acc;
}

int main(void) {
    unsigned r = accumulate(16);
    /* Expected: i in [0,16): low nibble cycles 0..7 twice ->
     * 2 * (0+..+7) = 56; high nibble: i/8 is 0..1 for the
     * first 16 iterations, contributing 0+1 = 1 once each.
     * acc = 56 + 8*0 + 8*1 = 64. */
    if (r != 64) {
        printf("FAIL: accumulate returned %u, expected 64\n", r);
        return 1;
    }
    printf("PASS: bitfield RMW canary intact\n");
    return 0;
}
