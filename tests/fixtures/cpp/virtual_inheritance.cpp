// m26_virtual_inheritance.cpp - 菱形虚继承 Itanium ABI 布局运行时验证
// 验证: 虚基子对象嵌入对象尾部 (单一共享实例)、__vbptr 指向 __vbt_<C>
// vbtable, 成员访问与上转型经 vbtable this-adjust.

struct Base {
    int b;
    Base() { b = 42; }
};

struct Mid1 : virtual Base {
    int m1;
};

struct Mid2 : virtual Base {
    int m2;
};

struct Final : virtual Mid1, virtual Mid2 {
    int f;
};

int main() {
    Final fo;
    // 共享虚基 Base 由 Final 的自动 ctor 初始化一次 (b = 42)
    if (fo.b != 42) return 1;
    fo.b = 7;
    fo.m1 = 1;
    fo.m2 = 2;
    fo.f = 3;
    if (fo.b != 7) return 2;
    if (fo.m1 != 1) return 3;
    if (fo.m2 != 2) return 4;
    if (fo.f != 3) return 5;
    // 上转型 this-adjust: (Base*)&fo 必须指向 Base 虚基子对象 (尾部),
    // 而非对象头部
    Base* pb = (Base*)&fo;
    if (pb->b != 7) return 6;
    if ((char*)pb == (char*)&fo) return 7;
    // vbtable 调整后的指针必须与对象内的 __vb_Base 子对象地址一致
    if ((char*)pb != (char*)&fo.__vb_Base) return 8;
    // Mid1 作为虚基: (Mid1*)&fo 也需调整到 Mid1 子对象
    Mid1* pm = (Mid1*)&fo;
    if (pm->m1 != 1) return 9;
    if ((char*)pm != (char*)&fo.__vb_Mid1) return 10;
    return 0;
}
