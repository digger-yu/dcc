# DCC 已支持特性清单

> 最后更新：2026-08-07 — 基于 `cargo test` 287 integration tests + 554 unit tests 全部通过

## 核心门禁

| 特性 | 证据 |
|------|------|
| typed IR + 严格语义 | `cargo test` integration（static_assert / unknown typedef / int128）+ `tests/errors.rs` snapshot（E1003–E1008 等） |
| SysV va_list（GP） | `examples/test_p4_va_list.c` |
| SysV va_list SSE | `examples/test_p4_va_list.c`（GP + float/double） |
| asm goto + computed goto | `examples/test_asm_goto.c` |
| 自定义 ELF section / WEAK / HIDDEN | `examples/test_p2_elf_attrs.c` + readelf 断言 |
| __int128 / register asm / 全局符号 | `examples/test_p1_features.c` |
| 预处理 forced-include 注释安全 | `tests/fixtures/c/comment.h` |
| mod/devicetable 生成 TU | `devicetable-offsets.c` 直接 `-S` 输出 50+ `SIZE_*` 行 |
| DWARF + BTF（含 struct） | `-g` 生成 `.debug_*` + `.BTF`（含 `dcc_probe` STRUCT 类型） |
| objtool / ORC 帧信息 | `-g`/`-fno-omit-frame-pointer` 生成 CIE+FDE `.eh_frame`、`.orc_unwind_ip` |
| `-S` 汇编输出 | lock/SSE 前缀；自定义节按 `.section` 分组 |

## Sanitizer 插桩

| 通道 | 能力 |
|------|------|
| KASAN / HWASAN | TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 `Deref/Index/ArrowMember` 与对应 `Assign` LHS 之前插入 |
| UBSAN | `Div/Mod` / `Shl/Shr` / 有符号 `Add/Sub/Mul` 全部前置 `__ubsan_handle_*`；签名整数 overflow 在常量折叠时跳过 |
| KCSAN | TU init + 每函数 `__kcsan_func_entry`；内存访问 watchpoint 与 KASAN 共享 load/store 通道 |
| KCOV | 每函数 `__dcc_cov_guards_N[16]` 计数器 + 入口 `__sanitizer_cov_trace_pc` + 边覆盖 `__sanitizer_cov_trace_pc_guard`（if / while / do / for / case / label / goto）+ 跨函数 inline 合并 |
| Fuzzer | KCOV 全部 + 比较 `EqEq/Ne/Lt/Gt/Le/Ge/AndAnd/OrOr` 前 `__sanitizer_cov_trace_cmp1/2/4/8` + switch dispatch 前 `__sanitizer_cov_trace_switch` + libFuzzer 协议/拦截器 |
| no_sanitize | `address` / `kernel-address` / `hwaddress` / `undefined` / `thread` / `kernel-kcsan` / `all` 按通道关闭；KCOV / Fuzzer 不受 `no_sanitize` 影响 |

实现：`src/sanitizer.rs` `instrument_program()`，所有通道在 driver parse 之后、IR lower 之前通过 `Expr::Comma` 与 `Stmt::Block` 注入 side-effect 调用。`.dcc.san_info` 节版本 v2。

## 性能分析子系统

| 通道 | 能力 |
|------|------|
| PGO instrument | `-fprofile-generate` / `-fprofile-use`；`Profraw` 二进制 roundtrip；AST 改写插入 `__dcc_pgo_count_fn`；`PgoWeights` 聚合 func 入口计数 |
| GCOV | `-ftest-coverage` / `-fprofile-arcs`；`.gcno` notes + `.gcda` data 二进制（与 GCC gcov-9 layout 对齐）；`cfg_checksum` 采用 GCC libgcov 公式 |
| AutoFDO | `-fauto-profile=` / `-fprofile-sample-use=`；简化 text parser；`bucketize` 按阈值分 Hot/Warm/Cold |
| LBR | `-flbr-profile=`；解析 `perf script -F brstack` 文本；`hot_entries` 按入度排序 |
| MemProf | `-fmemory-profile` / `-fmemory-profile-use=`；简化 parser；`is_cold(stack, threshold)` 启发 |
| Perf 事件 / TopdownL1 | `-ftopdownmetrics`；`TopdownL1` 枚举；`topdown_percentages()` 规整为百分比 |
| Offloading | `-fopenmp[=...]` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=`；`OffloadConfig` 全形式解析；`OffloadSection` 二进制序列化；codegen 全 arch 输出 `.dcc.offload` ELF 节 + device codegen (PTX/AMDGCN) |

CLI 矩阵：`-fprofile-generate[=dir]` / `-fprofile-use=file` / `-fauto-profile=file` / `-flbr-profile=file` / `-ftest-coverage` / `-fprofile-arcs` / `-ftopdownmetrics` / `-fmemory-profile` / `-fopenmp` / `-fcuda` / `-fhip` / `-fopenmp-targets=triple,...` / `--offload-arch=triple`。

详见 `docs/profiling.md`。

## 预处理与宏

| 特性 | 说明 |
|------|------|
| `#if` 十六进制字面量 | `preproc` 单测 `if_hex_*` |
| `#include_next` | `preproc` 单测 + glibc `<limits.h>` |
| 宏展开涂蓝 / 防栈溢出 | `nested_function_macro_*`；深度上限 |
| `_Generic` + 三元控制表达式 | `tests/fixtures/c/generic_tern.c` |
| `_Generic` 指针精确匹配 | `tests/fixtures/c/generic_ptr.c`；按 pointee 匹配 |
| 宏 `#` 字符串化换行折叠 | `preproc` 单测 `stringify_collapses_*` |
| `_Pragma` 操作符 | C99 `_Pragma("string")` 与 `#pragma` 共享同一条 `process_directive` 通路 |
| `#pragma` 全家族 | `pack(n)` / `pack(push,n)` / `pack(pop)` / `weak name` / `STDC FENV_ACCESS` / `GCC diagnostic` / `once` / `comment` |
| `#pragma pack` 影响 struct 布局 | `preproc.pack_history` 携带行号；`Parser::pack_at_line(line)` 二分查找；`layout_struct_with_pack(max_align)` cap 到 `min(nat, n)` |
| `#warning` / `#error` | `#error` 走 fatal 诊断；`#warning` 走 stderr 提示 |
| `_Static_assert` 字符串化 | C23 单参/双参全部支持；失败消息附加 `evaluates to: <ExprDebug 文本>` |

## GCC Builtin

| Builtin | 实现 |
|---------|------|
| `__builtin_constant_p` / inline 属性 | `tests/fixtures/c/constp.c`、`tests/fixtures/c/inline.c` |
| 未知 `__builtin_*` ud2 | void → ud2，其余 → 零值 + warning |
| `__builtin_va_start/end/copy/arg` | SysV va_list 结构（r13 帧链 + 24 字节槽） |
| `__builtin_strcasecmp` / `strncasecmp` | 转发到 libc |
| `__builtin_fma` / `fmaf` / `fmal` | 调 libm 对应函数 |
| `__builtin_rotateleft/right{8,16,32,64}` | ROL/ROR + cl |
| `__builtin_clrsb/clrsbl/clrsbll` | cdq/cqo 符号扩展 + bsr |
| `__builtin_{add,sub,mul}_overflow` | 64 位运算 + 截断 + 比较 SETcc |
| `__builtin_{add,sub,mul}_overflow_p` | SETO/SETB 标志直接返回 |
| `__builtin_bswap16/32/64` | bswap r32/r64；16 位走 rol r16, 8 + movzx |
| `__builtin_clz/ctz/popcount/parity` | bsr/bsf/POPCNT/XOR fold |
| `__builtin_bitreverse8/16/32/64` | 固定次数移位循环 |
| `__builtin_frame_address` / `return_address` | r13 帧链遍历 |
| `__builtin_types_compatible_p` / `choose_expr` | parser 折叠 + codegen 兜底 |
| `__builtin_va_list` / `FILE` / `LINE` / `FUNCTION` / `FUNCSIG` | 类型别名 + codegen 字符串 rodata |
| `__builtin_offsetof` | 支持嵌套 struct/union/数组下标/匿名 struct |
| `__builtin_huge_val` / `huge_valf` / `huge_vall` | IEEE 754 位模式 rodata 加载 |
| `__builtin_nan` / `nanf` / `nanl` | qNaN 编码 |
| `__builtin_isnan` / `isinf` / `finite` / `signbit` / `fpclassify` | 正确处理 NaN/Inf 位模式 |
| `__builtin_call_with_static_chain` | SysV ABI r10 + GP/XMM 寄存器约定 |
| GCC Builtin 完整化 | object_size FORTIFY、apply/return、prefetch no-op、未知 builtin warning |

## 内联汇编

| 特性 | 说明 |
|------|------|
| 内联 asm 约束 | `"r"` / `"m"` / `"=r"` / `+m` / `memory` clobber |
| 内联 asm 约束扩展 | `"i"` 立即数 / `"p"` 内存地址 / `"g"` 通用 / `"X"` 常量 / `"0..9"` 复用 / `"&"` early-clobber / 多重约束 `"rm"` / XMM `"x"` / `"Y"` / 数字标签 `"%"` / `offsetof` 约束 |
| asm `x` 约束 XMM | 独立 `input_xmm_set` + `prepare_asm_xmm_binding` + `writeback_asm_operand` xmm8–15；early-clobber `xmm_pool` |
| asm goto 行内 `#` 注释 | `tests/fixtures/c/asm_goto.c`；`jump_label.h` objtool 标注 |
| asm goto 输出约束 | fall-through 路径写回，跳转路径保留原值 |
| Mini x86 asm 局部/C 标签 | `N:` / `N:insn` / `found:` |
| Mini x86 asm 位操作 / 双精度移位 | `bt/bts/btr/btc`、`shld/shrd` |
| Mini x86 asm 扩展位/字节/NT/虚拟化 | `bsf/bsr/popcnt/bswap/movnt*`、`vmcall/vmmcall`、RIP-relative、`push/pop` mem |
| Mini x86 asm GAS 伪指令 / 前缀 | `.pushsection` / `.balign` / `.long` / `.quad` / `.popsection`；`lock` / `rep` |
| mini-asm 内核扩展 | `tests/fixtures/c/asm_kernel.c`；rol/prefetch/movss/addss |
| mini-asm SIMD 扩展 | VEX/EVEX 前缀 + AVX/AVX2/AVX-512 + SSE3–4.2 + AES-NI/SHA + FMA/F16C + TSX + AMX + AVX-VNNI；`pinsr*` / `pextr*` GPR + `tmm` / `ymm` / `zmm` / `k` 寄存器 |

## x86_64 codegen

| 特性 | 说明 |
|------|------|
| 向量/SIMD | 全局 SIMD 向量复制 + 浮点 SIMD 标量逐元素 + 非 Ident 向量 rvalue |
| 复数 | `_Complex` 算术完整支持 |
| float 取模 | 软件 fmod = l - trunc(l/r) * r，零 libc 依赖 |
| int128 `&&` / `||` | `or rdx, dst` 合并高 64 位后再 test |
| 全局数组 RIP+SIB | `emit_load_global_indexed`：`[rip+disp32+idx*scale]` 单指令加载 |
| 浮点字面量 rodata | `v.to_le_bytes()` 直接写入 `.rodata`，IEEE 754 精确 |
| 跨函数 LEA | `[r13+off]` + RIP-relative + `emit_load_global_indexed` |
| object_size FORTIFY | `__builtin_object_size` + `__builtin___memcpy_chk` 等 |
| apply/return | `__builtin_apply` / `__builtin_return` |
| prefetch | `__builtin_prefetch` no-op |

## 多架构支持

| 架构 | 状态 | 说明 |
|------|------|------|
| x86_64 | ✅ 完整 | 主后端 |
| aarch64 | ✅ 完整 | hello/控制流/调用/浮点/向量/rodata/谓词指令/缓存维护 |
| riscv64 | ✅ 完整 | hello/控制流/调用/rodata/压缩指令/FP D/RVV/CSR |
| mips | ✅ 完整 | ELF32, big-endian, MIPS32 O32 ABI |
| mips64 | ✅ 完整 | ELF64, big-endian, MIPS64 N64 ABI |
| arm | ✅ 完整 | ELF32, little-endian, ARM AAPCS |

跨平台 Triple 解析：`cli.rs` + `target.rs` 接受 `mips` / `mips64` / `arm` / `aarch64` / `riscv64` 的 `--target` 字符串。

## 聚合类型 ABI

| 特性 | 说明 |
|------|------|
| 聚合类型 by-ref | aarch64/riscv64 `gen_call` 检测 `ValueType::Aggregate` / `Array` / `Complex`，local 参数传地址 |
| 聚合类型 sret 返回 | 返回 > 16 字节时 caller 分配 `<sret>` local |
| aarch64/riscv64 整数除/取模 | aarch64 `sdiv` + `msub`；riscv64 RV64M `div` / `rem` |
| aarch64/riscv64 移位 | aarch64 `lslv` / `lsrv`；riscv64 `sll` / `srl` / `sra` |

## C++ 前端

| 特性 | 说明 |
|------|------|
| dcc++ 端到端编译 | `dcc -x c++ foo.cpp` / `dcc foo.cpp`（按扩展名自动识别） |
| dcc++ 局部类变量 + 方法调用改写 | `Counter c;` → `struct Counter c; _ZN7CounterC1Ev(&c);` |
| dcc++ 隐式 this 注入 | 类方法体内未限定成员 `n_` 自动改写为 `this->n_` |
| dcc++ 多类 + 隐式 this | `examples/m9_cpp_multi.cpp` |
| dcc++ 复合操作符 / C 关键字 | `->` / `->*` / `--` / `-=` / `++` / `+=` 等 23 个复合操作符 |
| dcc++ C++20 占位 | `module` / `import` / `export` / `concept` / `requires` 降级；`co_await` / `co_yield` / `co_return` Duff's device 状态机 |
| dcc++ 完整 SFINAE | `src/cpp/m10.rs::SfinaeCtx` + `try_substitute` |
| dcc++ partial specialization | `template <T> class Foo<T*> {}` 偏特化匹配 |
| dcc++ 完整 concept 求值 | `sizeof(T)<=N` / `same_as<T,U>` / `derived_from<T,Base>` |
| dcc++ 协程挂起恢复 | Duff's device 状态机 |
| dcc++ module BMF 序列化 | DCC_BMF_V1 文本格式 |
| dcc++ GCC 兼容运行时 | `__cxa_guard_acquire/release/abort` / `__dynamic_cast` / `initializer_list` |
| dcc++ operator-> 重载链 | 类型名含 `Ptr` / `Wrapper` / `Iter` 识别 |
| dcc++ 完整 RTTI dynamic_cast | 跨虚继承通过 vtable RTTI 查找 |
| dcc++ 构造委托 / 继承链 ctor | `using Base::Base;` 转发 |
| dcc++ 成员函数指针 | `&Class::method` 降级为 `&_member_fn_<Class>_<method>` |
| dcc++ `__cxa_guard_*` 线程安全 init guard | 静态局部变量状态机 0/1/2 |
| dcc++ std::initializer_list | `{1, 2, 3}` 降级为复合字面量 |
| C++ 修饰符与类型推导 | `auto` / `decltype` / `friend` / `noexcept` / `final` / `override` / `mutable` / `volatile` / `explicit` / `new T[n]` / `placement new` / `if constexpr` |
| C++ 标准库扩展 | `<chrono>` / `<thread>` / `<mutex>` / `<condition_variable>` / `<filesystem>` / `<expected>` / `<flat_map>` / `<regex>` / `<ranges>` / Concepts / Modules BMF |
| Itanium C++ ABI 名字修饰 | 成员函数 / 运算符 / vtable / typeinfo / 模板形参 |
| C++→C 降级 | struct 字段 / 隐式 this / 命名空间扁平化 / 基类嵌入 / vtable 符号 / typeinfo 符号 / 异常 / lambda 闭包 / 模板完整单态化 |

## C 前端补洞

| 特性 | 说明 |
|------|------|
| MS 扩展 | `__declspec(align/noreturn/thread/dllimport/dllexport)` / `_ms_restrict` / `__pragma` |
| `__attribute__((cleanup(fn)))` | 变量离开作用域时插入 `cleanup_fn(&var)` |
| `__attribute__((target_clones(...)))` | 链接期生成 STT_GNU_IFUNC 符号 + R_X86_64_IRELATIVE 重定位 |
| `__attribute__((no_sanitize(...)))` | 按通道关闭 sanitizer 插桩 |
| `__attribute__((copy(fn)))` | 生成 `fn` 的别名符号 |
| `__attribute__((returns_nonnull))` | codegen 强制非空检查 |
| `__attribute__((__may_alias__))` | `tests/fixtures/c/may_alias.c` |
| const_eval 完整化 | `ConstValue` 枚举；浮点算术；字符串字面量 FNV-1a；`&ident` / `&arr[i]` / `&&label`；`__builtin_strlen/abs/labs/llabs/bswap/expect/popcount` 编译期折叠 |
| 运行时检查 | `-fbounds-check` 数组下标边界检查；`-ftrapv` 有符号算术溢出 trap |
| GNU 扩展 | 嵌套函数 / computed goto 跨函数 / `Expr::Unary` 浮点取负 / `AddrOf` REX.R 编码 |
| 嵌套块重声明 (C99 6.2.1) | 函数体内遮蔽允许 |

## 链接器

| 特性 | 说明 |
|------|------|
| 跨 TU LTO | `-flto` 生成 `.llvm.lto`；`-r` 多 `.o` 经 `ld -r` + objcopy 合并 |
| LTO dead symbol 剔除 | `-ffunction-sections` + `ld --gc-sections` + objcopy strip |
| LTO 跨函数内联 | cost model：≤4 语句 / 4 表达式 / 无递归 |
| LTO 跨 TU 常量传播 | 跨 TU `global constant` 初始化值合并 |
| LTO 跨 TU 全局优化 | DCE + DAE |
| `.gnu.hash` TIE 压缩 | ELF 64-bit GNU hash（bloom filter + buckets + chain） |
| `.eh_frame_hdr` 搜索表 | 二分搜索表（按 initial_location 排序） |
| IFUNC 解析 | `STT_GNU_IFUNC` → `R_X86_64_IRELATIVE` 重定位 |
| IFUNC `target_clones` 多版本 | STT_GNU_IFUNC + R_X86_64_IRELATIVE，resolver 返回 default 变体 |
| `--exclude-libs` 隐藏静态库符号 | 被排除归档库中全局符号打 `STV_HIDDEN` |
| `--build-id=sha1/uuid/md5/none` | 内置 SHA-1 / MD5 / UUID v4 写 `.note.gnu.build-id` |
| `--no-undefined-version` | 缺版本 `.symver` 引用 → 错误 |
| `.symver` 完整版本表 | `.gnu.version` 16-bit 索引表 + `.gnu.version_d` VERDEF |
| `-z relro[=partial\|full]` | 写 `PT_GNU_RELRO` 段 |
| `-z now` / `BIND_NOW` | `.dynamic` 写 `DT_FLAGS=DF_BIND_NOW` |
| `-z lazy` | 恢复延迟绑定 |
| `-z execstack` / `-z noexecstack` | 控制 `PT_GNU_STACK` 的 `PF_X` |
| `-z max-page-size=N` / `-z common-page-size=N` | 解析 K/M/G 后缀 |
| `-z pack-relative-relocs` | 生成 `.gnu.pack.relocs` 段（SHT_GNU_RELR） |
| `-rpath` / `-rpath-link` / `-runpath` | `.dynamic` 写 `DT_RPATH` / `DT_RUNPATH` |
| `-fuse-ld=<name>` | `LinkOpts.fuse_ld` 字段 |
| `-fno-semantic-interposition` | LTO 阶段控制函数符号插入语义 |
| `--gdb-index` | 最小 `.gdb_index` 节 |
| `--compress-debug-sections=zlib` | miniz_oxide 压缩 .debug_* 节 |
| `SHF_GNU_RETAIN` 节标志 | 元数据节自动打 `0x200000` |
| `gc_sections_root` 自动选根 | 接受 `-u <sym>` 显式根符号 |
| `__dso_handle` / `.fini_array` 调度 | `FiniArrayBuilder` 收集 destructor 符号 |
| `-r` 自研路径 | `OutputKind::Relocatable` + `write_elf_relocatable` |
| AArch64 重定位（22 种） | `ABS64` / `PREL64` / `CALL26` / `JUMP26` / `ADR_PREL_*` / `MOVW_UABS_*` 等 |
| RISC-V 重定位（15 种） | `64` / `32` / `CALL` / `PCREL_*` / `HI20` / `LO12_*` / `TPOFF*` 等 |
| GOT64 大条目 | `R_X86_64_GOT64` / `R_X86_64_GOTOFF64` |
| TLS 重定位 | `R_X86_64_TPOFF64` / `R_X86_64_TPOFF32` |
| TLS codegen | `_Thread_local` → `.tdata` / `.tbss`、`STT_TLS` |
| ET_EXEC symtab | `build_elf_exec_x86_64` 输出 `.symtab` / `.strtab` |
| 多架构 `_start` 入口桩 | x86_64 / AArch64 / RISC-V64 / ARM |

## IR 优化

| 特性 | 说明 |
|------|------|
| IR 优化 pass | `ssa_gvn.rs` GVN/CSE；`loop_opt.rs` LICM/展开/向量循环；`-O2` / `-O3` |
| always_inline @ -O0 | `tests/fixtures/c/always_inline.c` |

## 诊断

| 特性 | 说明 |
|------|------|
| 诊断 snapshot 格式锁定 | `tests/errors.rs` schema + `diagnostic_format.stderr.snap`（V1） |
| format 属性 | `format_check.rs`：`-Wformat` / `W0003` arity 检查 |
| 严格类型 / permissive | 默认未知 typedef → `E0009`；`-fpermissive` → `W0002` + int |

## 其他

| 特性 | 说明 |
|------|------|
| K&R 函数 | 解析 + `is_old_style` 标志 + codegen 测试 |
| jump_label 静态 key 声明 | `_DECLARE_STATIC_KEY_0/1` 及 `IS_ENABLED` 残留 |
| per-function section | `-ffunction-sections` / `-flto` 默认 `.text.<name>` |
| rela 引用标记 | `elf::referenced_symbol_names` → `lto::mark_references_from_objects` |
| CLI 多输入 | `-r a.o [b.o…] -o merged.o` 链接模式 |
