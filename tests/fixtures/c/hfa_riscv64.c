/* RISC-V HFA (Homogeneous Floating-point Aggregate) test fixtures */
/* RV64 calling convention: struct with 1-4 same-type float/double -> HFA */
/* >4 members or mixed types -> general aggregate (by-ref/sret) */

/* HFA 1 element: return in fa0 */
struct HFA1 { double a; };
struct HFA1 make_hfa1(double a) {
    struct HFA1 s; s.a = a; return s;
}

/* HFA 2 elements: return in fa0, fa1 */
struct HFA2 { double a; double b; };
struct HFA2 make_hfa2(double a, double b) {
    struct HFA2 s; s.a = a; s.b = b; return s;
}

/* HFA 3 elements */
struct HFA3 { float a; float b; float c; };
struct HFA3 make_hfa3(float a, float b, float c) {
    struct HFA3 s; s.a = a; s.b = b; s.c = c; return s;
}

/* HFA 4 elements: maximum for RV64 */
struct HFA4 { double a; double b; double c; double d; };
struct HFA4 make_hfa4(double a, double b, double c, double d) {
    struct HFA4 s; s.a = a; s.b = b; s.c = c; s.d = d; return s;
}

/* >4 elements: NOT HFA on RISC-V, falls back to sret */
struct BigFloat {
    double a; double b; double c; double d;
    double e;
};
struct BigFloat make_big(double a, double b, double c, double d,
                          double e) {
    struct BigFloat s;
    s.a = a; s.b = b; s.c = c; s.d = d;
    s.e = e;
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
