/* F1b: mixed bitfield write + inc/dec RMW nesting must not overlap.
 *
 * Regression for H1b: the write path used a 2-slot / 16B-per-depth
 * geometry, the inc/dec path used a 3-slot / 24B-per-depth geometry,
 * yet both shared the same bf_rmw_depth counter. When an inc/dec
 * on a bitfield contained a bitfield write in its operand tree,
 * the outer RMW held slots at depth 0 and the inner write at depth
 * 1 fell on the same frame bytes the outer RMW was still reading.
 *
 * After unifying both geometries to 3 slots / 32B per depth this
 * overlap disappears; the fixture exercises exactly that shape.
 */
#include <stdio.h>

struct S {
    unsigned a : 4;
    unsigned b : 4;
};

int arr[64];

/* Bitfield write s->b = idx is evaluated, then used as array index;
 * then s->a post-increment feeds the returned sum. The compiler must
 * keep both RMW temporaries alive at the same time. */
int mixed_run(struct S *s) {
    int idx = (s->b = 2);
    unsigned old_a = s->a++;
    return arr[idx] + (int)old_a;
}

int main(void) {
    for (int i = 0; i < 64; i++) arr[i] = i * 10;

    struct S s;
    s.a = 5;
    s.b = 0;

    int r = mixed_run(&s);
    /* Expected: arr[2] = 20, post-increment returns old s->a = 5,
     * total 25. s->b must now be 2, s->a must now be 6. */
    if (r != 25) {
        printf("FAIL: mixed_run returned %d, expected 25\n", r);
        return 1;
    }
    if (s.b != 2) {
        printf("FAIL: s.b=%u expected 2\n", s.b);
        return 1;
    }
    if (s.a != 6) {
        printf("FAIL: s.a=%u expected 6\n", s.a);
        return 1;
    }

    /* Also exercise pre-decrement to make sure the write-inside-dec
     * shape doesn't regress either. */
    struct S t;
    t.a = 9;
    t.b = 0;
    int idx2 = (t.b = 1);
    unsigned old_t = --t.a;
    int r2 = arr[idx2] + (int)old_t;
    /* arr[1] = 10, pre-decrement of 9 -> 8, total 18. */
    if (r2 != 18) {
        printf("FAIL: mixed pre-dec returned %d, expected 18\n", r2);
        return 1;
    }
    if (t.a != 8 || t.b != 1) {
        printf("FAIL: t.a=%u t.b=%u expected 8,1\n", t.a, t.b);
        return 1;
    }

    printf("PASS: bitfield RMW mixed nesting\n");
    return 0;
}
