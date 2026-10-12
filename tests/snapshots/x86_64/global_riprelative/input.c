/* Daily hotspot: global variables (RIP-relative addressing).
 *
 * Every global access in a PIE/PIC build lowers to
 * `mov 0x0(%rip), %reg` (with a relocated addend). The snapshot
 * pins the REX.W width bit and the reg index chosen by the allocator.
 */
int    g_i;
long   g_l;
double g_d;

void set_i(int v)        { g_i = v; }
int  get_i(void)         { return g_i; }
void set_l(long v)       { g_l = v; }
long get_l(void)         { return g_l; }
void set_d(double v)     { g_d = v; }
double get_d(void)        { return g_d; }
