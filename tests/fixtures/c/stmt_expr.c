/* m3_3: statement expressions ({ ... }) GNU extension */

/* basic statement expression returning a value */
#define MAX(a, b) ({    \
    __typeof__(a) _a = (a); \
    __typeof__(b) _b = (b); \
    _a > _b ? _a : _b;  \
})

#define MIN(a, b) ({    \
    __typeof__(a) _a = (a); \
    __typeof__(b) _b = (b); \
    _a < _b ? _a : _b;  \
})

/* nested statement expression */
static int nested_stmt(void) {
    return ({
        int x = 10;
        ({
            int y = 20;
            x + y;
        });
    });
}

/* statement expression with side effects */
static int side_effect(int *p) {
    int v = ({
        *p += 5;
        *p * 2;
    });
    return v;
}

/* statement expression in array index */
static int arr_index(void) {
    int arr[] = {10, 20, 30, 40, 50};
    return arr[({ int i = 2; i; })];
}

int main(void) {
    int fail = 0;

    /* MAX macro */
    if (MAX(3, 7) != 7) fail |= 1;
    if (MAX(10, 5) != 10) fail |= 2;

    /* MIN macro */
    if (MIN(3, 7) != 3) fail |= 4;
    if (MIN(10, 5) != 5) fail |= 8;

    /* nested */
    if (nested_stmt() != 30) fail |= 16;

    /* side effects */
    int x = 10;
    if (side_effect(&x) != 30) fail |= 32;
    if (x != 15) fail |= 64;

    /* array index via stmt expr */
    if (arr_index() != 30) fail |= 128;

    return fail;
}
