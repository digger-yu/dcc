/* m3_4: typeof / __typeof__ operator */

/* typeof with type name */
static int typeof_type(void) {
    __typeof__(int) a = 42;
    __typeof__(long) b = 100L;
    __typeof__(a) c = a + b;
    return c;
}

/* typeof with expression */
static int typeof_expr(void) {
    int x = 10;
    __typeof__(x) y = 20;
    __typeof__(x + y) z = x + y;
    return z;
}

/* typeof with pointer */
static int typeof_ptr(void) {
    int val = 55;
    int *p = &val;
    __typeof__(p) q = &val;
    return *q;
}

/* typeof in struct context */
struct S { int a; long b; };
static long typeof_struct_field(void) {
    struct S s = {10, 20L};
    __typeof__(s.b) v = s.b + 1;
    return v;
}

/* typeof with array */
static int typeof_array(void) {
    int arr[] = {1, 2, 3, 4, 5};
    __typeof__(arr) arr2 = {10, 20, 30, 40, 50};
    return arr2[2];
}

/* typeof with function return type */
static double get_double(void) { return 3.14; }
static int typeof_fn_ret(void) {
    __typeof__(get_double()) v = get_double();
    /* just check sizeof matches double */
    return sizeof(v) == sizeof(double) ? 0 : -1;
}

/* typeof nested */
static int typeof_nested(void) {
    short s = 100;
    __typeof__(__typeof__(s)) t = s;
    return (int)t;
}

int main(void) {
    int fail = 0;

    if (typeof_type() != 142) fail |= 1;
    if (typeof_expr() != 30) fail |= 2;
    if (typeof_ptr() != 55) fail |= 4;
    if (typeof_struct_field() != 21L) fail |= 8;
    if (typeof_array() != 30) fail |= 16;
    if (typeof_fn_ret() != 0) fail |= 32;
    if (typeof_nested() != 100) fail |= 64;

    return fail;
}
