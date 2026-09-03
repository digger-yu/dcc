/* ARM 后端结构体成员偏移修复测试 */
/* 验证非零偏移字段的读写正确性 */

struct S {
    char a;       /* offset 0 */
    int b;        /* offset 4 (aligned) */
    short c;      /* offset 8 */
    double d;     /* offset 16 (aligned) */
    char e;       /* offset 24 */
};

struct Nested {
    struct S inner;  /* offset 0 */
    int x;           /* offset 32 (after padding) */
};

int main(void) {
    struct S s;
    s.a = 42;
    s.b = 100;
    s.c = 200;
    s.d = 3.14;
    s.e = 99;

    if (s.a != 42) return 1;
    if (s.b != 100) return 2;
    if (s.c != 200) return 3;
    if ((int)s.d != 3) return 4;
    if (s.e != 99) return 5;

    /* 指针访问 */
    struct S *p = &s;
    if (p->a != 42) return 6;
    if (p->b != 100) return 7;
    if (p->c != 200) return 8;
    if (p->e != 99) return 9;

    /* 嵌套结构体 */
    struct Nested n;
    n.inner.a = 10;
    n.inner.b = 20;
    n.x = 30;
    if (n.inner.a != 10) return 10;
    if (n.inner.b != 20) return 11;
    if (n.x != 30) return 12;

    return 0;
}
