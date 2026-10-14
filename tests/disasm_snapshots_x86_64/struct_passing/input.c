/* Daily hotspot: struct passing (by value).
 *
 * Small structs (1/2/8/16 bytes) are passed in registers per the
 * System V AMD64 ABI. This case pins which register group is used
 * (INTEGER vs SSE) based on struct field type and size.
 */
typedef struct { long a; long b; }    Pair;
typedef struct { float x; float y; }  PtF;

long  sum_pair(Pair p)   { return p.a + p.b; }
float sum_ptf(PtF p)     { return p.x + p.y; }

Pair  make_pair(long a, long b) {
    Pair p; p.a = a; p.b = b; return p;
}
