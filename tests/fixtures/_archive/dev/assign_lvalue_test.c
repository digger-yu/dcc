// Test: pointer operations, array indexing, struct member access
// for aarch64/riscv64 codegen.
// Also works on x86_64 for verification.

int g_val = 42;

struct Point {
    int x;
    int y;
};

struct Rect {
    struct Point origin;
    int w;
    int h;
};

int test_basic_assign(void) {
    int a = 10;
    int b = 20;
    a = b;
    return a; // 20
}

int test_pointer_deref(void) {
    int x = 100;
    int *p = &x;
    *p = 200;
    return x; // 200
}

int test_array_index(void) {
    int arr[5];
    arr[0] = 10;
    arr[1] = 20;
    arr[2] = 30;
    arr[3] = 40;
    arr[4] = 50;
    return arr[2] + arr[4]; // 30 + 50 = 80
}

int test_struct_member(void) {
    struct Point p;
    p.x = 10;
    p.y = 20;
    return p.x + p.y; // 30
}

int test_nested_struct(void) {
    struct Rect r;
    r.origin.x = 1;
    r.origin.y = 2;
    r.w = 100;
    r.h = 200;
    return r.origin.x + r.origin.y + r.w + r.h; // 303
}

int test_pointer_arith(void) {
    int arr[3];
    arr[0] = 11;
    arr[1] = 22;
    arr[2] = 33;
    int *p = arr;
    return p[0] + p[1] + p[2]; // 66
}

int test_ptr_to_struct(void) {
    struct Point p;
    p.x = 5;
    p.y = 7;
    struct Point *sp = &p;
    return sp->x + sp->y; // 12
}

int test_global_ptr(void) {
    int *gp = &g_val;
    return *gp; // 42
}

int test_compound_index(void) {
    int arr[4];
    int i;
    i = 0;
    while (i < 4) {
        arr[i] = i * 10;
        i = i + 1;
    }
    return arr[0] + arr[1] + arr[2] + arr[3]; // 0+10+20+30 = 60
}

int main(void) {
    int r0 = test_basic_assign();
    int r1 = test_pointer_deref();
    int r2 = test_array_index();
    int r3 = test_struct_member();
    int r4 = test_nested_struct();
    int r5 = test_pointer_arith();
    int r6 = test_ptr_to_struct();
    int r7 = test_global_ptr();
    int r8 = test_compound_index();

    // Expected: 20 + 200 + 80 + 30 + 303 + 66 + 12 + 42 + 60 = 813
    int total = r0 + r1 + r2 + r3 + r4 + r5 + r6 + r7 + r8;
    return total;
}
