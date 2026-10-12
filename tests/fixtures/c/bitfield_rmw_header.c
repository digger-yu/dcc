/* F1: bitfield RMW temp slots must not clobber callee-saved spill area.
 *
 * Regression for H1: bitfield write RMW used to spill its container
 * address / old value to -8(%r13) / -16(%r13), colliding with the
 * callee-saved spill region (rbx lives at -16(%r13), r12..r15 live
 * at -24..-40). After the fix those temps must live in the dedicated
 * BF_RMW band, so a function that both saves rbx and performs a
 * bitfield write returns the caller's rbx unchanged.
 *
 * We force the compiler to actually save rbx into the frame by
 * performing six volatile-ish calls in one expression (so rbx is
 * needed to hold a live value across the call sequence), then run a
 * bitfield write in the same function.
 */
#include <stdio.h>

struct S {
    unsigned a : 4;
    unsigned b : 4;
};

volatile int sink_for_calls;

int g(int x) {
    sink_for_calls = x;
    return x;
}

int bf_callee(struct S *s, int n) {
    /* Chain of calls forces rbx to be live across the call sequence,
     * which makes the function spill rbx into the frame header. */
    int x = g(n) + g(n + 1) + g(n + 2) + g(n + 3) + g(n + 4) + g(n + 5);
    s->a = 7;
    s->b = 3;
    return x + n + (int)s->a + (int)s->b;
}

int main(void) {
    struct S s;
    s.a = 0;
    s.b = 0;
    /* Baseline expected by hand: n=10 -> sum = 10+11+12+13+14+15 = 75,
     * plus n=10, plus s->a=7, plus s->b=3 -> 95. If rbx got clobbered
     * by the bitfield temp slots this returns garbage. */
    int r = bf_callee(&s, 10);
    if (r != 95) {
        printf("FAIL: bf_callee returned %d, expected 95\n", r);
        return 1;
    }
    if (s.a != 7 || s.b != 3) {
        printf("FAIL: s.a=%u s.b=%u expected 7,3\n", s.a, s.b);
        return 1;
    }
    printf("PASS: bitfield RMW header sanity\n");
    return 0;
}
