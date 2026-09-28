// Stage 9 zero-cost EH: 中间帧的析构清理顺序 + 跨帧 _Unwind_Resume。
// 关键规则: (1) 清理生效区间是「构造完成 → 正常析构」, 构造自身抛出不析构该对象;
// (2) action 链按作用域离开顺序 (自内向外); (3) _Unwind_Resume 必须有 lp=0/action=0
// 区间, 否则外层 handler 进不去 (实测表现为 std::terminate)。
#include <cstdio>
int seq[32];
int n = 0;
void p(int v) {
  if (n < 32) seq[n++] = v;
}
struct D {
  int id;
  D(int i) : id(i) { p(1000 + id); }
  ~D() { p(id); }
};
struct E {
  int id;
  E(int i) : id(i) {}
};
// 外层一个、内层一个: 内层块结束点抛出后外层仍要析构。
void inner(int k) {
  D a(1);
  {
    D b(2);
    if (k == 1) throw E(7);
  }
  if (k == 2) throw E(8);
}
int main() {
  try {
    inner(1);
  } catch (E e) {
    p(100 + e.id);
  }
  try {
    inner(2);
  } catch (E& r) {
    p(200 + r.id);
  }
  for (int i = 0; i < n; ++i) printf("%d ", seq[i]);
  printf("\n");
  return 0;
}
