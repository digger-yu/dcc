/* Daily hotspot: float/double arithmetic (scalar SSE).
 *
 * addss/mulss/divsd/addsd and the scalar-register-only operands
 * (%xmm0,%xmm1) exercise the SS/SD opcode table. The load/store
 * pairs around the arithmetic also pin SS vs SD prefix (f3 vs f2).
 */
float  muladd_f(float a, float b, float c) { return a * b + c; }
double muladd_d(double a, double b, double c) { return a * b + c; }
double recip(double x) { return 1.0 / x; }
