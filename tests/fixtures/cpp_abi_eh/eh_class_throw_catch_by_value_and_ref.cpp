// Stage 9 zero-cost EH: 类对象抛出 (需要拷贝构造进异常缓冲区) + 按值/按引用捕获。
// 验证 _ZTI 类型表项、__cxa_throw 的析构函数指针参数、以及两种 catch 绑定路径。
#include <cstdio>
struct E {
  int id;
  E(int i) : id(i) { printf("ctor %d\n", id); }
};
int main() {
  try {
    E src(7);
    throw src;
  } catch (E byValue) {
    printf("by value id=%d\n", byValue.id);
  }
  try {
    throw E(9);
  } catch (E& r) {
    printf("by ref id=%d\n", r.id);
  }
  printf("done\n");
  return 0;
}
