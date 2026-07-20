// M17.0: if constexpr 真编译期分支
// 覆盖: true / false 条件 + sizeof 比较 + 算术
int test_if_constexpr() {
  int s = 0;
  // 1. 常量 true 分支
  if constexpr (1) { s += 10; } else { s += 99; }
  // 2. 常量 false 分支
  if constexpr (0) { s += 88; } else { s += 5; }
  // 3. sizeof 比较
  if constexpr (sizeof(int) == 4) { s += 1; } else { s += 999; }
  // 4. 算术表达式
  if constexpr ((1 + 2) == 3) { s += 2; } else { s += 888; }
  return s;  // 期望 10 + 5 + 1 + 2 = 18
}
