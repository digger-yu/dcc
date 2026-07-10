// SIMD 向量类型测试
typedef int v4si __attribute__((vector_size(16)));

int main() {
    v4si a = {1, 2, 3, 4};
    v4si b = {10, 20, 30, 40};
    v4si c = a + b;
    int *p = (int *)&c;
    if (p[0] != 11) return 1;
    if (p[1] != 22) return 2;
    if (p[2] != 33) return 3;
    if (p[3] != 44) return 4;

    // 测试减法
    v4si d = b - a;
    int *q = (int *)&d;
    if (q[0] != 9) return 5;
    if (q[1] != 18) return 6;
    if (q[2] != 27) return 7;
    if (q[3] != 36) return 8;

    // 测试赋值
    v4si e = a;
    e = e + b;
    int *r = (int *)&e;
    if (r[0] != 11) return 9;
    if (r[3] != 44) return 10;

    return 0;
}
