/* Test: BTF type qualifier completeness (const/volatile/restrict) */
/* Verifies that struct/union field qualifiers are preserved in BTF output */

struct qualified_fields {
    const int ci;
    volatile int vi;
    const volatile int cvi;
    int *restrict rp;
    const int *cp;
    volatile int *vp;
    const volatile int *cvp;
    int *const pc;
    int *volatile pv;
};

union qualified_union {
    const int ci;
    volatile long vl;
    const volatile short cvs;
};

/* Nested pointer qualifiers */
struct nested_ptrs {
    const int *const *cpcp;
    volatile int *volatile *vpvp;
};

int main(void) {
    struct qualified_fields qf;
    qf.ci = 42;
    return qf.ci;
}
