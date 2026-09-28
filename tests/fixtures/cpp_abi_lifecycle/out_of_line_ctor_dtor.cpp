// Stage 4 / 类外定义: 构造函数与析构函数的**类外定义** (out-of-line definition)
// —— `B::B(int a) : x(a) {}` / `B::~B() {}` 这类写法在 C++ 里与类内定义完全等价,
// 但语法上「没有 decl-specifier」: `B::` 是 nested-name-specifier, 紧跟的 `B` 是
// 声明符名字。早前解析器把整条 `B::B` 限定链贪心吞成返回类型, 于是在取名字处
// 撞上 `(` 失败, 整条定义被顶层容错降级成 Opaque —— 构造函数体与初始化列表
// 全部丢失 (此前无任何夹具覆盖该写法, 只在类内定义上验证过)。
//
// 观察值全部由 printf 打出, 与 g++ 逐行对比。
#include <cstdio>

// 1) 普通类: 类外构造 (带/不带初始化列表) + 类外析构
struct Plain {
  int x;
  int tag;
  Plain(int a);
  Plain() : x(0), tag(0) {}
  ~Plain();
};

Plain::Plain(int a) : x(a), tag(1) {}

Plain::~Plain() {
  printf("~Plain x=%d tag=%d\n", x, tag);
}

// 2) 命名空间内的类: 限定链更长 (`ns::Cls::Cls`)
namespace ns {
struct Cls {
  int v;
  Cls(int a);
  ~Cls();
};

Cls::Cls(int a) : v(a * 2) {}

Cls::~Cls() {
  printf("~Cls v=%d\n", v);
}
}  // namespace ns

// 3) 多态类: 类外定义的虚析构必须进 vtable (虚表槽指向类外定义的函数体)
struct Poly {
  int id;
  Poly(int a);
  virtual ~Poly();
  virtual int value() {
    return id + 1;
  }
};

Poly::Poly(int a) : id(a) {}

Poly::~Poly() {
  printf("~Poly id=%d\n", id);
}

struct Derived : Poly {
  int extra;
  Derived(int a, int b);
  ~Derived() override;
  int value() override {
    return id + extra;
  }
};

Derived::Derived(int a, int b) : Poly(a), extra(b) {}

Derived::~Derived() {
  printf("~Derived extra=%d\n", extra);
}

// 4) 委托构造在类外定义: `Dtor::Dtor() : Dtor(7) {}` 形式
struct Chain {
  int n;
  Chain();
  Chain(int a);
};

Chain::Chain(int a) : n(a) {}

Chain::Chain() : Chain(7) {}

int main() {
  Plain p(3);
  printf("p.x=%d p.tag=%d\n", p.x, p.tag);
  Plain q;
  printf("q.x=%d q.tag=%d\n", q.x, q.tag);

  ns::Cls c(5);
  printf("c.v=%d\n", c.v);

  Poly* poly = new Derived(10, 20);
  printf("poly->value=%d\n", poly->value());
  delete poly;

  Chain ch;
  printf("ch.n=%d\n", ch.n);
  Chain ch2(9);
  printf("ch2.n=%d\n", ch2.n);

  Poly stack_poly(1);
  Poly* via_base = &stack_poly;
  printf("via_base->value=%d\n", via_base->value());
  return 0;
}
