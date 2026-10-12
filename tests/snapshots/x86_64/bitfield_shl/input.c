/* P11 regression #2: shl/rol encoding bit (Group 0xC1 modrm /n).
 *
 * Bit-field insert must emit `shl $N, %reg` (sub /4), not `rol $N, %reg`
 * (sub /0). The encoder confused Shift::Shl with Shift::Rol in the
 * immediate-count path.
 */
struct bf {
    unsigned b : 2;
    unsigned pad : 30;
};

void set_b(struct bf *s, unsigned v) {
    s->b = (s->b & ~0x3u) | (v & 0x3u);
}
