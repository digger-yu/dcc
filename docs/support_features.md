# DCC 已支持特性清单

> 最后更新：2026-08-24 — 基于 Linux 7.1 全树编译 (零 dcc 编译器错误) + `cargo test` 全量通过
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
| objtool / ORC 帧信息 | `-g`/`-fno-omit-frame-pointer` 生成 CIE+FDE `.eh_frame`；`-mfunction-return=thunk-extern` 下函数尾声、空 global asm 函数和 mini-asm `ret` 均跳转 `__x86_return_thunk`，避免 MITIGATION_RETHUNK 裸 `ret` |
| `-S` 汇编输出 | lock/SSE 前缀；自定义节按 `.section` 分组 |
| QEMU bzImage boot 门禁 | `scripts/qemu_boot_matrix.sh` 在 bzImage 存在时默认硬门禁，缺 QEMU 或缺 boot log 关键字符串均失败；`kernel_full_check.sh` strict 模式导出 `REQUIRE_QEMU_BOOT=1`；`strict_qemu_boot_*` 集成测试固化契约 |
| Linux kernel object corpus | 固化真实 Linux `.o` 样本：`tests/fixtures/corpus/arch/x86/kernel/io_delay.o`、`kernel/sched/fair.o`、`lib/clz_ctz.o`；`kernel_object_corpus_*` 测试校验 ELF64/ET_REL/ORC 节和代表性函数符号 |
| CI/examples 路径防回归 | `.github/workflows/kernel-bench.yml` 与 `scripts/bench_phase_e.sh` 使用当前 `examples/*/*` 路径；`scripts/check_fixtures.sh` 扫描 workflows/scripts 中的 examples 引用并要求文件存在 |

## Sanitizer 插桩

| 通道 | 能力 |
|------|------|
| KASAN (`-fsanitize=address`) | TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 `Deref/Index/ArrowMember` load、对应 `Assign` LHS 以及 `PreInc/PreDec/PostInc/PostDec` 内存目标（读-改-写）之前插入；非 1/2/4/8/16 大小走 compiler-rt 通用 `__asan_loadN(addr, size_t)` / `__asan_storeN(addr, size_t)`；host cc 链接自动追加 `-fsanitize=address`，内置 PIE 链接器记录 `DT_NEEDED libasan.so` 并允许对应 runtime undefined；codegen 级栈红区（每局部变量后 32B padding，帧分配后 `__asan_poison_memory_region` 毒化全帧 + `__asan_unpoison_memory_region` 逐变量解除毒化）+ 全局变量红区（.rodata 中每全局变量后追加 32B 零填充） |
| HWASAN (`-fsanitize=hwaddress`) | 同 KASAN 路径但使用 `__hwasan_loadN` / `__hwasan_storeN` 函数名（软件 tag 模式）；AArch64 目标上额外启用 MTE 硬件 tag 模式：prologue 发射 tag 栈帧（`irg`/`addg`/逐 granule `stg`），sp 携带本帧随机 tag，局部地址自动继承（指针 tag 流程），调用点 untag/retag、epilogue 清除 tag 并恢复 x19 |
| KASAN-kernel (`-fsanitize=kernel-address`) | 完整内联 shadow 检查（不调用户态运行时）：x86_64 shadow = (addr >> 3) + 0xdffffc0000000000；逐 8B granule 校验 shadow 字节（`v >= 128` 毒化 / `v > 0 && need > v` 部分毒化越界，need = (addr & 7) + size），违规调 `__asan_report_loadN/storeN(addr)`；不发射 `__asan_init`（内核自初始化） |
| UBSAN (`-fsanitize=undefined`) | `Div/Mod` / `Shl/Shr` / 有符号 `Add/Sub/Mul` / VLA bound / null deref / float cast overflow 插入 compiler-rt/libubsan 导出的 `__ubsan_handle_*`；host cc 链接自动追加 `-fsanitize=undefined`，内置 PIE 链接器记录 `DT_NEEDED libubsan.so` |
| KCSAN / TSan (`-fsanitize=thread`) | 用户态 thread sanitizer 走 libtsan 兼容 `__tsan_init` / `__tsan_func_entry` / `__tsan_readN/writeN` / range 回调，host cc 链接自动追加 `-fsanitize=thread`，内置 PIE 链接器记录 `DT_NEEDED libtsan.so`；`-fsanitize=kernel-kcsan` 保留内核 `__kcsan_*` 回调且不链接宿主 runtime |
| KCOV (`-fsanitize-coverage`) | 每函数 `__dcc_cov_guards_N[16]` 计数器 + 入口 `__sanitizer_cov_trace_pc` + 边覆盖 `__sanitizer_cov_trace_pc_guard` + 跨函数 inline 合并 (`profile::kcov_merge`) |
| Fuzzer (`-fsanitize=fuzzer`) | KCOV 全部 + `__sanitizer_cov_trace_cmp1/2/4/8` + `__sanitizer_cov_trace_switch` + libFuzzer 协议（`LLVMFuzzerTestOneInput` 入口包装、8-bit counters、PC table、malloc/free 拦截器） |
| no_sanitize | `address` / `kernel-address` / `hwaddress` / `undefined` / `thread` / `kernel-kcsan` / `all` 按通道关闭；KCOV / Fuzzer 不受 `no_sanitize` 影响 |

测试证据：`tests/integration.rs` 覆盖 KASAN 越界、UBSAN div/mod/shift/VLA/null/float-cast、KCSAN 竞态、HWASAN 软件 tag、KCOV/Fuzzer `trace_cmp*` / `trace_switch` / 8-bit counters / PC table / `no_sanitize` 交互，以及未知 sanitizer / coverage 名称负向诊断；`src/codegen/aarch64.rs` 覆盖 AArch64 MTE `irg` / `addg` / `stg` 字节级断言。

实现：`src/sanitizer.rs` `instrument_program()`，所有通道在 driver parse 之后、IR lower 之前通过 `Expr::Comma` 与 `Stmt::Block` 注入 side-effect 调用；codegen 侧 `gen_inc_dec` 支持 Deref/Index/Member 内存 lvalue 读-改-写（`(*p)++` 等）。`.dcc.san_info` 节版本 v2。

## 性能分析子系统

| 通道 | 能力 |
|------|------|
| PGO instrument | `-fprofile-generate` / `-fprofile-use` / `-fprofile-values`；`Profraw` v2 二进制 roundtrip (FNV-1a checksum 完整性校验 + value profiling 目标计数 + `merge` 多文件合并 + `profdata` 合并优化格式)；driver.rs 接入 `pgo::instrument_program` 并自动生成 `<stem>.pgort.c`；codegen 自动 emit `call __dcc_pgo_count_fn` / `__dcc_pgo_value_sample`；运行后写出真实 counter 与 value histogram；`PgoWeights` 聚合 func 入口计数 + edge 计数 (`with_edge_counts`)；`propagate_edge_weights` 生成 `BranchProbMap` (分支概率)；`propagate_loop_trip_counts` 推算循环 trip count (供 unroll 决策) |
| GCOV | `-ftest-coverage` / `-fprofile-arcs`；`.gcno` notes + `.gcda` data 二进制（与 GCC gcov-9 layout 对齐）；`cfg_checksum` 采用 GCC libgcov 公式；显式 little-endian 字节序 (跨架构兼容)；`read_notes()` / `read_data()` 读取器解析 .gcno/.gcda；行级覆盖率报告 `GcovReport` + `to_text()` 文本输出 |
| AutoFDO | `-fauto-profile=` / `-fprofile-sample-use=`；text parser + inline calls 链解析 + `bucketize` 按阈值分 Hot/Warm/Cold + `merge` 合并 + `summary` 统计 + roundtrip 序列化；二进制格式 `write_binary`/`read_binary` (magic "AFDO" + version 2) + discriminator 支持 (`disc` 记录 + `discriminators_for` 查询) + 自动检测文本/二进制；通过 `ProfileWeights` 聚合，hot 函数下沉到 `.text.hot` 段 |
| LBR | `-flbr-profile=`；解析 `perf script -F brstack` 文本 (含 mispredict/cycles 解析) + inbound/outbound 边查询 + `mispred_rate` 错误预测率 + `merge` 合并 + `summary` 统计 + `serialize` 文本输出；perf.data 二进制 LBR 记录解析 (`read_perf_data_binary` + PERF_RECORD_SAMPLE branch_stack) + 自动检测文本/二进制；通过 `ProfileWeights` 聚合 |
| MemProf | `-fmemory-profile` / `-fmemory-profile-use=`；text parser + `AllocClass` 分类 (Hot/Normal/Cold/Ephemeral) + lifetime 分析 + `dealloc` 解析 + `merge` 合并 + `summary` 统计 + roundtrip 序列化；二进制格式 `write_binary`/`read_binary` (.memprofdata magic "MEMPROFD") + 自动检测文本/二进制；malloc 插桩 `MemProfInstrumentor` (`__dcc_memprof_alloc_record`/`__dcc_memprof_free_record`)；冷分配栈对应函数下沉到 `.text.unlikely` 段 |
| Perf 事件 / TopdownL1 | `-ftopdownmetrics`；`TopdownL1` 四槽枚举 + `TopdownL2` 八子分类展开 + `perf stat` 文本解析 + `topdown_percentages()` + `primary_bottleneck()` 瓶颈诊断 + `merge` 合并 + `summary` 统计 + roundtrip 序列化；`PerfDataFile` 读 Linux `perf.data` v2 二进制 (magic "PERFILE2" + header + event attrs) + `parse_samples` 解析 PERF_RECORD_SAMPLE (ip/pid/tid/time/period) + `correlate_events` 样本关联到 event attr + `aggregate_by_ip` IP 聚合 |
| Offloading metadata / driver link | `-fopenmp[=...]` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=`；`OffloadConfig` 全形式解析；`OffloadSection` 二进制序列化；全 arch 输出 `.dcc.offload` ELF 节；`OffloadRuntimeLoader` 可读取 `.dcc.offload` 并暴露 kernel launch 诊断接口；`OffloadDriverLinker` 生成 CUDA/ROCm host link flags、device link commands、fat binary 描述与 link script |
| LTO/offload/profile 格式版本 | `.llvm.lto` summary v1、`.dcc.offload` v1、profraw v2、profdata v1、AutoFDO text v1/binary v2、MemProf text v1/binary v1 均只接受当前版本，错版返回 unsupported-version 错误 |
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
| 宏 `#` / `##` 字符串化 | `preproc` 单测 `stringify_collapses_*`；条件 token 拼接；`#` 操作符只转义 `"` 不转义 `\` (与 GCC 一致，内核 `__stringify(\reg)` 正确展开) |
| `__VA_OPT__` (C++20 / C23) | 空参整段移除，非空展开 content，嵌套按内→外顺序消解 |
| `__COUNTER__` | 单调递增，跨宏嵌套共享 preproc 状态；在 `#if` 中可被求值 |
| `_Pragma` 操作符 | C99 `_Pragma("string")` 与 `#pragma` 共享同一条 `process_directive` 通路；`_Pragma(expr)` 支持宏展开后为字符串字面量 |
| `#pragma` 全家族 | `pack(n)` / `pack(push,n)` / `pack(pop)` / `weak name` / `STDC FENV_ACCESS` / `STDC FP_CONTRACT` / `STDC CX_LIMITED_RANGE` / `GCC diagnostic` / `once` / `comment` / `locale` |
| `#pragma pack` 影响 struct 布局 | `preproc.pack_history` 携带行号；`Parser::pack_at_line(line)` 二分查找；`layout_struct_with_pack(max_align)` cap 到 `min(nat, n)` |
| `#pragma STDC FENV_ACCESS` | ON/OFF/DEFAULT 三态；ON 时 opt 禁用浮点常量折叠/GVN-CSE（舍入模式可被运行时修改） |
| `#pragma STDC FP_CONTRACT` | ON/OFF/DEFAULT 三态；ON 时 x86_64 后端把 `a*b+c` / `a*b-c` 收缩为 `fma` / `fmaf` / `fmal` 调用，OFF/DEFAULT 保持独立乘加 |
| `#pragma STDC CX_LIMITED_RANGE` | ON/OFF/DEFAULT 三态；ON 时 x86_64 complex 乘除法使用内联有限范围公式，OFF/DEFAULT 使用 `__mulsc3` / `__muldc3` / `__divsc3` / `__divdc3` range-safe helper |
| 字符集 / locale (C23) | `default`/`reset`/`"POSIX"`/`"UTF-8"`/`"ISO-8859-1"` 字符集切换 |
| 预定义宏 | `__linux__` / `__x86_64__` / `__GNUC__` / `__amd64` / `__PIC__` / `__PIE__` 等完整集合 |
| `__has_include` / `__has_attribute` / `__has_builtin` / `__has_c_attribute` / `__has_embed` | 编译期求值 |
| `__STDC_ENDIAN_*` (C23) | 仅 C23 模式预定义，NATIVE==LITTLE |
| 数字分隔符 | `1'000'000` |
| 模块化 include guards | C23 `#pragma once`；重复 include 去重 |
| 宏递归展开防护 | V7 include cache |
| 依赖文件 -MD / -MMD / -MP | GCC 兼容格式 (`\` 续行 + 路径转义 + `-MP` phony 目标)；fixdep 支持转义路径反转义 |
| `-Wp,*` 子选项白名单 | CLI 与 `dcc-host-wrapper.sh` 仅白名单支持预处理器子选项，未知项报 unsupported |
| x86 kernel wrapper flags | `arch_x86_compile.sh` 透传 `-fstack-clash-protection`、`-mrecord-mcount`、`-mfentry`、`-pg`、`-falign-loops=*`、`-falign-jumps=*` 等常见内核编译 flags；`-mno-sse`/`-mcmodel=kernel`/`-ftrivial-auto-var-init=zero` 已完整实现 |
| `-x` 输入语言集 | CLI 支持 `c`/`c-header`、`c++`/`cpp`/`cxx` 及 C++ header 别名，并区分 `assembler`（原始汇编）与 `assembler-with-cpp`（预处理汇编） |
| host wrapper dependency errors | `dcc-host-wrapper.sh` fails the host build when fixdep dependency generation fails instead of writing a stub `.d` file |
| `-fprofile-values` | 启用 PGO value sampling 插桩，在函数入口生成 `__dcc_pgo_value_sample(site)` 调用；driver 自动落盘 `<stem>.pgort.c`，运行时维护 per-site value histogram 并写入 profraw v2 的 value profile section |
| 增量预处理 (PCH) | `-fpch-include=` / `-fpch-create=` / `-fpch-include-only=`；`PchBundle` 序列化 + 宏状态哈希校验；加载/写入失败、hash 不一致和预处理错误均 warning 后回退 |
| Linux 内核头预处理 E2E | `compiler_types.h` / `rwonce.h` / `autoconf.h` 真实内核头预处理 + 依赖图闭包追踪（`linux_header_compiler_types_preprocess_e2e` / `linux_header_rwonce_preprocess_e2e` / `linux_header_autoconf_preprocess_e2e` / `linux_header_graph_dependency_closure`） |

## GCC Builtin

| Builtin | 实现 |
|---------|------|
| `__builtin_constant_p` / inline 属性 | `tests/fixtures/c/constp.c`、`tests/fixtures/c/inline.c` |
| 未知 `__builtin_*` 降级 | 未知 builtin 产生 `E0018` hard error，不再静默返回 0/ud2；`is_known_builtin()` 覆盖 100+ 已知 builtin；`kernel_builtin_coverage_runtime` 测试验证新增 builtin 运行时正确性 |
| `__builtin_va_start/end/copy/arg` | SysV va_list 结构（r13 帧链 + 24 字节槽）；MIPS O32 ($a0-$a3 保存区)；ARM AAPCS (r0-r3 保存区) |
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
| `__builtin_ffs/ffsl/ffsll` | bsf + setnz + add（运行时）；const_eval 编译期折叠 |
| `__builtin_isdigit` | sub '0' + cmp 9 + setbe |
| `__builtin_dynamic_object_size` | 同 __builtin_object_size |
| `__builtin_extract_return_addr` | x86_64 透传 |
| `__builtin_preserve_access_index` | BPF CO-RE 透传 |
| `__builtin_btf_type_id` / `has_attribute` | 返回 0 |
| `__builtin_counted_by_ref` | 透传指针 |
| `__builtin_alloca_uninitialized` | 同 alloca |
| `__builtin_strcat` | 转发到 libc strcat |
| `__builtin_warning` | 返回 0 |
| `__builtin_unreachable` | ud2 + 标记不可达路径 |
| `__builtin_trap` | ud2 |
| `__builtin_debugtrap` | int3 (0xCC) |
| `__builtin_assume_aligned` | 透传指针，codegen 优化对齐 |
| `__builtin_expect_with_probability` | 字面量首参编译期折叠 |
| `__builtin_strlen` / `abs` / `labs` / `llabs` / `expect` / `popcount*` | 编译期折叠 |
| `__builtin___memcpy_chk` / `memmove_chk` / `memset_chk` / `strcpy_chk` / `strncpy_chk` / `strcat_chk` / `strncat_chk` | Linux FORTIFY：识别 `__builtin___*_chk` 并剥离 `__builtin_` 前缀，链接到 libc `__*_chk` 符号；汇编输出 `call __memcpy_chk` |

## 内联汇编

| 特性 | 说明 |
|------|------|
| 内联 asm 约束 | `"r"` / `"m"` / `"=r"` / `+m` / `memory` clobber |
| 内联 asm 优化器集成 | SSA pipeline 跳过含 asm 的函数 (避免 SSA 重建丢失副作用); 指令调度将 asm memory clobber 及任何 `__dcc_asm_*` 符号引用视为调度屏障 (Bin/Un/Load/Cast 操作数防御性检查); 寄存器分配器统计 asm 标记寄存器压力 (`asm_reg_pressure` 计入 `__dcc_asm_*` Store 数); DCE 保留所有 asm (GCC 兼容) |
| 内联 asm clobber 完整支持 | `"cc"` 条件码 (x86 静默接受) / `"memory"` 编译器屏障 (mfence) / `"st"`/`"st(N)"` x87 FPU 栈寄存器 / `"xmm0"`..`"xmm15"` 从 XMM 池排除 / GP 寄存器名 (如 `"rax"`/`"rbx"`/`"eax"` 等) 从 GP 池排除；`parse_clobber_registers` 解析并返回排除集合 |
| 内联 asm 约束扩展 | `"i"` 立即数 / `"p"` 内存地址 / `"g"` 通用 / `"X"` 常量 / `"0..9"` 复用 / `"&"` early-clobber / 多重约束 `"rm"` / XMM `"x"` / `"Y"` / 数字标签 `"%"` / `offsetof` 约束 |
| 内联 asm 约束类型校验 | codegen 统一检查 matched constraint、多重约束、显式寄存器和 x87/MMX/XMM 分类；约束与表达式类型不匹配时发 `W0005` warning 且不阻断编译（如 `"r"`/`"rm"` 绑定浮点），GCC 对同类输入通常报 `impossible constraint` error |
| asm `x` 约束 XMM | 独立 `input_xmm_set` + `prepare_asm_xmm_binding` + `writeback_asm_operand` xmm8–15；early-clobber `xmm_pool` |
| asm 段寄存器约束 `={gs}`/`={fs}` | `mov %seg, [r13+off]` (0x8C) 写回；`prepare_asm_seg_binding` 加载段寄存器值到内存 |
| asm x87 FPU 约束 `f`/`t`/`u` | `fstp [r13+off]` (D9/DD) 写回 st(0)；`prepare_asm_x87_binding` 用 `fld` 推栈加载输入值；`+t` 读写约束支持 |
| asm MMX 寄存器约束 `y` | `movq/movd [r13+off], mm` (0F 7F/7E) 写回；`prepare_asm_mmx_binding` 用 `movq/movd mm, [r13+off]` (0F 6F/6E) 加载输入；MMX 指令 `por`/`pand`/`pxor`/`padd*`/`psub*`/`pcmpeq*`/`pcmpgt*` 已支持 |
| asm GCC 条件码约束 `@ccz`/`@ccs`/`@ccc` 等 | `setcc %al; movzx %al, %eax; mov %rax, [r13+off]` 写回（push/pop rax 保护其他输出）；`cc_letter_to_setcc_opcode` 映射全部 16 条件码字母序列到 setcc 操作码；不可见操作数不在模板中替换 |
| asm 显式子寄存器约束 `{eax}`/`{cx}`/`{cl}` | 按宽度假 8/16/32/64 位写回 (`mov [r13+off], r8/r16/r32/r64`)；`parse_reg_name_bits` 解析全部 GP 子寄存器名；`{gs}`/`{fs}` 段寄存器也支持 |
| asm goto 行内 `#` 注释 | `tests/fixtures/c/asm_goto.c`；`jump_label.h` objtool 标注 |
| asm goto 输出约束 | fall-through 路径写回，跳转路径保留原值；跨函数 `&&label` + asm goto 混用触发 warning |
| asm 调试信息 | `-g` 下含 asm 的函数正常生成 DWARF 行号表 + DIEs；asm 块起始 PC 记录到 `asm_line_entries` 并生成精确 `.debug_line` 条目；asm 块内每条指令生成独立 `.debug_line` 条目 (`asm_insn_debug_entries`)，使用 `DW_LNE_set_discriminator` 区分同一源行的不同指令，使 GDB 能单步进入 asm 块内部；SSA pipeline 跳过含 asm 函数避免重建丢失行号映射 |
| asm `%0` 约束 `"i"` 展开 | 带 `$` 前缀的 AT&T 立即数 |
| asm 模板修饰符 `%a` / `%P` / `%l` | `%aN`/`%a[name]`: 地址形式 (寄存器同名, 立即数/内存去前缀); `%PN`/`%P[name]`: 原始符号名 (去 % / $ 前缀); `%lN`/`%l[name]`: asm goto 标签名 (边界安全多位索引，索引越界走 W0004 warning); 立即数含运算符时自动加括号保护优先级 |
| Mini x86 asm 局部/C 标签 | `N:` / `N:insn` / `found:` |
| Mini x86 asm Linux legacy aliases | `cmpxchg8b` / `pushfl` / `popfl` / `iret` / `iretl` / 全条件码 `cmov*`（含宽度后缀别名）/ 裸 `in`/`out` / memory-indirect port / `shldw` / `shrdw` / 裸段前缀已覆盖；`asm_*_has_opcode_and_disassembly` 测试校验 `.text` 字节和 objdump 反汇编，固化不再降级为 nop |
| Mini x86 asm 全指令覆盖 | `mov`（含段寄存器 / MMX `movq`/`movd`）/ `push`/`pop` / `lea` / ALU / `test` / 移位 / `imul2`/`imul3` / `cmpxchg`/`cmpxchg8b` / `xadd` / `jmp`/`jcc`/`call` / `setcc` / `movzx`/`movsx` / `popcnt` / `movnt*` / `prefetch` / `int`/`iret`/`iretl`/`iretq`/`sysenter`/`sysexit` / `pushf`/`pushfl`/`pushfq`/`popf`/`popfl`/`popfq` / `swapgs`/`invlpg`/`wbinvd`/`cpuid`/`rdtsc` / `ud2` / `vmxon`/`vmread`/`vmwrite`/`vmptrld`/`vmptrst`/`vmclear` / `wrussq` (CET) / `lar` / MMX 逻辑 (`por`/`pand`/`pxor`) / MMX 算术 (`paddb/w/d`/`psubb/w/d`) / MMX 比较 (`pcmpeq*`/`pcmpgt*`)；`kernel_asm_instr_coverage_runtime` 测试验证内核 arch/x86/ 使用的全部指令编码正确 |
| SSE mov / SSE arith | xmm-xmm, mem-xmm, xmm-mem, SegMem 变体完整支持 |
| mini-asm SIMD 扩展 | VEX/EVEX 前缀 + AVX/AVX2/AVX-512 + SSE3–4.2 + AES-NI/SHA + FMA/F16C + TSX + AMX + AVX-VNNI；`pinsr*` / `pextr*` GPR + `tmm` / `ymm` / `zmm` / `k` 寄存器 |
| mini-asm 扩展操作数形式 | `rdrand`/`rdseed` 内存+符号操作数 (RIP-relative)；`ljmp`/`lcall` 间接内存 + `$off,$seg` 反序 + 符号偏移；`retf`/`lret` 符号立即数；`xbegin` rel32 + `xabort` 符号；AES-NI/SHA/pshuf*/palignr/pblendw/insertps/pshufw 内存源+符号源 (RIP-relative)；`pinsr*`/`pextr*`/`extractps` 符号源/目标；`vpinsr*`/`vpextr*`/`vextractps`/`vextractpd`/`vcvtps2ph` 符号源/目标 |
| mini-asm 未知指令诊断 | 未识别 mnemonic 或已识别指令的不合法操作数在 `-c` 编码路径产生 `E0311` hard error，不再 warning 后发 `nop` |
| asm `.rept`/`.endr` 重复块 | `AsmDirectiveExpander::parse_rept` 解析重复次数, 递归展开体内指令 |
| asm `.irp`/`.irpc` 迭代块 | `.irp sym, v1, v2, ...` 按值列表迭代 / `.irpc sym, chars` 按字符迭代; 体内 `\sym` 替换为当前值 |
| asm `.macro`/`.endm` 宏定义与调用 | `parse_macro_def` 解析参数列表, `try_expand_macro_call` 按调用参数替换 `\arg` / `\(arg)` 并展开 |
| asm `.include "file"` | `parse_include` 读取文件内容并递归展开; 文件不存在时静默跳过 (GCC 兼容) |
| asm `.if`/`.else`/`.endif` 条件汇编 | `parse_conditional` + `collect_conditional_block` 支持 `.if`/`.ifdef`/`.ifndef`/`.else`/`.elseif`/`.endif`; 简单整数表达式求值 (`eval_simple_expr`) |
| asm `.intel_syntax`/`.att_syntax` 语法切换 | 识别并透传至 `-S` 输出; `AsmDirectiveExpander::intel_syntax` 状态跟踪 |
| asm `.altmacro`/`.noaltmacro` 交替宏模式 | 识别并透传; `AsmDirectiveExpander::alt_macro` 状态跟踪 |
| 跨架构内联 asm 操作数处理 | aarch64/riscv64/mips/mips64/arm 后端完整实现 `emit_inline_asm`: 约束解析 (`resolve_asm_operands`) → 输入加载 (inout `+r` 和纯输入 `r`) → 模板替换 → asm 发射 (-S 文本/-c mini-assembler 编码) → 输出写回 (`=r`/`+r`) → 表达式结果 (第一输出寄存器→返回值寄存器); 架构屏障: aarch64 DMB ISH / riscv64 FENCE / mips SYNC / arm DMB |
| `asm_output` 属性 (GCC15) | `__attribute__((asm_output("...")))` 解析并存储到 `FuncDecl::asm_output_template`; 属性合并逻辑支持 |
| 非 x86_64 后端 inline asm | aarch64/riscv64/mips/mips64/arm 后端完整处理 `Decl::GlobalAsm` / `Stmt::AsmGoto` / `Expr::AsmExpr`; 共享约束解析模块 `asm_constraints.rs` 解析 `=r`/`r`/`+r`/`m`/`i`/matched/memory/cc 约束并从架构调用者保存寄存器池分配; `-S` 模式输出带架构实际寄存器名的汇编文本 (AArch64 w0-w15 / RISC-V a0-a7,t0-t6 / MIPS $v0-$t9 / ARM r0-r3,r12); 增强版模板替换 `%N`→寄存器名 / `%[name]`→名称查找 / `%lN`→标签 / `%wN`/`%xN`→AArch64 宽度修饰; memory clobber 发射架构屏障指令 (AArch64 DMB ISH / RISC-V FENCE / MIPS SYNC / ARM DMB ISH); `-c` 模式 mini-assembler 编码机器码 (`mini_asm.rs` 支持 nop/dmb/dsb/isb/ret/mov/add/sub/br/blr/svc/fence/sync 等常见指令) |

## x86_64 codegen

| 特性 | 说明 |
|------|------|
| 向量/SIMD | 全局 SIMD 向量复制 + 浮点 SIMD 标量逐元素 + 非 Ident 向量 rvalue + 同尺寸 vector/scalar、vector/vector、complex/vector cast；IR validator 对 binop/unop/cast 使用显式白名单，并用 E1007/E1008 测试拒绝不同尺寸或未枚举转换 |
| 复数 | `_Complex` 算术完整支持 + 字面量 + `__real__` / `__imag__` 关键字；IR validator 对 binop/unop/cast 使用显式白名单，并固化 complex/vector 同尺寸边界与 complex→signed integer 枚举转换 |
| float 取模 | 软件 fmod = l - trunc(l/r) * r，零 libc 依赖；float validator 支持 `%` 并拒绝未枚举运算/转换 |
| int128 `&&` / `||` | `or rdx, dst` 合并高 64 位后再 test |
| 全局数组 RIP+SIB | `emit_load_global_indexed`：`[rip+disp32+idx*scale]` 单指令加载 |
| 浮点字面量 rodata | `v.to_le_bytes()` 直接写入 `.rodata`，IEEE 754 精确 |
| 跨函数 LEA | `[r13+off]` + RIP-relative + `emit_load_global_indexed` |
| object_size FORTIFY | `__builtin_object_size` + `__builtin___memcpy_chk` 等 |
| apply/return | `__builtin_apply` / `__builtin_return` |
| return thunk | `-mfunction-return=thunk-extern` 将 C 函数尾声、空 global asm 函数和 mini-asm `ret`/`retq` 发射为外部 `__x86_return_thunk` 跳转；`-mfunction-return=keep` 保留裸 `ret` |
| prefetch | `__builtin_prefetch` no-op |
| unknown expression fallback | unsupported expression / 非标量 InitList rvalue 输出带源位置 warning，并发射 `ud2` 后清零目标寄存器，避免静默返回 0 |
| `-mno-sse` x87 FPU 回退 | 内核中断上下文禁用 XMM 寄存器，浮点运算使用 x87 FPU 指令 (fld/fstp/faddp/fsubrp/fmulp/fdivrp/fucomip)；降级为 W0300 警告而非硬错误 |
| `-mcmodel=kernel` 代码模型 | 内核运行在 -2GB 高地址空间，定义 `__code_model_kernel__` 宏；外部函数地址使用直接 PC-relative 而非 GOT 间接寻址 |
| `-ftrivial-auto-var-init=zero` | 安全特性，CONFIG_INIT_STACK_ALL_ZERO 依赖；使用 rep stosq 零初始化整个栈帧 |
| 栈保护非可重定位路径 | canary 失败始终调用 `__stack_chk_fail`（STB_GLOBAL/STT_FUNC 未定义符号），不再对 ET_EXEC 走 exit(255) syscall；`m3_10_stack_canary` 测试验证 |
| ftrace 插桩 | `-pg` 在函数 prologue 后插入 `call mcount`；`-mfentry` 在函数标签后立即（prologue 前）插入 `call __fentry__`；`-mrecord-mcount` 同时生成 `__mcount_loc` 段记录所有 call site 地址（带 R_X86_64_64 重定位指向 .text）；三种模式均添加对应未定义符号到 ELF 符号表 |
| 栈保护 guard 寄存器自定义 | `-mstack-protector-guard-reg=gs` 支持内核 per-CPU canary（`%gs:0x28`）；prologue canary 加载和 epilogue canary 校验均使用可配置段前缀（fs: 0x64 / gs: 0x65）；默认 `%fs:0x28` 用于用户态 |
| `-fstack-clash-protection` | 大块栈分配（> 4096 字节）时逐页探测 guard page；发射探测循环：先减剩余字节，再循环每页 `sub rsp, 4096` + `mov byte [rsp], 0` 触发缺页异常；小帧保持普通 `sub rsp, imm32` |
| Retpoline 完整验证 | `-mfunction-return=thunk-extern` 与内核侧 `__x86_return_thunk` 符号兼容；objtool ORC 帧信息正确生成；`-mindirect-branch=thunk-extern` 已实现（见 objtool / ORC 行） |
| `-fno-strict-aliasing` | 优化器不做类型双关假设（type-based alias analysis）；常量折叠使用 `checked_add/sub/mul` 不假设无符号溢出；当前优化器已保守处理，标志被接受并存储 |
| `-fno-strict-overflow` | 优化器不假设有符号溢出为未定义行为；常量折叠在溢出时返回 `None` 不折叠；循环优化不做基于无溢出的范围推断 |
| `-fshort-wchar` | `wchar_t` 为 2 字节（UTF-16LE）模式；预处理器定义 `__WCHAR_TYPE__` 为 `short unsigned int`、`__WCHAR_MAX__` 为 65535；parser 宽字符串/字符字面量编码为 UTF-16LE（含代理对处理）；null 终止符为 2 字节 |
| `-fno-common` | 公共符号（tentative definition）直接放入 `.bss` 而非合并到 common 段；当前 codegen 已默认按此行为处理未初始化全局变量 |

## 多架构支持

| 架构 | 状态 | 说明 |
|------|------|------|
| x86_64 | ✅ 完整 | 主后端 |
| aarch64 | ✅ 完整（compile/ELF 层） | 谓词指令 (CCMP/CSEL/CSINC/CSNEG) + 双精度浮点比较 + 后置/前置索引寻址 + 扩展寄存器寻址 + 缓存维护 (dc/ic/dsb/isb) + BL/ADR/LDR 范围校验 + HFA store + TLSDESC 序列 + 嵌套 sret (>16B 多层调用链) |
| riscv64 | ✅ 完整（compile/ELF 层） | 压缩指令 C 扩展 (自动 4→2 字节压缩优化: C.ADDI/C.LI/C.LUI/C.MV/C.ADD/C.LW/C.SW/C.LD/C.SD) + FP D 扩展双精度 + RVV 完整向量指令集 (宽化/窄化/掩码/浮点向量) + 特权指令/CSR + JAL 范围校验 + HFA 分类 |
| mips | ✅ 完整 | ELF32, big-endian, MIPS32 O32 ABI; ET_EXEC 可执行 + ET_REL 可重定位; 分支回填 + 全局变量初始化 + 字符串字面量 (.rodata) + switch/do-while/break/continue |
| mips64 | ✅ 完整 | ELF64, big-endian, MIPS64 N64 ABI; ET_EXEC 可执行 + ET_REL 可重定位; 64-bit LD/SD 指令 + 帧布局适配 |
| arm | ✅ 完整 | ELF32, little-endian, ARM AAPCS; ET_EXEC 可执行 + ET_REL 可重定位; 分支回填 + 全局变量初始化 + 字符串字面量 (.rodata) + switch/do-while/break/continue |

跨平台 Triple 解析：`cli.rs` + `target.rs` 接受 `mips` / `mips64` / `arm` / `aarch64` / `riscv64` 的 `--target` 字符串。
Musl sysroot：`--sysroot=<path>` + `TargetTriple::musl_loader_path()`。

## 聚合类型 ABI

| 特性 | 说明 |
|------|------|
| 聚合类型 by-ref | aarch64/riscv64 `gen_call` 检测 `ValueType::Aggregate` / `Array` / `Complex`，local 参数传地址 |
| 聚合类型 sret 返回 | 返回 > 16 字节时 caller 分配 `<sret>` local；aarch64 `memcpy_sp_to_x0`；riscv64 `memcpy_sp_to_a0`；aarch64 嵌套 sret 支持多层调用链 (gen_function 入口保存 x0 到 `<sret_ptr>`, return 时恢复) |
| aarch64/riscv64 整数除/取模 | aarch64 `sdiv` + `msub`；riscv64 RV64M `div` / `rem` |
| aarch64/riscv64 浮点函数调用 | 浮点表达式中的 `ExprKind::Call` 复用普通调用路径；AArch64 结果保留在 `s0/d0`，RISC-V 结果保留在 `fa0`，compile/ELF 层有回归测试 |
| aarch64/riscv64 移位 | aarch64 `lslv` / `lsrv`；riscv64 `sll` / `srl` / `sra` |
| aarch64 va_arg GP + float + 栈溢出 | x0..x7 + v0..v7 保存区 + 顺序读；>8 个变参从 caller 栈读取 |
| riscv64 va_arg GP + float + 栈溢出 | a0..a7 + fa0..fa7 保存区 + 顺序读；>8 个变参从 caller 栈读取 |
| mips va_arg GP | $a0-$a3 保存区 + 顺序读；超出 4 个从 caller 栈读取；O32 ABI |
| arm va_arg GP | r0-r3 保存区 + 顺序读；超出 4 个从 caller 栈读取；AAPCS ABI |
| aarch64 gen_float_expr Local/Conditional | 浮点局部变量从栈加载到 s0；浮点条件表达式用分支实现；避免未处理表达式静默返回 0.0 |

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
| dcc++ 完整 concept 求值 | `sizeof(T)<=N` / `same_as<T,U>` / `derived_from<T,Base>`；解析 + 简化求值；运行时测试 `semantic_enum_type_traits_runtime` |
| dcc++ enum 语义保留 | 常规 enum / scoped enum (`enum class`) / 匿名 enum；运行时测试验证枚举值与底层类型 |
| dcc++ 协程 | `co_await` / `co_yield` / `co_return` Duff's device 状态机 + promise type 推导；运行时测试 `m26_coroutine_runtime` + `coroutine_co_await_runtime` |
| dcc++ module BMF 序列化 | DCC_BMF_V1 文本格式；`to_bmf`/`from_bmf` round-trip；运行时测试 `m26_modules_bmf` + `m26_module_cross_tu_import` + `m26_module_partition_import` + ODR 测试 |
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
| dcc++ `virtual` 继承完整布局 | 传递虚基 DFS 去重、`__vbptr` + `__vbt_<C>` vbtable（vbase_offset 项）、虚基子对象嵌入对象尾部、自动默认 ctor 初始化 vbptr 并调用虚基 ctor、成员访问与上转型经 vbtable 运行时 this-adjust；同时修复 dcc C 后端 `ptr->field` 成员访问偏移 bug |
| dcc++ 标准库模板实参 | `std::vector<int>` 等限定名模板实参语法解析 + 局部变量声明支持；std::vector<T> 为真正类模板，单态化由 `TemplateRegistry` 完成 |
| dcc++ 动态 RTTI | typeinfo/vtable 生成 + 继承虚方法检测 + RTTI slot |
| dcc++ 继承成员访问 | 递归继承链查找 + `__base_BaseName` 路径改写 |
| dcc++ `__dcc_mbr` 继承成员 | 继承成员偏移辅助函数 + `__base_` 链路径计算 |
| dcc++ `auto` 类型推导 | int / `const char*` / class 真推导 |
| dcc++ `decltype` / `decltype(auto)` | 函数返回类型注册表 + 成员调用/成员字段类型推导 + `new T`→`T*`；自由函数调用改为与定义一致的 mangled 名 |
| dcc++ `operator new[]` / `delete[]` | Itanium ABI `_Znam` / `_ZdaPv` 符号 |
| dcc++ `new T[n]` 数组分配 | `CppExpr::NewArr` + `_Znam` |
| dcc++ `placement new` | `new (ptr) T(args)` 原地构造 |
| dcc++ `static` 类成员 | C `static` 字段 + Itanium ABI mangling |
| C++ 修饰符 | `noexcept` / `noexcept(expr)` 异常可能性分析（函数调用查 noexcept 注册表、`new`/`dynamic_cast`/多态 `typeid` 视为可抛、内建运算/字面量/cast 不抛） / `final` / `override` / `mutable` / `volatile` / `explicit` / `friend` / `alignas` / `alignof` |
| 模板嵌套深度 | `MAX_TEMPLATE_DEPTH = 900`（`src/cpp/template_depth.rs`），与 GCC 默认一致（Clang 为 1024），含深度超限诊断 |
| `if constexpr` | 编译期分支：const_eval 在 lower 端选分支 |
| C++ 标准库扩展 | `<chrono>` / `<thread>` / `<mutex>` / `<condition_variable>` / `<filesystem>` / `<expected>` / `<flat_map>` / `<regex>` / `<ranges>` |
| C++23 多维下标 | `obj[i,j,k]` → `__dcc_multi_index_N` |
| Itanium C++ ABI 名字修饰 | 成员函数 / 运算符 / vtable / typeinfo / 模板形参 |
| C++→C 降级 | struct 字段 / 隐式 this / 命名空间扁平化 / 基类嵌入 / vtable 符号 / typeinfo 符号 / 异常 / lambda 闭包 / 模板完整单态化 |

## C 前端补洞

| 特性 | 说明 |
|------|------|
| MS 扩展 | `__declspec(align/noreturn/thread/dllimport/dllexport)` 接受无 codegen 差异 / `_ms_restrict` / `__pragma` 接受无操作 |
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
| 嵌套块重声明 (C99 6.2.1) | 函数体内遮蔽允许 (不同类型不报 E0007)；文件作用域类型不匹配仍报 E0007 |
| K&R 函数 | 解析自动检测 + `is_old_style` |
| `(*name)[N]` 指针到数组声明符 | `pmd_t (*early_pgts)[512]` 形式；局部变量 + 全局变量 |
| `__auto_type` / C23 `auto` 类型推导 | 由初始化表达式推导类型；支持多声明符 |
| 预置 C11 `atomic_*` 宏 | hosted 模式预置 `atomic_int`/`atomic_bool`/`memory_order_*`/`atomic_thread_fence`/`kill_dependency`/`ATOMIC_*_LOCK_FREE` 等；freestanding 模式 `__STDC_HOSTED__=0` + `__STDC_NO_ATOMICS__=1` 且不预置 |

## 链接器

| 特性 | 说明 |
|------|------|
| 跨 TU LTO | `-flto` 生成 `.llvm.lto`；`-r` 多 `.o` 经内置路径合并 |
| `.llvm.lto` 摘要版本 | `.llvm.lto` 使用 `DCC_LTO\0` + version 1，解析器拒绝非当前版本 |
| LTO dead symbol 剔除 | `-ffunction-sections` + `ld --gc-sections` + objcopy strip |
| LTO 跨函数内联 | cost model：≤4 语句 / 4 表达式 / 无递归 |
| LTO 跨 TU 常量传播 | 跨 TU `global constant` 初始化值合并 |
| LTO 跨 TU 全局优化 | DCE + DAE |
| LTO 4 pass 增强 | `LtoConfig` 配置化 + Pass 7 no_semantic_interposition |
| LTO IPA-SRA / PGO 集成 | `src/ipa_sra.rs` + `src/profile/pgo_integration.rs` |
| `gc_sections_root` 自动选根 | 无显式根时自动选所有 STB_GLOBAL/STB_WEAK 符号 |
| object reader 输入边界 | 接受 ELF64 little-endian (x86_64/AArch64/RISC-V) 和 ELF32 (MIPS big-endian/ARM little-endian) ET_REL；支持 SHT_REL 和 SHT_RELA 重定位；ELF32 符号表 16 字节/条目，ELF64 24 字节/条目；同时接受 COFF/PE AMD64 目标文件 (machine 0x8664)，自动检测格式 |
| Windows COFF codegen | `--target x86_64-pc-windows-msvc` 生成 COFF/PE AMD64 可重定位目标文件；`src/codegen/coff.rs` 实现 COFF 文件头、节头、符号表、重定位表和字符串表；节名映射 (.text/.rdata/.data/.bss/.rdata$T/.bss$T)；COFF 重定位类型 (ADDR64/ADDR32/ADDR32NB/REL32)；符号存储类映射；object reader 可解析 COFF 输入 |
| Windows PE executable link | `--target x86_64-pc-windows-msvc` 可链接生成 PE32+ (64-bit) 可执行文件；`src/link/pe.rs` 实现 DOS 头、PE 签名、COFF 文件头、PE32+ 可选头和节表；节映射 (.text/.rdata/.data/.bss)；默认 ImageBase 0x140000000；SectionAlignment 4KB；FileAlignment 512B；Subsystem Console；`OutputFormat::Pe` 在 `LinkConfig` 中按目标 OS 自动选择 |
| relocation 支持边界 | 按当前架构白名单分发 relocation；未枚举或错架构命名空间的 relocation 返回 `UnsupportedType` |
| `.gnu.hash` TIE 压缩 | ELF 64-bit GNU hash（bloom filter + buckets + chain） |
| `.eh_frame_hdr` 搜索表 | 二分搜索表（按 initial_location 排序） |
| IFUNC 解析 | `STT_GNU_IFUNC` → `R_X86_64_IRELATIVE` 重定位 |
| IFUNC `target_clones` 多版本 | STT_GNU_IFUNC + R_X86_64_IRELATIVE，resolver 返回 default 变体 |
| `--exclude-libs` 隐藏静态库符号 | 被排除归档库中全局符号打 `STV_HIDDEN` |
| `--build-id=sha1/uuid/md5/none` | 内置 SHA-1 / MD5 / UUID v4 写 `.note.gnu.build-id` |
| `--no-undefined-version` | 缺版本 `.symver` 引用 → 错误 |
| `.symver` 完整版本表 | `.gnu.version` 16-bit 索引表 + `.gnu.version_d` VERDEF |
| `-z relro` / `-z now` / `-z lazy` / `-z execstack` / `-z noexecstack` | 段处理完整；未知或参数非法的 `-z` 选项严格报错 |
| `-z ibt` / `-z shstk` | CET/IBT/SHSTK 开关进入 `.gnu.property` 与安全 PLT 配置 |
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
| 共享库 PLT (lazy/CET/BIND_NOW) | `plt_got.rs` .plt (lazy) / .plt.sec (CET/IBT) / .plt.got (BIND_NOW) 生成；`-z ibt` 启用 endbr64 安全 PLT + `.gnu.property` CET 标记；`-z now` 启用 GOT-resolved PLT 无 lazy stub |
| 自定义 dynamic linker 路径 | `PT_INTERP` 程序头 + `.interp` 节；生成的 PIE/动态 ELF 依赖 host `ld.so`，dcc 自身可用 musl 静态链接 |
| vDSO 集成 | `--vdso=<path>` 解析 vDSO ELF，嵌入 `.text.vdso` + `.note.GNU-vdso` + 符号表 |
| PIE/Shared 输出 | 多架构 `_start` 入口桩 (x86_64/aarch64/riscv64/arm/mips/mips64) |
| AArch64/RISC-V/MIPS/ARM ELF 输出 | `elf_exec.rs` 多架构包装函数 |
| AArch64 重定位（33 种） | `ABS64` / `PREL64` / `CALL26` / `JUMP26` / `ADR_PREL_*` / `MOVW_UABS_*` / `TLSDESC_*` (8 种) / `TLSIE_*` (2 种) / `TLSLE_*` (2 种) |
| RISC-V 重定位（15 种） | `64` / `32` / `CALL` / `PCREL_*` / `HI20` / `LO12_*` / `TPOFF*` 等 |
| GOT64 大条目 | `R_X86_64_GOT64` / `R_X86_64_GOTOFF64` |
| TLS 重定位 | `R_X86_64_TPOFF64` / `R_X86_64_TPOFF32` |
| TLS codegen | `_Thread_local` → `.tdata` / `.tbss`、`STT_TLS`；AArch64 TLSDESC 经典序列 (3 重定位) + 优化序列 (5 重定位: OFF_G1/OFF_G0_NC/LDR/ADD/CALL) + IE/LE 序列；linker 全类型解析 |
| ET_EXEC symtab | `build_elf_exec_x86_64` 输出 `.symtab` / `.strtab` |
| wchar_t libc 转发 | wcs*/wmem* 11 个入口在 link implicit undef 白名单 |
| signal libc 转发 | `sigaction` / `sigaltstack` / `signal` 白名单 |
| 复杂数学 libm 转发 | `cproj` / `cabs` / `carg` 白名单 |
| 符号解析: 强/弱/未定义 | `resolve.rs` 实现完整符号解析规则：强 vs 强 → 错误；强 vs 弱 → 强胜；弱 vs 弱 → size 最大者胜；弱未定义 → 解析为 0 (NULL)；强未定义 → 报错 (允许列表除外)；`nm` 可见未定义符号 (U) 和弱符号 (W) |
| Tentative definition 合并 | 跨 TU 的未初始化全局变量 (`int x;`) 多个强定义自动合并 (取最大 size)，与 GCC common symbol 行为一致；仅适用于 `STT_OBJECT`/`STT_NOTYPE`，函数重复定义仍报错 |
| 未定义符号输出 .symtab | 允许列表中的未定义符号和弱未定义符号均写入输出 `.symtab`，`nm` 显示为 `U` (undefined)；ELF `st_shndx = SHN_UNDEF (0)` |

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
| `.orc_unwind_ip` / `.orc_unwind` | 由 objtool 统一生成，dcc 不输出（避免 "file already has .orc_unwind section" 冲突） |
| DWARF line table | `-g1` 下自动去重; `DW_LNE_set_discriminator` 支持同 PC 多行条目 (asm 逐指令调试) |
| DWARF type info | 位域成员 `DW_AT_bit_size` + `DW_AT_bit_offset` |
| `-gsplit-dwarf` / `-gpubnames` | `.dwo` 文件生成 |

## 诊断

| 特性 | 说明 |
|------|------|
| 诊断 snapshot 格式锁定 | E0001-E0018 / E0100-E0102 / E0200-E0209 / E0300 / E1001-E1010 / W0001 / W0004-W0005 / W0007-W0008 / W0200 |
| `-fdiagnostics-color` | `always` / `never` / `auto` |
| `-fdiagnostics-format` | `github` / `clang` / `msvc` / `vi` / `sarif` |
| `-fdiagnostics-show-template-tree` | 模板树显示 |
| format 属性 | `format_check.rs`：`-Wformat` / `W0003` arity 检查 |
| asm goto 标签验证 | `W0004`：asm 模板中引用的标签不在 goto label 列表中 |
| asm 语义校验 | `E0014`: 输出约束缺少 `=`/`+` 前缀; `W0005`: 约束与表达式类型不匹配 (如 `r` 约束绑浮点); `W0007`: 命名/位置操作数混用; `W0008`: 模板占位符索引越界、命名操作数不存在、matching constraint 数字无效、重复操作数名等非法 asm 操作数均 warning 后由 mini-asm 发 `nop` fallback |
| 严格类型 / permissive | 默认未知 typedef → `E0009`；`-fpermissive` → `W0002` + int；C99/GNU99+ 未声明用户函数调用默认 `E0017`，`-fpermissive` 或 C89/GNU89 → `W1000` + int；`__builtin_*` / `__atomic_*` / `__sync_*` 仍按编译器内建处理 |

## 内联 asm 优化器集成

| 能力 | 说明 |
|------|------|
| asm 周围指令重排别名分析 | 指令调度将任何 `__dcc_asm_*` 符号引用视为调度屏障; memory clobber 产生 Store 到 `__dcc_asm_memory_barrier` 被 Store 分支捕获; 防御性检查扩展到 Bin/Un/Load/Cast 操作数 |
| DCE 不消除无副作用无输出 asm | 所有 inline asm 视为潜在副作用，DCE 不消除 (GCC 兼容) |
| 寄存器分配器 asm 统一规划 | `register_allocation` 统计 asm 标记寄存器压力 (`asm_reg_pressure` 计入 `__dcc_asm_*` Store 数); 总压力 = SSA 值数 + asm 压力; codegen 侧 `parse_clobber_registers` 排除 clobbered 寄存器 |

## 内联 asm 调试信息

| 能力 | 说明 |
|------|------|
| DWARF 行号精度 | `-g` 下 asm 块起始 PC 记录到 `asm_line_entries`; 有 asm 行号时重新生成带 `line_entries` 的完整 DWARF CU; `addr2line` 可定位到 asm 语句起始行 |
| GDB step-into asm 体验 | asm 块内每条指令生成独立 `.debug_line` 条目 (`asm_insn_debug_entries`); 块级条目 `is_stmt=true` 标记语句边界, 后续指令 `is_stmt=false` + 递增 discriminator; `write_line_entry` 支持 `DW_LNE_set_discriminator` 扩展操作码; GDB 可单步进入 asm 块内部每条指令 |

## 内联 asm 测试覆盖

| 场景 | 说明 |
|------|------|
| 多 TU 跨文件 asm goto 跳转 | `asm_goto_multi_tu_compiles_and_runs`: 分别编译两个 `.o` 后 gcc 链接，验证 asm goto 跨 TU 符号解析 |
| asm goto 输出约束跨路径写回 | `asm_goto_output_paths_compiles_and_runs`: 验证跳转路径不写回、fall-through 路径正确写回 |
| SIMD 指令端到端运行验证 | `asm_avx512_runtime_compiles_and_runs`: 使用 SSE2 标量指令验证运行时结果正确性 |
| asm goto 与 `&&label` 混合 | `asm_goto_label_addr_mix_compiles_and_runs`: 同一 CU 内 asm goto 与 computed goto 共存 |
| 嵌套 asm `.macro` / `.rept` 块 | `asm_nested_macro_rept_compiles_and_runs`: 多层 `.rept`/`.irp` 嵌套展开验证 |
| memory clobber + 优化器重排 | `asm_memory_clobber_optimizer_compiles_and_runs`: `-O2` 下 memory clobber 编译器屏障端到端验证 |
| 逐指令 asm 调试信息 | `asm_per_insn_debug_insn_entries`: `-g -c` 下多指令 asm 块生成独立 `.debug_line` 条目，`.debug_line` 节包含 `DW_LNE_set_discriminator`; 运行时结果验证 |

## 杂项

| 特性 | 说明 |
|------|------|
| `setjmp` / `longjmp` | libc + DWARF CFI 兼容寄存器保存表 |
| `alloca` | `__builtin_alloca` codegen: sub rsp + 16 字节对齐 |
| 信号 | `sigaction` / `sigaltstack` / `signal` 运行时验证通过 |
| TLS dtv / TLS 描述符 | AArch64 TLSDESC 经典 4 指令序列 + 3 重定位 + 优化 MOVW 对 5 重定位；IE/LE 路径；linker 全类型支持 |
| AArch64 vector 表达式 | 支持 16B vector 局部槽位、aggregate initializer、local load/store、call/return 参数、assignment、cast、conditional、lane index 和二元算术/按位路径；覆盖非二元表达式不再落到零值 fallback |
| non-x86 QEMU runtime matrix | `non_x86_qemu_runtime_matrix_runs_when_available` + 四个扩展测试（`non_x86_qemu_runtime_hello/control_flow/aggregate_abi/va_arg`）在 `qemu-aarch64`、`qemu-riscv64`、`qemu-mips`、`qemu-mips64`、`qemu-arm` 存在时运行 hello、control-flow、aggregate ABI、va_arg 语义门禁，缺少 qemu-user 时显式 skip |
| AArch64/RISC-V HFA ABI | AArch64 HFA 1–8 元素 + >8 sret fallback（`aarch64_hfa_abi_compile_and_elf_structure` / `aarch64_hfa_asm_output_has_all_functions`）；RISC-V HFA 1–4 + >4 sret fallback（`riscv64_hfa_abi_compile_and_elf_structure` / `riscv64_hfa_asm_output_has_all_functions`）；ELF machine/type 验证 |
| non-x86 inline asm 测试 | `-S` 全局 asm 文本保留验证五架构（`non_x86_asm_minus_s_preserves_global_asm_text`）；`-c` asm goto 警告验证（`non_x86_asm_minus_c_warns_on_asm_goto`）；arm/mips/mips64 asm 表达式警告（`non_x86_asm_arm_mips_warn_on_inline_asm_expr`）；约束解析寄存器名替换验证四架构（`non_x86_asm_constraint_resolution_minus_s`）；memory clobber 屏障五架构（`non_x86_asm_memory_clobber_barrier`） |
| unsupported target 负向测试 | 6 个未知架构 (wasm32/avr/msp430/xtensa/powerpc64/loongarch64) `unknown arch` 错误验证（`unsupported_target_arch_rejected`）；Windows MSVC COFF 输出 + Darwin ELF 输出验证（`windows_darwin_triple_object_format`），Windows MSVC 生成 COFF AMD64 目标文件，Darwin 仍输出 ELF |
| atexit / `__cxa_atexit` | `FiniArrayBuilder` 按 priority 排序 |
| `printf` / `scanf` format string | 完整 flag/width/precision/length/type 匹配 + 指针类型验证 |
| `wchar_t` / 宽字符字面量 `L"..."` | wcs*/wmem* libc 转发 |
| `char16_t` / `char32_t` (C11/C++11) | uint16/uint32 + u""/U"" 字符串 |
| 原子混合大小 | `__atomic_*` 1/2 字节变体（lock xadd/xchg/cmpxchg 8/16 位） |
| `stdatomic.h` 全部 | `atomic_flag` / `atomic_signal_fence` / `atomic_thread_fence` |
| 浮点环境 (`fenv.h`) | `fegetround`/`fesetround`/`feclearexcept`/`feraiseexcept`/`fetestexcept`/`fegetexceptflag`/`fesetexceptflag`/`fetestexceptflag`/`fegetenv`/`fesetenv`/`feholdexcept`/`feupdateenv` 内联为 MXCSR/x87 指令 (不依赖 libm)；`pragma STDC FENV_ACCESS` ON/OFF/DEFAULT 驱动 opt 阶段浮点折叠开关 |
| 复杂数学函数 | `cproj` / `cabs` / `carg` 走 libm |
| executable `.pushsection` jump targets | `.fixup` / `.altinstr_replacement` code moved from `.text` keeps valid PC32 relocations for objtool jump-destination checks; executable section alignment padding is emitted as `nop` bytes so objtool can keep decoding through function/section tails |
| per-function section | `-ffunction-sections` / `-flto` 默认 `.text.<name>` |
| jump_label 静态 key 声明 | `_DECLARE_STATIC_KEY_0/1` |
| rela 引用标记 | `elf::referenced_symbol_names` → `lto::mark_references_from_objects` |
| CLI 多输入 | `-r a.o [b.o…] -o merged.o` 链接模式 |
| Bootstrap 回归对比 | `src/bootstrap.rs`：字节级 / 语义级三层对比 |
