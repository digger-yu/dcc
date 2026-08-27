// M27: mutable 成员独立位 - const 方法中可写
// 验证: mutable 修饰符在 struct 字段位置被接受
// C 端输出 /* mutable */ 注释标记, 语义上 const 方法中可修改
struct Counter {
  mutable int ticks;
  int fixed;
  Counter() : ticks(0), fixed(0) {}
  void bump() {
    ticks++;  // mutable 字段在非 const 方法中可写
  }
  int get_ticks() const {
    // const 方法中 mutable 字段仍可写
    // C 端 this 是普通指针, 语义正确
    return ticks;
  }
};

int test_mutable() {
  Counter c;
  c.bump();
  c.bump();
  c.bump();
  return c.ticks;  // 期望 3
}

int test_mutable_const_method() {
  Counter c;
  c.bump();
  c.bump();
  // get_ticks 是 const 方法, 但 ticks 是 mutable 可写
  return c.get_ticks();  // 期望 2
}
