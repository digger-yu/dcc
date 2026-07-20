// m16_tco.c
// 尾部调用优化: 递归 sum_to(n) 应在 codegen 后正确求值
// 通过: exit 0

int sum_to(int n, int acc) {
    if (n == 0) return acc;
    return sum_to(n - 1, acc + n);
}

int main() {
    if (sum_to(10, 0) != 55) return 1;  // 1+2+...+10
    if (sum_to(100, 0) != 5050) return 2;
    return 0;
}
