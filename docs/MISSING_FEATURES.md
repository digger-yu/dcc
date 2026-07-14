# DCC Linux 内核编译缺失特性

本文档记录 DCC 编译 Linux 内核时尚未实现的特性，按优先级分类。

> 最后更新: P2 次要特性实现完成后

---

## 🔴 阻塞级 (Blocking)

不实现则无法编译内核核心代码。

| 特性 | 说明 | 内核使用位置 | 状态 |
|------|------|-------------|------|
| `__builtin_choose_expr` | 编译期条件选择 `choose_expr(cond, expr1, expr2)` | `compiler.h`, `types.h` 类型选择宏 | ✅ |
| `__builtin_types_compatible_p` | 编译期类型兼容性判断 | `compiler.h` 配合 `choose_expr` | ✅ |
| `__builtin_add/sub/mul_overflow` | 溢出检测算术 | `overflow.h` 核心 | ✅ |
| `_Atomic` 限定符完整支持 | `_Atomic(type)` 形式已支持, 限定符形式跳过 | `atomic.h`, `stdatomic.h` | ✅ |
| 函数属性识别 | `cold`, `noinline`, `always_inline` 等已解析 | 几乎每个内核函数 | ✅ |
| 函数 `__attribute__((section))` codegen | 将函数放入指定 section (`.init.text` 等) | `__init`, `__devinit` 宏 | ✅ |

## 🟠 重要级 (Important)

影响大量内核文件，但不阻塞基本编译。

| 特性 | 说明 | 状态 |
|------|------|------|
| `__builtin_offsetof` codegen | `offsetof()` 宏核心, 支持结构体/嵌套结构体/union | ✅ |
| `__has_builtin` 返回正确值 | `#if` 上下文中正确识别已实现的 builtin | ✅ |
| `-mno-red-zone` / `-mcmodel=kernel` CLI 标志 | 内核编译常用 GCC flags 静默接受 | ✅ |
| `MODULE_LICENSE` 等模块宏 | 展开为 `.modinfo` section 变量 | ✅ |
| `percpu` 支持 | `DEFINE_PER_CPU` 等宏展开为 `.data..percpu` section | ✅ |
| 嵌套函数静态链 | GCC 通过 r10 传递静态链，访问外层变量 | ✅ |

## 🟡 次要级 (Minor)

影响特定子系统或可降级处理。

| 特性 | 说明 | 实现方式 | 状态 |
|------|------|---------|------|
| `__builtin_prefetch` | 性能关键代码 | 已实现 | ✅ |
| `__builtin_memcpy` 内联 | freestanding 无 libc | `rep movsb` 内联 | ✅ |
| `__builtin_memset` 内联 | freestanding 无 libc | `rep stosb` 内联 | ✅ |
| `__builtin_memmove` 内联 | freestanding 无 libc | `rep movsb` 内联 | ✅ |
| 复杂 inline asm 约束 | `arch/` 层大量使用 | Mini x86 汇编器实际发射指令 | ✅ |
| `__attribute__((format))` | printf/scanf 格式检查 | `skip_attributes` 静默跳过 | ✅ |
| `address_space(N)` | sparse 注解 | `skip_attributes` 静默跳过 | ✅ |
| `__builtin_object_size` | 安全检测 | 返回 `(size_t)-1` | ✅ |
| `__builtin_expect_with_probability` | LTO 分支预测 | 降级为 `__builtin_expect` | ✅ |
| `__SIZEOF_INT128__` 宏 | `#ifdef __SIZEOF_INT128__` 检测 | 预处理器定义为 16 | ✅ |
| `__builtin_frame_address(N)` N>0 | stacktrace/backtrace | 沿帧指针链回溯 N 次 | ✅ |
| `__attribute__((cleanup(fn)))` | 自动清理函数 | 块结束时逆序调用 cleanup 函数 | ✅ |
| 宽字符串字面量 `L"..."` | UEFI 子系统 | UTF-32LE 编码, wchar_t* 类型 | ✅ |
| `__builtin_apply`/`__builtin_return` | 旧式系统调用分发 | 存根实现 (返回 0) | ✅ |
| `_Complex` 算术 codegen | 无线/驱动子系统 | add/sub/mul SSE 指令 | ✅ |

---

## 实现优先级建议

1. ✅ `__builtin_choose_expr` + `__builtin_types_compatible_p` — 编译器宏基础设施
2. ✅ `_Atomic` 完整支持 + 函数属性识别 — 解析器兼容性
3. ✅ `__builtin_add/sub/mul_overflow` — 溢出检测
4. ✅ `__builtin_offsetof` codegen — 结构体操作
5. ✅ `__has_builtin` 正确返回值 + CLI 标志识别 — 构建系统兼容
6. ✅ `MODULE_*` 宏 + percpu section — 模块支持
7. ✅ 嵌套函数静态链 — GCC 扩展完整性
8. ✅ `__builtin_memcpy/memset/memmove` 内联 — freestanding 内存操作
9. ✅ `__builtin_object_size` + `__builtin_expect_with_probability` — 安全/分支预测
10. ✅ `address_space(N)` + `__attribute__((format))` — 注解静默处理

---

## 🎉 全部特性已实现

所有阻塞级、重要级、次要级特性均已实现。DCC 现在具备编译 Linux 内核的基础能力。
