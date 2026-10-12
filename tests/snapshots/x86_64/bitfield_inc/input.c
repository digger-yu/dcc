/* P11 regression #6: bit-field store-back register pairing.
 *
 * `s->b += 1` reads the field, adds 1, and writes back. The read and
 * write-back stages must use the same register for the field value; an
 * earlier bug had the write-back reload the original (unmodified) value
 * because the read stage and write-back stage disagreed on which
 * register held the RMW result.
 */
struct bf {
    unsigned b : 3;
    unsigned pad : 29;
};

void inc_b(struct bf *s) {
    s->b += 1;
}
