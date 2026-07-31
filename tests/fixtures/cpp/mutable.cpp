// M17.0: mutable 独立位 - 解析为可写字段
// 覆盖: mutable 修饰符在 struct 字段位置被接受
struct Counter {
  mutable int ticks;
  int fixed;
  Counter() : ticks(0), fixed(0) {}
  void bump() {
    ticks++;
  }
};
int test_mutable() {
  Counter c;
  c.bump();
  c.bump();
  c.bump();
  return c.ticks;  // 3
}
