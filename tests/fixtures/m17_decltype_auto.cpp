// M17.0: decltype(auto) 真推导 (简化版)
// 覆盖: 函数返回类型 decltype(auto) 推断
int g_val() { return 7; }
int test_decltype_auto() {
  // 使用 decltype(auto) 作为变量类型 -> 简化: 推断为 int
  decltype(auto) x = g_val();
  return x + 3;
}
