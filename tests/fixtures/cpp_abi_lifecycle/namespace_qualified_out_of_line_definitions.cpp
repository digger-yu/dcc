// Stage 2/4 交叉验证: **多段限定名**的类外定义。
//
// 两条缺口都由本夹具钉住 (此前 native 路径直接拒绝整份 TU):
// 1. 解析器只认 `Class::method(` 一段限定, 于是 `ns::Class::method()` /
//    `ns::Class::~Class()` 掉出「函数 vs 变量」分类器, 整条声明降级成 Opaque;
// 2. sema 把 `ns::f(...)` 的限定前缀当成类名, 命名空间作用域自由函数的类外定义
//    报 `unknown class 'ns'`。
// 现在解析按 [basic.lookup.qual] 逐段走嵌套名说明符, sema 按 [basic.def.outside]
// 区分「类的成员」与「命名空间的自由函数」, 并复用命名空间里那条声明的符号。
#include <cstdio>

namespace geo {
struct Point {
  int x;
  int y;
  Point() : x(0), y(0) {}
  int sum() const;
  int shifted(int k) const;
  ~Point();
};

// 命名空间作用域自由函数: 类内声明, 类外定义。
int total(const Point& p);
}  // namespace geo

// 类外定义: 命名空间里的类的成员 (两段限定)。
int geo::Point::sum() const { return x + y; }

// 带形参的 const 成员函数。
int geo::Point::shifted(int k) const { return x + y + k; }

// 类外析构定义 (同样走多段限定名的解析路径)。
geo::Point::~Point() { printf("dtor"); }

// 命名空间作用域自由函数的类外定义。
int geo::total(const geo::Point& p) { return p.sum() * 2; }

// 三段限定名: 嵌套命名空间里的类。
namespace outer {
namespace inner {
struct Deep {
  int v;
  Deep(int seed);
  int twice() const;
};
int bonus();
}  // namespace inner
}  // namespace outer

outer::inner::Deep::Deep(int seed) : v(seed + 1) {}
int outer::inner::Deep::twice() const { return v * 2; }
int outer::inner::bonus() { return 5; }

int main() {
  geo::Point p;
  printf("sum=%d\n", p.sum());
  printf("shifted=%d\n", p.shifted(10));
  printf("total=%d\n", geo::total(p));
  outer::inner::Deep d(3);
  printf("twice=%d bonus=%d\n", d.twice(), outer::inner::bonus());
  return 0;
}
