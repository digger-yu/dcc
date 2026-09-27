// 构造/析构期虚派发: C++ 标准 [class.base.init]/13、[basic.life]/5 要求, 在
// 某个类 C 的构造函数 / 析构函数体内**直接或间接**执行的虚调用, 必须绑定到
// **当前正在构造/析构的类 C** 的 vtable, 而非对象的最派生类. Itanium 的实现手
// 段是: 进入 Cx 的 ctor/dtor 体前把对象的 vptr 暂置为 C 自己的 vtable, 体执行
// 完 (或析构进入时) 再由基类子对象的 ctor/dtor 覆盖. dcc 在 sema 层对 ctor 的
// 完整/基子对象变体、对 dtor 体入口分别发射 SetVTablePtr{class=C}.
//
// 每个子检查失败置一位, main 返回累计值 (0 = 全部通过, 与 g++ 原生一致).

// === 构造期: 三层 Base -> Mid -> Leaf, 每层 ctor 体调用 who() 记录绑定的层号 ===
int g_base_ctor_probe; // Base ctor 体内 who() 结果 (期望 1: 绑定 Base)
int g_mid_ctor_probe;  // Mid  ctor 体内 who() 结果 (期望 2: 绑定 Mid)
int g_leaf_ctor_probe; // Leaf ctor 体内 who() 结果 (期望 3: 绑定 Leaf)

struct Base {
    virtual int who() { return 1; }
    Base() { g_base_ctor_probe = who(); }
    virtual ~Base() {}
};

struct Mid : Base {
    int who() override { return 2; }
    Mid() { g_mid_ctor_probe = who(); }
};

struct Leaf : Mid {
    int who() override { return 3; }
    Leaf() { g_leaf_ctor_probe = who(); }
};

// === 析构期: 两层 D0 -> D1, 析构进入时 vptr 暂置为当前类, who() 应绑定当前层 ===
// 单个累加器全局: ~D1 (最派生) 先执行 -> 追加 2, 再 ~D0 -> 追加 1, 期望 21.
// 这验证析构体里对**基类虚函数**的调用绑定基类而非派生类 (D1 已析构后, ~D0
// 里 who() 必须返回 1, 而不是派生 D1 的 2).
int g_dtor_seq;

struct D0 {
    virtual int who() { return 1; }
    D0() {}
    virtual ~D0() { g_dtor_seq = g_dtor_seq * 10 + who(); }
};

struct D1 : D0 {
    int who() override { return 2; }
    ~D1() override { g_dtor_seq = g_dtor_seq * 10 + who(); }
};

int main() {
    int r = 0;

    // 构造期: 进入作用域时按 Base->Mid->Leaf 逐层构造, 每层 who() 绑定当层.
    {
        Leaf l;
        (void)l;
    }
    if (g_base_ctor_probe != 1) r |= 1;
    if (g_mid_ctor_probe != 2) r |= 2;
    if (g_leaf_ctor_probe != 3) r |= 4;

    // 析构期: 离开作用域时按 ~D1->~D0 逆序析构.
    {
        D1 d;
        (void)d;
    }
    // ~D1: seq = 0*10 + 2 = 2;  ~D0: seq = 2*10 + 1 = 21.
    if (g_dtor_seq != 21) r |= 8;

    return r;
}
