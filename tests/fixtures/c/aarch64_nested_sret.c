/* AArch64 nested sret: large struct > 16B returned through multi-level call chain.
 * Tests that the sret pointer saved at function entry survives nested calls. */

struct Big {
    long a, b, c, d;  /* 32 bytes > 16 */
};

struct Big make_big(long x) {
    struct Big r;
    r.a = x;
    r.b = x + 1;
    r.c = x + 2;
    r.d = x + 3;
    return r;
}

/* Middle function: calls make_big (nested), then returns its own big struct.
 * The sret pointer passed by outer() must survive the inner call to make_big. */
struct Big middle(long x) {
    struct Big tmp = make_big(x * 10);
    struct Big result;
    result.a = tmp.a + 100;
    result.b = tmp.b + 200;
    result.c = tmp.c + 300;
    result.d = tmp.d + 400;
    return result;
}

/* Outer function: calls middle, which calls make_big — 3-level sret chain. */
struct Big outer(long x) {
    struct Big r = middle(x);
    struct Big final;
    final.a = r.a + 1000;
    final.b = r.b + 2000;
    final.c = r.c + 3000;
    final.d = r.d + 4000;
    return final;
}

int main(void) {
    struct Big r = outer(1);
    int fail = 0;
    /* outer(1): middle(1) -> make_big(10) -> {10,11,12,13}
     * middle: {110, 211, 312, 413}
     * outer: {1110, 2211, 3312, 4413} */
    if (r.a != 1110) fail |= 1;
    if (r.b != 2211) fail |= 2;
    if (r.c != 3312) fail |= 4;
    if (r.d != 4413) fail |= 8;
    return fail;
}
