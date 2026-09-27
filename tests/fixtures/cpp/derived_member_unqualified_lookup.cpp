// C++ [basic.lookup.unqual]/1 叠加 [class.member.lookup]/1: 成员函数体里的非限定
// 名字, 除词法作用域链外还必须在**当前类及其全部基类 (含虚基类, 逐层)** 中查找,
// 语义等价于隐式 `this->name`; 命中的成员若属于基类, 隐式 `this` 还必须转换到
// **该成员所属的子对象** (非虚基 = 编译期常量偏移, 虚基 = vbtable 运行期偏移)。
//
// 两个真实缺陷 (逐层暴露):
//   1) dcc 的 sema `lookup_unqualified` 只走词法作用域链, 派生类里 `return prot();`
//      直接报 `undeclared identifier: prot`;
//   2) 名字修好后 `this` 仍是最派生对象首地址, 于是 `C : A, B` 里裸调次基类的
//      `b1()` 按 B 的布局读 `this->y`, 实际读到了 A::x (实测 dcc 47 / g++ 43),
//      虚基类方法同理读到 0 (实测 dcc 100 / g++ 103)。
//
// 每个失败位对应一个子检查 (构造体里用显式构造赋值, 不依赖 NSDMI):
//   bit0 单继承链上的非限定方法调用 (跨 Mid → Base 两层)
//   bit1 单继承链上的非限定虚函数调用
//   bit2 基类字段的非限定读 + 写回 (含写后回读)
//   bit3 MI 次基类字段的非限定读
//   bit4 MI 次基类非虚方法 (this 折到次基子对象)
//   bit5 MI 次基类虚方法
//   bit6 虚基类字段的非限定读
//   bit7 虚基类虚方法 (this 经 vbtable 运行期调整)
// main 返回累计值, 0 = 全通过 (与 g++ 原生编译运行的退出码一致)。

struct Base {
    int n;
    Base() : n(0) {}
    virtual int tag() { return 1; }
    int prot() { return n + 10; }
};

struct Mid : Base {
    Mid() { n = 5; }
    int mid_only() { return 100; }
    int bump() { n = n + 1; return n; }
};

struct Der : Mid {
    int probe_prot() { return prot(); }   // 基类的基类: 15
    int probe_mid() { return mid_only(); } // 100
    int probe_tag() { return tag(); }      // 1, 虚派发
    int probe_field() { return n; }        // 5
    int probe_bump() { return bump(); }    // 6
};

// 多继承: 次基类 `Side` 不在对象头部, 非限定使用其成员必须同时改 this 与槽来源。
struct Side {
    int s;
    Side() : s(0) {}
    virtual int stag() { return 2; }
    int get_s() { return s + 20; }
};

struct Multi : Mid, Side {
    Multi() { s = 7; }
    int probe_side_field() { return s; }       // 7
    int probe_side_call() { return get_s(); }  // 27
    int probe_side_virt() { return stag(); }   // 2
    int probe_mid_again() { return prot(); }   // 15 (主基类链)
};

// 虚继承: 共享虚基子对象在对象尾部, 偏移只有运行期才知道。
// 注: 虚基子对象按 [class.base.init] 由**最派生类**初始化, 故 `v = 1` 写在
// `VDer` 的构造体里 —— 写在中间基类 `VMid` 构造体里需要 construction vtable
// (dcc 当前把 vbtable 槽按**各类自身**静态发射, 中间基类构造期读到的是自己的
// 那份 delta, 会写到别处; 那是 vtable 布局层的独立缺口, 不是名字查找问题)。
struct VB {
    int v;
    VB() : v(0) {}
    virtual int vfoo() { return v + 200; }
};

struct VMid : virtual VB {
    VMid() {}
};

struct VDer : VMid {
    VDer() { v = 1; }                      // 最派生类初始化虚基
    int probe_field() { return v; }        // 1
    int probe_virt() { return vfoo(); }    // 201
};

int main() {
    int r = 0;

    Der d;
    if (d.probe_prot() != 15 || d.probe_mid() != 100) r |= 1;
    if (d.probe_tag() != 1) r |= 2;
    if (d.probe_field() != 5) r |= 4;
    if (d.probe_bump() != 6) r |= 4; // 非限定写基类字段
    if (d.probe_field() != 6) r |= 4; // 写回必须落在 Base 子对象的 n
    if (d.prot() != 16) r |= 4;

    Multi m;
    if (m.probe_side_field() != 7) r |= 8;
    if (m.probe_side_call() != 27) r |= 16;
    if (m.probe_side_virt() != 2) r |= 32;
    if (m.probe_mid_again() != 15) r |= 1;

    VDer vd;
    if (vd.probe_field() != 1) r |= 64;
    if (vd.probe_virt() != 201) r |= 128;

    return r;
}
