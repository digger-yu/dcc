// `delete p` 的析构语义 (C++ [expr.delete]/3): 对**完整对象**且其类需要析构时,
// 必须先按点型的静态类型调用析构函数, 再把存储交还 `operator delete`:
//   - 点型是多态类 → 析构调用必须是**虚派发** (从对象 vptr 的析构槽取实现),
//     于是 `delete (Base*)pd` 会跑最派生类的 `~Derived`, 再由它链到 `~Base`;
//   - 点型非多态 → 直调本类的 complete 析构。
// dcc 早期两条都缺: `lower_delete` 只发 `operator delete(void*)`, 析构整体不跑
// (实测 `delete pb` 之后计数全局仍是 0), 且 `new Derived` 不写 vptr。
//
// 计数全局刻意**紧邻声明**: 窄对象 (`int`, 4 字节) 的回写一旦被发射成 8 字节
// store, `g_dtor_b = g_dtor_b + 1` 就会把相邻的 `g_dtor_d` 高位清零 —— 表现为
// "派生类析构没跑" 的假象。故本夹具同时是 store 宽度真源的运行期回归。
//
// 每位失败对应一个子检查, main 返回累计值 (0 = 全通过, 与 g++ 原生一致)。

int g_dtor_d = 0;
int g_dtor_b = 0; // 紧邻 g_dtor_d: 8 字节 store 会互相踩 (store 宽度回归)
int g_dtor_plain = 0;
int g_ctor_d = 0;

struct Base {
    virtual ~Base() { g_dtor_b = g_dtor_b + 1; }
    virtual int tag() { return 1; }
};

struct Derived : Base {
    Derived() { g_ctor_d = g_ctor_d + 1; }
    ~Derived() override { g_dtor_d = g_dtor_d + 1; }
    int tag() override { return 2; }
};

// 非多态类: `delete` 必须直调本类 complete 析构 (无 vptr 可查)。
struct Plain {
    int v;
    ~Plain() { g_dtor_plain = g_dtor_plain + 1; }
};

int main() {
    int r = 0;

    // 1) 多态点型: `delete pb` (pb 静态类型 Base*, 动态类型 Derived*)
    Derived* pd = new Derived();
    if (g_ctor_d != 1) r |= 1; // new 必须跑默认构造 (vptr 由此写入)
    if (pd->tag() != 2) r |= 2; // vptr 未初始化时这一句即读不到派生实现
    Base* pb = pd;
    delete pb;
    if (g_dtor_d != 1) r |= 4; // ~Derived 必须经虚派发执行
    if (g_dtor_b != 1) r |= 8; // ~Base 由 ~Derived 链上执行

    // 2) 派生点型: `delete pd2` 同样只完整析构一次 (不双跑基类)
    Derived* pd2 = new Derived();
    delete pd2;
    if (g_dtor_d != 2) r |= 16;
    if (g_dtor_b != 2) r |= 32;

    // 3) 非多态点型: 直调本类析构
    Plain* pp = new Plain();
    pp->v = 5;
    delete pp;
    if (g_dtor_plain != 1) r |= 64;

    // 4) 计数全局未互相踩 (store 宽度真源): 上面各值必须仍然精确
    if (g_ctor_d != 2) r |= 128;

    return r;
}
