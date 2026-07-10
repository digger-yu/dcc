// 定点数类型测试
int main() {
    _Fract a = 0x40000000;  // 0.5 in 1.31 format
    _Fract b = 0x20000000;  // 0.25 in 1.31 format
    _Fract c = a + b;       // 0.75 = 0x60000000

    if (c != 0x60000000) return 1;

    _Accum x = 0x00010000;  // 1.0 in 16.16 format
    _Accum y = 0x00020000;  // 2.0 in 16.16 format
    _Accum z = x + y;       // 3.0 = 0x00030000

    if (z != 0x00030000) return 2;

    // 测试减法
    _Fract d = a - b;  // 0.25 = 0x20000000
    if (d != 0x20000000) return 3;

    return 0;
}
