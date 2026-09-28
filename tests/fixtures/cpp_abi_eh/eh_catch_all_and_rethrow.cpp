// Stage 9 zero-cost EH: catch (...) 与 catch 体内 throw; (rethrow)。
// 省略号 handler 在类型表里是值为 0 的空表项但 filter 仍为正; rethrow 走
// __cxa_rethrow 并登记 lp=0/action=0 区间。
#include <cstdio>
struct E { int id; E(int i) : id(i) {} };
int main() {
  try {
    try {
      throw E(3);
    } catch (int) {
      printf("wrong handler\n");
    }
  } catch (E x) {
    printf("outer caught id=%d\n", x.id);
  }
  try {
    throw 5;
  } catch (...) {
    printf("catch-all\n");
  }
  int rethrown = 0;
  try {
    try {
      throw 6;
    } catch (int) {
      printf("inner catch, rethrow\n");
      throw;
    }
  } catch (int v) {
    rethrown = v;
  }
  printf("rethrown=%d\n", rethrown);
  return 0;
}
