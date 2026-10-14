/* P11 regression #3: MOVD/MOVQ missing 66 prefix.
 *
 * The compiler must emit `66 0F 6E` (movq %r64 -> %xmm) and `66 0F 7E`
 * (movq %xmm -> %r64) when moving a 64-bit GPR into/out of an XMM
 * register as a scalar. Without the 66 prefix, `0F 6E/7E` decode as MMX
 * `movd %mm` - the exact failure P11 fixed in dbf186b6 / Bucket K
 * (28cdf5ee).
 *
 * The minimal trigger is float negation: dcc lowers `-x` on float to
 * movabs $0x80000000,%rax; movq %rax,%xmm1; xorps %xmm1,%xmm0. The
 * movq between GPR and XMM is the load-bearing MOVD/MOVQ instruction.
 */
float negate_f(float x) {
    return -x;
}

double negate_d(double x) {
    return -x;
}
