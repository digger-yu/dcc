/* Test: C99 Unicode character names in identifiers (6.4.3) */
/* \uXXXX (4 hex digits) and \UXXXXXXXX (8 hex digits) */

/* Basic \uXXXX: \u0041 = 'A' */
int \u0041 = 1;

/* Basic \UXXXXXXXX: \U00000042 = 'B' */
int \U00000042 = 2;

/* UCN in middle of identifier */
int foo\u0043ar = 3;  /* fooCar */

/* UCN at start of identifier */
int \u0044elta = 4;   /* Delta */

/* Multiple UCNs in one identifier */
int \u0045\u0046 = 5;  /* EF */

/* UCN with digits */
int var\u0030 = 6;     /* var0 */

int main(void) {
    /* Verify values */
    if (\u0041 != 1) return 1;
    if (\U00000042 != 2) return 2;
    if (foo\u0043ar != 3) return 3;
    if (\u0044elta != 4) return 4;
    if (\u0045\u0046 != 5) return 5;
    if (var\u0030 != 6) return 6;
    return 0;
}
