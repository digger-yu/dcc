// M54 根因回归: 嵌套循环体里调用成员函数 (构造真实 CFG 回边: 调用点把
// 循环体块与 header 之间的回边撑活, 不会被折叠成直线代码).
// 每个校验点编码成 `return N`, 全部通过返回 0.
struct Adder {
    int base;
    int add(int x) const { return base + x; }
};

int main() {
    Adder a;
    a.base = 1;

    // 两层 for 里调成员函数: 9 次迭代, Σ (1 + i*j) = 9 + 9 = 18
    int s = 0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            s = s + a.add(i * j);
        }
    }
    if (s != 18) return 1;

    // while 套 do-while 里调成员函数:
    // (i=0,j=1):1 (i=0,j=2):1 (i=1,j=1):2 (i=1,j=2):3 → 7
    int t = 0;
    int i = 0;
    while (i < 2) {
        int j = 0;
        do {
            j = j + 1;
            t = t + a.add(i * j);
        } while (j < 2);
        i = i + 1;
    }
    if (t != 7) return 2;

    // 三层 for + 成员函数 (每次迭代都调用): 3*3*2=18 次调用, 各加 1 → 18
    int c = 0;
    for (int x = 0; x < 3; x++) {
        for (int y = 0; y < 3; y++) {
            for (int z = 0; z < 2; z++) {
                c = c + a.add(0);
            }
        }
    }
    if (c != 18) return 3;
    return 0;
}
