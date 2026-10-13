/*
 * Switch/case range comparison correctness tests.
 *
 * Bug 1: switch value in rcx clobbered by gen_expr when evaluating
 *         case expressions that use rcx as temporary (e.g. shifts).
 *         Fix: reload rcx from [rsp] before each comparison.
 *
 * Bug 2: range comparison used signed jl/jg for unsigned switch values.
 *         Unsigned values with high bit set (e.g. 0xFFFFFFFE) are negative
 *         when interpreted as signed, causing incorrect range matching.
 *         Fix: use jb/ja for unsigned switch expressions.
 */

/* Bug 1: shift in case expression clobbers rcx (the switch value register) */
int test_shift_case_expr(int x) {
    switch (x) {
    case (1 << 5) + 3:   /* 35 */
        return 1;
    case (1 << 3) + 1:   /* 9 */
        return 2;
    case (2 << 4) + 2:   /* 34 */
        return 3;
    default:
        return 0;
    }
}

/* Bug 1 variant: shift in GNU case range bounds */
int test_shift_range(int x) {
    switch (x) {
    case (1 << 5) ... (1 << 5) + 5:   /* 32 ... 37 */
        return 1;
    case (1 << 3) ... (1 << 3) + 2:   /* 8 ... 10 */
        return 2;
    default:
        return 0;
    }
}

/* Bug 2: unsigned switch with large values (high bit set).
 * With signed jl/jg, 0xFFFFFFFE (-2 as signed) would be considered
 * less than 0xFFFFFFF0, failing the range check.
 * With unsigned jb/ja, both are correctly in range. */
int test_unsigned_range(unsigned int x) {
    switch (x) {
    case 0xFFFFFFFEu:
        return 1;
    case 0x80000000u:
        return 2;
    default:
        return 0;
    }
}

/* Bug 2: unsigned range with values above INT_MAX */
int test_unsigned_range_bounds(unsigned int x) {
    switch (x) {
    case 0xFFFFFFF0u ... 0xFFFFFFFFu:
        return 1;
    case 0x80000000u ... 0x8000000Fu:
        return 2;
    default:
        return 0;
    }
}

/* Combined: unsigned enum switch (common kernel pattern) */
enum unsigned_enum {
    ENUM_A = 0xFFFFFF00u,
    ENUM_B = 0xFFFFFF01u,
    ENUM_C = 0x80000000u,
};

int test_unsigned_enum(unsigned int x) {
    switch (x) {
    case ENUM_A:
        return 1;
    case ENUM_B:
        return 2;
    case ENUM_C:
        return 3;
    default:
        return 0;
    }
}

int main(void) {
    /* Bug 1: shift case expressions */
    if (test_shift_case_expr(35) != 1) return 1;
    if (test_shift_case_expr(9) != 2) return 2;
    if (test_shift_case_expr(34) != 3) return 3;
    if (test_shift_case_expr(0) != 0) return 4;

    /* Bug 1: shift range bounds */
    if (test_shift_range(32) != 1) return 10;
    if (test_shift_range(37) != 1) return 11;
    if (test_shift_range(33) != 1) return 12;
    if (test_shift_range(8) != 2) return 13;
    if (test_shift_range(10) != 2) return 14;
    if (test_shift_range(0) != 0) return 15;
    if (test_shift_range(38) != 0) return 16;

    /* Bug 2: unsigned exact match with high bit set */
    if (test_unsigned_range(0xFFFFFFFEu) != 1) return 20;
    if (test_unsigned_range(0x80000000u) != 2) return 21;
    if (test_unsigned_range(42u) != 0) return 22;

    /* Bug 2: unsigned range with high-bit values */
    if (test_unsigned_range_bounds(0xFFFFFFF5u) != 1) return 30;
    if (test_unsigned_range_bounds(0xFFFFFFFFu) != 1) return 31;
    if (test_unsigned_range_bounds(0x80000005u) != 2) return 32;
    if (test_unsigned_range_bounds(0x8000000Fu) != 2) return 33;
    if (test_unsigned_range_bounds(42u) != 0) return 34;
    /* Values between the two ranges should not match */
    if (test_unsigned_range_bounds(0x7FFFFFFFu) != 0) return 35;
    if (test_unsigned_range_bounds(0x90000000u) != 0) return 36;

    /* Unsigned enum exact match */
    if (test_unsigned_enum(ENUM_A) != 1) return 40;
    if (test_unsigned_enum(ENUM_B) != 2) return 41;
    if (test_unsigned_enum(ENUM_C) != 3) return 42;

    return 0;
}
