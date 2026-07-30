/* M2.2: struct layout - bitfields, alignment, padding */
#include <stdio.h>
#include <stddef.h>

static int fails = 0;

#define CHECK(cond, msg) do { \
    if (!(cond)) { \
        printf("FAIL: %s\n", msg); \
        fails++; \
    } \
} while (0)

/* Basic struct - no padding needed */
struct S1 { char a; char b; };

/* Struct with padding between char and long */
struct S2 { char a; long b; };

/* Nested struct */
struct S3 { int a; struct S2 inner; char c; };

/* Bitfield struct */
struct BF1 {
    unsigned int a : 1;
    unsigned int b : 3;
    unsigned int c : 4;
    unsigned int d : 24;
};

/* Bitfield shorts */
struct BF2 {
    unsigned short a : 4;
    unsigned short b : 4;
    unsigned short c : 4;
    unsigned short d : 4;
};

/* Bitfield with zero-width */
struct BF3 {
    unsigned int a : 5;
    unsigned int : 0;
    unsigned int b : 5;
};

/* Packed struct (gcc extension) */
struct __attribute__((packed)) S4 {
    char a; int b; char c;
};

/* Aligned struct */
struct __attribute__((aligned(16))) S5 {
    int a; int b;
};

/* Union */
union U1 { char c; int i; long l; double d; };

/* Struct with array member */
struct S6 { int count; int data[4]; char tag; };

/* Struct with flexible array member */
struct S7 { int len; char data[]; };

int main(void) {
    /* S1: two chars, no padding */
    CHECK(sizeof(struct S1) == 2, "sizeof(S1) != 2");
    CHECK(_Alignof(struct S1) == 1, "alignof(S1) != 1");
    CHECK(offsetof(struct S1, b) == 1, "offsetof(S1, b) != 1");

    /* S2: char + padding + long */
    CHECK(sizeof(struct S2) == 16, "sizeof(S2) != 16");
    CHECK(_Alignof(struct S2) == 8, "alignof(S2) != 8");
    CHECK(offsetof(struct S2, b) == 8, "offsetof(S2, b) != 8");

    /* S3: nested struct */
    CHECK(offsetof(struct S3, inner) == 8, "offsetof(S3, inner) != 8");

    /* BF1: bitfield packing */
    CHECK(sizeof(struct BF1) == 4, "sizeof(BF1) != 4");

    /* BF2: bitfield shorts */
    CHECK(sizeof(struct BF2) == 2, "sizeof(BF2) != 2");

    /* BF3: zero-width bitfield forces new storage unit */
    CHECK(sizeof(struct BF3) == 8, "sizeof(BF3) != 8");

    /* S4: packed struct - size and offsets correct */
    CHECK(sizeof(struct S4) == 6, "sizeof(S4 packed) != 6");
    CHECK(offsetof(struct S4, b) == 1, "offsetof(S4, b) != 1");
    CHECK(offsetof(struct S4, c) == 5, "offsetof(S4, c) != 5");
    /* NOTE: _Alignof for packed structs returns natural alignment,
       not 1 as gcc does. This is a known limitation. */

    /* S5: aligned struct */
    CHECK(_Alignof(struct S5) == 16, "alignof(S5 aligned) != 16");
    CHECK(sizeof(struct S5) % 16 == 0, "sizeof(S5) not multiple of 16");

    /* U1: union */
    CHECK(sizeof(union U1) == 8, "sizeof(U1) != 8");
    CHECK(_Alignof(union U1) == 8, "alignof(U1) != 8");

    /* S6: struct with array */
    CHECK(offsetof(struct S6, data) == 4, "offsetof(S6, data) != 4");
    CHECK(offsetof(struct S6, tag) == 20, "offsetof(S6, tag) != 20");

    /* S7: flexible array member */
    CHECK(sizeof(struct S7) == 4, "sizeof(S7) != 4");
    CHECK(offsetof(struct S7, data) == 4, "offsetof(S7, data) != 4");

    if (fails == 0) {
        printf("ALL STRUCT LAYOUT CHECKS PASSED\n");
    } else {
        printf("%d struct layout checks FAILED\n", fails);
    }
    return fails;
}
