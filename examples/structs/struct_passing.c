/* struct 传参/赋值/返回 测试 */

struct Point {
    int x;
    int y;
};

struct Big {
    int a;
    int b;
    int c;
    int d;
};

/* struct 返回 (<=8 字节, 在 rax 中) */
struct Point make_point(int x, int y) {
    struct Point p;
    p.x = x;
    p.y = y;
    return p;
}

/* struct 传参 (<=8 字节, 一个寄存器) */
int point_sum(struct Point p) {
    return p.x + p.y;
}

/* struct 赋值 (整体拷贝) */
struct Point copy_point(struct Point src) {
    struct Point dst;
    dst = src;
    return dst;
}

/* 大 struct 返回 (>8 字节, rax:rdx) */
struct Big make_big(int a, int b, int c, int d) {
    struct Big r;
    r.a = a;
    r.b = b;
    r.c = c;
    r.d = d;
    return r;
}

/* 大 struct 赋值 */
int big_sum(struct Big b) {
    return b.a + b.b + b.c + b.d;
}

int main(void) {
    int fail = 0;

    /* 测试 1: struct 返回 + 成员访问 */
    struct Point p1 = make_point(3, 4);
    if (p1.x != 3) fail = fail + 1;
    if (p1.y != 4) fail = fail + 2;

    /* 测试 2: struct 传参 */
    int s = point_sum(p1);
    if (s != 7) fail = fail + 4;

    /* 测试 3: struct 赋值拷贝 */
    struct Point p2 = copy_point(p1);
    if (p2.x != 3) fail = fail + 8;
    if (p2.y != 4) fail = fail + 16;

    /* 测试 4: 大 struct 返回 */
    struct Big b1 = make_big(10, 20, 30, 40);
    if (b1.a != 10) fail = fail + 32;
    if (b1.d != 40) fail = fail + 64;

    /* 测试 5: 大 struct 传参 */
    int bs = big_sum(b1);
    if (bs != 100) fail = fail + 128;

    return fail; /* 0 = 全通过 */
}
