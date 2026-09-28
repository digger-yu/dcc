// Stage 4 交叉验证: 委托构造只执行一次成员初始化。
// GCC 实测委托到目标的 **C1 完整对象变体** (`S::C2(int)` 里 `call _ZN1SC1Ev`),
// 因为委托构造负责把整个对象 (含虚基/vptr) 建完; 且委托后本构造**不再**
// 重复跑成员初始化列表。
#include <cstdio>

static int p_ctor = 0;
static int p_dtor = 0;

struct P {
    P() { p_ctor++; }
    ~P() { p_dtor++; }
};

struct S {
    P p;
    int a;
    int b;
    S() : p(), a(1), b(2) {}
    S(int x) : S() { a = x; }
    S(int x, int y) : S(x) { b = y; }
};

int main() {
    S s1;
    S s2(7);
    S s3(7, 8);
    printf("s1=%d %d\n", s1.a, s1.b);
    printf("s2=%d %d\n", s2.a, s2.b);
    printf("s3=%d %d\n", s3.a, s3.b);
    printf("p_ctor=%d\n", p_ctor);
    { S s4(1, 2); printf("s4=%d %d\n", s4.a, s4.b); }
    printf("p_ctor=%d p_dtor=%d\n", p_ctor, p_dtor);
    return 0;
}
