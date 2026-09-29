// `typedef enum { ... } Name;` —— 枚举量与枚举别名都必须可用。
//
// 早前这条写法走「跳过成员体 + `Typedef{target:int}` 占位」的退化路径, 枚举量
// 因此**完全不存在** (使用点报 `undeclared identifier: E_A`), 而 C 头里
// `typedef enum { ... } kind_t;` 是基础写法。这里同时验证:
// 1. 枚举量在作用域里可见 (unscoped enum 把枚举量注入包围作用域);
// 2. 隐式递增与显式初值都正确;
// 3. `sizeof(别名)` 是枚举的底层整型大小;
// 4. 匿名枚举 (无 typedef 名) 的枚举量同样可见。
#include <cstdio>

typedef enum {
    E_A,
    E_B,
    E_C = 10,
    E_D,
} Kind;

typedef enum {
    FLAG_NONE = 0,
    FLAG_READ = 1 << 0,
    FLAG_WRITE = 1 << 1,
} Flags;

// 匿名枚举: 没有 typedef 名, 枚举量直接进包围作用域
enum {
    ANON_ONE = 5,
    ANON_TWO,
};

// 带 tag 的枚举 typedef: tag 与别名都可用
enum class Scoped : unsigned char {
    S_A = 3,
    S_B,
};

int main() {
    Kind k = E_A;
    printf("kind=%d,%d,%d,%d size=%zu\n", (int)E_A, (int)E_B, (int)E_C, (int)E_D,
           sizeof(Kind));
    for (int i = 0; i < 3; i++) {
        printf("iter%d=%d\n", i, (int)k);
        k = (Kind)((int)k + 1);
    }

    Flags f = (Flags)(FLAG_READ | FLAG_WRITE);
    printf("flags=%d,%d,%d\n", (int)FLAG_NONE, (int)FLAG_READ, (int)f);

    printf("anon=%d,%d\n", ANON_ONE, ANON_TWO);

    Scoped s = Scoped::S_B;
    printf("scoped=%d,%d\n", (int)Scoped::S_A, (int)s);

    // 枚举量用作数组维度: 必须是编译期常量
    int table[E_D];
    table[0] = 1;
    table[E_C] = 2;
    printf("table=%d,%d n=%zu\n", table[0], table[E_D - 1], sizeof(table) / sizeof(int));
    return table[0] + table[E_C] - 3;
}
