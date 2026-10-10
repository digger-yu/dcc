// 位域后端验证: 位偏移计算 + 掩码移位 + 有符号扩展
struct bits {
    int a : 3;   // 位 0-2
    int b : 5;   // 位 3-7
    int c;       // 下一个容器
};

// 跨容器边界测试
struct cross {
    unsigned int x : 6;   // 位 0-5
    unsigned int y : 10;  // 位 6-15
    unsigned int z : 16;  // 位 16-31
};

// 小容器位域 (short)
struct small {
    short a : 4;   // 位 0-3
    short b : 12;  // 位 4-15
};

// 位域 + 普通字段混合
struct mixed {
    int flag : 1;   // 1 位无符号更合适, 但有符号也可以 (值 0 或 -1)
    unsigned int mode : 3;  // 无符号 3 位: 0-7
    unsigned int count : 8; // 无符号 8 位: 0-255
    int normal;
};

int main(void) {
    // === 基本读写 ===
    struct bits s;
    s.a = 3;   // 最大 3 位有符号 = 3
    s.b = 15;  // 最大 5 位有符号 = 15
    s.c = 42;
    if (s.a != 3) return 1;
    if (s.b != 15) return 2;
    if (s.c != 42) return 3;

    // === 有符号负数 ===
    s.a = -1;  // 3 位: -1 补码 = 111 = 7 无符号, 但有符号 = -1
    s.b = -8;  // 5 位: -8
    if (s.a != -1) return 4;
    if (s.b != -8) return 5;

    // === 零值 ===
    s.a = 0;
    s.b = 0;
    if (s.a != 0) return 6;
    if (s.b != 0) return 7;

    // === 跨容器边界 ===
    struct cross cr;
    cr.x = 63;   // 6 位最大: 63
    cr.y = 1023; // 10 位最大: 1023
    cr.z = 65535; // 16 位最大: 65535
    if (cr.x != 63) return 10;
    if (cr.y != 1023) return 11;
    if (cr.z != 65535) return 12;

    // === 小容器 (short) ===
    struct small sm;
    sm.a = 7;    // 4 位有符号最大 7
    sm.b = 2047; // 12 位有符号最大 2047
    if (sm.a != 7) return 20;
    if (sm.b != 2047) return 21;

    // === 混合字段 ===
    struct mixed mx;
    mx.flag = 1;   // 1 位有符号: 位模式 1 = -1
    mx.mode = 5;   // 3 位无符号: 5
    mx.count = 200; // 8 位无符号: 200
    mx.normal = 99;
    if (mx.flag != -1) return 30;  // 1 位有符号, 位模式 1 = -1
    if (mx.mode != 5) return 31;
    if (mx.count != 200) return 32;
    if (mx.normal != 99) return 33;

    // === 位域运算 ===
    s.a = 2;
    s.b = 10;
    int sum = s.a + s.b;
    if (sum != 12) return 40;

    // === sizeof 验证 ===
    // struct bits: int(4) 容器含 a:3+b:5, 然后 int c(4) = 8 字节
    if (sizeof(struct bits) != 8) return 50;
    // struct cross: 一个 unsigned int = 4 字节
    if (sizeof(struct cross) != 4) return 51;
    // struct small: 一个 short = 2 字节
    if (sizeof(struct small) != 2) return 52;

    return 0;
}
