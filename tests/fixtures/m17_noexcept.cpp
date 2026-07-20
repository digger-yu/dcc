// M17.0: noexcept 真 codegen (__attribute__((nothrow)))
// 覆盖: noexcept ctor / dtor / 普通成员函数 标 nothrow 属性
struct NoThrow {
  int v;
  NoThrow() noexcept : v(0) {}
  NoThrow(int x) noexcept : v(x) {}
  ~NoThrow() noexcept {}
  int get() const noexcept { return v; }
  void set(int x) noexcept { v = x; }
};
int test_noexcept() {
  NoThrow n(7);
  n.set(13);
  return n.get();  // 13
}
