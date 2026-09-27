// M54 缺陷 3 回归夹具: 全局 / 命名空间作用域对象的**静态初值**必须真的发射
// (旧行为: 初值整个丢掉, 对象落 .bss, `int ga = 7;` 读回 0)。
// 覆盖面: 整型 / const / constexpr / 浮点 (含负数与 float 后缀) / 取址初值
// (需要链接期重定位) / 字符串字面量指针 / 结构体聚合对象 / 值初始化。
//
// 验证点以 `return N` 编号; 编译期可证的量用 `static_assert` 卡死。

struct Point {
  int x;
  int y;
};
struct Conf {
  double ratio;
  long count;
  char flag;
};

int ga = 7;
const int cb = 9;
constexpr int kd = 11;
const int ke = kd + cb;
double d = 1.5;
double nd = -2.25;
float fs = 0.5f;
int *p = &ga;
int garr[4] = {1, 2, 3, 4};
// 数组名衰减成指针 + 指针常量算术: 两者都是链接期可表示的地址初值。
int *qdec = garr;
int *qmid = garr + 2;
const char *msg = "hi";
Point pt = {3, 4};
Point zero = {};
Conf cf = {2.5, 6, 1};
Point pts[2] = {{1, 2}, {3, 4}};

static_assert(kd == 11, "constexpr 标量初值");
static_assert(ke == 20, "constexpr 算术初值");
static_assert(sizeof(p) == sizeof(void *), "指针对象");

int probe() {
  if (ga != 7) return 1;
  if (cb != 9) return 2;
  if (kd != 11) return 3;
  if (ke != 20) return 4;
  if (d != 1.5) return 5;
  // 注: 浮点**一元负号** (`-nd`) 在本后端的运行期路径上有已知缺陷 (仅报告,
  // 不在本次修复的文件边界内), 故这里用不依赖它的两条形态证明初值确实是
  // -2.25 而不是 +2.25 / 0: 符号位与反号后的宽度。
  if (nd < 0.0) {}
  else return 6;
  if (0.0 - nd != 2.25) return 61;
  if (fs != 0.5f) return 7;
  // 取址初值必须真的指向那个对象 (链接期重定位, 不是 0 也不是别家)
  if (p != &ga) return 8;
  if (*p != 7) return 9;
  ga = 20;
  if (*p != 20) return 10;
  // 字符串字面量指针初值: 槽里必须是 rodata 里那个串的**地址**(链接期重定位)。
  // 注: 本后端对「静态存储指针直接下标 `msg[0]` / `msg->`」有已知缺陷(仅报告),
  // 故串内容用等价的 `*(msg + n)` 形态检查, 与本夹具的修复点无关但同样卡死初值。
  if (*msg != 'h') return 101;
  if (*(msg + 1) != 'i') return 102;
  if (*(msg + 2) != '\0') return 11;
  if (pt.x != 3 || pt.y != 4) return 12;
  if (zero.x != 0 || zero.y != 0) return 13;
  // 注: 本后端对「`a || <浮点比较>` 短路链」有已知缺陷 (仅报告, 不在本次
  // 修复的文件边界内), 故带浮点比较的条件逐条分写, 各自给唯一编号。
  if (cf.ratio != 2.5) return 141;
  if (cf.count != 6) return 142;
  if (cf.flag != 1) return 143;
  // 成员写入不得踩到同一对象的其他字段 (聚合对象的布局/尺寸真源)
  cf.flag = 3;
  if (cf.flag != 3) return 151;
  if (cf.count != 6) return 152;
  if (cf.ratio != 2.5) return 153;
  if (pts[0].x != 1 || pts[0].y != 2 || pts[1].x != 3 || pts[1].y != 4) return 16;
  // 聚合数组写入不越界踩邻居
  pts[1].y = 40;
  if (pts[1].y != 40 || pts[0].y != 2 || cf.flag != 3) return 17;
  pt.x = 100;
  if (pt.x != 100 || pt.y != 4 || zero.x != 0) return 18;
  // 地址初值真指到数组内部 (而非零初始化 / 错元素)
  if (qdec != &garr[0]) return 19;
  if (*qdec != 1) return 20;
  if (*qmid != 3) return 21;
  if (*(qdec + 3) != 4) return 22;
  garr[2] = 99;
  if (*qmid != 99) return 23;
  return 0;
}

int main() { return probe(); }
