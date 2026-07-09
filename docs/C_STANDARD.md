# DCC C 标准支持

DCC 默认 `-std=gnu23` (与 GCC 15 一致)，完整支持以下标准及对应 GNU 扩展。

## C89 / C90 (ANSI C)

- 基础类型 (int, char, long, ...)
- struct / union / enum
- 函数声明与定义
- K&R 风格函数 (M5 之后实装)
- 预处理器 (#define, #include, #if, #ifdef, #ifndef, #else, #elif, #endif, #undef)

## C99

- 复合字面量 `(int[]){1,2,3}`
- 指定初始化器 `int a[10] = { [0]=1, [5]=2 };`
- 变长数组 (VLA)
- 柔性数组成员
- `long long`, `stdint.h` 整数类型
- `_Bool`
- `for (int i = 0; ...)` 声明
- `inline` 关键字

## C11

- `_Generic`
- `_Static_assert`
- `_Atomic` (基础实装)
- `_Thread_local`
- 匿名 struct/union
- `<threads.h>`, `<stdatomic.h>`

## C17

- C11 修正版, DCC 完全复用 C11 路径

## C23

- `typeof` (GCC 扩展标准化)
- `constexpr`
- `[[attributes]]`
- `_BitInt(N)`
- 数字分隔符 `1'000'000`
- 改进的 #embed

## GNU 扩展

- `__attribute__((...))`
- `__asm__ __volatile__`
- `__builtin_*` (offsetof, va_list, expect, constant_p)
- 语句表达式 `({...})`
- 零长度数组
- `__VA_ARGS__`
- 复合语句宏

## 重要选项

| 选项 | 含义 | 默认 |
|------|------|------|
| `-std=gnuXX` | 选择 C 标准 + GNU 扩展 | gnu23 |
| `-std=cXX` | 严格 C 标准 | - |
| `-ffreestanding` | 不假设标准库存在 | off (假设 hosted) |
| `-fno-builtin` | 不使用 builtin | off |

## Linux 内核编译

Linux 内核使用 `-std=gnu11` 默认；DCC 编内核时:
```bash
make HOSTCC="dcc -std=gnu11 -ffreestanding" \
     HOSTCXX="dcc++ -std=gnu++17 -ffreestanding" \
     -j$(nproc) vmlinux
```

`-ffreestanding` 告诉 DCC：不要假设存在 libc/CRT/启动文件。
