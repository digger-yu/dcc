// M54 根因回归: C++ 后端两层 for 嵌套 (SSA→AST 重建曾在内层回边无限互递归,
// 任意两层嵌套的 C++ 程序直接栈溢出). 夹具把每个校验点编码成 `return N`,
// 全部通过时返回 0; g++ -std=c++20 参照与 dcc 的退出码必须一致.
int main() {
    // 复现用例本体: Σ_{i,j<3} i*j = (0+1+2)*(0+1+2) = 9
    int s = 0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            s = s + i * j;
        }
    }
    if (s != 9) return 1;

    // 内层计数: 3x3 共 9 次
    int t = 0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            t = t + 1;
        }
    }
    if (t != 9) return 2;

    // 对称累加: Σ (i - j) 全 4x4 = 0
    int u = 0;
    for (int i = 0; i < 4; i++) {
        for (int j = 0; j < 4; j++) {
            u = u + i - j;
        }
    }
    if (u != 0) return 3;

    // 嵌套后外层的续体 (循环之后的代码) 也必须执行到
    int z = 40;
    for (int i = 0; i < 2; i++) {
        for (int j = 0; j < 2; j++) {
            z = z + i * j;
        }
    }
    if (z != 41) return 4;
    return 0;
}
