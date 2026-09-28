// Stage 9 zero-cost EH: 多 handler 的首个匹配顺序与「不匹配则向外展开」。
#include <cstdio>
struct Base { int tag; Base(int t) : tag(t) {} };
struct Derived { int tag; Derived(int t) : tag(t) {} };
int main() {
  try {
    throw Derived(11);
  } catch (int) {
    printf("int\n");
  } catch (Base b) {
    printf("base %d\n", b.tag);
  } catch (Derived d) {
    printf("derived %d\n", d.tag);
  }
  try {
    throw Base(22);
  } catch (Derived) {
    printf("derived-first\n");
  } catch (Base b) {
    printf("base %d\n", b.tag);
  }
  try {
    throw 1;
  } catch (Base) {
    printf("should not reach\n");
  } catch (int v) {
    printf("int %d\n", v);
  }
  return 0;
}
