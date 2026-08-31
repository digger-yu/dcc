/* C99 restrict qualifier semantics test */
#include <stdio.h>

/* Function with restrict parameters - pointers don't alias */
void add_arrays(int *restrict dst, const int *restrict src1, const int *restrict src2, int n) {
    for (int i = 0; i < n; i++) {
        dst[i] = src1[i] + src2[i];
    }
}

/* Function without restrict - compiler must assume pointers might alias */
void add_arrays_no_restrict(int *dst, const int *src1, const int *src2, int n) {
    for (int i = 0; i < n; i++) {
        dst[i] = src1[i] + src2[i];
    }
}

int main(void) {
    int a[4] = {1, 2, 3, 4};
    int b[4] = {10, 20, 30, 40};
    int c[4] = {0};
    int d[4] = {0};

    add_arrays(c, a, b, 4);
    add_arrays_no_restrict(d, a, b, 4);

    printf("c: %d %d %d %d\n", c[0], c[1], c[2], c[3]);
    printf("d: %d %d %d %d\n", d[0], d[1], d[2], d[3]);

    /* Verify results */
    int ok = 1;
    for (int i = 0; i < 4; i++) {
        if (c[i] != a[i] + b[i]) ok = 0;
        if (d[i] != a[i] + b[i]) ok = 0;
    }

    if (ok) {
        printf("PASS\n");
        return 0;
    } else {
        printf("FAIL\n");
        return 1;
    }
}
