# DCC 已支持特性清单

> 最后更新：2026-08-07 — 基于 `cargo test` 287 integration tests + 554 unit tests 全部通过
>
> 缺失特性与已知限制详见 `docs/missing_features.md`

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
| KASAN (`-fsanitize=address`) | TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 `Deref/Index/ArrowMember` 与对应 `Assign` LHS 之前插入；N > 16 走双参 `__asan_loadN(addr, size_t)` |
| HWASAN (`-fsanitize=hwaddress`) | 同 KASAN 路径但使用 `__hwasan_loadN` / `__hwasan_storeN` 函数名 |
| KASAN-kernel (`-fsanitize=kernel-address`) | 内联 shadow 地址计算；x86_64: shadow = (addr >> 3) + 0xdffffc0000000000 |
| UBSAN (`-fsanitize=undefined`) | `Div/Mod` / `Shl/Shr` / 有符号 `Add/Sub/Mul` / VLA bound / null deref / float cast overflow 全部前置 `__ubsan_handle_*` |
| KCSAN (`-fsanitize=thread`) | TU init + 每函数 `__kcsan_func_entry`；KCSAN-only 模式走 `__kcsan_check_access(addr, size, is_write)` 三参调用 |
| KCOV (`-fsanitize-coverage`) | 每函数 `__dcc_cov_guards_N[16]` 计数器 + 入口 `__sanitizer_cov_trace_pc` + 边覆盖 `__sanitizer_cov_trace_pc_guard` + 跨函数 inline 合并 (`profile::kcov_merge`) |
| Fuzzer (`-fsanitize=fuzzer`) | KCOV 全部 + `__sanitizer_cov_trace_cmp1/2/4/8` + `__sanitizer_cov_trace_switch` + libFuzzer 协议（`LLVMFuzzerTestOneInput` 入口包装、8-bit counters、PC table、malloc/free 拦截器） |
| no_sanitize | `address` / `kernel-address` / `hwaddress` / `undefined` / `thread` / `kernel-kcsan` / `all` 按通道关闭；KCOV / Fuzzer 不受 `no_sanitize` 影响 |

实现：`src/sanitizer.rs` `instrument_program()`，所有通道在 driver parse 之后、IR lower 之前通过 `Expr::Comma` 与 `Stmt::Block` 注入 side-effect 调用。`.dcc.san_info` 节版本 v2。

## 性能分析子系统

| 通道 | 能力 |
|------|------|
| PGO instrument | `-fprofile-generate` / `-fprofile-use`；`Profraw` 二进制 roundtrip；driver.rs 接入 `pgo::instrument_program`，codegen 自动 emit `call __dcc_pgo_count_fn`；`PgoWeights` 聚合 func 入口计数 + edge 计数 (`with_edge_counts`)；`propagate_edge_weights` 生成 `BranchProbMap` (分支概率)；`propagate_loop_trip_counts` 推算循环 trip count (供 unroll 决策) |
| GCOV | `-ftest-coverage` / `-fprofile-arcs`；`.gcno` notes + `.gcda` data 二进制（与 GCC gcov-9 layout 对齐）；`cfg_checksum` 采用 GCC libgcov 公式；显式 little-endian 字节序 (跨架构兼容) |
| AutoFDO | `-fauto-profile=` / `-fprofile-sample-use=`；text parser + inline calls 链解析 + `bucketize` 按阈值分 Hot/Warm/Cold + `merge` 合并 + `summary` 统计 + roundtrip 序列化；通过 `ProfileWeights` 聚合，hot 函数下沉到 `.text.hot` 段 |
| LBR | `-flbr-profile=`；解析 `perf script -F brstack` 文本 (含 mispredict/cycles 解析) + inbound/outbound 边查询 + `mispred_rate` 错误预测率 + `merge` 合并 + `summary` 统计；通过 `ProfileWeights` 聚合 |
| MemProf | `-fmemory-profile` / `-fmemory-profile-use=`；text parser + `AllocClass` 分类 (Hot/Normal/Cold/Ephemeral) + lifetime 分析 + `dealloc` 解析 + `merge` 合并 + `summary` 统计 + roundtrip 序列化；冷分配栈对应函数下沉到 `.text.unlikely` 段 |
| Perf 事件 / TopdownL1 | `-ftopdownmetrics`；`TopdownL1` 四槽枚举 + `TopdownL2` 八子分类展开 + `perf stat` 文本解析 + `topdown_percentages()` + `primary_bottleneck()` 瓶颈诊断 + `merge` 合并 + `summary` 统计 + roundtrip 序列化；`PerfDataFile` 读 Linux `perf.data` v2 二进制 |
| Offloading | `-fopenmp[=...]` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=`；`OffloadConfig` 全形式解析；`OffloadSection` 二进制序列化；codegen 全 arch 输出 `.dcc.offload` ELF 节 + device codegen (PTX/AMDGCN)；`OffloadRuntimeLoader` 读 `.dcc.offload` 节 + kernel launch 接口 + runtime 可用性检查 + 诊断输出 |
| 跨源权重聚合 | `ProfileWeights` 聚合 4 源 PGO/AutoFDO/LBR/MemProf，输出统一 hot/cold func 集合；`FuncDecl.is_hot` 控制 `.text.hot` / `.text.unlikely` 段切换 |

CLI 矩阵：`-fprofile-generate[=dir]` / `-fprofile-use=file` / `-fauto-profile=file` / `-flbr-profile=file` / `-ftest-coverage` / `-fprofile-arcs` / `-ftopdownmetrics` / `-fmemory-profile` / `-fopenmp` / `-fcuda` / `-fhip` / `-fopenmp-targets=triple,...` / `--offload-arch=triple`。

详见 `docs/profiling.md`。

## 预处理与宏

| 特性 | 说明 |
|------|------|
| `#if` 十六进制字面量 | `preproc` 单测 `if_hex_*` |
| `#include_next` | `preproc` 单测 + glibc `<limits.h>`；多搜索路径链式解析 |
| `#elifdef` / `#elifndef` (C23) | 已/未定义宏分支选择 |
| `#embed` (C23) | 字节数组嵌入；`limit(N)` 截断；`prefix`/`suffix` 包裹；`if_empty` 替代 |
| `#warning` / `#error` | `#error` 走 fatal 诊断；`#warning` 走 stderr 提示 |
| `_Static_assert` 字符串化 | C23 单参/双参全部支持；失败消息附加 `evaluates to: <ExprDebug 文本>` |
| 宏展开涂蓝 / 防栈溢出 | `nested_function_macro_*`；缓存 V7；深度上限 |
| `_Generic` + 三元控制表达式 | `tests/fixtures/c/generic_tern.c` |
| `_Generic` 指针精确匹配 | `tests/fixtures/c/generic_ptr.c`；按 pointee 匹配 |
| 宏 `#` / `##` 字符串化 | `preproc` 单测 `stringify_collapses_*`；条件 token 拼接 |
| `__VA_OPT__` (C++20 / C23) | 空参整段移除，非空展开 content，嵌套按内→外顺序消解 |
| `__COUNTER__` | 单调递增，跨宏嵌套共享 preproc 状态；在 `#if` 中可被求值 |
| `_Pragma` 操作符 | C99 `_Pragma("string")` 与 `#pragma` 共享同一条 `process_directive` 通路 |
| `#pragma` 全家族 | `pack(n)` / `pack(push,n)` / `pack(pop)` / `weak name` / `STDC FENV_ACCESS` / `GCC diagnostic` / `once` / `comment` / `locale` |
| `#pragma pack` 影响 struct 布局 | `preproc.pack_history` 携带行号；`Parser::pack_at_line(line)` 二分查找；`layout_struct_with_pack(max_align)` cap 到 `min(nat, n)` |
| `#pragma STDC FENV_ACCESS` | ON/OFF/DEFAULT 三态；FP_CONTRACT / CX_LIMITED_RANGE 静默 noop |
| 字符集 / locale (C23) | `default`/`reset`/`"POSIX"`/`"UTF-8"`/`"ISO-8859-1"` 字符集切换 |
| 预定义宏 | `__linux__` / `__x86_64__` / `__GNUC__` / `__amd64` / `__PIC__` / `__PIE__` 等完整集合 |
| `__has_include` / `__has_attribute` / `__has_builtin` / `__has_c_attribute` / `__has_embed` | 编译期求值 |
| `__STDC_ENDIAN_*` (C23) | 仅 C23 模式预定义，NATIVE==LITTLE |
| 数字分隔符 | `1'000'000` |
| 模块化 include guards | C23 `#pragma once`；重复 include 去重 |
| 宏递归展开防护 | V7 include cache |
| 依赖文件 -MD / -MMD / -MP | GCC 兼容格式 (`\` 续行 + 路径转义 + `-MP` phony 目标)；fixdep 支持转义路径反转义 |
| 增量预处理 (PCH) | `-fpch-include=` / `-fpch-create=` / `-fpch-include-only=`；`PchBundle` 序列化 + 宏状态哈希校验 |

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
| `__builtin_alloca` | sub rsp + 16 字节对齐 |
| `__builtin_unreachable` | ud2 + 标记不可达路径 |
| `__builtin_trap` | ud2 |
| `__builtin_debugtrap` | int3 (0xCC) |
| `__builtin_assume_aligned` | 透传指针，codegen 优化对齐 |
| `__builtin_expect_with_probability` | 字面量首参编译期折叠 |
| `__builtin_strlen` / `abs` / `labs` / `llabs` / `expect` / `popcount*` | 编译期折叠 |
| GCC Builtin 完整化 | object_size FORTIFY、apply/return、prefetch no-op、未知 builtin warning |

## 内联汇编

| 特性 | 说明 |
|------|------|
| 内联 asm 约束 | `"r"` / `"m"` / `"=r"` / `+m` / `memory` clobber |
| 内联 asm 约束扩展 | `"i"` 立即数 / `"p"` 内存地址 / `"g"` 通用 / `"X"` 常量 / `"0..9"` 复用 / `"&"` early-clobber / 多重约束 `"rm"` / XMM `"x"` / `"Y"` / 数字标签 `"%"` / `offsetof` 约束 |
| asm `x` 约束 XMM | 独立 `input_xmm_set` + `prepare_asm_xmm_binding` + `writeback_asm_operand` xmm8–15；early-clobber `xmm_pool` |
| asm goto 行内 `#` 注释 | `tests/fixtures/c/asm_goto.c`；`jump_label.h` objtool 标注 |
| asm goto 输出约束 | fall-through 路径写回，跳转路径保留原值 |
| asm `%0` 约束 `"i"` 展开 | 带 `$` 前缀的 AT&T 立即数 |
| Mini x86 asm 局部/C 标签 | `N:` / `N:insn` / `found:` |
| Mini x86 asm 全指令覆盖 | `mov`（含段寄存器）/ `push`/`pop` / `lea` / ALU / `test` / 移位 / `imul2`/`imul3` / `cmpxchg` / `xadd` / `jmp`/`jcc`/`call` / `setcc` / `movzx`/`movsx` / `popcnt` / `movnt*` / `prefetch` / `int`/`sysenter`/`sysexit` / `swapgs`/`invlpg`/`wbinvd`/`cpuid`/`rdtsc` / `ud2` |
| SSE mov / SSE arith | xmm-xmm, mem-xmm, xmm-mem, SegMem 变体完整支持 |
| mini-asm SIMD 扩展 | VEX/EVEX 前缀 + AVX/AVX2/AVX-512 + SSE3–4.2 + AES-NI/SHA + FMA/F16C + TSX + AMX + AVX-VNNI；`pinsr*` / `pextr*` GPR + `tmm` / `ymm` / `zmm` / `k` 寄存器 |

## x86_64 codegen

| 特性 | 说明 |
|------|------|
| 向量/SIMD | 全局 SIMD 向量复制 + 浮点 SIMD 标量逐元素 + 非 Ident 向量 rvalue |
| 复数 | `_Complex` 算术完整支持 + 字面量 + `__real__` / `__imag__` 关键字 |
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
| aarch64 | ✅ 完整 | 谓词指令 (CCMP/CSEL/CSINC/CSNEG) + 双精度浮点比较 + 后置/前置索引寻址 + 扩展寄存器寻址 + 缓存维护 (dc/ic/dsb/isb) + BL/ADR/LDR 范围校验 + HFA store + TLSDESC 序列 |
| riscv64 | ✅ 完整 | 压缩指令 C 扩展 + FP D 扩展双精度 + RVV 向量基础 + 特权指令/CSR + JAL 范围校验 + HFA 分类 |
| mips | ✅ 完整 | ELF32, big-endian, MIPS32 O32 ABI |
| mips64 | ✅ 完整 | ELF64, big-endian, MIPS64 N64 ABI |
| arm | ✅ 完整 | ELF32, little-endian, ARM AAPCS |

跨平台 Triple 解析：`cli.rs` + `target.rs` 接受 `mips` / `mips64` / `arm` / `aarch64` / `riscv64` 的 `--target` 字符串。
Musl sysroot：`--sysroot=<path>` + `TargetTriple::musl_loader_path()`。

## 聚合类型 ABI

| 特性 | 说明 |
|------|------|
| 聚合类型 by-ref | aarch64/riscv64 `gen_call` 检测 `ValueType::Aggregate` / `Array` / `Complex`，local 参数传地址 |
| 聚合类型 sret 返回 | 返回 > 16 字节时 caller 分配 `<sret>` local；aarch64 `memcpy_sp_to_x0`；riscv64 `memcpy_sp_to_a0` |
| aarch64/riscv64 整数除/取模 | aarch64 `sdiv` + `msub`；riscv64 RV64M `div` / `rem` |
| aarch64/riscv64 移位 | aarch64 `lslv` / `lsrv`；riscv64 `sll` / `srl` / `sra` |
| aarch64 va_arg GP + float + 栈溢出 | x0..x7 + v0..v7 保存区 + 顺序读；>8 个变参从 caller 栈读取 |
| riscv64 va_arg GP + float + 栈溢出 | a0..a7 + fa0..fa7 保存区 + 顺序读；>8 个变参从 caller 栈读取 |

## C++ 前端

| 特性 | 说明 |
|------|------|
| dcc++ 端到端编译 | `dcc -x c++ foo.cpp` / `dcc foo.cpp`（按扩展名自动识别） |
| dcc++ 局部类变量 + 方法调用改写 | `Counter c;` → `struct Counter c; _ZN7CounterC1Ev(&c);` |
| dcc++ 隐式 this 注入 | 类方法体内未限定成员 `n_` 自动改写为 `this->n_` |
| dcc++ 多类 + 隐式 this | `examples/m9_cpp_multi.cpp` |
| dcc++ 复合操作符 / C 关键字 | `->` / `->*` / `--` / `-=` / `++` / `+=` 等 23 个复合操作符 |
| dcc++ 完整 SFINAE | `src/cpp/m10.rs::SfinaeCtx` + `try_substitute` |
| dcc++ partial specialization | `template <T> class Foo<T*> {}` 偏特化匹配；多形参 pattern |
| dcc++ 完整 concept 求值 | `sizeof(T)<=N` / `same_as<T,U>` / `derived_from<T,Base>`；解析 + 简化求值 |
| dcc++ 协程 | `co_await` / `co_yield` / `co_return` Duff's device 状态机 + promise type 推导 |
| dcc++ module BMF 序列化 | DCC_BMF_V1 文本格式；`to_bmf`/`from_bmf` round-trip |
| dcc++ GCC 兼容运行时 | `__cxa_guard_acquire/release/abort` / `__dynamic_cast` / `initializer_list`（均 weak，多 TU 安全） |
| dcc++ operator-> 重载链 | 类型名含 `Ptr` / `Wrapper` / `Iter` 识别 |
| dcc++ 完整 RTTI dynamic_cast | 跨虚继承通过 vtable RTTI 查找 |
| dcc++ typeid(e) 真 RTTI | `expr_static_class` 推断静态类，生成 `(&__dcc_ti_<class>)` 符号引用 |
| dcc++ 构造委托 / 继承链 ctor | `using Base::Base;` 转发 |
| dcc++ 成员函数指针 | `&Class::method` 降级为 `&_member_fn_<Class>_<method>` |
| dcc++ `obj.*ptr_to_member` | `.*` 与 `->*` 语法，走 `__dcc_mbr_<class>_<member>` 偏移（weak，多 TU 安全） |
| dcc++ vtable/typeinfo 多文件链接 | `_ZTV` / `_ZTI` / `__dcc_mbr_*` 均 weak，多 TU 包含相同类定义不报 multiple definition |
| dcc++ `__cxa_guard_*` 线程安全 init guard | 静态局部变量状态机 0/1/2 |
| dcc++ std::initializer_list | `{1, 2, 3}` 降级为复合字面量 |
| dcc++ 模板完整单态化 | `TemplateRegistry` 注册 + 缓存 + 偏特化匹配；模板-id `T<U>` 解析 |
| dcc++ 模板基类 | `class Derived : public Base<int>` 模板基类实例化 + 嵌入继承 |
| dcc++ 模板友元 | `friend class Foo<T>` 模板友元声明解析 + 跳过 |
| dcc++ `export template` | `export template <T> class Foo` 模板导出 + BMF 序列化 |
| dcc++ concept 与 SFINAE 联动 | concept 定义注册 + requires 子句展开 + SFINAE 约束过滤 |
| dcc++ module ODR 检查 | `OdrChecker` 跨模块重复定义检测 + 冲突报告 |
| dcc++ `virtual` 继承复杂场景 | 菱形继承检测 + 虚基类指针表达 + 警告报告 |
| dcc++ 标准库模板实参 | `std::vector<int>` 等限定名模板实参语法解析 |
| dcc++ 动态 RTTI | typeinfo/vtable 生成 + 继承虚方法检测 + RTTI slot |
| dcc++ 继承成员访问 | 递归继承链查找 + `__base_BaseName` 路径改写 |
| dcc++ `__dcc_mbr` 继承成员 | 继承成员偏移辅助函数 + `__base_` 链路径计算 |
| dcc++ `auto` 类型推导 | int / `const char*` / class 真推导 |
| dcc++ `decltype` / `decltype(auto)` | 解析 + 占位 |
| dcc++ `operator new[]` / `delete[]` | Itanium ABI `_Znam` / `_ZdaPv` 符号 |
| dcc++ `new T[n]` 数组分配 | `CppExpr::NewArr` + `_Znam` |
| dcc++ `placement new` | `new (ptr) T(args)` 原地构造 |
| dcc++ `static` 类成员 | C `static` 字段 + Itanium ABI mangling |
| C++ 修饰符 | `noexcept` / `final` / `override` / `mutable` / `volatile` / `explicit` / `friend` / `alignas` / `alignof` |
| `if constexpr` | 编译期分支：const_eval 在 lower 端选分支 |
| C++ 标准库扩展 | `<chrono>` / `<thread>` / `<mutex>` / `<condition_variable>` / `<filesystem>` / `<expected>` / `<flat_map>` / `<regex>` / `<ranges>` |
| C++23 多维下标 | `obj[i,j,k]` → `__dcc_multi_index_N` |
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
| `__attribute__((returns_nonnull))` | codegen 强制非空检查：`test rax, rax; jnz .ok; ud2` |
| `__attribute__((__may_alias__))` | `tests/fixtures/c/may_alias.c` |
| `__attribute__((weak(...)))` / `#pragma weak` | codegen 写 `STB_WEAK` 绑定 |
| `__attribute__((constructor/destructor(priority)))` | 按 priority 排序 |
| `__attribute__((alias/ifunc/retain/used/flatten/error/warning/deprecated))` | 全部实装 |
| const_eval 完整化 | `ConstValue` 枚举；浮点算术；字符串字面量 FNV-1a；`&ident` / `&arr[i]` / `&&label`；`#embed` 字节数 |
| 运行时检查 | `-fbounds-check` 数组下标边界检查；`-ftrapv` 有符号算术溢出 trap |
| 严格类型检查 | `-Wint-truncation` / `-Wint-conversion`；仅 warning 不阻塞 |
| GNU 扩展 | 嵌套函数 / computed goto 跨函数 / `Expr::Unary` 浮点取负 / `AddrOf` REX.R 编码 / `_Complex` 字面量 / `__real__`/`__imag__` |
| 嵌套块重声明 (C99 6.2.1) | 函数体内遮蔽允许 |
| K&R 函数 | 解析自动检测 + `is_old_style` |
| 预置 C11 `atomic_*` 宏 | hosted 模式预置；freestanding 模式 `__STDC_NO_ATOMICS__=1` |

## 链接器

| 特性 | 说明 |
|------|------|
| 跨 TU LTO | `-flto` 生成 `.llvm.lto`；`-r` 多 `.o` 经内置路径合并 |
| LTO dead symbol 剔除 | `-ffunction-sections` + `ld --gc-sections` + objcopy strip |
| LTO 跨函数内联 | cost model：≤4 语句 / 4 表达式 / 无递归 |
| LTO 跨 TU 常量传播 | 跨 TU `global constant` 初始化值合并 |
| LTO 跨 TU 全局优化 | DCE + DAE |
| LTO 4 pass 增强 | `LtoConfig` 配置化 + Pass 7 no_semantic_interposition |
| LTO IPA-SRA / PGO 集成 | `src/ipa_sra.rs` + `src/profile/pgo_integration.rs` |
| `gc_sections_root` 自动选根 | 无显式根时自动选所有 STB_GLOBAL/STB_WEAK 符号 |
| `.gnu.hash` TIE 压缩 | ELF 64-bit GNU hash（bloom filter + buckets + chain） |
| `.eh_frame_hdr` 搜索表 | 二分搜索表（按 initial_location 排序） |
| IFUNC 解析 | `STT_GNU_IFUNC` → `R_X86_64_IRELATIVE` 重定位 |
| IFUNC `target_clones` 多版本 | STT_GNU_IFUNC + R_X86_64_IRELATIVE，resolver 返回 default 变体 |
| `--exclude-libs` 隐藏静态库符号 | 被排除归档库中全局符号打 `STV_HIDDEN` |
| `--build-id=sha1/uuid/md5/none` | 内置 SHA-1 / MD5 / UUID v4 写 `.note.gnu.build-id` |
| `--no-undefined-version` | 缺版本 `.symver` 引用 → 错误 |
| `.symver` 完整版本表 | `.gnu.version` 16-bit 索引表 + `.gnu.version_d` VERDEF |
| `-z relro` / `-z now` / `-z lazy` / `-z execstack` / `-z noexecstack` | 段处理完整 |
| `-z max-page-size` / `-z common-page-size` | 解析 K/M/G 后缀 |
| `-z pack-relative-relocs` | 生成 `.gnu.pack.relocs` 段（SHT_GNU_RELR） |
| `-rpath` / `-rpath-link` / `-runpath` | `.dynamic` 写 `DT_RPATH` / `DT_RUNPATH` |
| `-fuse-ld=<name>` | `LinkerBackend` 枚举 + `resolve_linker_backend` |
| `-fno-semantic-interposition` | LTO 阶段控制函数符号插入语义 |
| `--gdb-index` | 最小 `.gdb_index` 节 |
| `--compress-debug-sections=zlib\|zstd` | miniz_oxide / zstd 压缩 .debug_* 节 |
| `SHF_GNU_RETAIN` 节标志 | 元数据节自动打 `0x200000` |
| `__dso_handle` / `.fini_array` 调度 | `FiniArrayBuilder` 收集 destructor 符号按 priority 排序 |
| `-r` 自研路径 | `OutputKind::Relocatable` + `write_elf_relocatable` |
| 共享库 lazy PLT | `plt_got.rs` 完整 .plt.sec/.plt.got 生成 |
| 自定义 dynamic linker 路径 | `PT_INTERP` 程序头 + `.interp` 节 |
| vDSO 集成 | `--vdso=<path>` 解析 vDSO ELF，嵌入 `.text.vdso` + `.note.GNU-vdso` + 符号表 |
| PIE/Shared 输出 | 多架构 `_start` 入口桩 |
| AArch64/RISC-V/MIPS/ARM ELF 输出 | `elf_exec.rs` 多架构包装函数 |
| AArch64 重定位（22 种） | `ABS64` / `PREL64` / `CALL26` / `JUMP26` / `ADR_PREL_*` / `MOVW_UABS_*` 等 |
| RISC-V 重定位（15 种） | `64` / `32` / `CALL` / `PCREL_*` / `HI20` / `LO12_*` / `TPOFF*` 等 |
| GOT64 大条目 | `R_X86_64_GOT64` / `R_X86_64_GOTOFF64` |
| TLS 重定位 | `R_X86_64_TPOFF64` / `R_X86_64_TPOFF32` |
| TLS codegen | `_Thread_local` → `.tdata` / `.tbss`、`STT_TLS`；AArch64 TLSDESC 序列 |
| ET_EXEC symtab | `build_elf_exec_x86_64` 输出 `.symtab` / `.strtab` |
| wchar_t libc 转发 | wcs*/wmem* 11 个入口在 link implicit undef 白名单 |
| signal libc 转发 | `sigaction` / `sigaltstack` / `signal` 白名单 |
| 复杂数学 libm 转发 | `cproj` / `cabs` / `carg` 白名单 |

## IR 优化

| 特性 | 说明 |
|------|------|
| `-O0` always_inline | `tests/fixtures/c/always_inline.c` |
| `-O1` 常量折叠 + 内联 | — |
| `-O2` GVN/CSE/LICM/分支折叠 | `ssa_gvn.rs` + `loop_opt.rs` |
| `-O3` 循环展开 + 向量元素循环向量化 | 小 trip≤4 |
| SSA pass 全家族 (15 pass) | P1 跨函数 DCE (call graph reachability, 删除不可达 static) / P2 强度削减 (Mul→Shl, 无符号 Div→Shr) / P3 归纳变量优化 (回边检测) / P4 别名分析 (Steensgaard union-find) / P5 指令调度 (BB 内 list scheduling) / P6 PGO (profdata hot/cold) / P7 自动并行 (safe loop 检测) / P8 循环分块 (2x unroll) / P9 循环交换 (block_order swap) / P10 TCO (尾递归→Jump) / P11 函数属性传播 (cold/hot/noreturn) / P12 寄存器分配 (SSA 值计数) / P13 别名集合 (identity/disjoint/overlap) / P14 投机执行 (纯计算外提) / P15 SCCP (lattice + worklist) |

## 调试信息

| 特性 | 说明 |
|------|------|
| DWARF v4 | x86_64 / AArch64 / RISC-V 全部架构 |
| `.BTF` | x86_64（struct dcc_probe）/ AArch64 / RISC-V |
| `.eh_frame` | CIE + FDE；setjmp CFI 寄存器保存表 |
| `.orc_unwind_ip` | x86_64 / AArch64 / RISC-V |
| DWARF line table | `-g1` 下自动去重 |
| DWARF type info | 位域成员 `DW_AT_bit_size` + `DW_AT_bit_offset` |
| `-gsplit-dwarf` / `-gpubnames` | `.dwo` 文件生成 |

## 诊断

| 特性 | 说明 |
|------|------|
| 诊断 snapshot 格式锁定 | E0001-E0013 / E0100-E0102 / E0200-E0209 / E0300 / E1001-E1010 / W0001 / W0200 |
| `-fdiagnostics-color` | `always` / `never` / `auto` |
| `-fdiagnostics-format` | `github` / `clang` / `msvc` / `vi` / `sarif` |
| `-fdiagnostics-show-template-tree` | 模板树显示 |
| format 属性 | `format_check.rs`：`-Wformat` / `W0003` arity 检查 |
| 严格类型 / permissive | 默认未知 typedef → `E0009`；`-fpermissive` → `W0002` + int |

## 杂项

| 特性 | 说明 |
|------|------|
| `setjmp` / `longjmp` | libc + DWARF CFI 兼容寄存器保存表 |
| `alloca` | `__builtin_alloca` codegen: sub rsp + 16 字节对齐 |
| 信号 | `sigaction` / `sigaltstack` / `signal` 运行时验证通过 |
| TLS dtv / TLS 描述符 | AArch64 TLSDESC 4 指令序列 + 3 重定位；IE/LE 路径 |
| atexit / `__cxa_atexit` | `FiniArrayBuilder` 按 priority 排序 |
| `printf` / `scanf` format string | 完整 flag/width/precision/length/type 匹配 + 指针类型验证 |
| `wchar_t` / 宽字符字面量 `L"..."` | wcs*/wmem* libc 转发 |
| `char16_t` / `char32_t` (C11/C++11) | uint16/uint32 + u""/U"" 字符串 |
| 原子混合大小 | `__atomic_*` 1/2 字节变体（lock xadd/xchg/cmpxchg 8/16 位） |
| `stdatomic.h` 全部 | `atomic_flag` / `atomic_signal_fence` / `atomic_thread_fence` |
| 浮点环境 (`fenv.h`) | `pragma STDC FENV_ACCESS` ON/OFF/DEFAULT |
| 复杂数学函数 | `cproj` / `cabs` / `carg` 走 libm |
| per-function section | `-ffunction-sections` / `-flto` 默认 `.text.<name>` |
| jump_label 静态 key 声明 | `_DECLARE_STATIC_KEY_0/1` |
| rela 引用标记 | `elf::referenced_symbol_names` → `lto::mark_references_from_objects` |
| CLI 多输入 | `-r a.o [b.o…] -o merged.o` 链接模式 |
| Bootstrap 回归对比 | `src/bootstrap.rs`：字节级 / 语义级三层对比 |
