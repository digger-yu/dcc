/* Test: GNU range initializer [first ... last] = val */
/* GCC extension commonly used in Linux kernel */

/* Basic range initializer in array */
int arr1[10] = {
    [0] = 1,
    [1 ... 5] = 2,
    [6 ... 9] = 3,
};

/* Range initializer with single value */
int arr2[8] = {
    [0 ... 7] = 42,
};

/* Mixed designated and range initializers */
int arr3[20] = {
    [0] = 10,
    [1 ... 3] = 20,
    [5] = 30,
    [10 ... 15] = 40,
    [19] = 50,
};

/* Range initializer in struct array */
struct point {
    int x;
    int y;
};

struct point pts[5] = {
    [0 ... 4] = { .x = 1, .y = 2 },
};

/* Nested range in inner array */
int matrix[3][4] = {
    [0 ... 2][0 ... 3] = 7,
};

/* Range with zero fill */
int sparse[100] = {
    [0] = 1,
    [50] = 2,
    [99] = 3,
};

/* Verify values at runtime */
int printf(const char *, ...);
int main(void) {
    /* Check arr1 */
    if (arr1[0] != 1) return 1;
    if (arr1[1] != 2) return 2;
    if (arr1[5] != 2) return 3;
    if (arr1[6] != 3) return 4;
    if (arr1[9] != 3) return 5;

    /* Check arr2 */
    if (arr2[0] != 42) return 6;
    if (arr2[7] != 42) return 7;

    /* Check arr3 */
    if (arr3[0] != 10) return 8;
    if (arr3[2] != 20) return 9;
    if (arr3[5] != 30) return 10;
    if (arr3[12] != 40) return 11;
    if (arr3[19] != 50) return 12;

    return 0;
}
