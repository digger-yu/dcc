// Stage 9 zero-cost EH: 循环作用域内的对象在循环整个期间保持可清理,
// 出循环后由正常析构语句退休 (之后的抛出不得再析构它)。
#include <cstdio>
int seq[32];
int n = 0;
void p(int v) {
  if (n < 32) seq[n++] = v;
}
struct D {
  int id;
  D(int i) : id(i) {}
  ~D() { p(id); }
};
struct E { int id; E(int i) : id(i) {} };
int main() {
  for (int i = 0; i < 2; ++i) {
    try {
      D d(i + 1);
      if (i == 1) throw E(50);
      p(1000 + i);
    } catch (E e) {
      p(e.id);
    }
  }
  p(-1);
  for (int i = 0; i < n; ++i) printf("%d ", seq[i]);
  printf("\n");
  return 0;
}
