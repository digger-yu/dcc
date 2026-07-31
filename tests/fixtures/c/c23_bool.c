// C23: bool / true / false 关键字 (仅 -std=c23/gnu23 模式生效)
#include <stdio.h>

bool g_true = true;
bool g_false = false;

static _Bool check_conv(void) {
    // bool 与 _Bool 类型兼容
    bool b = (bool)5;
    return b;
}

int main(void) {
    bool a = true;
    bool b = false;
    if (a != 1 || b != 0) return 1;
    if (sizeof(bool) != 1) return 2;

    // true/false 是整型常量表达式 (int 类型, 值为 1/0)
    int sum = true + true + false;
    if (sum != 2) return 3;

    // bool 参与算术: 提升为 int
    int v = a * 10 + b;
    if (v != 10) return 4;

    // 条件/逻辑上下文
    bool cond = a && !b;
    if (!cond) return 5;

    if (g_true != 1 || g_false != 0) return 6;
    if (!check_conv()) return 7;

    // 结构体字段、函数参数
    struct S { bool flag; int n; } s = { true, 3 };
    if (s.flag != 1 || s.n != 3) return 8;

    printf("OK bool=%d %d sum=%d v=%d cond=%d g=%d,%d\n",
           a, b, sum, v, cond, g_true, g_false);
    return 0;
}
