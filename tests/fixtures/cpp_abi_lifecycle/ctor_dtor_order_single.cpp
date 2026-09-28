// Stage 4 交叉验证: 单继承的构造顺序 (基→成员→派生体) 与析构逆序。
#include <cstdio>

static char trace[64];
static int tn = 0;
static void note(char c) { if (tn < 62) trace[tn++] = c; trace[tn] = 0; }

struct Base {
    int b;
    Base() { note('b'); b = 1; }
    ~Base() { note('B'); }
};

struct Mid : Base {
    int m;
    Mid() : Base() { note('m'); m = 2; }
    ~Mid() { note('M'); }
};

struct Leaf : Mid {
    int l;
    Leaf() { note('l'); l = 3; }
    ~Leaf() { note('L'); }
};

int main() {
    {
        Leaf x;
        note('X');
        if (x.b != 1 || x.m != 2 || x.l != 3) note('?');
    }
    printf("order=%s\n", trace);
    return 0;
}
