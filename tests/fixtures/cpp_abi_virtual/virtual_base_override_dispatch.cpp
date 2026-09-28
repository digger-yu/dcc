// Stage 6 ABI 交叉验证: 通过虚基指针的虚派发必须绑到最派生的覆写者。
// 虚基子对象在完整对象里要有**自己的** vptr, 由最派生构造函数指向
// 「本类为该链准备的 secondary vftable」; 缺了它就只能调到 V 自己的实现
// (实测 gcc 107 / dcc 7)。覆写槽还需要 this-adjust thunk 把 this 从
// V 子对象调回完整对象开头。
#include <cstdio>

struct V {
    virtual int fv();
    virtual int gv() { return 8; }
    int v;
};

struct M : virtual V {
    int m;
};

struct D : M {
    int d;
    virtual int fv() { return 100 + v + m + d; }
};

int V::fv() { return 7; }

int main() {
    D obj;
    obj.v = 1;
    obj.m = 2;
    obj.d = 3;
    V* pv = &obj;
    M* pm = &obj;
    D* pd = &obj;
    printf("viaV fv=%d gv=%d\n", pv->fv(), pv->gv());
    printf("viaM fv=%d\n", pm->fv());
    printf("viaD fv=%d\n", pd->fv());
    printf("offV=%ld offM_top=0\n", (char*)(V*)&obj - (char*)&obj);
    // 独立 V 对象仍走 V 自己的表
    V solo;
    solo.v = 40;
    V* ps = &solo;
    printf("solo fv=%d gv=%d\n", ps->fv(), ps->gv());
    return 0;
}
