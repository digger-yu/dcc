struct bits {
    int a : 3;
    int b : 5;
    int c;
};

int main(void) {
    struct bits s;
    /* a: 3位有符号 (-4~3), b: 5位有符号 (-16~15) */
    s.a = 3;
    s.b = 15;
    s.c = 42;
    if (s.a != 3) return 1;
    if (s.b != 15) return 2;
    if (s.c != 42) return 3;
    /* 负数测试 */
    s.a = -1;
    s.b = -8;
    if (s.a != -1) return 4;
    if (s.b != -8) return 5;
    /* 边界值 */
    s.a = 0;
    s.b = 0;
    if (s.a != 0) return 6;
    if (s.b != 0) return 7;
    return 0;
}
