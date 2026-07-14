// P1 测试 - 简化版
int main(void) {
    // Test 1: register asm
    register int rx __asm__("r10");
    rx = 42;
    if (rx != 42) return 1;

    // Test 2: __int128 add
    __int128 a = 1;
    __int128 b = 2;
    __int128 c = a + b;
    if (c != 3) return 2;

    // Test 3: __int128 sub
    __int128 d = 100;
    __int128 e = 50;
    if (d - e != 50) return 3;

    // Test 4: aligned
    return 0;
}
