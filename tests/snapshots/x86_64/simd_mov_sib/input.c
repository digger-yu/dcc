/* P11 regression #4: SimdMov SIB scale.
 *
 * double arrays require scale=8, float arrays scale=4. The earlier
 * SimdMov encoder accepted only scale=1 and silently dropped the whole
 * instruction when the real index needed scale=4/8.
 */
double g_d[16];
float g_f[16];

double get_d(int i) { return g_d[i]; }
void set_d(int i, double v) { g_d[i] = v; }
float  get_f(int i) { return g_f[i]; }
void   set_f(int i, float v) { g_f[i] = v; }
