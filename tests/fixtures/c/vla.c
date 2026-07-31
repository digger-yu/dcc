/* m3_5: VLA (variable-length arrays) and zero-length arrays */

#include <stdlib.h>
#include <string.h>

/* basic VLA: allocate and fill */
static int vla_basic(int n) {
    int arr[n];
    int sum = 0;
    for (int i = 0; i < n; i++)
        arr[i] = i + 1;
    for (int i = 0; i < n; i++)
        sum += arr[i];
    return sum;
}

/* zero-length array in struct (GNU extension) */
struct ZeroLen {
    int count;
    int data[0];
};

/* flexible array member (C99) */
struct FlexArr {
    int count;
    int data[];
};

static int zero_len_test(void) {
    /* sizeof(struct ZeroLen) should == sizeof(int) */
    if (sizeof(struct ZeroLen) != sizeof(int)) return -1;

    /* allocate with extra space */
    struct ZeroLen *z = malloc(sizeof(struct ZeroLen) + 3 * sizeof(int));
    z->count = 3;
    z->data[0] = 10;
    z->data[1] = 20;
    z->data[2] = 30;
    int sum = z->data[0] + z->data[1] + z->data[2];
    free(z);
    return sum == 60 ? 0 : -2;
}

static int flex_arr_test(void) {
    /* sizeof(struct FlexArr) should == sizeof(int) */
    if (sizeof(struct FlexArr) != sizeof(int)) return -1;

    /* allocate with extra space */
    struct FlexArr *f = malloc(sizeof(struct FlexArr) + 4 * sizeof(int));
    f->count = 4;
    for (int i = 0; i < 4; i++)
        f->data[i] = (i + 1) * 10;
    int sum = f->data[0] + f->data[1] + f->data[2] + f->data[3];
    free(f);
    return sum == 100 ? 0 : -2;
}

int main(void) {
    int fail = 0;

    /* VLA basic: sum of 1..5 = 15 */
    if (vla_basic(5) != 15) fail |= 1;

    /* zero-length array */
    if (zero_len_test() != 0) fail |= 4;

    /* flexible array member */
    if (flex_arr_test() != 0) fail |= 8;

    return fail;
}
