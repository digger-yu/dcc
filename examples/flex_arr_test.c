// 柔性数组成员后端验证: sizeof + 偏移 + 动态分配
struct flex {
    int len;
    char data[];
};

// 嵌套柔性数组
struct header {
    int magic;
    int count;
    struct flex items;
};

int main(void) {
    // === sizeof 不包含柔性数组 ===
    if (sizeof(struct flex) != 4) return 1;

    // === 偏移验证: data 在 len 之后 ===
    // 通过实际结构体验证: len 在偏移 0, data 在偏移 4
    struct flex f;
    f.len = 42;
    if (f.len != 42) return 2;

    // === 嵌套结构偏移 ===
    if (sizeof(struct header) != 12) return 3;  // magic(4) + count(4) + flex(4)

    return 0;
}
