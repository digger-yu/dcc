// m16_strength_reduction.c
// 强度削减验证: i * 8 应被优化为 i << 3
// 通过: exit 0

int main() {
    int i = 5;
    int r = i * 8;        // 期望 → i << 3
    if (r != 40) return 1;
    r = i / 4;            // 期望 → i >> 2
    if (r != 1) return 2;
    r = i * 16;           // 期望 → i << 4
    if (r != 80) return 3;
    return 0;
}
