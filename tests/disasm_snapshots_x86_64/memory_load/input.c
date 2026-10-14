/* Daily hotspot: memory load (modes + width variants).
 *
 * Loads through a pointer, through a pointer with offset, RIP-
 * relative global load, and different widths (char/short/int/long).
 * Pins the ModRM/SIB/REX width selection; off-by-one on any of
 * those fields produces a legal-looking load that reads the wrong
 * byte.
 */
signed char  l_b (const signed char  *p) { return *p; }
short       l_s (const short       *p) { return *p; }
int         l_i (const int         *p) { return *p; }
long        l_l (const long        *p) { return *p; }
long        l_off(const long        *p) { return p[3]; }

long g_glob;
long        l_glob(void)                { return g_glob; }
