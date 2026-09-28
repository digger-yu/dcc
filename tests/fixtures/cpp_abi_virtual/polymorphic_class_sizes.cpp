// Stage 5/6 ABI 交叉验证: 多态类的对象尺寸必须与 g++ 逐字节一致。
// vptr 槽与尾部填充都算进 sizeof; 少算会让栈槽/`new` 字节数不足,
// 构造函数写 vptr 直接越界 (曾实测 sizeof(S) dcc=1 / gcc=8)。
#include <cstdio>

struct Empty {
};

struct Plain {
    char c;
};

struct S {
    virtual int f() { return 1; }
};

struct U {
    virtual int f() { return 1; }
    int x;
};

struct Base {
    virtual void foo();
    int x;
};

struct Derived : virtual Base {
    int y;
};

struct V {
    virtual int fv();
    virtual int gv();
    int v;
};

struct M : virtual V {
    int m;
};

struct D : M {
    int d;
    virtual int fv() { return 107; }
};

struct NoV {
    int b;
};

struct Mid2 : virtual NoV {
    int m2;
    virtual int f() { return 3; }
};

int V::fv() { return 7; }
int V::gv() { return 8; }
void Base::foo() {}

int main() {
    printf("Empty=%zu\n", sizeof(Empty));
    printf("Plain=%zu\n", sizeof(Plain));
    printf("S=%zu\n", sizeof(S));
    printf("U=%zu\n", sizeof(U));
    printf("Base=%zu\n", sizeof(Base));
    printf("Derived=%zu\n", sizeof(Derived));
    printf("V=%zu\n", sizeof(V));
    printf("M=%zu\n", sizeof(M));
    printf("D=%zu\n", sizeof(D));
    printf("NoV=%zu\n", sizeof(NoV));
    printf("Mid2=%zu\n", sizeof(Mid2));
    return 0;
}
