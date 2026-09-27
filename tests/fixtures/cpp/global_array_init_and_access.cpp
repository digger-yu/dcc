// M54 缺陷 1/3 回归夹具: 全局数组的**静态初值**必须发射, 且
// 部分初始化剩余元素零初始化 ([dcl.init.aggr]/8)、
// `char s[] = "abc";` 的维度必须由初值长度推出 (含 '\0' -> 4, [dcl.array]/2)。
//
// 验证点以 `return N` 编号, 退出码即诊断; 尺寸另用 `static_assert` 在编译期卡死。

int full[4] = {10, 20, 30, 40};
int part[4] = {1, 2};
int nested[2][3] = {{1, 2, 3}, {4, 5, 6}};
int nested_part[2][3] = {{7}, {8, 9}};
char s[] = "abc";
const char cs[] = "xyz";
int tail_guard[2] = {-1, -2};

static_assert(sizeof(s) == 4, "char s[] = \"abc\" 的维度必须是 4 (含终止符)");
static_assert(sizeof(cs) == 4, "const char cs[] 同样由初值推维度");
static_assert(sizeof(full) == 16, "full 是 4 个 int");
static_assert(sizeof(nested_part) == 24, "nested_part 是 2*3 个 int");
static_assert(sizeof(s) / sizeof(s[0]) == 4, "元素个数含终止符");

int probe() {
  // 完整初始化: 逐个读回
  if (full[0] != 10 || full[1] != 20 || full[2] != 30 || full[3] != 40) return 1;
  // 部分初始化: 给出前两个, 后两个必须静态零初始化 (不是脏值也不是别的元素)
  if (part[0] != 1 || part[1] != 2) return 2;
  if (part[2] != 0 || part[3] != 0) return 3;
  // 多维: 内层列表逐个对应, 未给出的行元素补零
  if (nested[0][0] != 1 || nested[0][2] != 3 || nested[1][0] != 4 || nested[1][2] != 6) return 4;
  if (nested_part[0][0] != 7 || nested_part[0][1] != 0 || nested_part[0][2] != 0) return 5;
  if (nested_part[1][0] != 8 || nested_part[1][1] != 9 || nested_part[1][2] != 0) return 6;
  // 字符串初值: 逐字节 + 终止符
  if (s[0] != 'a' || s[1] != 'b' || s[2] != 'c') return 7;
  if (s[3] != '\0') return 8;
  if (cs[0] != 'x' || cs[3] != '\0') return 9;
  // 运行时 sizeof 与编译期同源
  if (static_cast<int>(sizeof(s)) != 4) return 10;
  if (static_cast<int>(sizeof(nested)) != 24) return 11;
  // 写入后读回, 且邻居对象未被踩
  full[2] = 77;
  if (full[2] != 77 || full[1] != 20 || full[3] != 40) return 12;
  part[3] = 9;
  if (part[3] != 9 || part[2] != 0) return 13;
  s[0] = 'Z';
  if (s[0] != 'Z' || s[1] != 'b') return 14;
  if (tail_guard[0] != -1 || tail_guard[1] != -2) return 15;
  // 数组到指针衰减 + 下标算术
  int *p = full;
  if (p[0] != 10 || p[2] != 77) return 16;
  if (static_cast<int>(&full[3] - full) != 3) return 17;
  return 0;
}

int main() { return probe(); }
