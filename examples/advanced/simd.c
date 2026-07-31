// SIMD 向量类型后端验证: 初始化 + 加减乘 + 赋值 + sizeof
typedef int v4si __attribute__((vector_size(16)));

int main() {
    // === sizeof 验证 ===
    if (sizeof(v4si) != 16) return 1;

    // === 初始化 + 加法 ===
    v4si a = {1, 2, 3, 4};
    v4si b = {10, 20, 30, 40};
    v4si c = a + b;
    int *p = (int *)&c;
    if (p[0] != 11) return 2;
    if (p[1] != 22) return 3;
    if (p[2] != 33) return 4;
    if (p[3] != 44) return 5;

    // === 减法 ===
    v4si d = b - a;
    int *q = (int *)&d;
    if (q[0] != 9) return 6;
    if (q[1] != 18) return 7;
    if (q[2] != 27) return 8;
    if (q[3] != 36) return 9;

    // === 赋值 + 复合运算 ===
    v4si e = a;
    e = e + b;
    int *r = (int *)&e;
    if (r[0] != 11) return 10;
    if (r[3] != 44) return 11;

    // === 乘法 ===
    v4si f = {2, 3, 4, 5};
    v4si g = {10, 20, 30, 40};
    v4si h = f * g;
    int *s = (int *)&h;
    if (s[0] != 20) return 12;
    if (s[1] != 60) return 13;
    if (s[2] != 120) return 14;
    if (s[3] != 200) return 15;

    return 0;
}
