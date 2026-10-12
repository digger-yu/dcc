/* P11 regression #7: uint128 `and %al,%ah` (REX ambiguity).
 *
 * `unsigned __int128` equality check lowers to a compare that touches
 * the low bytes of two 64-bit halves. The encoder assumed field 4..7
 * always needs REX (=> spl/bpl/sil/dil) and forgot that without REX
 * the same field is ah/ch/dh/bh.
 */
int u128_eq(unsigned __int128 a, unsigned __int128 b) {
    return a == b;
}
