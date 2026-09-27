// M54 根因回归: for 嵌套 while、while 嵌套 do-while 的混合循环.
// 每个校验点编码成 `return N`, 全部通过返回 0.
int main() {
    // for 套 while + continue: 每 i: j=1 时 m+=i, j=2 continue, j=3 时 m+=3i
    // → 4i; Σ_{i<3} 4i = 12
    int m = 0;
    for (int i = 0; i < 3; i++) {
        int j = 0;
        while (j < 3) {
            j = j + 1;
            if (j == 2) continue;
            m = m + i * j;
        }
    }
    if (m != 12) return 1;

    // while 套 do-while: 每 i 内层跑 j=1,2 → (i+1)+(i+2); Σ_{i<3} = 15
    int d = 0;
    int i = 0;
    while (i < 3) {
        int j = 0;
        do {
            j = j + 1;
            d = d + i + j;
        } while (j < 2);
        i = i + 1;
    }
    if (d != 15) return 2;

    // while 套 while + break: 内层 j 每 i 只跑 j=1 (j==1 处 break 前累加)
    int w = 0;
    int p = 0;
    while (p < 4) {
        int q = 0;
        while (q < 5) {
            q = q + 1;
            w = w + p * q;
            if (q == 1) break;
        }
        p = p + 1;
    }
    // Σ_{p<4} p*1 = 6
    if (w != 6) return 3;
    return 0;
}
