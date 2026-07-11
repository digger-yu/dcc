// 定点数类型后端验证: _Fract(1.31) + _Accum(16.16) + sizeof + 负数
int main() {
    // === sizeof 验证 ===
    if (sizeof(_Fract) != 4) return 1;
    if (sizeof(_Accum) != 4) return 2;

    // === _Fract 加法 (1.31 格式) ===
    _Fract a = 0x40000000;  // 0.5
    _Fract b = 0x20000000;  // 0.25
    _Fract c = a + b;       // 0.75 = 0x60000000
    if (c != 0x60000000) return 3;

    // === _Accum 加法 (16.16 格式) ===
    _Accum x = 0x00010000;  // 1.0
    _Accum y = 0x00020000;  // 2.0
    _Accum z = x + y;       // 3.0 = 0x00030000
    if (z != 0x00030000) return 4;

    // === 减法 ===
    _Fract d = a - b;  // 0.25 = 0x20000000
    if (d != 0x20000000) return 5;

    // === 负数 (用减法构造) ===
    _Fract small = (_Fract)100;
    _Fract big = (_Fract)300;
    _Fract diff = small - big;  // -200 = 0xFFFFFF38 (32位补码)
    // 验证: diff + big == small
    _Fract check = diff + big;
    if (check != 100) return 6;

    // === 零值 ===
    _Fract zero = 0;
    if (zero != 0) return 7;

    // === 结合律 ===
    _Fract x1 = (_Fract)10;
    _Fract x2 = (_Fract)20;
    _Fract x3 = (_Fract)30;
    _Fract r1 = x1 + x2 + x3;
    if (r1 != 60) return 8;

    return 0;
}
