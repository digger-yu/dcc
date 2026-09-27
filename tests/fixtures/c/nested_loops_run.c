/* M54 根因回归 (C 侧同构): 与 cpp/nested_for_two_level.cpp /
 * nested_for_three_level_break_continue.cpp / nested_for_while_and_while_dowhile.cpp
 * 同样的嵌套循环形状. C 前端直接建 AST 不经 SSA→AST 重建, 本夹具证明
 * 共用代码 (find_loop_body / phi 插入) 的修改没有让 C 路径退化:
 * gcc -O0 与 dcc 的退出码、stdout 必须逐字节一致 (由 run_gcc_cross_check
 * 保证), 且所有检查点通过时返回 0. */
int main(void) {
    int s = 0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            s = s + i * j;
        }
    }
    if (s != 9) return 1;

    int t = 0;
    for (int i = 0; i < 4; i++) {
        for (int j = 0; j < 4; j++) {
            if (j == 2) continue;
            if (j == 3) break;
            for (int k = 0; k < 3; k++) {
                if (k == 0) continue;
                if (k == 2) break;
                t = t + i * j * k;
            }
        }
    }
    if (t != 6) return 2;

    int m = 0;
    for (int i = 0; i < 3; i++) {
        int j = 0;
        while (j < 3) {
            j = j + 1;
            if (j == 2) continue;
            m = m + i * j;
        }
    }
    if (m != 12) return 3;

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
    if (d != 15) return 4;

    /* 6 层嵌套压力 (与 C++ 深层夹具同构): Σ 各维 = 6 * 2^5 = 192 */
    int deep = 0;
    for (int a = 0; a < 2; a++)
        for (int b = 0; b < 2; b++)
            for (int c = 0; c < 2; c++)
                for (int e = 0; e < 2; e++)
                    for (int f = 0; f < 2; f++)
                        for (int g = 0; g < 2; g++) {
                            deep = deep + a + b + c + e + f + g;
                        }
    if (deep != 192) return 5;
    return 0;
}
