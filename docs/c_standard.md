# DCC C 标准支持

DCC 默认 `-std=gnu23`（与 GCC 15 的默认方言一致）。下列条目描述
当前已实现的语法和代码生成子集；未特别说明时，不代表与 GCC 完全兼容。

## C89 / C90 (ANSI C)

- 基础类型 (int, char, long, ...)
- struct / union / enum
- 函数声明与定义
- K&R 风格函数定义与参数声明
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
- `<stdatomic.h>` 内建宏（hosted 模式预置 `atomic_int`/`atomic_bool`/`memory_order_*`/`atomic_thread_fence`/`atomic_signal_fence`/`kill_dependency`；freestanding 模式 `__STDC_HOSTED__=0` + `__STDC_NO_ATOMICS__=1` 且不预置）
- `_Generic` 泛型选择表达式
- `_Thread_local` / `__thread` 线程局部存储（`.tdata`/`.tbss` + `STT_TLS`）
- `_Noreturn` 函数限定符 (解析器静默跳过)

## C17

- C11 修正版, DCC 完全复用 C11 路径

## C23

- `typeof` (GCC 扩展标准化) ✅
- `typeof_unqual` — ✅ 已实装 (去除限定符的 typeof)
- `constexpr` — ✅ 已实装 (函数声明等价 `inline` + `always_inline`，无强制编译期求值)
- `bool` / `true` / `false` — ✅ 已实装 (C23 关键字, `-std=gnu23`/`c23` 启用)
- `alignas` / `thread_local` — ✅ 已实装 (C23 关键字)
- `nullptr` / `nullptr_t` — ✅ 已实装 (解析为 `(void*)0`, `nullptr_t` 为内置类型)
- `static_assert` 无消息参数 — ✅ 已实装 (文件级与函数内)
- 枚举底层类型 `enum E : type` — ✅ 已实装 (枚举常量类型=底层类型)
- 省略 `=` 的大括号初始化 `int x{5}` — ✅ 已实装 (标量/数组/结构体/嵌套)
- `[[attributes]]` — ✅ 已实装 (解析器静默跳过 `[[deprecated]]`, `[[maybe_unused]]`, `[[nodiscard]]`)
- `_BitInt(N)` — ✅ 已实装 (按宽度映射到标准整数类型 8/16/32/64)
- 数字分隔符 `1'000'000` — ✅ 已实装 (词法分析器跳过 `'`)
- `#embed` — ✅ 已实装 (预处理器嵌入文件内容为字节数组, 支持 `limit`/`prefix`/`suffix`/`if_empty` 参数)
- `__has_embed` — ✅ 已实装 (预处理特性检测)
- `__has_c_attribute` — ✅ 已实装 (预处理特性检测)
- `__STDC_ENDIAN_*` — ✅ 已实装 (预定义宏: `__STDC_ENDIAN_BIG__`/`__STDC_ENDIAN_LITTLE__`/`__STDC_ENDIAN_NATIVE__`)
- `#elifdef` / `#elifndef` — ✅ 已实装 (预处理器条件编译)
- `%b` / `%B` printf 二进制格式 — ✅ 已实装 (格式检查与运行时行为)
- `-std=gnu23` / `-std=c23` — ✅ 已实装 (Driver 选项, 默认 `gnu23`)

## GNU 扩展

- `__int128` / `unsigned __int128` / `__int128_t` / `__uint128_t`
  （16 字节大小和对齐；整数加减、位运算、比较、移位、乘/除/取模；
  SysV 传参与返回值占两个 GP 寄存器 `rax:rdx`）
- 声明属性 `section`、`aligned`、`weak`、`visibility`、`unused`、
  `cold`、`noinline`、`always_inline`；ET_REL 输出会传递节、对齐、
  弱绑定和 ELF visibility，`cold` 默认进入 `.text.unlikely`
- `register TYPE name asm("reg")` 固定寄存器变量
- `__attribute__((...))` 多位置属性
- `__asm__ __volatile__` 带约束内联汇编 (Mini x86 汇编器实际发射指令,
  支持 100+ 常见指令；rol/prefetch/movss/addss 等内核常用扩展)
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
- `DEFINE_PER_CPU` / `DECLARE_PER_CPU` / `DEFINE_PER_CPU_SHARED_ALIGNED` percpu 变量声明
- `__percpu` 类型限定符 (展开为 `.data..percpu` section + `__seg_gs`)
- `__this_cpu_read` / `__this_cpu_write` / `__this_cpu_add` / `__this_cpu_inc` / `__this_cpu_dec` per-cpu 访问宏
- `__get_cpu_var` / `per_cpu` / `this_cpu_read` / `this_cpu_write` per-cpu 访问宏
- `__seg_gs` / `__seg_fs` 地址空间限定符 (存入类型系统, 用于 per-cpu 变量访问)
- 嵌套函数定义 (GCC 扩展, 含静态链外层变量访问)
- `__builtin_memcpy` / `__builtin_memset` / `__builtin_memmove` 内联实现 (rep movsb/stosb, freestanding 无需 libc)
- `__builtin_object_size` 编译期对象大小（`&var`、数组 decay、常量下标/成员、
  模式 0–3）；未知时 `(size_t)-1`（parser/codegen 双路径折叠）
- `__builtin_apply` / `__builtin_return` SysV 子集（常量 size、`void (*)(void)` 回调）
- `__builtin_prefetch` 及 `__builtin_prefetch_*` 变体：void 编译期 no-op
- 未知 `__builtin_*`：warning + 零值（有返回值）或 **ud2**（void），
  不静默链接不存在的 libc 符号
- `__builtin_constant_p`：parser/opt/codegen 共享 `const_eval::is_constant`
- `__builtin_*` 编译期折叠 (M15)：`__builtin_strlen`/`__builtin_abs/labs/llabs`/
  `__builtin_bswap16/32/64`/`__builtin_expect`/`__builtin_popcount*` —
  解析阶段走 `const_eval::eval_full` 折叠为整数常量
- `__builtin_expect_with_probability` (M24) 与 `__builtin_expect` 解析层同路径:
  字面量首参编译期折叠为整数常量, 其它形态走 codegen builtin 路径
- 严格类型检查 warning (M15)：`-Wint-truncation` / `-Wint-conversion` —
  cast 阶段检测整数截断与隐式指针↔整数转换，仅 warning 不阻塞
- 运行时边界检查 (M15)：`-fbounds-check` 对 `ArraySize::Const(n)` 数组
  下标插入 `cmp rcx, n*es; jb ok; call __ubsan_handle_out_of_bounds`
- 整数溢出 trap (M15)：`-ftrapv` 在 `Add/Sub/Mul/Div` 后置
  `jo` 跳到 `__dcc_trapv_overflow` 桩函数
- `address_space(N)` sparse 注解 (静默跳过)
- `__seg_fs` / `__seg_gs` / `__seg_cs` / `__seg_ds` / `__seg_es` / `__seg_ss`
  x86 段寄存器地址空间限定符 (仅解析不处理: 解析器跳过这些关键字,
  不存入类型系统, 也不影响代码生成. 接受 decl-specifier / 类型与
  `*` 之间 / `*` 与标识符之间 / cast / typedef / typeof / struct
  字段 / 函数指针声明符 / 函数指针返回类型 / 函数指针字段声明符
  / sizeof / _Alignof 等所有位置. 详见 `docs/missing_features.md`
  中段寄存器 codegen 限制. 测试: `tests/fixtures/c/seg_qualifier.c`)
- `__attribute__((format))` printf/scanf 完整格式检查（M23: flag/width/precision/length/type 匹配）
- `&&label` 标签地址 (GNU 扩展, 通过 RIP-relative LEA 实现)
- `asm goto` 实际发射 (支持标签跳转)
- `goto *expr` 计算跳转

### GNU ABI 限制

- `__builtin_va_list` 当前采用 x86-64 简化指针模型。函数序言保存六个
  GP 参数寄存器，可支持常见整数/指针变参；SSE 变参、跨 GP 保存区与
  栈参数边界、超过 8 字节的 `va_arg` 尚不等价于完整 SysV ABI。
- `noinline` / `always_inline`：`-O0+` 仍内联 `always_inline` 单返回体；
  `-O1+` 完整内联 pass；`noinline` 永不内联；`-O3` opportunistic 小函数。
- **优化级别**：`-O2` 启用 GVN/CSE、拷贝传播、LICM；`-O3` 额外循环
  展开（trip≤4）与向量元素循环向量化（标量 SSE/GP 后端不变）。
- ELF 的 section/weak/visibility 属性作用于 `-c`/`-r` 可重定位输出；
  ET_EXEC 输出含 `.symtab`/`.strtab` 与函数/全局符号。
- `-fpermissive`：未知 typedef 降级为 `int` 并发出 `W0002`（默认严格 `E0009`）。
  在 C99/GNU99 及更新标准中，未声明函数调用默认报 `E0017`；
  `-fpermissive` 或 C89/GNU89 模式保留 `W1000` + `int` 降级。
  dcc++ 降级到内部 C 后仍以 `-fpermissive` 编译生成的兼容 C。
- **Sanitizer（AST 层完整桩）**：`-fsanitize=address|undefined|thread|hwaddress|fuzzer|kernel-*`、
  `-fsanitize=a,b` 组合、`-fsanitize-coverage`。Driver 在 IR lower 前于
  AST 插桩：TU 级 `__asan_init` / `__hwasan_init` / `__ubsan_init` /
  `__kcsan_init` 仅在首个有体函数前各插入一次（内核模式不发射
  `__asan_init` / `__hwasan_init`）；每个有体函数（含嵌套函数）
  入口插入 `__sanitizer_cov_trace_pc`（coverage）与 `__kcsan_func_entry`
  （KCSAN）；表达式层在 `Deref/Index/ArrowMember` 与对应 `Assign` LHS、
  `PreInc/PreDec/PostInc/PostDec` 内存目标之前注入
  `__asan_load1/2/4/8/16` / `__asan_storeN`；`kernel-address` 走内联
  shadow 检查 + `__asan_report_loadN/storeN`；`Div/Mod` /
  `Shl/Shr` / 有符号 `Add/Sub/Mul` 之前注入 `__ubsan_handle_*`；比较
  之前注入 `__sanitizer_cov_trace_cmp1/2/4/8`；switch dispatch 之前
  `__sanitizer_cov_trace_switch`；KCOV 在每个 `if/while/do/for/case/label/goto`
  边位置插 `__sanitizer_cov_trace_pc_guard(&__dcc_cov_guards_N[i])`。
  `-c` 输出附带 `.dcc.san_info`（`DCC_SAN` v2 + 六标志位）。
  `no_sanitize("address"/"undefined"/"thread"/"all")` 按通道关闭；
  KCOV / Fuzzer 不受 `no_sanitize` 影响。

## 重要选项

| 选项 | 含义 | 默认 |
|------|------|------|
| `-std=gnuXX` | 选择 C 标准 + GNU 扩展 | gnu23 |
| `-std=cXX` | 严格 C 标准 | - |
| `-ffreestanding` | 不假设标准库存在 | off (假设 hosted) |
| `-fno-builtin` | 不使用 builtin | off |
| `-fsanitize=address` | KASAN 桩：TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 load/store（含 `(*p)++` 读-改-写）前 + codegen 级栈/全局变量红区插桩 | off |
| `-fsanitize=hwaddress` | HWASAN 桩：同 KASAN 路径但使用 `__hwasan_loadN/storeN`；AArch64 目标上 codegen 启用 MTE 硬件 tag 模式（prologue tag 栈帧 + 指针 tag 流程）；内核模式 (`kernel-hwaddress`) 发内联 tag 检查（指针 tag vs 内存 tag），不匹配调 `__hwasan_tag_mismatch{size}` | off |
| `-fsanitize=undefined` | UBSAN 桩：div / shift / 有符号 overflow / VLA bound / null deref / float cast 前 `__ubsan_handle_*` | off |
| `-fsanitize=thread` | KCSAN 桩：TU init + 每函数 entry + 每次访存独立 `__kcsan_check_access(addr, size, is_write)`，可与 KASAN 共存 | off |
| `-fsanitize=kernel-address` | 内联 shadow 检查（多架构 shadow 偏移：x86_64 `0xdffffc0000000000` / AArch64 `0xdfff600000000000` / RISC-V 64 `0xdffffe0000000000`）+ `__asan_report_loadN/storeN`，无用户态运行时依赖 | off |
| `-fsanitize=fuzzer` | Fuzzer：KCOV + `__sanitizer_cov_trace_cmpN` + `__sanitizer_cov_trace_switch` | off |
| `-fsanitize-coverage` | KCOV：`__sanitizer_cov_trace_pc` + 边覆盖 guard 计数器 | off |

## Linux 内核编译

Linux 内核使用 `-std=gnu11` 默认；DCC 编内核时:
```bash
make HOSTCC="dcc -std=gnu11 -ffreestanding" \
     HOSTCXX="dcc++ -std=gnu++17 -ffreestanding" \
     -j$(nproc) vmlinux
```

`-ffreestanding` 告诉 DCC：不要假设存在 libc/CRT/启动文件。
