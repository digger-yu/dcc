// M17.0: placement new 转发 (C++ 解析层)
// 覆盖: new (ptr) T(args) 语法
struct Cell {
  int v;
  Cell() : v(0) {}
  Cell(int x) : v(x) {}
};
int test_placement_new() {
  int storage = 0;
  int* p = &storage;
  Cell* c = new (p) Cell(42);
  return c->v;
}
