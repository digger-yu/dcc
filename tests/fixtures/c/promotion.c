/* M2.3: integer promotion and implicit conversion rules */
#include <stdio.h>
#include <stdint.h>

static int fails = 0;

#define CHECK(cond, msg) do { \
    if (!(cond)) { \
        printf("FAIL: %s\n", msg); \
        fails++; \
    } \
} while (0)

int main(void) {
    /* Integer promotion: char/short -> int in arithmetic */
    char c1 = 100, c2 = 100;
    CHECK(sizeof(c1 + c2) == sizeof(int), "char+char not promoted to int");
    CHECK(c1 + c2 == 200, "char addition wrong");

    short s1 = 30000, s2 = 30000;
    CHECK(sizeof(s1 + s2) == sizeof(int), "short+short not promoted to int");
    CHECK(s1 + s2 == 60000, "short addition wrong");

    /* long vs int: result type is long (usual arithmetic conversions) */
    long l = 1;
    int i2 = 1;
    CHECK(sizeof(l + i2) == sizeof(long), "long+int not promoted to long");

    /* Integer literal conversions */
    CHECK(sizeof(0) == sizeof(int), "literal 0 not int");
    CHECK(sizeof(0L) == sizeof(long), "literal 0L not long");
    CHECK(sizeof(0LL) == sizeof(long long), "literal 0LL not long long");
    CHECK(sizeof(0U) == sizeof(unsigned int), "literal 0U not unsigned");

    /* Narrowing conversions (truncation) */
    int big = 0x12345678;
    char small = (char)big;
    CHECK(small == 0x78, "int to char truncation");

    /* Zero extension */
    unsigned char uc = 255;
    int zero_ext = uc;
    CHECK(zero_ext == 255, "zero extension of unsigned char");

    /* Float/double promotion in sizeof */
    float f = 1.5f;
    double d = 2.5;
    CHECK(sizeof(f + d) == sizeof(double), "float+double not double");
    CHECK(sizeof(f + 1) == sizeof(float), "float+int not float");
    CHECK(sizeof(d + 1) == sizeof(double), "double+int not double");

    /* Multi-type arithmetic */
    long la = 100L;
    int ia = 200;
    long result = la + ia;
    CHECK(result == 300L, "long + int arithmetic");

    long long lla = 1000LL;
    long lba = 200L;
    CHECK(sizeof(lla + lba) == sizeof(long long), "long long + long");

    if (fails == 0) {
        printf("ALL INTEGER PROMOTION CHECKS PASSED\n");
    } else {
        printf("%d promotion checks FAILED\n", fails);
    }
    return fails;
}
