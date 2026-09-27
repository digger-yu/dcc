// M54 缺陷 1/2 回归夹具: 全局 (含命名空间作用域) 数组的**维度**必须进类型系统,
// 于是 `sizeof` / 元素 `sizeof` / 下标算术 / 最后一元素写入全部与 GCC 一致。
//
// 每个验证点用 `if (...) return N;` 编号, 退出码即诊断; 尺寸另用
// `static_assert` 在编译期卡死 (维度一旦丢回元素大小, 编译就直接失败)。

int g[10];
int m[2][3];
char chars[7];
extern int ext_sized[4];
int ext_sized[4];

// 对象尺寸: 一维 = 10*4, 二维 = 2*3*4, char[7] = 7
static_assert(sizeof(g) == 40, "g must be 10 ints");
static_assert(sizeof(m) == 24, "m must be 6 ints");
static_assert(sizeof(chars) == 7, "chars must be 7 bytes");
static_assert(sizeof(ext_sized) == 16, "ext_sized must be 4 ints");
// 元素尺寸经由下标表达式与解引用取得, 不能退化成整个数组或指针宽度
static_assert(sizeof(g[0]) == 4, "g[0] must be an int");
static_assert(sizeof(*g) == 4, "*g must be an int");
static_assert(sizeof(m[1]) == 12, "m[1] must be a row of 3 ints");
static_assert(sizeof(g) / sizeof(g[0]) == 10, "g must hold 10 elements");

int probe() {
  // 下标读写 (旧实现因类型不是数组直接编译失败)
  g[3] = 7;
  if (g[3] != 7) return 1;
  // 下标算术: `&g[3] - g` 按元素计为 3
  if (static_cast<int>(&g[3] - g) != 3) return 2;
  if (static_cast<int>(g + 6 - g) != 6) return 3;
  // 运行时 sizeof 与编译期同源
  if (static_cast<int>(sizeof(g)) != 40) return 4;
  if (static_cast<int>(sizeof(m)) != 24) return 5;
  if (static_cast<int>(sizeof(m[0])) != 12) return 6;
  if (static_cast<int>(sizeof(*m)) != 12) return 7;
  if (static_cast<int>(sizeof(chars)) != 7) return 8;
  if (static_cast<int>(sizeof(ext_sized)) != 16) return 9;
  // 最远元素写入必须落在对象内: 先污染, 再确认邻居未被踩
  g[9] = 99;
  m[1][2] = 1234;
  chars[6] = 'z';
  if (g[9] != 99) return 10;
  if (g[0] != 0 || g[8] != 0) return 11;
  if (m[1][2] != 1234 || m[0][0] != 0) return 12;
  if (chars[6] != 'z' || chars[0] != 0) return 13;
  if (ext_sized[3] != 0) return 14;
  ext_sized[3] = 5;
  if (ext_sized[3] != 5) return 15;
  return 0;
}

int main() { return probe(); }
