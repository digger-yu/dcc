# DCC C 标准支持

DCC 默认 `-std=gnu23`（与 GCC 15 的默认方言一致）。下列条目描述
当前已实现的语法和代码生成子集；未特别说明时，不代表与 GCC 完全兼容。

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

- `__int128` / `unsigned __int128` / `__int128_t` / `__uint128_t`
  （16 字节大小和对齐；整数加减、位运算、比较和移位使用 `rax:rdx`）
- 声明属性 `section`、`aligned`、`weak`、`visibility`、`unused`、
  `cold`、`noinline`、`always_inline`；ET_REL 输出会传递节、对齐、
  弱绑定和 ELF visibility，`cold` 默认进入 `.text.unlikely`
- `register TYPE name asm("reg")` 固定寄存器变量
- `__attribute__((...))` 多位置属性
- `__asm__ __volatile__` 带约束内联汇编 (Mini x86 汇编器实际发射指令, 支持 100+ 常见指令)
- `__builtin_*` (expect, expect_with_probability, constant_p, memcpy, memset, memmove, object_size, abs, unreachable, trap, clz, ctz, popcount, strcpy, strlen, bswap, parity, frame_address, return_address 等)
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
- `__builtin_offsetof` 结构体成员偏移计算 (支持嵌套结构体)
- `MODULE_LICENSE` / `MODULE_AUTHOR` / `MODULE_DESCRIPTION` 等内核模块宏
- `DEFINE_PER_CPU` / `DECLARE_PER_CPU` percpu 变量支持
- 嵌套函数定义 (GCC 扩展, 含静态链外层变量访问)
- `__builtin_memcpy` / `__builtin_memset` / `__builtin_memmove` 内联实现 (rep movsb/stosb, freestanding 无需 libc)
- `__builtin_object_size` 编译期对象大小（`&var`、数组 decay、常量下标/成员、
  模式 0–3）；未知时 `(size_t)-1`（parser/codegen 双路径折叠）
- `__builtin_apply` / `__builtin_return` SysV 子集（常量 size、`void (*)(void)` 回调）
- `__builtin_prefetch` 及 `__builtin_prefetch_*` 变体：void 编译期 no-op
- 未知 `__builtin_*`：warning + 零值/ud2，不静默链接不存在的 libc 符号
- `__builtin_expect_with_probability` 降级为 `__builtin_expect`
- `address_space(N)` sparse 注解 (静默跳过)
- `__attribute__((format))` printf/scanf 格式检查 (静默跳过)
- `&&label` 标签地址 (GNU 扩展, 通过 RIP-relative LEA 实现)
- `asm goto` 实际发射 (支持标签跳转)
- `goto *expr` 计算跳转

### GNU ABI 限制

- `__builtin_va_list` 当前采用 x86-64 简化指针模型。函数序言保存六个
  GP 参数寄存器，可支持常见整数/指针变参；SSE 变参、跨 GP 保存区与
  栈参数边界、超过 8 字节的 `va_arg` 尚不等价于完整 SysV ABI。
- `noinline` / `always_inline` 当前只保留在 AST 中。后端尚无通用内联
  优化 pass，因此二者不会改变机器码。
- `__int128` 乘法、除法、取模和完整 SysV 参数分类仍在完善中。
- ELF 的 section/weak/visibility 属性作用于 `-c`/`-r` 可重定位输出；
  直接 ET_EXEC 模式不提供完整符号表语义。

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
