// C23: alignas 关键字 (C23 拼写) 与 _Alignas (C11 拼写)
#include <stdio.h>

// alignas(常量表达式)
alignas(16) int a16 = 1;
// alignas(类型)
alignas(double) int a_dbl = 2;
// C11 拼写 _Alignas 无条件支持
_Alignas(8) int a8 = 3;

int main(void) {
    if (((unsigned long)&a16) % 16 != 0) return 1;
    if (((unsigned long)&a_dbl) % 8 != 0) return 2;   // alignof(double) == 8
    if (((unsigned long)&a8) % 8 != 0) return 3;

    // alignas(32) 局部变量: 栈变量对齐暂不支持, 仅测全局变量
    // struct 字段对齐: Field 结构体尚缺 alignment 字段, 暂不测

    printf("OK a16=%d a_dbl=%d a8=%d\n", a16, a_dbl, a8);
    return 0;
}