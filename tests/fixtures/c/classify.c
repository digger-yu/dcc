/* builtin_classify_compat.c - 验证 classify_type / types_compatible_p */

struct s { int a; };
union u { int a; int b; };
typedef int my_int;
typedef int *my_int_ptr;
typedef int (*my_fn_ptr)(void);

int main(void) {
    int c1 = __builtin_classify_type(void);                /* 4 */
    int c2 = __builtin_classify_type(int);                 /* 1 */
    int c3 = __builtin_classify_type(char);                /* 1 */
    int c4 = __builtin_classify_type(long long);           /* 1 */
    int c5 = __builtin_classify_type(struct s);            /* 12 */
    int c6 = __builtin_classify_type(union u);              /* 13 */
    int c7 = __builtin_classify_type(int *);                /* 5 */
    int c8 = __builtin_classify_type(struct s *);          /* 5 */
    int c9 = __builtin_classify_type(double);              /* 8 */
    int c10 = __builtin_classify_type(float);              /* 8 */
    int c11 = __builtin_classify_type(_Bool);              /* 1 */
    int c12 = __builtin_classify_type(my_int_ptr);         /* 5 */
    int c13 = __builtin_classify_type(my_fn_ptr);          /* 5 (函数指针) */

    int k1 = __builtin_types_compatible_p(int, int);       /* 1 */
    int k2 = __builtin_types_compatible_p(int, long);      /* 0 */
    int k3 = __builtin_types_compatible_p(int, const int); /* 1 (cv 忽略) */
    int k4 = __builtin_types_compatible_p(int *, int *);   /* 1 */
    int k5 = __builtin_types_compatible_p(my_int, int);    /* 1 */
    int k6 = __builtin_types_compatible_p(my_fn_ptr,
                                           my_fn_ptr);      /* 1 */

    if (c1 != 4) return 11;
    if (c2 != 1) return 12;
    if (c3 != 1) return 13;
    if (c4 != 1) return 14;
    if (c5 != 12) return 15;
    if (c6 != 13) return 16;
    if (c7 != 5) return 17;
    if (c8 != 5) return 18;
    if (c9 != 8) return 19;
    if (c10 != 8) return 20;
    if (c11 != 1) return 21;
    if (c12 != 5) return 22;
    if (c13 != 5) return 23;
    if (k1 != 1) return 31;
    if (k2 != 0) return 32;
    if (k3 != 1) return 33;
    if (k4 != 1) return 34;
    if (k5 != 1) return 35;
    if (k6 != 1) return 36;
    return 0;
}
