/* Test: simple aligned with numeric constant */
struct A {
    unsigned long flags;
    void *lru[2];
    void *mapping;
    unsigned long index;
    unsigned long private;
} __attribute__((__aligned__(16)));

/* raw = 8+16+8+8+8 = 48, align = 16, 48 is multiple of 16 → size = 48 */
_Static_assert(sizeof(struct A) == 48, "sizeof(struct A) should be 48");
_Static_assert(__alignof__(struct A) == 16, "alignof(struct A) should be 16");

int main(void) { return 0; }
