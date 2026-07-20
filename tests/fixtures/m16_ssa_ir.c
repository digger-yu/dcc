// m16_ssa_ir.c
// SSA IR 基础设施: 控制流 (if/else/while/for) 应当正确转换
// 通过: exit 0

int fact(int n) {
    int r = 1;
    int i = 1;
    while (i <= n) {
        r = r * i;
        i = i + 1;
    }
    return r;
}

int main() {
    if (fact(5) != 120) return 1;
    if (fact(0) != 1) return 2;
    if (fact(1) != 1) return 3;
    if (fact(6) != 720) return 4;
    return 0;
}
