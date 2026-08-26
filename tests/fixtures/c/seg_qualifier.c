/* Test: __seg_fs / __seg_gs (and friends) address space qualifiers.
 *
 * x86 segment-register address space qualifiers are GCC extensions
 * used to access memory through a non-default segment register.
 * They are the qualifier form of "asm (..., "=m" (*(int __seg_gs *)p))"
 * and underpin Linux kernel per-cpu access (via %gs: prefix).
 *
 * dcc parses these qualifiers and silently skips them. They are NOT
 * recorded in the type system and do NOT influence code generation.
 * This means: the program compiles and runs as if no segment
 * qualifier was present, but the asm output uses default segment
 * (no %fs:/%gs: prefix). The kernel build can therefore compile
 * per-cpu declarations, but runtime behaviour is unchanged.
 *
 * Coverage:
 *   1. Decl-specifier position: __seg_gs int x;
 *   2. Function return type: __seg_gs int *foo(void);
 *   3. Function parameter: int foo(int * __seg_gs p);
 *   4. Type-to-star: int __seg_gs *p;
 *   5. Local variable: int __seg_gs *lp;
 *   6. Cast: (int __seg_gs *)p
 *   7. Struct field: struct { __seg_gs int a; }
 *   8. typedef: my_int_t __seg_gs *lp;
 *   9. typeof: __typeof__(int) __seg_gs *p;
 *  10. Storage class combo: static __seg_gs int v;
 *  11. Multiple qualifiers: int * __seg_fs __seg_gs p
 *  12. Combined with const: const int __seg_gs v;
 *  13. All x86 segment qualifiers: fs / gs / cs / ds / es / ss
 */

__seg_gs int gs_global = 0;
__seg_fs int fs_global = 0;

/* 1+2: decl-specifier position, function return type */
__seg_gs int * make_gs_ptr(void) {
    static int x = 42;
    return &x;
}

/* 3: function parameter (both styles) */
int use_param_fs(int * __seg_fs p) { return *p; }
int use_param_gs(int __seg_gs * p)  { return *p; }

/* 4+5: type-to-star and local variable */
int local_test(int *p) {
    int __seg_gs * lp = p;
    return *lp;
}

/* 6: cast */
int cast_test(int *p) {
    return *(int __seg_gs *)p;
}

/* 7: struct field */
struct with_seg {
    __seg_gs int a;
    int __seg_fs * b;
};

/* 8: typedef */
typedef int my_int_t;
int typedef_test(int *p) {
    my_int_t __seg_gs * lp = (my_int_t __seg_gs *)p;
    return *lp;
}

/* 9: typeof */
int typeof_test(void) {
    __typeof__(int) __seg_gs * p;
    static int x = 99;
    p = &x;
    return *p;
}

/* 10: storage class combo */
static __seg_gs int static_gs_var;

/* 11: multiple qualifiers */
int multi_seg(int * __seg_fs __seg_gs p) { return *p; }

/* 12: combined with const */
const int __seg_gs const_seg_var = 42;

/* 13: all x86 segment qualifiers (cs/ds/es/ss/fs/gs) */
int test_cs(int * __seg_cs p) { return *p; }
int test_ds(int * __seg_ds p) { return *p; }
int test_es(int * __seg_es p) { return *p; }
int test_ss(int * __seg_ss p) { return *p; }

/* 14: function pointer declarator with __seg_gs in * vs identifier */
int *(*make_fnptr(int init))(void) {
    static int x;
    x = init;
    int *(* __seg_gs fn)(void) = 0;
    (void)fn;
    return 0;
}

/* 15: function pointer return type with __seg_gs between ( and * */
int (*__seg_gs ret_fnptr(void))(int) {
    static int local = 99;
    (void)local;
    return 0;
}

/* 16: function pointer field declarator */
struct with_fnptr {
    int *(* __seg_gs fp)(void);
};

/* 17: sizeof / Alignof with __seg_gs in type-leading position */
int test_sizeof_seg(void) { return sizeof(__seg_gs int); }
int test_alignof_seg(void) { return _Alignof(__seg_gs int); }
int test_sizeof_seg_ptr(void) { return sizeof(__seg_gs int *); }


int main(void) {
    int x = 5;
    gs_global = 10;
    fs_global = 20;
    struct with_seg s;
    s.a = 30;
    s.b = &x;
    /* Sum is deterministic: only requires that reads/writes succeed. */
    return gs_global + fs_global + s.a + *s.b
        + use_param_fs(&x) + use_param_gs(&x)
        + local_test(&x) + cast_test(&x) + typeof_test()
        + multi_seg(&x) + const_seg_var
        + test_cs(&x) + test_ds(&x) + test_es(&x) + test_ss(&x)
        + *make_gs_ptr();
}
