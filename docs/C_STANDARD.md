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

## C11 (按需实装)

- `_Static_assert` (解析器静默跳过)
- `_Atomic` (基础类型表支持)
- 匿名 struct/union
- `<stdatomic.h>` 内建宏定义
- `_Generic` 泛型选择表达式
- `_Thread_local` 线程存储类 (解析器静默跳过, freestanding 下无需特殊处理)
- `_Noreturn` 函数限定符 (解析器静默跳过)

## C17

- C11 修正版, DCC 完全复用 C11 路径

## C23 (部分实装)

- `typeof` (GCC 扩展标准化) ✅
- `constexpr` — ✅ 已实装 (解析器静默跳过)
- `[[attributes]]` — ✅ 已实装 (解析器静默跳过 `[[deprecated]]`, `[[maybe_unused]]`, `[[nodiscard]]`)
- `_BitInt(N)` — ✅ 已实装 (按宽度映射到标准整数类型 8/16/32/64)
- 数字分隔符 `1'000'000` — ✅ 已实装 (词法分析器跳过 `'`)
- `#embed` — ✅ 已实装 (预处理器嵌入文件内容为字节数组，支持 `limit`/`prefix` 参数)
- `nullptr` — ✅ 已实装 (解析为 `(void*)0`)
- `#elifdef` / `#elifndef` — ✅ 已实装 (预处理器条件编译)

## GNU 扩展

- `__attribute__((...))` 多位置属性
- `__asm__ __volatile__` 带约束内联汇编
- `__builtin_*` (expect, constant_p, memcpy, memset, abs, unreachable, trap, clz, ctz, popcount, strcpy, strlen, bswap, parity, frame_address, return_address 等)
- 语句表达式 `({...})`
- 零长度数组 / 柔性数组成员
- `__VA_ARGS__` 可变参数宏
- `__extension__` 解析器直接跳过
- `_Alignof` 对齐查询
- `__complex__` / `_Complex` 复数类型
- `_Decimal32/64/128` 十进制浮点
- `_Fract` / `_Accum` 定点数类型
- `__builtin_va_list` / `__builtin_va_start` / `__builtin_va_arg` / `__builtin_va_end` 可变参数支持
- `__builtin_choose_expr` / `__builtin_types_compatible_p` 编译期类型选择与判断
- `__builtin_add_overflow` / `__builtin_sub_overflow` / `__builtin_mul_overflow` 溢出检测算术
- 嵌套函数定义 (GCC 扩展, 不含静态链外层变量访问)

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
