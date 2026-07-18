/* builtin_choose.c - 验证 choose_expr + object_size + constant_p */

int main(void) {
    /* choose_expr */
    int t1 = __builtin_choose_expr(1, 100, 200);   /* 100 */
    int t2 = __builtin_choose_expr(0, 100, 200);   /* 200 */
    int t3 = __builtin_choose_expr(2 - 2, 300, 400); /* 400 */

    /* constant_p */
    int cp1 = __builtin_constant_p(42);            /* 1 */
    int cp2 = __builtin_constant_p(1 + 2);         /* 1 */
    volatile int v;
    int cp3 = __builtin_constant_p(v);             /* 0 (volatile) */

    /* object_size */
    char buf[64];
    int os1 = __builtin_object_size(buf, 0);   /* 64 (整个对象) */
    int os2 = __builtin_object_size(buf, 1);   /* 1  (mode 1: 子对象=元素) */
    int os3 = __builtin_object_size(buf, 2);   /* 64 (mode 2: 严格整个) */
    int os4 = __builtin_object_size("abcd", 0);    /* 5 (含 '\0') */
    int os5 = __builtin_object_size("abcd", 1);    /* 1 (mode 1: 元素大小) */
    int os6 = __builtin_object_size("abcd", 2);    /* 5 */
    int os7 = __builtin_object_size("abcd", 3);    /* 1 (mode 3: 元素大小) */
    int os8 = __builtin_object_size(buf + 1, 0);   /* -1 (偏移表达式未知) */

    if (t1 != 100) return 1;
    if (t2 != 200) return 2;
    if (t3 != 400) return 3;
    if (cp1 != 1) return 4;
    if (cp2 != 1) return 5;
    if (cp3 != 0) return 6;
    if (os1 != 64) return 7;
    if (os2 != 1) return 8;
    if (os3 != 64) return 9;
    if (os4 != 5) return 10;
    if (os5 != 1) return 11;
    if (os6 != 5) return 12;
    if (os7 != 1) return 13;
    if (os8 != -1) return 14;
    return 0;
}
