// Stage 4 交叉验证: 合成拷贝构造/拷贝赋值按成员逐个复制; const 左值走拷贝。
#include <cstdio>

struct Leaf {
    int v;
    Leaf(int x) : v(x) {}
};

struct Mid {
    int a;
    int b;
    Mid() : a(0), b(0) {}
    Mid(int x) : a(x), b(x * 2) {}
};

struct Pair {
    Mid m;
    Leaf l;
    int tail;
    Pair() : m(), l(0), tail(0) {}
    Pair(int x) : m(x), l(x + 1), tail(x + 2) {}
};

int main() {
    Pair p1(5);
    Pair p2(p1);            // 拷贝构造
    printf("p2=%d %d %d %d\n", p2.m.a, p2.m.b, p2.l.v, p2.tail);
    Pair p3;
    p3 = p1;                // 拷贝赋值
    printf("p3=%d %d %d %d\n", p3.m.a, p3.m.b, p3.l.v, p3.tail);
    const Pair p4(9);
    Pair p5(p4);
    printf("p5=%d %d %d %d\n", p5.m.a, p5.m.b, p5.l.v, p5.tail);
    return 0;
}
