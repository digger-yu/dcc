// Stage 9 zero-cost EH: 标量抛出 + 本帧按值捕获。
// 验证 invoke/landing pad/selector 分派与 __cxa_begin_catch/__cxa_end_catch 收尾。
#include <cstdio>
int main() {
  int got = 0;
  try {
    throw 42;
  } catch (int e) {
    got = e;
    printf("caught int %d\n", e);
  }
  printf("after=%d end_catch_ran=%d\n", got, got == 42);
  return 0;
}
