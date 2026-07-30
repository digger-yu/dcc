/* m3_6: __builtin_expect, __builtin_offsetof,
 * __builtin_constant_p, __builtin_choose_expr */
#include <stddef.h>

struct Inner { int x; long y; };
struct Outer { char a; struct Inner b; int c; };

/* __builtin_expect: compiler hint, runtime value unchanged */
static int expect_test(void) {
    int x = 42;
    if (__builtin_expect(x, 42) != 42) return -1;
    if (__builtin_expect(x != 0, 1) != 1) return -2;
    return 0;
}

/* __builtin_offsetof */
static int offsetof_test(void) {
    int fail = 0;

    /* Inner: x at 0, y at 8 (aligned to long) */
    if (__builtin_offsetof(struct Inner, x) != 0) fail |= 1;
    if (__builtin_offsetof(struct Inner, y) != 8) fail |= 2;

    /* Outer: a at 0, b.x at 8 (padded), b.y at 16, c at 24 */
    if (__builtin_offsetof(struct Outer, a) != 0) fail |= 4;
    if (__builtin_offsetof(struct Outer, b) != 8) fail |= 8;
    if (__builtin_offsetof(struct Outer, c) != 24) fail |= 16;

    /* nested offsetof */
    if (offsetof(struct Outer, b) + offsetof(struct Inner, y) != 16)
        fail |= 32;

    return fail;
}

/* __builtin_constant_p */
static int constant_p_test(void) {
    int fail = 0;

    /* compile-time constants → 1 */
    if (__builtin_constant_p(42) != 1) fail |= 1;
    if (__builtin_constant_p(1 + 2) != 1) fail |= 2;
    if (__builtin_constant_p(sizeof(int)) != 1) fail |= 4;

    /* runtime values → 0 (at -O0) */
    volatile int v = 10;
    if (__builtin_constant_p(v) != 0) fail |= 8;

    return fail;
}

/* __builtin_choose_expr: compile-time conditional */
static int choose_expr_test(void) {
    int fail = 0;

    /* choose_expr(const_expr, expr1, expr2) */
    int a = __builtin_choose_expr(1, 100, 200);
    if (a != 100) fail |= 1;

    int b = __builtin_choose_expr(0, 100, 200);
    if (b != 200) fail |= 2;

    /* choose_expr with sizeof (always const) */
    int c = __builtin_choose_expr(sizeof(long) == 8, 64, 32);
    if (c != 64) fail |= 4;

    return fail;
}

int main(void) {
    int fail = 0;

    if (expect_test() != 0) fail |= 1;
    if (offsetof_test() != 0) fail |= 2;
    if (constant_p_test() != 0) fail |= 4;
    if (choose_expr_test() != 0) fail |= 8;

    return fail;
}
