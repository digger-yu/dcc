// M54 根因回归: 深层嵌套 (8 层 for + 混合 while/do-while) 压力用例.
// 层数远超任何可能的"上限"补丁所允许的阈值 —— 修复必须是收敛性的,
// 不允许靠循环层数上限报错来规避栈溢出.
// 每个校验点编码成 `return N`, 全部通过返回 0.
int main() {
    // 8 层 for: 每变量取 0/1, 内层体跑 2^8=256 次, 每层变量各出现一半迭代
    // → Σ = 8 * 128 = 1024
    int s = 0;
    for (int a = 0; a < 2; a++)
        for (int b = 0; b < 2; b++)
            for (int c = 0; c < 2; c++)
                for (int d = 0; d < 2; d++)
                    for (int e = 0; e < 2; e++)
                        for (int f = 0; f < 2; f++)
                            for (int g = 0; g < 2; g++)
                                for (int h = 0; h < 2; h++) {
                                    s = s + a + b + c + d + e + f + g + h;
                                }
    if (s != 1024) return 1;

    // 6 层 for 混合累加: Σ_{abcd<2}(a+b+c+d) = 4*2^3 = 32, Σ_{ij<3} i*j = 9,
    // 两维独立 → 总计 32 * 9 = 288
    int t = 0;
    for (int a = 0; a < 2; a++)
        for (int b = 0; b < 2; b++)
            for (int c = 0; c < 2; c++)
                for (int d = 0; d < 2; d++)
                    for (int i = 0; i < 3; i++)
                        for (int j = 0; j < 3; j++) {
                            t = t + (a + b + c + d) * i * j;
                        }
    if (t != 288) return 2;

    // 4 层 while + 最内 do-while: 2*2*2*2 外层组合 × do-while 跑 2 次 = 32
    int u = 0;
    int i0 = 0;
    while (i0 < 2) {
        int i1 = 0;
        while (i1 < 2) {
            int i2 = 0;
            while (i2 < 2) {
                int i3 = 0;
                while (i3 < 2) {
                    int k = 0;
                    do {
                        u = u + 1;
                        k = k + 1;
                    } while (k < 2);
                    i3 = i3 + 1;
                }
                i2 = i2 + 1;
            }
            i1 = i1 + 1;
        }
        i0 = i0 + 1;
    }
    if (u != 32) return 3;
    return 0;
}
