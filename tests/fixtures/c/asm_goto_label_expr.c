/* Test: asm goto extended label expressions
 * Verifies support for %c[label] and label+offset expressions.
 */

/* Basic asm goto with %l[name] (existing functionality) */
int test_basic_label_ref(int x) {
    asm goto("test %0, %0\n\t"
             "jnz %l[positive]"
             :
             : "r"(x)
             : "cc"
             : positive);
    return 0;

positive:
    return 1;
}

/* %c[label] - strip immediate prefix, resolve to label */
int test_c_label_ref(int x) {
    asm goto("test %0, %0\n\t"
             "jnz %c[target]"
             :
             : "r"(x)
             : "cc"
             : target);
    return -1;

target:
    return 42;
}

/* Multiple goto labels */
int test_multi_labels(int x) {
    int result = 0;
    asm goto("nop\n\t"
             "test %1, %1\n\t"
             "jz %l[case_zero]\n\t"
             "jmp %l[case_nonzero]"
             :
             : "r"(result), "r"(x)
             : "cc"
             : case_zero, case_nonzero);
    return -1;

case_zero:
    return 0;

case_nonzero:
    return 1;
}

int main(void) {
    int r1 = test_basic_label_ref(5);
    if (r1 != 1) return 1;

    int r1b = test_basic_label_ref(0);
    if (r1b != 0) return 11;

    int r2 = test_c_label_ref(5);
    if (r2 != 42) return 2;

    int r2b = test_c_label_ref(0);
    if (r2b != -1) return 22;

    int r3 = test_multi_labels(0);
    if (r3 != 0) return 3;

    int r4 = test_multi_labels(5);
    if (r4 != 1) return 4;

    return 0;
}
