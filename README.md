# dcc

> 一个极简 C 编译器。<br>
"Make doesn't strictly require GCC, <br> but you obviously need a C compiler to build C code. <br> Try dcc; it's noticeably faster."

## 目录

- [1. 概述](#1-概述)
- [2. 调用约定](#2-调用约定)
- [3. 语言支持](#3-语言支持)
- [4. 目标平台](#4-目标平台)
- [5. 代码生成](#5-代码生成)
- [6. 诊断与调试](#6-诊断与调试)
- [7. 内建函数](#7-内建函数)
- [8. 不支持的功能](#8-不支持的功能)
- [9. 路线图](#9-路线图)
- [10. 交叉编译](#10-交叉编译)
- [11. 限制与已知问题](#11-限制与已知问题)
- [12. 引用文档](#12-引用文档)

## 1. 概述

| 维度 | 当前状态 |
|------|----------|
| 实现语言 | Rust（suckless 零 crate 依赖，ADR-005） |
| 输入 | C89/99/11/17/23 + GNU 扩展；C++ 子集（M7–M18，详见 §3.2） |
| 中间层 | typed IR（`src/ir.rs`）+ 轻量 SSA（M16，`src/ssa.rs`） |
| 输出 | ELF64（ET_EXEC / ET_REL / ET_DYN），自写 `src/codegen/elf.rs` |
| 后端 | x86_64 / aarch64 / riscv64 / mips / mips64 / arm 全部完整（`has_backend() == true`） |
| 链接器 | 自有 `src/link/` 内置链接器；旧路径走 host `ld -r`（`link/legacy.rs`） |
| LTO | 自有 `.dcc.lto` 段 + DBC 位码（`src/link/lto_bitcode.rs`） |
| 当前阶段 | **alpha**（M0–M25 已完成；M5 自举未开始；内核全树未首跑） |

## 2. 调用约定

`dcc [options] <input>`，详细实现在 `src/cli.rs`。

### 2.1 总体

| 选项 | 含义 |
|------|------|
| `-c` | 输出 `.o`（ET_REL），不链接 |
| `-r` | 链接 `.o` → `.o`（合并可重定位），依赖 host `ld` 或内置链接器 |
| `-S` | 输出 `.s` 汇编文本，不汇编 |
| `-E` | 只做预处理，stdout 输出 |
| `-o <file>` | 输出路径，默认 `a.out` |
| `-x <lang>` | 强制输入语言 `c` / `c++` / `assembler-with-cpp` |
| `--target <triple>` | 目标三元组（见 §4） |
| `-std=<mode>` | C 标准模式（见 §3.1） |
| `-ffreestanding` | 不假设 libc/CRT |
| `-fpermissive` | 未知 typedef 降级为 `int`（W0002） |
| `-fsyntax-only` | 只跑解析与语义检查 |
| `-pipe` | 接受（no-op） |
| `-dump-ast` | 打印 AST 后退出 |
| `-v`, `--verbose` | 详细输出 |
| `--version` / `--help` / `-dumpversion` / `-dumpmachine` / `-print-file-name=` / `-print-search-dirs` | GCC 兼容查询 |

### 2.2 优化与调试

| 选项 | 含义 |
|------|------|
| `-O0` / `-O1` / `-O2` / `-O3` / `-Os` | 优化级别（默认 O0，Os 合并到 O2） |
| `-g0` / `-g1` / `-g2` / `-g3` | DWARF 调试等级（默认 0，-g 等价 -g2） |
| `-flto` | 启用 LTO（per-function sections + `.dcc.lto` 摘要） |
| `-ffunction-sections` / `-fno-function-sections` | 每函数独立 `.text.<name>` 节 |
| `-fno-omit-frame-pointer` / `-fomit-frame-pointer` | 帧指针开关 |
| `-mno-red-zone` | 关闭 128 字节红区（内核必需） |
| `-mno-sse` / `-mno-sse2` | 关闭 SSE 浮点（E0300 与浮点 TU 冲突） |
| `-D <name>[=<val>]` | 预定义宏 |
| `-I <dir>` | 头文件搜索目录 |
| `-isystem <dir>` | 系统头搜索目录 |
| `-include <file>` | 强制包含 |
| `-MD` / `-MMD` | 生成 `.d` 依赖文件 |
| `-MF <file>` / `-MT <tgt>` / `-MQ <tgt>` / `-MP` | 依赖文件参数 |
| `-Wp,<opts>` | 预处理器参数透传 |
| `-Wa,<opts>` | 汇编器参数透传 |
| `-Wl,<opts>` | 链接器参数透传（无 `-fuse-ld` 时走 host） |
| `-fpch-create=<f>` / `-fpch-include=<f>` / `-fpch-include-only=<f>` | 预编译头（M19） |
| `-ftrapv` / `-fbounds-check` / `-fbounds-safety` | 整数溢出 trap / 数组边界检查（M15） |
| `-Wint-conversion` / `-Wint-truncation` | 严格整数类型检查（M15） |
| `-W` 其它 | 接受但 no-op |

### 2.3 链接器 / 内置链接

| 选项 | 含义 |
|------|------|
| `-pie` / `-shared` / `-fpie` / `-fpic` / `-fPIE` / `-fPIC` | 输出 ET_DYN（PIE / 共享库） |
| `-fbuiltin-linker` / `-fno-builtin-linker` | 强制走内置链接器 |
| `-fuse-ld=<name>` | `dcc` / `gold` / `lld` / `bfd`（`DCC_FUSE_LD` 覆盖） |
| `-z relro` / `-z now` / `-z lazy` / `-z execstack` / `-z noexecstack` / `-z max-page-size=N` / `-z pack-relative-relocs` | GNU ld 兼容 |
| `-rpath` / `-rpath-link` / `-runpath` | DT_RPATH / DT_RUNPATH |
| `-Wl,-soname=` / `-Wl,--exclude-libs=` / `-Wl,--build-id=sha1\|uuid\|md5\|none` / `-Wl,--no-undefined-version` | ELF 扩展 |
| `-Wl,--gdb-index` / `-Wl,--compress-debug-sections=zlib\|zstd` | 调试节 |
| `-Wl,-u <sym>` / `-u <sym>` | gc-sections 根符号 |
| `-fno-semantic-interposition` / `-fsemantic-interposition` | LTO 插入语义 |
| `-flto-partition=N` | 接受但仅单分区 |
| `-e <sym>` | 自定义 entry |
| `-l<lib>` / `-L<dir>` | 链接库搜索 |
| `--sysroot=<path>` / `-Wl,--sysroot=` | musl 交叉 sysroot（M25） |
| `-static` / `-nostdinc` / `-nostdlib` / `-nodefaultlibs` / `-fno-pic` / `-fno-stack-protector` / `-fno-strict-aliasing` / `-fno-common` 等 | 接受（常用于内核 makefile） |

### 2.4 诊断输出

| 选项 | 含义 |
|------|------|
| `-fdiagnostics-color=always\|never\|auto` | ANSI 颜色（默认 auto） |
| `-fdiagnostics-format=github\|clang\|msvc\|vi\|sarif` | 输出风格（默认 github） |
| `-fdiagnostics-show-template-tree` | C++ 模板树展开（C 端 no-op） |
| `--bootstrap-compare=<ref.o>` | 自举回归：byte / struct / semantic 三层对比（M25） |

### 2.5 Sanitizer（`-fsanitize=` 子集，详细码表见 `docs/diagnostics.md` §3）

| 取值 | 状态 |
|------|------|
| `address` / `kernel-address` | KASAN 桩（TU init + load/store 通道） |
| `hwaddress` | HWASAN 桩（共享 KASAN 路径，ARM MTE 未做） |
| `undefined` | UBSAN 桩（div / shift / 有符号 overflow / null deref / float cast / VLA bound） |
| `thread` / `kernel-thread` / `kernel-kcsan` | KCSAN 桩（每函数 entry，专用 `__kcsan_check_access`） |
| `fuzzer` | KCOV + cmp/switch 追踪 |
| `-fsanitize-coverage` | KCOV 边覆盖（独立开关） |
| `no_sanitize("address"/"undefined"/"thread"/"all")` 属性 | 通道级关闭 |

## 3. 语言支持

### 3.1 C

默认 `-std=gnu23`，与 GCC 15 一致。

| 标准 | 状态 | 覆盖 |
|------|------|------|
| C89 / C90 | ✅ | 含 K&R 函数（自动检测 `is_old_style`） |
| C99 | ✅ | 复合字面量 / 指定初始化 / VLA / `long long` / `_Bool` / `inline` / `for` 内声明 |
| C11 | ✅ | `_Static_assert` / `_Atomic` / 匿名 struct/union / `_Generic` / `_Thread_local` / `_Noreturn` |
| C17 | ✅ | C11 修正版，复用 C11 路径 |
| C23 | ✅ 完整 | `typeof` / `typeof_unqual` / `constexpr` / `bool`/`true`/`false` 关键字 / `alignas` / `thread_local` / `nullptr`/`nullptr_t` / `static_assert` 无消息 / 枚举底层类型 `: type` / 省略 `=` 大括号初始化 / `[[attributes]]` / `_BitInt(N)` / 数字分隔符 / `#embed`+`prefix`/`suffix`/`if_empty` / `__has_embed` / `__has_c_attribute` / `__STDC_ENDIAN_*` / `#elifdef`/`#elifndef` / `%b`/`%B` printf 二进制 / `-std=gnu23`/`c23` |
| GNU 扩展 | ✅ | `__int128`（`rax:rdx`）/ 语句表达式 / `__VA_ARGS__` / 嵌套函数 / computed goto / `asm goto` / `&&label` / `__attribute__` 全家族 / `__declspec` 翻译 / `_Complex` / `_Decimal` / `_Fract` / `_Accum` |

### 3.2 C++

C++ 走"降级到 C"路径（`src/cpp/lower.rs`），Itanium ABI 名字修饰，
复用 C 后端 + 链接器。**不**另起 IR。

| 范围 | 状态 | 覆盖 |
|------|------|------|
| 类与对象 | ✅ | class / struct / ctor / dtor / 成员函数 / 成员变量 / `this->` / 引用→指针 |
| 继承 | ✅ | 单 / 多 / 钻石；虚函数 + vtable（`_ZTV`）+ RTTI（`_ZTI`） |
| 运算符 | ✅ | 命名空间 / operator / Itanium 编码（`pl`/`mi`/`dv`/...） |
| 异常 | ✅ | `try`/`catch`/`throw` → `__cxa_*` 桩 |
| Lambda | ✅ | 匿名 `__lambda_N` + `operator()` |
| 模板 | ✅ | 解析 + 完整单态化 (src/cpp/template.rs)：注册/实例化缓存/偏特化匹配/模板-id 调用解析；496 单元测试 + 255 集成测试 |
| SFINAE / concepts / coroutine | ✅ | `SfinaeCtx` 替换失败不报错；概念求值；`co_await`/`co_yield`/`co_return` Duff's device 状态机 (src/cpp/coroutine.rs) |
| modules | ✅ | BMF 序列化 (src/cpp/bmf.rs)：DCC_BMF_V1 文本格式，`to_bmf`/`from_bmf` round-trip；ODR 检查待后续 |
| C++17 修饰 | ✅ | `decltype` / `auto` 真推导 / `friend` / `noexcept` / `final` / `override` / `mutable` / `volatile` 成员函数 / `explicit` / `if constexpr` / `using Base::Base` 构造委托 |
| C++20/23 库 | ✅ | `std::filesystem` / `thread` / `mutex` / `condition_variable` / `chrono` / `expected` / `flat_map` / `regex`（子集）/ `ranges` / `views` / 多维下标 / placement new / `new T[n]` / `operator new[]` / `delete[]` |
| GCC 兼容 | ✅ | `obj.*ptr_to_member` / `typeid(e)` 真 RTTI / 模板实参 `T<U>` / 跨虚继承 `dynamic_cast` / `__cxa_guard_*` / `std::initializer_list` / `__dcc_typeinfo_t` / `__dynamic_cast` / 成员指针偏移宏 (src/cpp/gcc_compat.rs) |

### 3.3 内联汇编

| 范围 | 状态 |
|------|------|
| `__asm__` / `asm` / 文件级 `asm()` | ✅ |
| AT&T 约束全家族 | ✅ `r` / `m` / `=r` / `+m` / `&` (early-clobber) / `i` / `0..9` / 命名操作数 |
| `asm goto` | ✅ 标签跳转真实发射（M11.2） |
| mini-asm (x86_64) | ✅ 100+ 指令；AVX/AVX2/AVX-512(EVEX) / SSE3-SSE4.2 / AES-NI / SHA / FMA / F16C / TSX / AMX / AVX-VNNI 全部（M13） |
| mini-asm (aarch64) | ✅ BL ±128MiB / ADR ±1MiB / HFA / sret / CCMP/CSEL / 缓存维护 / SIMD 基础 |
| mini-asm (riscv64) | ✅ JAL ±1MiB / 压缩 C 扩展 / D 扩展 / V 扩展基础 / Zicsr 全 6 条 |

### 3.4 预处理器

实现：`src/preproc.rs`；缓存 V7，`~/.dcc/cache/<filehash>_<contenthash>`，
键含宏表状态哈希（`-D_FORTIFY_SOURCE=2` 切换自动失配）。

| 特性 | 状态 |
|------|------|
| `#include` / `#define` / `#if` 全家族 / `#elifdef` / `#elifndef` / `#endif` / `#include_next` / `#pragma once` | ✅ |
| `#warning` / `#error` | ✅ |
| `#pragma` 全家族 + `_Pragma` + `__pragma` + `#pragma STDC FENV_ACCESS/FP_CONTRACT/CX_LIMITED_RANGE` | ✅ |
| `#embed` (C23) 字节数组 + `limit` / `prefix` / `suffix` / `if_empty` | ✅ |
| `__has_embed` (C23) / `__has_c_attribute` (C23) | ✅ |
| `__STDC_ENDIAN_BIG__` / `__STDC_ENDIAN_LITTLE__` / `__STDC_ENDIAN_NATIVE__` (C23) | ✅ |
| `__VA_ARGS__` / `__VA_OPT__` / `__has_include` / `__has_attribute` / `__has_builtin` | ✅ |
| `__FILE__` / `__LINE__` / `__DATE__` / `__TIME__` / `__COUNTER__` | ✅（COUNTER 跨宏嵌套共享） |
| 预定义宏 | ✅ `__linux__` / `__GNUC__` / `__amd64__` / `__x86_64` / `__USER_LABEL_PREFIX__` / `__VERSION__` / `__OPTIMIZE__` / `__PIC__` / `__PIE__` / `__code_model_small__` / `__SEG_GS` / `__SEG_FS` |
| 增量预处理 PCH（`PchBundle` + 宏状态校验 + DCC_PCH_V1 魔数） | ✅ `-fpch-create=` / `-fpch-include=`（M19 实装） |

> **M26 增量（2026-08-06）**：C23 预处理全家族测试覆盖补全 — 新增
> 27 个单元测试 + 10 个集成测试（fixture `elifdef.c` / `has_macros.c`），
> 覆盖 `#elifdef`/`#elifndef`、`#embed`（limit/prefix/suffix/if_empty）、
> `__has_embed`/`__has_include`/`__has_c_attribute`/`__has_builtin`/
> `__has_attribute`、`__STDC_ENDIAN_*`、`#pragma once`、`#warning`/
> `#error`、`#include_next` 与预定义宏在 `#if` 中的求值。

## 4. 目标平台

`Arch::has_backend()` 当前对全部 6 个 arch 返回 `true`（`src/cli.rs` §1）。

| Triple | 状态 | 指针 | 字节序 | 调用约定 | ELF | 几何 |
|--------|------|------|--------|----------|------|------|
| `x86_64-unknown-linux-gnu` | ✅ 完整（M0） | 8 | LE | SysV | ELF64 | SSE/AVX 全家族 |
| `aarch64-linux-gnu` | ✅ 完整（M7-M8） | 8 | LE | AAPCS64 | ELF64 | HFA / sret / TLSDESC（M25） |
| `riscv64-linux-gnu` | ✅ 完整（M7-M8） | 8 | LE | SysV | ELF64 | D 扩展 / V 扩展基础 / Zicsr（M25） |
| `mips-linux-gnu` | ✅ 完整（M8） | 4 | BE | O32 | ELF32 | big-endian |
| `mips64-linux-gnuabi64` | ✅ 完整（M8） | 8 | BE | N64 | ELF64 | big-endian |
| `arm-linux-gnueabi(hf)` | ✅ 完整（M8） | 4 | LE | AAPCS | ELF32 | `armv7` alias 接受 |

变参（`va_list` / `va_arg`）支持范围：

| 架构 | GP / 整数 | FP / 浮点 | 栈溢出 |
|------|-----------|-----------|--------|
| x86-64 SysV | ✅ 6 个 GP 保存区 | ✅ XMM0–7 保存区 | 栈溢出（>8 个）部分支持 |
| aarch64 AAPCS64 | ✅ x0–x7 保存区（M25） | 🟠 v0–7 stub（返回 0.0） | 🔴 |
| riscv64 SysV | ✅ a0–a7 保存区（M25） | 🟠 fa0–fa7 stub | 🔴 |
| mips / mips64 / arm | 🟠 stub | 🟠 stub | 🔴 |

TLS 模式：local-exec / TLSDESC（aarch64 4 指令序列 + 3 个 R_AARCH64_TLSDESC_*
重定位，glibc `_dl_tlsdesc_resolve` 由 host 提供）。

## 5. 代码生成

### 5.1 优化遍（`src/opt.rs` + `src/ssa*.rs` + `src/loop_opt.rs`）

| 级别 | 启用 | 包含 |
|------|------|------|
| `-O0` | 默认 | `always_inline` 强制内联单返回体 |
| `-O1` | 显式 | 常量折叠 + 内联 |
| `-O2` | 默认推荐 | GVN / CSE / 拷贝传播 / LICM / 分支折叠（保留 goto 目标）/ 死代码消除（`cross_function_dce`） / 强度削减 / 归纳变量优化 / 别名分析（Steensgaard） |
| `-O3` | 显式 | 循环展开（trip≤4）+ 向量元素循环向量化（标量 SSE/GP）+ 指令调度（BB 内 list scheduling）+ TCO / 循环分块 / 循环交换 / 稀疏条件常量传播 / 函数属性传播（hot/cold） |
| `-O2+` | 默认 | 函数级并行 codegen（`std::thread` 池，`DCC_NO_PARALLEL=1` 关闭） |

### 5.2 LTO（`src/link/lto_bitcode.rs`）

| 项 | 状态 |
|----|------|
| 自有 `.dcc.lto` 段 | ✅ 魔数 `DBC\0` + version 1 |
| 跨 TU DCE | ✅ |
| 跨函数内联 | ✅ cost model = 语句 ≤4 ∧ 表达式 ≤4 ∧ 无递归 |
| 跨 TU 常量传播 | ✅ |
| 全局 DAE | ✅ |
| `LtoConfig` 配置化 | ✅ |
| Pass 7 `no_semantic_interposition` | ✅ |
| IPA-SRA / PGO 集成 | ✅ |

### 5.3 链接器（`src/link/`）

| 模块 | 能力 |
|------|------|
| `mod.rs` | 链接器入口管线 + 多架构 `_start` 桩合成（`ensure_start_stub`：x86_64/AArch64/RISC-V64/ARM，PIE 自动 PT_INTERP） |
| `object.rs` / `resolve.rs` | ET_REL 解析；跨 TU 强 > 弱 > undef；implicit undef 白名单（`wcs*`/`wmem*`/`sigaction`/`signal`/`cproj`/`cabs`/`carg`） |
| `layout.rs` | 同名节合并 + R+X / R+W 2 段 |
| `relocate.rs` | x86-64 13 / AArch64 22 / RISC-V 15 / TLS / GOT64 重定位 |
| `plt_got.rs` | `.plt.sec` / `.plt.got` / `.dynamic` / `.dynsym` |
| `elf_exec.rs` / `elf_dyn.rs` | 5 架构 ET_EXEC / ET_DYN（PIE/Shared）+ PT_INTERP |
| `gnu_hash.rs` | `.gnu.hash` TIE（bloom filter + buckets） |
| `eh_frame.rs` / `build_id.rs` | `.eh_frame_hdr` 二分表 + `.note.gnu.build-id`（sha1/md5/uuid） |
| `ifunc.rs` + `target_clones.rs` | `R_X86_64_IRELATIVE` + 多版本 IFUNC |
| `symver.rs` | `VERDEF` / `VERNEED` + `--no-undefined-version` |
| `link_opts.rs` | `-z relro/now/lazy/execstack/noexecstack/max-page-size=N/pack-relative-relocs`、RPATH/RUNPATH、`-fuse-ld=`、`-fno-semantic-interposition`、SHF_GNU_RETAIN |
| `fini_dispatch.rs` | `__dso_handle` / `__cxa_atexit` / `atexit` / `.fini_array` 按 priority |
| `legacy.rs` | `-r` 内置 `link_relocatable_builtin` 优先；旧路径 `link_relocatable_legacy`（host `ld -r --gc-sections`） |
| 调试节压缩 | zlib + zstd（`ELFCOMPRESS_ZSTD` 头，level 3） |

## 6. 诊断与调试

### 6.1 错误码

详细码表见 [`docs/diagnostics.md`](./docs/diagnostics.md)：
E0001–E0013（Parser）、E0100–E0102 / E0300（Driver）、E0200–E0209
（Preprocessor；E0200–E0204 与 Link 路径**号段重叠**）、E1001–E1010
（Typed IR）、W0001 / W0200。Snapshot 回归：`tests/fixtures/errors/`
共 37 个 `.stderr.snap`，版本常量 `DIAGNOSTIC_FORMAT_VERSION = 1`。

### 6.2 输出格式

`-fdiagnostics-format=github`（默认 GitHub 四段式）/ `clang` /
`msvc` / `vi` / `sarif`（SARIF 2.1.0 JSON 写 stderr）。

### 6.3 调试信息（`src/debug.rs`）

| 项 | 状态 |
|----|------|
| DWARF v4 | ✅ x86_64 / aarch64 / riscv64 |
| `.BTF` / `.eh_frame` + CFI / `.orc_unwind_ip` | ✅ x86_64（M24 setjmp FDE 含 `DW_CFA_*` ops）；其它架构 placeholder |
| DWARF line table | ✅ M24 增强：相邻同源同行条目在 `-g1` 下自动去重 |
| 位域 DWARF | ✅ `DW_AT_bit_size` + `DW_AT_bit_offset`（M24） |
| `-gsplit-dwarf` / `.gdb_index` | ✅ `.dwo` 文件生成 + 最小 `.gdb_index` 占位（GDB 可识别） |

### 6.4 性能分析（`src/profile/`，详见 `docs/profiling.md`）

| 子系统 | CLI | 状态 |
|--------|------|------|
| PGO (frontend) | `-fprofile-generate[=dir]` / `-fprofile-use=file` | ✅ `.profraw` 二进制；driver/codegen 接入 `__dcc_pgo_count_fn` |
| PGO (instr) | `-fprofile-instr-generate` / `-fprofile-instr-use=` | ✅ stub（仅入口计数器） |
| GCOV | `-ftest-coverage` / `-fprofile-arcs` | ✅ `.gcno` + `.gcda`（GCC gcov-9 layout 对齐，CFG checksum 拓扑敏感） |
| AutoFDO | `-fauto-profile=` / `-fprofile-sample-use=` | ✅ text parser + bucketize + inline calls 链 + merge + summary；接入 hot 函数下沉 `.text.hot` |
| LBR | `-flbr-profile=` | ✅ `perf script -F brstack` 解析 (含 mispredict/cycles) + inbound/outbound 边查询 + merge + summary |
| MemProf | `-fmemory-profile` / `-fmemory-profile-use=` | ✅ text parser + AllocClass 分类 (Hot/Normal/Cold/Ephemeral) + lifetime 分析 + merge + summary |
| 跨源权重聚合 | 任意 `-fprofile-*` 组合 | ✅ `ProfileWeights` 聚合 4 源（M25） |
| TopdownL1 | `-ftopdownmetrics` | ✅ L1 四 slot + L2 八子分类 + `perf stat` 解析 + `primary_bottleneck` 诊断 + merge + summary |
| Offloading | `-fopenmp` / `-fopenmp=simd` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=` | ✅ `OffloadConfig` 全形式解析 + `OffloadSection` 二进制序列化 + `.dcc.offload` ELF 节 + device codegen (PTX/AMDGCN) |

## 7. 内建函数

实现：`src/codegen/x86_64.rs::builtin_call` / `src/const_eval.rs`。
详细列表见 [`docs/c_standard.md` §GNU 扩展](./docs/c_standard.md)。

| 类别 | 代表 |
|------|------|
| 变参 | `__builtin_va_list` / `__builtin_va_start` / `__builtin_va_end` / `__builtin_va_arg` / `__builtin_va_copy`（架构支持见 §4 表） |
| 框架 / 栈 | `__builtin_frame_address` / `__builtin_return_address` / `__builtin_call_with_static_chain` |
| 溢出 / 算术 | `__builtin_add_overflow` / `__builtin_sub_overflow` / `__builtin_mul_overflow` / `__builtin_abs` / `__builtin_labs` / `__builtin_llabs` |
| 位 / 字节 | `__builtin_clz` / `__builtin_ctz` / `__builtin_popcount` / `__builtin_parity` / `__builtin_bswap16/32/64` |
| 内存 | `__builtin_memcpy/memset/memmove` / `__builtin_strcpy` / `__builtin_strlen` / `__builtin_object_size`（模式 0–3；未知 `(size_t)-1`） |
| 分支 / 控制流 | `__builtin_expect` / `__builtin_expect_with_probability`（M24 编译期折叠字面量首参）/ `__builtin_unreachable` / `__builtin_trap` / `__builtin_choose_expr` / `__builtin_types_compatible_p` / `__builtin_constant_p`（parser/opt/codegen 共享 `const_eval::is_constant`） |
| 杂项 | `__builtin_offsetof` / `__builtin_prefetch`（no-op）/ `__builtin_apply` / `__builtin_return`（SysV 子集）/ `__builtin_huge_val` / `__builtin_nan` / `__builtin_isnan` / `__builtin_isinf` / `__builtin_finite` / `__builtin_signbit` / `__builtin_fpclassify` |
| 编译期折叠（M15） | `__builtin_strlen` / `__builtin_abs/labs/llabs` / `__builtin_bswap16/32/64` / `__builtin_expect` / `__builtin_popcount*` — 解析阶段走 `const_eval::eval_full` 折叠为整数常量 |
| 未知 `__builtin_*` | warning + 零值（有返回值）或 `ud2`（void），不静默链接不存在的 libc 符号 |

## 8. 不支持的功能

完整清单见 [`docs/missing_features.md`](./docs/missing_features.md)。

| 范围 | 缺口 | 备注 |
|------|------|------|
| aarch64 | NEON 浮点聚合 > 4 元素 HFA；大结构体 > 16B 嵌套返回 sret | 单层已实装 |
| riscv64 | RVV 完整指令集；压缩指令自动 4→2 字节压缩 | 仅基础 VSETVLI/LOAD/STORE/ADD/MUL/REDUCE |
| x86_64 | bswap 16 位操作数 | GCC 同 |
| 链接 | 运行时 dynamic loader (`ld.so`) | IPA-SRA / PGO 集成已实装；`ld.so` 为设计决策 |
| C++ | 完整 concept SFINAE 联动；模板基类 / 友元；`export template` | 模板单态化 / BMF modules / 协程真状态机均已实装；模板基类/友元/export template 仍不支持 |
| 平台 | Windows MSVC（codegen SysV-like）；Darwin / Mach-O；wasm32；AVR / MSP430 / Xtensa；PowerPC；LoongArch64 | 🔴 |
| 自举 | `dcc0 → dcc1` 起步；`dcc1 → dcc2` 验证 | 字节/语义对比基础设施 `--bootstrap-compare=` 已实装（M25） |

## 9. 路线图

完整版见 [`docs/roadmap.md`](./docs/roadmap.md) / [`docs/next_steps.md`](./docs/next_steps.md) / [`docs/missing_features.md`](./docs/missing_features.md)。

**近期（未完成里程碑）**

- **M5 自举**：`dcc0 → dcc1` 起步；需先补 std 子集。
- **M6 Linux 内核全树首跑**：`make CC=dcc` 推进至 vmlinux + bzImage + QEMU 启动（`scripts/kernel_full_check.sh` 脚本就绪，`BUILD_BZIMAGE=1` 时非 SKIP）。
- **M10 跨平台二进制**：`x86_64-dcc` / `aarch64-dcc` / `riscv64-dcc` 命名分发。

**中期（质量门禁 / 补洞）**

- vmlinux 工具链（pahole BTF / objtool check）非 SKIP 验证；QEMU 启动矩阵至少一种配置 PASS。
- C++ concept SFINAE 完整联动；模板基类/友元；多平台 binary。
- LTO `no_semantic-interposition` Pass 7 完整化。

**远期（架构 / 性能）**

- 自举完成 + 字节级差为 0（CI 锁定）；GCC cc1/cc1plus 全树编译（需本地 ~5G 磁盘）。
- AArch64 / RISC-V profile runtime helper；Offloading device codegen 完整后端（当前已支持 PTX/AMDGCN stub 生成）。

## 10. 交叉编译

详细见 [`docs/cross_compile.md`](./docs/cross_compile.md)。

```bash
dcc --target=aarch64-linux-gnu -o hello-arm64 hello.c
dcc --target=x86_64-linux-musl --sysroot=/opt/musl-sysroot -o hello hello.c
CROSS_COMPILE=aarch64-linux-gnu- make ARCH=arm64 dcc
```

`--target` 接受 `x86_64` / `aarch64` / `riscv64` / `mips` / `mips64` / `arm` / `armv7` 别名；
OS 接受 `linux` / `linux-musl` / `freestanding` / `windows` / `darwin`；
ABI 接受 `gnu` / `musl` / `eabi` / `eabihf` / `msvc` / `darwin`。
工具链命名：`<arch>-<vendor>-<os>-<abi>-dcc` 与 `…-dcc-cc` 别名。

## 11. 限制与已知问题

| 范围 | 限制 | 跟踪 |
|------|------|------|
| 后端 | ARM MTE 标签插入未完全集成 | AArch64 MTE 指令已实现 (IRG/ADDG/SUBG/STG/LDG/SETTCO)，待集成到 sanitizer 流程 |
| 后端 | 浮点变参（v0–7 / fa0–fa7）未保存，stub 返回 0.0 | unimplemented §11 |
| 后端 | 栈溢出变参（>8 个）未实装，返回 0 + 警告 | unimplemented §11 |
| C++ | `obj.*ptr_to_member` 多文件链接可能触发重复定义 | lower.rs#L1653 生成完整函数定义，但每个 TU 都会 emit |
| 链接 | Windows 本地无法完整验证链接路径（依赖 host `ld` / `objcopy`） | Windows 无等效工具链 |

## 12. 引用文档

| 文档 | 简介 |
|------|------|
| [`docs/adr.md`](./docs/adr.md) | 8 条架构决策（Rust / AST-as-IR / 全局类型表 / gnu23 / 零 crate 依赖 / mmap / 跨平台优先 / LTO gc-sections） |
| [`docs/architecture.md`](./docs/architecture.md) | 整体架构 + 性能优化表 + x86-64/AArch64/RISC-V 后端 + 链接器管线 + C++ 降级 + SSA/GVN + 性能分析子系统 |
| [`docs/c_standard.md`](./docs/c_standard.md) | C89–C23 + GNU 扩展支持矩阵 + 重要选项表 + Linux 内核编译示例 |
| [`docs/ci_guide.md`](./docs/ci_guide.md) | 推送与触发 CI + 失败处理 + e2e 失败定位 |
| [`docs/cross_compile.md`](./docs/cross_compile.md) | 跨平台三元组 + 工具链命名 + 6 架构状态 |
| [`docs/diagnostics.md`](./docs/diagnostics.md) | 错误码权威说明（E/W 码表 + 输出格式 + snapshot 回归 + 新增 checklist） |
| [`docs/gcc_build_guide.md`](./docs/gcc_build_guide.md) | GCC 源码子集（libiberty / fixincludes / libdecnumber）+ libiberty 125/0/4 基线 |
| [`docs/kernel_build_guide.md`](./docs/kernel_build_guide.md) | Linux 7.1 x86_64 编译全流程 + asm-offsets 验收 + modpost / pahole / objtool / QEMU 脚本 |
| [`docs/missing_features.md`](./docs/missing_features.md) | Linux 7.1 缺失特性自动化证据 + 通过门禁 + 阻塞项 + libiberty + sanitizer + profiling |
| [`docs/next_steps.md`](./docs/next_steps.md) | 20 步路线图 checklist（A 内核全树 / B 质量门禁 / C CI / D 编译器补洞 / E 性能 / F profiling） |
| [`docs/performance.md`](./docs/performance.md) | 性能模型 + 加速来源 + 实测 + KPI |
| [`docs/profiling.md`](./docs/profiling.md) | PGO / GCOV / AutoFDO / LBR / MemProf / TopdownL1 / Offloading 7 子系统详述 |
| [`docs/roadmap.md`](./docs/roadmap.md) | M0–M25 里程碑 + 后端能力矩阵 + 第一性原理设计决策 |
| [`docs/support_features.md`](./docs/support_features.md) | 已支持特性全量清单（codegen / linker / sanitizer / C / C++ / preproc / debug / profiling） |