// Stage 6 ABI 交叉验证: 菱形虚继承下虚基只有**一个共享实例**, 且它被抬到
// 最派生对象尾部; 经由不同路径拿到的虚基指针必须相等。
// 同时验证「非虚基类的虚基」被派生时不会复制两份 (实测 sizeof(D)=56 vs gcc 32)。
#include <cstdio>

struct V {
    virtual int fv() { return 1; }
    int v;
};

struct M : virtual V {
    int m;
};

struct D : M {
    int d;
    virtual int fv() { return 2; }
};

struct E : M {
    int e1, e2, e3, e4;
};

struct Left : virtual V {
    int l;
};

struct Right : virtual V {
    int r;
};

struct Diamond : Left, Right {
    int dd;
};

int main() {
    D a;
    E b;
    Diamond c;
    a.v = 1;
    b.v = 2;
    c.v = 3;
    printf("sizeD=%zu sizeE=%zu sizeDiamond=%zu\n", sizeof(D), sizeof(E), sizeof(Diamond));
    printf("D offV=%ld\n", (char*)(V*)&a - (char*)&a);
    printf("E offV=%ld off_e1=%ld\n", (char*)(V*)&b - (char*)&b, (char*)&b.e1 - (char*)&b);
    V* from_left = (Left*)&c;
    V* from_right = (Right*)&c;
    V* direct = &c;
    printf("shared=%d\n", (int)((char*)from_left == (char*)from_right && (char*)from_left == (char*)direct));
    printf("off_direct=%ld off_left=%ld off_right=%ld\n",
        (char*)direct - (char*)&c, (char*)from_left - (char*)&c, (char*)from_right - (char*)&c);
    printf("fv_d=%d fv_e=%d fv_c=%d\n", ((V*)&a)->fv(), ((V*)&b)->fv(), direct->fv());
    c.l = 4;
    c.r = 5;
    c.dd = 6;
    printf("l=%d r=%d dd=%d v=%d\n", c.l, c.r, c.dd, c.v);
    return 0;
}
