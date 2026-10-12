/* Daily hotspot: 128-bit SIMD (movups/movaps + add/sub).
 *
 * Vector of 4 floats loaded from memory, arithmetic, stored back.
 * Pins movups (unaligned) vs movaps (aligned) selection and the
 * ADDPS/SUBPS opcode form.
 */
typedef float v4sf __attribute__((vector_size(16)));

v4sf vadd(v4sf a, v4sf b) { return a + b; }
v4sf vsub(v4sf a, v4sf b) { return a - b; }

float vdot(v4sf a, v4sf b) {
    v4sf p = a * b;
    return p[0] + p[1] + p[2] + p[3];
}
