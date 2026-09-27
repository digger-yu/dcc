// M54 缺陷 1/2/3 的命名空间作用域变体 + `extern` 声明/定义配对回归夹具:
// 命名空间里的全局数组同样必须把维度带进类型 (对象尺寸 = 元素 x 各维乘积),
// 下标访问可编译可执行, 静态初值真的发射, `extern` 声明与后文定义配对成同一
// 个对象 (而不是两个独立对象 / 丢掉初值)。
//
// 期望值先由 `g++ -std=c++20` 实测, 不臆造。

namespace ns {
int a[5];
int b[3] = {1, 2, 3};
constexpr int dim() {
  return 4;
}
char c[dim() + 2];
int m[2][3];
int mi[2][3] = {{1, 2, 3}, {4, 5, 6}};

// `extern` 声明在前, 定义在同命名空间后文 —— 必须是同一个对象。
extern int eg;
extern char ea[];
extern int em[][3];
int eg = 42;
char ea[4] = {9, 8, 7, 6};
int em[2][3] = {{1, 2, 3}, {4, 5, 6}};

struct Pair {
  int k;
  int v;
};
Pair pairs[2] = {{10, 11}, {20, 21}};
}

// 全局作用域的 `extern` 声明 + 后文定义
extern int gx;
extern int garr[];
int gx = 3;
int garr[4] = {5, 6, 7, 8};

static_assert(sizeof(ns::a) == 20, "命名空间数组维度");
static_assert(sizeof(ns::c) == 6, "constexpr 调用做维度");
static_assert(sizeof(ns::m) == 24, "二维命名空间数组");
static_assert(sizeof(garr) == 16, "extern 不完整数组由定义补全维度");
static_assert(sizeof(ns::ea) == 4, "命名空间 extern 数组维度补全");

int probe() {
  int r = 0;
  if (sizeof(ns::a) != 20) return 21;
  if (sizeof(ns::b) != 12) return 22;
  if (sizeof(ns::c) != 6) return 23;
  if (sizeof(ns::m) != 24) return 24;
  if (sizeof(ns::pairs) != 16) return 25;
  // 命名空间数组写入/读回, 步长必须是元素宽度
  ns::a[0] = 100;
  ns::a[4] = 400;
  if (ns::a[0] != 100) return 26;
  if (ns::a[4] != 400) return 27;
  if (ns::a[1] != 0 || ns::a[3] != 0) return 28;
  if (ns::b[0] != 1 || ns::b[2] != 3) return 29;
  if (&ns::a[3] - &ns::a[0] != 3) return 30;
  // 二维: 行主序步长 3*4=12
  ns::m[1][2] = 77;
  if (ns::m[1][2] != 77) return 31;
  if (ns::m[0][2] != 0 || ns::m[1][1] != 0) return 32;
  if (ns::mi[0][0] != 1 || ns::mi[1][2] != 6) return 33;
  if (ns::mi[1][0] != 4 || ns::mi[0][2] != 3) return 34;
  if (ns::c[5] != 0) return 35;
  ns::c[5] = 66;
  if (ns::c[5] != 66) return 36;
  if (ns::c[0] != 0) return 37;
  // 结构体数组对象
  if (ns::pairs[0].k != 10 || ns::pairs[1].v != 21) return 38;
  ns::pairs[1].v = 99;
  if (ns::pairs[1].v != 99 || ns::pairs[0].v != 11) return 39;
  // extern 声明与定义配对: 读到的是定义侧的初值
  if (ns::eg != 42) return 40;
  if (ns::ea[0] != 9 || ns::ea[3] != 6) return 41;
  if (ns::em[1][0] != 4) return 42;
  if (gx != 3) return 43;
  if (sizeof(garr) != 16) return 44;
  if (garr[0] != 5 || garr[3] != 8) return 45;
  garr[2] = 77;
  if (garr[2] != 77 || garr[1] != 6) return 46;
  return r;
}

int main() {
  return probe();
}
