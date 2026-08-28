/* Test: C99 hexadecimal floating-point constants */

/* Basic hex floats */
double a = 0x1.0p0;     /* 1.0 */
double b = 0x1.4p3;     /* 10.0 (1.25 * 8) */
double c = 0x1p10;      /* 1024.0 */
double d = 0x0p0;       /* 0.0 */

/* Hex floats with fractional part */
double e = 0x1.8p1;     /* 3.0 */
double f = 0x0.8p1;     /* 1.0 (0.5 * 2) */

/* Negative exponent */
double g = 0x1p-1;      /* 0.5 */
double h = 0x1.0p-4;    /* 0.0625 */

/* Float and long double suffixes */
float i = 0x1.4p3f;     /* 10.0f */
long double j = 0x1.0p0L;

/* Upper case X and P */
double k = 0X1.Ap4;     /* 26.0 */
double l = 0X1P0;       /* 1.0 */

/* Use in expressions */
double m = 0x1.0p0 + 0x1.0p1;  /* 3.0 */

int printf(const char *, ...);
int main(void) {
    if (a != 1.0) return 1;
    if (b != 10.0) return 2;
    if (c != 1024.0) return 3;
    if (e != 3.0) return 4;
    if (g != 0.5) return 5;
    if (f != 1.0) return 6;
    if (h != 0.0625) return 7;
    if (k != 26.0) return 8;
    if (m != 3.0) return 9;
    return 0;
}
