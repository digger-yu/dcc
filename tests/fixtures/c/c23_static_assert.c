// C23: 文件级 static_assert (C23 中 static_assert 为关键字, 不依赖 <assert.h>)
// 文件作用域 + 块作用域 + 无消息形式
#include <stdio.h>

_Static_assert(1, "C11 spelling always available");
static_assert(sizeof(int) == 4, "int must be 4 bytes");
static_assert(sizeof(char) == 1, "char must be 1 byte");
static_assert(1, "");               // 空消息
static_assert(2 + 2 == 4);          // C23 无消息形式
static_assert(2 > 1);               // 常量表达式求值

// 依赖类型大小的断言
static_assert(sizeof(long) >= 4, "long too small");
static_assert(sizeof(void *) == 8, "64-bit target");

// 枚举底层类型 (依赖 C23 enum 语法)
enum E : unsigned char { EA = 0, EB = 1 };
static_assert(sizeof(enum E) == 1, "enum underlying type");

struct S { char c; int n; };
static_assert(sizeof(struct S) > 1, "struct layout");

int main(void) {
    // 块作用域断言
    static_assert(1, "block scope");
    static_assert(sizeof(int) >= 2);
    int x = 3;
    static_assert(sizeof(x) == 4, "sizeof local");
    if (x != 3) return 1;
    printf("OK\n");
    return 0;
}
