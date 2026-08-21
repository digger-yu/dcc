/* AArch64 HFA (Homogeneous Floating-point Aggregate) test fixtures */
/* AAPCS64: struct with 1-8 same-type float/double members -> HFA */
/* >8 members or mixed types -> general aggregate (by-ref) */

/* HFA 1 element: return in v0 */
struct HFA1 { double a; };
struct HFA1 make_hfa1(double a) {
    struct HFA1 s; s.a = a; return s;
}

/* HFA 2 elements: return in v0, v1 */
struct HFA2 { double a; double b; };
struct HFA2 make_hfa2(double a, double b) {
    struct HFA2 s; s.a = a; s.b = b; return s;
}

/* HFA 3 elements */
struct HFA3 { float a; float b; float c; };
struct HFA3 make_hfa3(float a, float b, float c) {
    struct HFA3 s; s.a = a; s.b = b; s.c = c; return s;
}

/* HFA 4 elements */
struct HFA4 { double a; double b; double c; double d; };
struct HFA4 make_hfa4(double a, double b, double c, double d) {
    struct HFA4 s; s.a = a; s.b = b; s.c = c; s.d = d; return s;
}

/* HFA 8 elements: maximum for AAPCS64 */
struct HFA8 {
    double a; double b; double c; double d;
    double e; double f; double g; double h;
};
struct HFA8 make_hfa8(double a, double b, double c, double d,
                       double e, double f, double g, double h) {
    struct HFA8 s;
    s.a = a; s.b = b; s.c = c; s.d = d;
    s.e = e; s.f = f; s.g = g; s.h = h;
    return s;
}

/* >8 elements: NOT HFA, falls back to general aggregate (sret) */
struct BigFloat {
    double a; double b; double c; double d;
    double e; double f; double g; double h;
    double i;
};
struct BigFloat make_big(double a, double b, double c, double d,
                          double e, double f, double g, double h,
                          double i) {
    struct BigFloat s;
    s.a = a; s.b = b; s.c = c; s.d = d;
    s.e = e; s.f = f; s.g = g; s.h = h;
    s.i = i;
    return s;
}

/* Mixed types: NOT HFA */
struct Mixed { double a; float b; };
struct Mixed make_mixed(double a, float b) {
    struct Mixed s; s.a = a; s.b = b; return s;
}

int consume_hfa2(struct HFA2 s) {
    return (int)(s.a + s.b);
}

int main(void) {
    struct HFA2 s = make_hfa2(1.0, 2.0);
    return consume_hfa2(s);
}
