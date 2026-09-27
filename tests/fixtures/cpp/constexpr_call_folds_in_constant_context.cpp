// M54 缺陷 4 回归夹具: constexpr **函数调用**必须在常量上下文里做编译期求值。
//
// 覆盖的常量上下文:
//   * 全局数组维度 `char buf[square(12)]`
//   * 由 constexpr 变量再次参与的维度算术 `int arr[sq12 / 4]`
//   * constexpr 数组的维度 + 聚合初值元素个数
//   * `switch` 的 case 标签
//   * 无名 enum / `enum class` 的枚举数
//   * `static_assert`
//   * 函数体内的局部数组维度
// 期望值一律先由 `g++ -std=c++20` 实测得到, 不臆造。

constexpr int square(int x) {
  return x * x;
}

// 递归 constexpr 调用 (解释器需支持自调用)
constexpr int tri(int x) {
  return x <= 0 ? 0 : x + tri(x - 1);
}

constexpr int sq12 = square(12);

char buf[square(12)];
int arr[sq12 / 4];
constexpr int table[square(2)] = {1, 2, 3, 4};

enum {
  E = square(3),
  F = E + square(1),
};

enum class Color : int {
  Red = square(2),
  Green = square(3),
};

static_assert(square(12) == 144, "constexpr 调用折进 static_assert");
static_assert(sq12 == 144, "constexpr 变量本身");
static_assert(sizeof(buf) == 144, "数组维度来自 constexpr 调用");
static_assert(sizeof(arr) == 144, "维度里的 constexpr 算术");
static_assert(sizeof(table) == 16, "constexpr 维度 x int 元素");
static_assert(E == 9 && F == 10, "enum 数用 constexpr 调用");

int classify(int x) {
  switch (x) {
    case square(2):
      return 1;
    case E:
      return 2;
    case sq12:
      return 3;
    default:
      return 0;
  }
}

int probe() {
  if (sizeof(buf) != 144) return 1;
  if (sizeof(arr) != 144) return 2;
  if (sizeof(table) != 16) return 3;
  if (table[3] != 4) return 4;
  buf[3] = 65;
  if (buf[3] != 65) return 5;
  if (buf[143] != 0) return 6;
  arr[35] = 7;
  if (arr[35] != 7) return 7;
  if (classify(4) != 1) return 8;
  if (classify(9) != 2) return 9;
  if (classify(144) != 3) return 10;
  if (classify(5) != 0) return 11;
  if (tri(5) != 15) return 12;
  if ((int)Color::Red != 4) return 13;
  if ((int)Color::Green != 9) return 14;
  // 函数体内的局部数组维度同样要折叠
  int local_buf[square(3)];
  if (sizeof(local_buf) != 36) return 15;
  local_buf[8] = 1234;
  if (local_buf[8] != 1234) return 16;
  char cbuf[square(2) + 1];
  if (sizeof(cbuf) != 5) return 17;
  cbuf[4] = 7;
  if (cbuf[4] != 7) return 18;
  return 0;
}

int main() {
  return probe();
}
