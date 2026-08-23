/* Test: struct with __aligned__(2 * sizeof(unsigned long)) after body */
struct page {
    unsigned long flags;
    void *lru[2];
    void *mapping;
    unsigned long index;
    unsigned long private;
} __attribute__((__aligned__(2 * sizeof(unsigned long))));

/* raw = 8+16+8+8+8 = 48, align = 16, 48 is multiple of 16 → size = 48 */
_Static_assert(sizeof(struct page) == 48, "sizeof(struct page) should be 48");
_Static_assert(__alignof__(struct page) == 16, "alignof(struct page) should be 16");

int main(void) {
    return 0;
}
