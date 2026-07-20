// M17.0: decltype(expr) 真类型查询 (简化版)
// 覆盖: decltype(int) 给出 int, decltype(x) 给出 int
int test_decltype() {
  int a = 10;
  decltype(a) b = 20;  // int
  decltype(a + 1) c = 30;  // int (算术)
  return b + c;
}
