/* P11 regression #1: movsd store direction.
 *
 * `g_arr[i] = x` must emit `movsd %xmm_src, (%idx_reg)`, not the reverse
 * (load into xmm). The earlier bug had the 0x11/0x10 opcode directions
 * swapped in SimdMov store path.
 */
double g_arr[8];

void store_d(double x, int i) {
    g_arr[i] = x;
}

double load_d(int i) {
    return g_arr[i];
}
