// m16_sccp.c
// SCCP (稀疏条件常量传播): 编译期可确定的分支应当被折叠
// 通过: exit 0

int main() {
    int x = 10;
    int y = 20;
    if (x == 10) {
        if (y == 20) {
            return 0;  // 期望 → 常量 0
        }
        return 1;
    }
    return 2;
}
