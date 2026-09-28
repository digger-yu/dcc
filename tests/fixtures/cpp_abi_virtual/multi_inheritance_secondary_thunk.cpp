// Stage 5 ABI 交叉验证: 多继承下 secondary vtable 槽必须指向 this-adjust
// thunk, 不能直接指向最终覆写实现。
// 反例 (修复前实测): `B* pb = &x; pb->fb()` 拿着 B 子对象的 this 去读
// `C::c` 所在的偏移, gcc 220 / dcc 110。
#include <cstdio>

struct A {
    virtual int fa();
    int a;
};

struct B {
    virtual int fb();
    double b;
};

struct C : A, B {
    virtual int fa() { return 110; }
    virtual int fb() { return (int)(b * 10) + c; }
    int c;
};

int A::fa() { return 1; }
int B::fb() { return 2; }

int main() {
    C x;
    x.a = 7;
    x.b = 22.0;
    x.c = 3;
    A* pa = &x;
    B* pb = &x;
    C* pc = &x;
    printf("fa=%d\n", pa->fa());
    printf("fb=%d\n", pb->fb());
    printf("fb_via_c=%d\n", pc->fb());
    // 基类指针各自调到自己的子对象, 读自己的字段
    pa->a = 8;
    pb->b = 33.0;
    printf("a=%d b=%d c=%d\n", x.a, (int)x.b, x.c);
    printf("sizeA=%zu sizeB=%zu sizeC=%zu\n", sizeof(A), sizeof(B), sizeof(C));
    return 0;
}
