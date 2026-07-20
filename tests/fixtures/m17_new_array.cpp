// M17.0: new T[n] 数组分配 + operator new[] / delete[] (C++ 解析层)
// 覆盖: new T[n] 语法 + delete[] p 语法
struct Item {
  int v;
  Item() : v(0) {}
  Item(int x) : v(x) {}
};
int test_new_array() {
  int* ia = new int[5];
  ia[0] = 0;
  ia[1] = 1;
  ia[2] = 2;
  ia[3] = 3;
  ia[4] = 4;
  int s = 0;
  s = s + ia[0];
  s = s + ia[1];
  s = s + ia[2];
  s = s + ia[3];
  s = s + ia[4];
  delete[] ia;
  return s;
}
