/* P11 regression #5: signed bitfield SHR.
 *
 * Reading a signed bit-field must emit `sar $N, %reg` (sign-extracting
 * shift after the SHR that extracts the field). The encoder used plain
 * `shr` which left the sign bit zero-extended.
 */
struct sbf {
    signed b : 4;
    unsigned pad : 28;
};

int read_b(struct sbf *s) {
    return s->b;
}
