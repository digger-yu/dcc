// M54 根因回归: 三层 for 嵌套 + break/continue.
// 旧缺陷有两个层面, 都在此覆盖:
//   1) 内层循环 header 在外层循环体内被当普通块无限递归 (栈溢出);
//   2) `if (c) continue;` 的 latch 块不在单 latch 反向 DFS 的循环体里,
//      被误判为"循环外后继"发成 break, continue 之后少跑的迭代静默丢头.
// 每个校验点编码成 `return N`, 全部通过返回 0.
int main() {
    // 三层: j==2 continue / j==3 break 只作用于中层; 内层 k==0 continue /
    // k==2 break, 只有 k==1 参与累加. s = Σ_{i<4} Σ_{j∈{0,1}} i*j*1 = 6*1 = 6
    int s = 0;
    for (int i = 0; i < 4; i++) {
        for (int j = 0; j < 4; j++) {
            if (j == 2) continue;
            if (j == 3) break;
            for (int k = 0; k < 3; k++) {
                if (k == 0) continue;
                if (k == 2) break;
                s = s + i * j * k;
            }
        }
    }
    if (s != 6) return 1;

    // 计数型: i<2, j<3, k<2 全跑满 → 2*3*2 = 12 次
    int c = 0;
    for (int i = 0; i < 2; i++) {
        for (int j = 0; j < 3; j++) {
            for (int k = 0; k < 2; k++) {
                c = c + 1;
            }
        }
    }
    if (c != 12) return 2;

    // 内层 break + 外层 continue 混用: 中层 i==2 continue 后, 外层必须继续
    // 跑 i=3 的迭代 (旧缺陷里 continue 的 latch 区被 break 截断)
    int t = 0;
    for (int i = 0; i < 4; i++) {
        for (int j = 0; j < 3; j++) {
            if (i == 2) continue;
            if (j == 2) break;
            t = t + i * j;
        }
    }
    // i∈{0,1,3} × j∈{0,1}: Σ = (0+1+3)*(0+1) = 4
    if (t != 4) return 3;
    return 0;
}
