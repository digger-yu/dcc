// m16_loop_opt.c
// 循环优化 (Tiling / Interchange / IVOP) 不应破坏正确性
// 通过: exit 0

int sum_array(int *a, int n) {
    int s = 0;
    for (int i = 0; i < n; i = i + 1) {
        s = s + a[i];
    }
    return s;
}

int main() {
    int a[5];
    a[0] = 1; a[1] = 2; a[2] = 3; a[3] = 4; a[4] = 5;
    if (sum_array(a, 5) != 15) return 1;
    int i = 0;
    int s = 0;
    while (i < 10) {
        s = s + i * 2;   // 强度削减: i*2 -> i<<1
        i = i + 1;
    }
    if (s != 90) return 2;  // 0+2+4+6+8+10+12+14+16+18 = 90
    return 0;
}
