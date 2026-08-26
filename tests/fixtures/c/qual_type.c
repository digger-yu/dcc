/* QualType: const/volatile/restrict qualifier tests */

/* const-qualified scalar */
const int ci = 42;

/* volatile-qualified scalar (hardware register) */
volatile int hw_reg = 0;

/* const pointer to int */
int *const cptr = (int *)0;

/* pointer to const int */
const int *pic = &ci;

/* volatile pointer to int */
int *volatile vptr = (int *)0;

/* pointer to volatile int */
volatile int *pvi = &hw_reg;

/* const restrict pointer (multi-qualifier) */
int *const restrict crrptr = (int *)0;

/* multi-level pointer qualifiers: const int * volatile * */
static int multi_qual_buf = 99;
static volatile int *multi_qual_inner = &multi_qual_buf;
static volatile int *const *multi_qual = &multi_qual_inner;

/* function with const-qualified parameter */
static int func_const_param(const int x) {
    return x + 1;
}

/* function returning pointer to const */
static const int *func_ret_const_ptr(void) {
    return &ci;
}

/* volatile struct member */
struct hw_device {
    volatile int status;
    volatile int data;
    const int readonly_id;
};

/* restrict-qualified pointer parameter (C99 alias hint) */
static void copy_restrict(int *restrict dst, const int *restrict src, int n) {
    for (int i = 0; i < n; i++) {
        dst[i] = src[i];
    }
}

/* volatile in compound type: array of volatile int */
volatile int vol_array[4];

/* const struct field access */
static int read_const_field(const struct hw_device *dev) {
    return dev->readonly_id;
}

/* volatile read must not be optimized away */
static int read_volatile_reg(void) {
    return hw_reg;
}

/* volatile write must not be eliminated */
static void write_volatile_reg(int val) {
    hw_reg = val;
}

int main(void) {
    struct hw_device dev;
    dev.status = 1;
    dev.data = 0xFF;
    dev.readonly_id = 7;

    /* const access */
    int a = ci + func_const_param(10);

    /* volatile round-trip */
    write_volatile_reg(a);
    int b = read_volatile_reg();

    /* restrict copy */
    int src[3] = {1, 2, 3};
    int dst[3];
    copy_restrict(dst, src, 3);

    /* multi-qualifier deref */
    int c = **multi_qual;

    /* struct volatile field */
    int d = dev.status;

    return (a + b + c + d + dst[0]) - (a + b + c + d + dst[0]);
}
