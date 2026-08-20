// m26_enum_semantics.cpp - C++ enum 保留完整枚举类型语义
// 验证: 枚举值 + 显式初始化 + scoped enum + 底层类型 + 匿名枚举
// 均被保留, 且可在运行时按值比较.

enum Color { RED, GREEN = 5, BLUE };
enum class Scoped : unsigned char { A = 1, B = A + 2 };
enum { ANON_X = 42 };

int main() {
    int c = GREEN;
    int s = (int)Scoped::B;
    int a = ANON_X;
    int r = 0;
    if (c != GREEN) r |= 1;      // GREEN == 5
    if (s != 3) r |= 2;          // B == A + 2 == 3
    if (a != 42) r |= 4;         // ANON_X == 42
    if (BLUE != 6) r |= 8;       // 隐式后续值 6
    if (RED != 0) r |= 16;       // 首值 0
    if (sizeof(Scoped) != 1) r |= 32; // unsigned char 底层类型
    return r;
}
