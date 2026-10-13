/* F4 — P2: trampoline band relocation (H3 fix).
 *
 * Escaping nested function (forces trampoline emission) combined with a
 * deep vector binop chain (forces vec_temp band to grow up into the region
 * where trampolines used to live). Before P2, both bands sat in the bottom
 * 256 bytes of the frame and overlapped once vec_temp offset >= ~10 slots;
 * after P2 they are disjoint by construction.
 *
 * This fixture is a smoke test: it must compile and run cleanly under dcc.
 */
typedef int v4si __attribute__((vector_size(16)));
extern v4si pick(int);

int both_over(int n) {
    int x = n;
    int inner(void) { return x + 1; }
    int (*p)(void) = inner;
    v4si a = pick(0), b = pick(1), c = pick(2), d = pick(3);
    v4si e = pick(4), f = pick(5), g2 = pick(6), h = pick(7);
    v4si i2 = pick(8), j = pick(9), k = pick(10), l = pick(11);
    v4si m = pick(12), o = pick(13);
    v4si r = a + b + c + d + e + f + g2 + h + i2 + j + k + l + m + o;
    return r[0] + r[1] + p();
}
