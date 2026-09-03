/* Bitfield runtime test: read/write/sizeof for signed, unsigned,
   mixed, cross-container, and small container bitfields. */
#include <stdio.h>

struct bits {
    int a : 3;
    int b : 5;
    int c;
};

struct cross {
    unsigned int x : 6;
    unsigned int y : 10;
    unsigned int z : 16;
};

struct small {
    short a : 4;
    short b : 12;
};

struct mixed {
    int flag : 1;
    unsigned int mode : 3;
    unsigned int count : 8;
    int normal;
};

static int fails = 0;

#define CHECK(cond, msg) do { \
    if (!(cond)) { \
        printf("FAIL: %s\n", msg); \
        fails++; \
    } \
} while (0)

int main(void) {
    /* === Basic read/write === */
    struct bits s;
    s.a = 3;
    s.b = 15;
    s.c = 42;
    CHECK(s.a == 3, "s.a != 3");
    CHECK(s.b == 15, "s.b != 15");
    CHECK(s.c == 42, "s.c != 42");

    /* === Signed negative === */
    s.a = -1;
    s.b = -8;
    CHECK(s.a == -1, "s.a != -1");
    CHECK(s.b == -8, "s.b != -8");

    /* === Zero === */
    s.a = 0;
    s.b = 0;
    CHECK(s.a == 0, "s.a != 0 after zero");
    CHECK(s.b == 0, "s.b != 0 after zero");

    /* === Cross-container === */
    struct cross cr;
    cr.x = 63;
    cr.y = 1023;
    cr.z = 65535;
    CHECK(cr.x == 63, "cr.x != 63");
    CHECK(cr.y == 1023, "cr.y != 1023");
    CHECK(cr.z == 65535, "cr.z != 65535");

    /* === Small container (short) === */
    struct small sm;
    sm.a = 7;
    sm.b = 2047;
    CHECK(sm.a == 7, "sm.a != 7");
    CHECK(sm.b == 2047, "sm.b != 2047");

    /* === Mixed fields === */
    struct mixed mx;
    mx.flag = 1;
    mx.mode = 5;
    mx.count = 200;
    mx.normal = 99;
    CHECK(mx.flag == -1, "mx.flag != -1");
    CHECK(mx.mode == 5, "mx.mode != 5");
    CHECK(mx.count == 200, "mx.count != 200");
    CHECK(mx.normal == 99, "mx.normal != 99");

    /* === Bitfield arithmetic === */
    s.a = 2;
    s.b = 10;
    int sum = s.a + s.b;
    CHECK(sum == 12, "sum != 12");

    /* === sizeof === */
    CHECK(sizeof(struct bits) == 8, "sizeof(bits) != 8");
    CHECK(sizeof(struct cross) == 4, "sizeof(cross) != 4");
    CHECK(sizeof(struct small) == 2, "sizeof(small) != 2");

    /* === Overwrite preserves neighbors === */
    cr.x = 0;
    cr.y = 0;
    cr.z = 0;
    cr.x = 30;
    cr.y = 500;
    CHECK(cr.x == 30, "cr.x != 30 after overwrite");
    CHECK(cr.y == 500, "cr.y != 500 after overwrite");
    CHECK(cr.z == 0, "cr.z != 0 after overwrite");

    if (fails == 0) {
        printf("ALL BITFIELD CHECKS PASSED\n");
    } else {
        printf("%d BITFIELD CHECKS FAILED\n", fails);
    }
    return fails;
}
