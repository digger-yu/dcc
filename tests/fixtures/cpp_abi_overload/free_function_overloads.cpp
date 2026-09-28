// Stage 2/4 交叉验证: 自由函数重载的**链接名**与**候选选定**。
//
// 这里同时锁两条独立的行为 (任一条缺失都会让另一条看起来"正常"而实际错):
//   1. 同名异签的自由函数 (含命名空间内的) 必须得到互不相同的链接名 —— 否则
//      两份定义生成同名符号 (链接期 multiple definition), 且 ELF 层按名字索引的
//      labels/func_cfi/eh_funcs 会让后一份覆盖前一份, 前一份成为不可达死代码。
//   2. 调用点必须按实参类型序列选中正确候选 —— 名字查找只能拿到重载集的第一个
//      候选, `f(1.0)` 会调进 `f(int)` (浮点→整数转换被当成可用匹配)。
// 第 3 条 (ADL) 专门把**参数个数不同**的两个候选按"错的顺序"声明, 这样
// "取第一个候选"的实现会直接给出可见的错误输出。
#include <cstdio>

namespace ns {
int f(int a) {
  return a + 1;
}
double f(double a) {
  return a + 1.5;
}
const char* f(const char* s) {
  return s;
}
}  // namespace ns

// 只有参数个数可区分, 且把多参数的放在前面: 取第一个候选的实现会选错。
namespace geo {
struct Point {
  int x;
};
int width(const Point& p, int pad) {
  return 200 + pad;
}
int width(const Point& p) {
  return 100 + p.x;
}
}  // namespace geo

// 全局作用域的同名重载 (无命名空间前缀, 链接名就是源名)。
int g(long a) {
  return (int)(a * 10);
}
int g(int a) {
  return a;
}
int h() {
  return 1;
}
int h(int a) {
  return a + 2;
}

int main() {
  printf("%d\n", ns::f(1));
  printf("%f\n", ns::f(1.0));
  printf("%s\n", ns::f("str"));
  geo::Point p;
  p.x = 5;
  // 非限定调用: 候选只来自 ADL (geo), 必须按实参个数选中正确重载。
  printf("%d %d\n", width(p), width(p, 7));
  printf("%d %d\n", g(1L), g(1));
  printf("%d %d\n", h(), h(3));
  return 0;
}
