// M17.0: volatile 成员函数 - this 视为 volatile*
// 覆盖: volatile 成员函数标记 + volatile 字段读取
struct Sensor {
  volatile int reg;  // 模拟硬件寄存器
  Sensor() : reg(0) {}
  int read() const volatile {
    // volatile load: 不优化掉重复读
    int a = reg;
    int b = reg;
    return a + b;
  }
};
int test_volatile_member() {
  Sensor s;
  // 简化: 不真访问硬件, 仅验证编译通过
  // C 端 volatile 语义由 caller 决定, C++ 端标记函数为 volatile
  return 0;
}
