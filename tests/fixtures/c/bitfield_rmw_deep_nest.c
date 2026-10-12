/* F3: deeply nested bitfield RMW must not wrap around into positive
 * r13 displacements.
 *
 * Regression for H1/H2: each RMW temp slot was computed as
 *   (-8 - depth*16) as i8  /  (-8 - depth*24) as i8
 * Once depth crossed the i8 range (depth>=8 for the write path,
 * depth>=6 for the inc/dec path) the value wrapped to a *positive*
 * r13 offset, silently writing into the caller's frame and return
 * address.
 *
 * After the fix, bf_rmw_slot returns i32 (no wrapping) and the
 * slots live in the dedicated BF_RMW band, well below the frame
 * header. This fixture builds a deeply chained bitfield assignment
 * and verifies the final struct contents.
 */
#include <stdio.h>

struct Deep {
    unsigned a : 4;
    unsigned b : 4;
    unsigned c : 4;
    unsigned d : 4;
    unsigned e : 4;
    unsigned f : 4;
    unsigned g : 4;
    unsigned h : 4;
    unsigned i : 4;
};

int main(void) {
    struct Deep s;
    s.a = s.b = s.c = s.d = s.e = s.f = s.g = s.h = s.i = 0;

    /* 9-deep nested bitfield assignment: each inner write feeds the
     * next outer assignment's value. This triggers depth up to 8+
     * on the write path. */
    s.a = (s.b = (s.c = (s.d = (s.e = (s.f = (s.g = (s.h = (s.i = 3))))))));

    if (s.a != 3 || s.b != 3 || s.c != 3 || s.d != 3 ||
        s.e != 3 || s.f != 3 || s.g != 3 || s.h != 3 || s.i != 3) {
        printf("FAIL: deeply nested bitfield assignment lost values: "
               "a=%u b=%u c=%u d=%u e=%u f=%u g=%u h=%u i=%u\n",
               s.a, s.b, s.c, s.d, s.e, s.f, s.g, s.h, s.i);
        return 1;
    }

    /* Also chain a post-increment inside a deeply nested write so the
     * inc/dec RMW counter climbs past the old i8 wrap threshold. */
    s.f = 0;
    s.g = 0;
    unsigned sum = (s.i = 1) + (s.h = s.f++ + (s.g = 2));
    /* After: s.i=1, s.g=2, s.f incremented from 0 to 1, s.h = 0 + 2 = 2.
     * sum = 1 + 2 = 3. */
    if (sum != 3 || s.f != 1 || s.g != 2 || s.h != 2 || s.i != 1) {
        printf("FAIL: deep mixed chain gave sum=%u f=%u g=%u h=%u i=%u\n",
               sum, s.f, s.g, s.h, s.i);
        return 1;
    }

    printf("PASS: bitfield RMW deep nesting\n");
    return 0;
}
