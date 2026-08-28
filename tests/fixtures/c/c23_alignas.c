// C23: alignas 关键字 (C23 拼写) 与 _Alignas (C11 拼写)
#include <stdio.h>

// alignas(常量表达式)
alignas(16) int a16 = 1;
// alignas(类型)
alignas(double) int a_dbl = 2;
// C11 拼写 _Alignas 无条件支持
_Alignas(8) int a8 = 3;

// C23 alignas 在 struct 成员上: 影响字段对齐和结构体布局
struct aligned_fields {
    char a;                    // offset 0, size 1
    alignas(8) char b;         // offset 8 (aligned to 8)
    int c;                     // offset 12 (natural align)
    alignas(16) int d;         // offset 16 (aligned to 16)
};

// _Alignas 在 struct 成员上同样生效
struct aligned_fields_c11 {
    char x;
    _Alignas(4) char y;        // offset 4
    _Alignas(8) short z;       // offset 8
};

int main(void) {
    if (((unsigned long)&a16) % 16 != 0) return 1;
    if (((unsigned long)&a_dbl) % 8 != 0) return 2;   // alignof(double) == 8
    if (((unsigned long)&a8) % 8 != 0) return 3;

    // struct 成员 alignas 验证
    struct aligned_fields s;
    if (sizeof(s.a) != 1) return 4;
    // b 应该对齐到 8 字节边界
    if (((unsigned long)&s.b - (unsigned long)&s) % 8 != 0) return 5;
    // d 应该对齐到 16 字节边界
    if (((unsigned long)&s.d - (unsigned long)&s) % 16 != 0) return 6;

    // C11 拼写验证
    struct aligned_fields_c11 s2;
    if (((unsigned long)&s2.y - (unsigned long)&s2) % 4 != 0) return 7;
    if (((unsigned long)&s2.z - (unsigned long)&s2) % 8 != 0) return 8;

    printf("OK a16=%d a_dbl=%d a8=%d\n", a16, a_dbl, a8);
    return 0;
}