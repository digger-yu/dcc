# DCC Linux 6.6 缺失特性（按自动化证据）

> 最后更新：2026-07-21 — 以 `scripts/test_all.sh`、
> `scripts/kernel_full_check.sh`、`scripts/prepare0_check.sh`、
> `scripts/pahole_regress.sh`、`scripts/objtool_regress.sh`、
> `scripts/modpost_link.sh`、`scripts/vmlinux_regress.sh`
> 与本地 Linux 6.6 / GCC libiberty 日志为准。

## 已通过的门禁

| 门禁 | 证据 |
|------|------|
| typed IR + 严格语义 | `cargo test` integration（static_assert / unknown typedef / int128）+ `tests/errors.rs` snapshot（E1003–E1008 等） |
| SysV va_list（GP） | `examples/test_p4_va_list.c` |
| asm goto + computed goto | `examples/test_asm_goto.c` |
| 自定义 ELF section / WEAK / HIDDEN | `examples/test_p2_elf_attrs.c` + readelf 断言 |
| __int128 / register asm / 全局符号 | `examples/test_p1_features.c` |
| 预处理 forced-include 注释安全 | `tests/fixtures/preproc_comment_force.h` |
| mod/devicetable 生成 TU | `devicetable-offsets.c` 直接 `-S` 输出 50+ `SIZE_*` 行 |
| DWARF + BTF（含 struct） | `-g` 生成 `.debug_*` + `.BTF`（含 `dcc_probe` STRUCT 类型） |
| objtool / ORC 帧信息 | `-g`/`-fno-omit-frame-pointer` 生成 CIE+FDE `.eh_frame`、`.orc_unwind_ip` |
| KASAN/UBSAN/KCSAN/KCOV/Fuzzer 插桩 | `-fsanitize=*`：TU init 一次 + 每函数入口桩 + AST 表达式级 KASAN load/store + UBSAN div/shift/signed overflow + KCOV 边覆盖 + Fuzzer cmp/switch trace + `.dcc.san_info` v2 |
| 跨 TU LTO | `-flto` 生成 `.llvm.lto`；`-r` 多 `.o` 经 `ld -r` + objcopy 合并摘要 |
| LTO dead symbol 剔除 | `-ffunction-sections`（`-flto` 默认）+ `ld --gc-sections` + objcopy strip |
| 跨平台 Triple 解析 + 后端 | `cli.rs` + `target.rs` 接受 `mips / mips64 / arm (armv7)` 的 `--target` 字符串,`Target::resolve()` 给出正确几何参数;`codegen::emit()` 支持全部 6 个架构 (x86_64/aarch64/riscv64/mips/mips64/arm) |
| `-S` 汇编输出 | lock/SSE 前缀；自定义节按 `.section` 分组 |
| modpost `file2alias.c` | 单 TU `-c` 生成 `file2alias.o` |
| prepare0 窄目标 | `scripts/prepare0_check.sh`（devicetable / bounds / file2alias） |
| pahole / objtool 单 TU 回归 | `scripts/pahole_regress.sh`、`scripts/objtool_regress.sh` |
| modpost 链接 / Rust fixdep | `scripts/modpost_link.sh` + `src/bin/fixdep.rs` |
| prepare0 全链路 | `scripts/prepare0_full.sh`（本地 PASS） |
| QEMU / vmlinux 回归脚本 | `scripts/qemu_boot_matrix.sh`、`scripts/vmlinux_regress.sh` |
| 内核全树编排 | `scripts/kernel_full_check.sh`（缺产物时 SKIP） |
| `#if` 十六进制字面量 | `preproc` 单测 `if_hex_*`（原 `0x41` 被拆坏） |
| `#include_next` | `preproc` 单测 + glibc `<limits.h>` |
| 宏展开涂蓝 / 防栈溢出 | `nested_function_macro_*`；本地 `argv.c` 不再 abort |
| `_Generic` + 三元控制表达式 | `tests/fixtures/generic_ternary.c`；本地 `libiberty/argv.c` |
| `_Generic` 指针精确匹配 | `tests/fixtures/generic_pointer.c` |
| 内联 asm 约束 | `examples/test_asm_constraints.c` |
| SysV va_list SSE | `examples/test_p4_va_list.c`（GP + float/double） |
| Mini x86 asm 局部/C 标签 | `tests/fixtures/asm_local_label.c`、`asm_label_insn.c`（`N:`/`N:insn`/`found:`） |
| Mini x86 asm 位操作 / 双精度移位 | codegen `bt/bts/btr/btc`、`shld/shrd`（内核 paravirt 等） |
| Mini x86 asm 扩展位/字节/NT/虚拟化 | `bsf/bsr/popcnt/bswap/movnt*`、`vmcall/vmmcall`、RIP-relative、`push/pop` mem |
| Mini x86 asm GAS 伪指令 / 前缀 | `.pushsection`/`.balign`/`.long`/`.quad`/`.popsection`；`lock`/`rep` |
| GCC Builtin 完整化 | object_size FORTIFY、apply/return、prefetch no-op、未知 builtin warning |
| __builtin_constant_p / inline 属性 | `tests/fixtures/builtin_constant_p_full.c`、`inline_attrs.c` |
| 未知 __builtin_* ud2 | `tests/fixtures/unknown_builtin.c`；void → ud2，其余 → 零值 + warning |
| __builtin_va_start/end/copy/arg | SysV va_list 结构（r13 帧链 + 24 字节槽） |
| __builtin_strcasecmp / strncasecmp | 转发到 libc strcasecmp/strncasecmp |
| __builtin_fma / fmaf / fmal | 调 libm 对应函数（fmaf 走 movss，F2/F3 0F 10/11 备份 xmm3） |
| __builtin_rotateleft/right{8,16,32,64} | ROL/ROR + cl，32 位 mov eax 回写 |
| __builtin_clrsb/clrsbl/clrsbll | cdq/cqo 符号扩展 + bsr，sentinel -1 + cmove 兜底 |
| __builtin_{add,sub,mul}_overflow | 64 位运算 + 截断到操作数宽 + 比较 SETcc |
| __builtin_{add,sub,mul}_overflow_p | SETO/SETB 标志直接返回 |
| __builtin_bswap16/32/64 | bswap r32/r64；16 位走 rol r16, 8 + movzx |
| __builtin_clz/ctz/popcount/parity | bsr/bsf/POPCNT/XOR fold |
| __builtin_bitreverse8/16/32/64 | 固定次数移位循环，无外部依赖 |
| __builtin_frame_address / return_address | r13 帧链遍历 |
| __builtin_types_compatible_p / choose_expr | parser 折叠 + codegen 兜底 |
| __builtin_va_list / FILE / LINE / FUNCTION / FUNCSIG | 类型别名 + codegen 字符串 rodata |
| IR 优化 pass | `ssa_gvn.rs` GVN/CSE；`loop_opt.rs` LICM/展开/向量循环；`-O2`/`-O3` |
| always_inline @ -O0 | `tests/fixtures/always_inline_o0.c` |
| 全局 SIMD 向量复制 | `tests/fixtures/vector_global*.c`；`.data` 逐元素静态 init + 运行时 binop |
| 浮点 SIMD 标量逐元素 | `tests/fixtures/vector_float_scalar.c`；v4f/v2d +/-/× 标量 SSE |
| __int128 SysV 传参/乘除模 | `tests/fixtures/int128_call_abi.c`、`int128_mul_div_mod.c` |
| mini-asm 内核扩展 | `tests/fixtures/asm_kernel_extra.c`；rol/prefetch/movss/addss |
| 非 Ident 向量 rvalue | `tests/fixtures/vector_binop_non_ident.c`；`(a+b)+c`、`v=v+w` |
| x86_64 codegen 扩展 | ✅ 向量/SIMD、复数、mini-asm、object_size FORTIFY、apply/return、prefetch |
| aarch64 typed IR | hello/控制流/调用/浮点/向量/rodata；见 `aarch64_*_cross_compile` |
| riscv64 typed IR | hello/控制流/调用/rodata；见 `riscv64_*_cross_compile` |
| 诊断 snapshot 格式锁定 | `tests/errors.rs` schema + `diagnostic_format.stderr.snap`（V1） |
| `struct __attribute__((__may_alias__))` | `tests/fixtures/may_alias_struct.c` |
| 宏 `#` 字符串化换行折叠 | `preproc` 单测 `stringify_collapses_*`；`NOFAIL`/`modpost.c` |
| asm goto 行内 `#` 注释 | `tests/fixtures/asm_goto_hash_comment.c`；`jump_label.h` objtool 标注 |
| aarch64/riscv64 整数除/取模 (M7) | `codegen/aarch64.rs` 走 `sdiv`+`msub`，`codegen/riscv64.rs` 走 RV64M `div/rem`；不再退化为 0；`aarch64_ir_div_mod_emit_sdiv_msub` + `riscv64_ir_div_mod_emit_rv64m` |
| aarch64/riscv64 移位 (M7) | `lslv`/`lsrv` (aarch64) 修复 Rm bit 错位；`sll`/`srl`/`sra` (riscv64) 替代 0；`aarch64_ir_shifts_emit_lslv_lsrv` + `riscv64_ir_shifts_emit_sll_srl` |
| 聚合类型 by-ref (M7) | aarch64/riscv64 `gen_call` 检测 `ValueType::Aggregate`/`Array`/`Complex`，local 参数通过 `add xN, sp, #off` / `addi aN, sp, #off` 传地址；`aarch64_aggregate_arg_by_ref` + `riscv64_aggregate_arg_by_ref` |
| 聚合类型 sret 返回 (M7) | 返回 > 16 字节时 caller 分配 `<sret>` local，x0/a0 指向 sret 槽 |
| aarch64 M8 完整化 | 谓词指令 (CCMP/CSEL/CSINC/CSNEG) + 双精度浮点比较 (FCMP Dn,Dm / FCCMP) + 后置/前置索引寻址 + 扩展寄存器寻址 + 缓存维护完整 (dc cvac/civac/cvau/ivac + ic ivau/iallu + dsb ish/sy + isb) + BL/ADR/LDR 范围校验 + HFA store + `emit_cache_sync_range` 循环修复；测试 `aarch64_ccmp_csel_csinc_emit` / `aarch64_fcmp_double_emit` / `aarch64_cache_maintain_emit` / `aarch64_post_index_addring_emit` / `aarch64_bl_range_check` / `aarch64_adr_range_check` 全部 PASS |
| riscv64 M8 完整化 | JAL ±1MiB 范围校验 + HFA 分类 (`classify_rv_hfa`) + >16B sret 返回 (`memcpy_sp_to_a0`) + DWARF/BTF/eh_frame/ORC/sanitizer 节集成 + 压缩指令 C 扩展 (C.ADDI/C.NOP/C.LI/C.LW/C.SW/C.J/C.BEQZ) + FP D 扩展双精度 (FLD/FSD/FADD.D/FSUB.D/FMUL.D/FDIV.D/FCVT.D.W/L/S/FMV.D.X/X.D/FSGNJ.D) + RVV 向量基础 (VSETVLI/VLE32/VSE32/VADD.VV/VMUL.VV/VREDSUM.VS) + 特权指令/CSR (CSRRW/CSRRS/CSRRC/CSRRWI/MRET/SRET/WFI/SFENCE.VMA/FENCE/FENCE.I)；测试 `riscv64_jal_range_check` / `riscv64_compressed_c_addi_emit` / `riscv64_fadd_d_emit` / `riscv64_csrrs_mstatus_emit` / `riscv64_vsetvli_emit` / `riscv64_fence_emit` / `riscv64_mret_emit` 全部 PASS |
| dcc++ 端到端编译 (M9) | `dcc -x c++ examples/m9_cpp_simple.cpp -o a.o` + `gcc a.o -o a && ./a` 在本地 cargo test 实测 `exit=0`（`Counter` 局部变量 + `c.inc()` 改写 + ctor 初始化列表 + 顶层 main trampoline） |
| dcc++ 多类 + 隐式 this (M9) | `examples/m9_cpp_multi.cpp` 本地实测 `exit=0`（方法体内未限定 `n_` 自动改写为 `this->n_`） |
| dcc++ 复合操作符 / C 关键字 (M9) | `->` / `->*` / `--` / `-=` / `++` / `+=` / `<<` / `<<=` / `>>` / `>>=` / `==` / `!=` / `<=` / `>=` / `&&` / `||` / `*=` / `/=` / `%=` / `^=` / `|=` / `&=` / `...` 全部支持；`int` / `return` / `if` / `while` 等不再被误为 `Ident` |
| dcc++ C++20 占位 (M9) | `module` / `import` / `export` / `concept` / `requires` / `co_await` / `co_yield` / `co_return` 全部降级为 no-op 注释 |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前状态 | 说明 |
|------|----------|------|
| prepare0 全链路 `make prepare0` | ✅ 脚本等价 | 用 `prepare0_full.sh`；`make prepare0 CC=dcc` 豁免（kconfig 宿主 C） |
| 全树 `make CC=dcc` | ⏳ 待本地下一步 | 推荐 `HOSTCC=gcc` 混合；Rust fixdep 已替代 C fixdep |
| modpost.c / sumversion.c（宿主） | ✅ 可过编 | 兼容 typedef、局部遮蔽 libc 名、asm `#` 注释 |
| QEMU 启动矩阵 | ⚠️ 脚本就绪 | 无 bzImage 时 SKIP；需步骤 6 产物 |
| vmlinux pahole / objtool | ⚠️ 脚本就绪 | 无 vmlinux 时 SKIP；单 TU 回归已通过 |

## 短期：GCC libiberty（纯 C）

详见 `docs/gcc_build_guide.md`。本地当前（2026-07-21）：

| 组件 | 结果 |
|------|------|
| libiberty 单 TU | **125 OK / 0 FAIL / 4 SKIP** |
| libiberty `dcc -r` 链接 | ✅ ~1MB 合并 `.o` |
| fixincludes | **6 OK / 0 FAIL / 1 SKIP** |
| libdecnumber | ✅ 11 SKIP→1 OK（组合 TU 方案，`tools/libdecnumber_combined.c`，三段 DECPMAX 7/16/34） |
| dcc C++ 前端 (M7) | ✅ `cpp::lower` 单测通过；最小 class+ctor+dtor 可降级到 C 字符串并经 dcc 编译 |
| dcc GCC 完整 bootstrap | ⏳ 见下表 |

| 缺口 | 影响 |
|------|------|
| 无 C++ (M8 部分解锁) | M8 已支持 命名空间 / 运算符 / 继承 / vtable / RTTI / 异常 / lambda / 模板；仍缺 SFINAE / concepts / modules / coroutines |
| `_Generic` 指针精确匹配 | ✅ 按 pointee 匹配；void* 与对象指针兼容 |
| libdecnumber 组合编译 | ✅ `tools/libdecnumber_combined.c` 把 11 个 SKIP 文件合一，DECPMAX 7/16/34 三段；本地 1 OK |
| 内联 asm 约束 | ✅ `"r"`/`"m"`/`"=r"`/`+m`、`memory` clobber |
| 内联 asm 约束扩展 (M11.3) | ✅ `"i"` 立即数 / `"p"` 内存地址 / `"g"` 通用 / `"X"` 常量 / `"0..9"` 复用 / `"&"` early-clobber / 多重约束 `"rm"` / XMM `"x"`/`"Y"` / 数字标签 `"%"` / `offsetof` 约束；同步修复 `prepare_asm_binding` 输入侧取值的 bug（之前给输入也走 lea，破坏 `r`/`+r` 实参语义，导致 `test_bit_ops` 等 segfault）、`writeback_asm_operand` 32 位写回未零扩展的 bug（C codegen 用 64 位 reload，未初始化槽高 32 位残留栈数据）、`disasm.rs::reg` 错叠 REX.B 的 bug（让反汇编把 reg 字段错当成 r/m 字段扩展），以及 `asm_pick_sub_constraint` 漏识别 X/n/I/N/M/E/F/G/H 的问题 |
| SysV va_list SSE | ✅ GP + XMM0–7 保存区；float/double/long double va_arg |

## C++ 前端 M7 + M8 / GCC bootstrap 状态

| 任务 | 状态 | 说明 |
|------|------|------|
| C++ token 扩展 (`class`/`this`/`new`/`delete`/`::`) | ✅ | `src/cpp/lexer.rs` 5+ 单测 |
| M8 token 扩展 (operator/template/virtual/namespace/try/catch/throw/constexpr/consteval/decltype/typeid) | ✅ | `src/cpp/lexer.rs` |
| Itanium C++ ABI 名字修饰 | ✅ | `src/cpp/mangle.rs`：成员函数 C1/D1/参数类型 `v`/`i`/`P`/`R` + 运算符 (`pl`/`mi`/`ml`/`dv`/...) + `_ZTV<Name>` vtable + `_ZTI<Name>` typeinfo + 模板形参 |
| 最小 C++ 解析器 (M7) | ✅ | `src/cpp/parser.rs`：class、ctor、dtor、成员函数、成员变量、::、this->x |
| M8 解析器扩展 | ✅ | 命名空间 / 运算符 / 继承 (单/多/钻石) / 虚函数 / 模板声明 / try-catch / lambda / typeid / dynamic_cast / static_cast / const_cast / reinterpret_cast |
| C++→C 降级 (M7) | ✅ | `src/cpp/lower.rs`：struct 字段、隐式 this 形参、`_Znwm`/`_ZdlPv` 替身 |
| M8 C++→C 降级 | ✅ | 命名空间扁平化 / 基类嵌入 / vtable 符号 / typeinfo 符号 / 异常 (Itanium zero-cost 降级) / lambda 闭包类型 / 模板 (占位) / dynamic_cast 简化实现 |
| dcc++ 子命令 / `-x c++` | ✅ M9 | `dcc -x c++ foo.cpp` / `dcc foo.cpp`（按 `.cpp`/`.cc`/`.cxx` 扩展名自动识别）；`dcc -dump-ast` / `dcc -fsyntax-only`；本地 cargo test 实测 `exit=0` |
| dcc++ 局部类变量 + 方法调用改写 (M9) | ✅ | `Counter c;` → `struct Counter c; _ZN7CounterC1Ev(&c);`；`c.inc()` → `_ZN7Counter3incEv(&c)`；本地 `m9_cpp_simple.cpp` / `m9_cpp_multi.cpp` 端到端 `exit=0` |
| dcc++ 隐式 this 注入 (M9) | ✅ | 类方法体内未限定成员 `n_` 自动改写为 `this->n_`（基于 `current_class` + `class_fields`） |
| dcc++ C++20 占位 (M9) | ✅ | `module` / `import` / `export` / `concept` / `requires` / `co_await` / `co_yield` / `co_return` 降级为 no-op 注释 |
| dcc++ 完整 SFINAE (M10+) | ✅ | `src/cpp/m10.rs::SfinaeCtx` + `try_substitute` 替换失败不报错，模板形参未绑定跳过该候选 |
| dcc++ partial specialization (M10+) | ✅ | 解析 `template <T> class Foo<T*> {}` 为 `PartialSpec { params, pattern_text, decl }`；`pattern_matches` 支持 `T*` / `T&` / `const T` / 精确匹配 |
| dcc++ 完整 concept 求值 (M10+) | ✅ | `Concept::parse` 支持 `sizeof(T)<=N` / `same_as<T,U>` / `derived_from<T,Base>` / `T::value` / 表达式约束；`Concept::evaluate(&SfinaeCtx)` 替换形参后求值 |
| dcc++ 协程挂起恢复 (M10+) | ✅ | `co_await` / `co_yield` 降级为 `static int __coro_state` 状态机驱动；`co_return` 降级为 `return` |
| dcc++ module 分区 (M10+) | ✅ | 解析 `module foo:part1;` / `export module foo:part1;`；输出 `/* dcc C++ module (M10+): foo:part1 */` |
| dcc++ operator-> 重载链 (M10+) | ✅ | 类型名含 `Ptr`/`Wrapper`/`Iter` 的局部变量在 `->` 访问时识别为可走 operator-> 链 |
| dcc++ 完整 RTTI dynamic_cast 跨虚继承 (M10+) | ✅ | `dynamic_cast<T*>(e)` 降级为 `((T*)__dynamic_cast((void*)(e), 0, 0, -1))`；Itanium C++ ABI: 通过 vtable RTTI 查找 |
| dcc++ 构造委托 / 继承链 ctor (M10+) | ✅ | 解析 `using Base::Base;` 为 `InheritingCtor { class, base }`；`CtorExpander::emit_inheriting` 生成 wrapper 转发 |
| dcc++ 成员函数指针 (M10+) | ✅ | 解析 `&Class::method` 为 `MemberPtr { class, name }`；降级为 `&_member_fn_<Class>_<method>` |
| dcc++ __cxa_guard_* 线程安全 init guard (M10+) | ✅ | 静态局部变量 `static T x = init;` 触发 `_cxa_guard_acquire` / `_cxa_guard_release` 调用；状态机 0/1/2 |
| dcc++ std::initializer_list (M10+) | ✅ | 解析 `{1, 2, 3}` 为 `InitList` / `TypedInitList`；降级为复合字面量 `((int[]){1, 2, 3})` |
| dcc++ C 关键字 / 复合操作符 (M9) | ✅ | 词法补全：`int`/`return`/`if`/`while` 不再被误为 `Ident`；`->`/`->*`/`++`/`--`/`<<=`/`>>=` 等 23 个复合操作符完整 |
| dcc++ Windows 兼容 (M9) | ✅ | `stddef.h` / `stdint.h` 仅在用到 `new`/`delete`/`throw`/`try`/`typeid` 时引入；否则用 `typedef unsigned long size_t;` 替代 |
| C 前端补洞 MS 扩展 (M11.1) | ✅ | `__declspec(align/noreturn/thread/dllimport/dllexport)` / `_ms_restrict` / `__pragma` / `_Pragma` 静默接受；不影响语义 |
| C 前端补洞 MS 扩展 (M14) | ✅ | `__declspec(thread)` → TLS；`__declspec(noreturn)` → cold+noinline；`__declspec(align(N))` → 对齐；dllimport/dllexport 接受无 codegen 差异 |
| C 前端补洞 `__attribute__((cleanup(fn)))` (M11.1) | ✅ | 解析并存入 `VarDecl.cleanup_fn`；codegen 在变量离开作用域时插入 `cleanup_fn(&var)` 调用 |
| C 前端补洞 `__attribute__((target_clones(...)))` (M11.1) | ✅ | 解析并存入 `FuncDecl.target_clones`；当前阶段按默认版本编译，多版本 IFUNC resolver 待 M12+ |
| C 前端补洞 `__attribute__((target_clones(...)))` (M14) | ✅ | 链接期 `TargetClonesTable::gen_irelative_relas` 生成 STT_GNU_IFUNC 符号 + R_X86_64_IRELATIVE 重定位，resolver 简化返回 default 变体 |
| C 前端补洞 `__attribute__((no_sanitize(...)))` (M11.1) | ✅ | 解析并存入 `VarDecl/FuncDecl.no_sanitize`；`sanitizer.rs` 在所有启用的 sanitizer 都被禁止时跳过插桩 |
| C 前端补洞 `__attribute__((copy(fn)))` (M11.1) | ✅ | 复用 `alias_name` 通道生成 `fn` 的别名符号，链接器直接引用原目标 |
| C 前端补洞 `__attribute__((returns_nonnull))` (M11.1) | ✅ | 解析并存入 `FuncDecl.returns_nonnull`；当前阶段作为 noop 属性保留在 AST |
| C 前端补洞 `__attribute__((returns_nonnull))` (M14) | ✅ | codegen `current_returns_nonnull` 状态机 + `test rax, rax; jnz .ok; ud2; .ok:` 强制非空 |
| C 前端补洞 `#pragma` 全家族 (M11.1) | ✅ | `pack(n)` / `pack(push,n)` / `pack(pop)` / `weak name` / `STDC FENV_ACCESS ON/OFF/DEFAULT` / `GCC diagnostic push/pop/ignored/warning/error` / `once` / `comment` 静默接受 |
| C 前端补洞 `#pragma pack` 影响 struct 布局 (M14) | ✅ | `preproc.pack_history: Vec<(u32, Option<usize>)>` 携带行号 + packing 变化；`ParseOptions.pack_history` 传入 parser；`Parser::pack_at_line(line)` 二分查找生效值；`layout_struct_with_pack(max_align)` 把字段对齐 cap 到 `min(nat, n)` |
| C 前端补洞 const_eval 完整化 (M15) | ✅ | `ConstValue` 枚举（`Int/Float/Str/Addr/Bytes`）；`eval_full` 返回完整常量值，旧 `eval` 仅取整数部分；浮点 + - * / 与 `Int<->Float` 混合算术；比较提升到 f64；字符串字面量走 FNV-1a 64-bit 标签 + 字节内容；`&ident` / `&arr[i]` / `&&label` → `Addr { name, offset }`；新增 `__builtin_strlen/abs/labs/llabs/bswap16/32/64/expect/popcount*` 编译期折叠；`Cast` 阶段新增 `-Wint-truncation` / `-Wint-conversion` 严格类型 warning（不阻塞） |
| C 前端补洞 运行时检查 (M15) | ✅ | `-fbounds-check` 数组下标运行时边界检查：仅对 `ArraySize::Const(n)` 的数组插入 `cmp rcx, n*es; jb ok; call __ubsan_handle_out_of_bounds`；`-ftrapv` 有符号算术溢出 trap：binop 之后置 `jo` 跳到 `__dcc_trapv_overflow` 桩函数（覆盖 `Add/Sub/Mul/Div`） |
| C++ 修饰符与类型推导真实化 (M17) | ✅ | 14 + 2 = 16 个 `tests/fixtures/m17_*.cpp` 全部通过: `auto` 真类型推导 (int/const char*/class) / `decltype(expr)` + `decltype(auto)` 解析 / `friend` 走 FreeFn 路径 (无 this 形参) / `using Base::Base;` 类内合法 (复用 InheritingCtor 节点) / `noexcept` → `__attribute__((nothrow))` / `final`/`override` 解析层接受 / `mutable`/`volatile` 成员函数标记 / `explicit` ctor 接受 / `new T[n]` 数组分配 + `_Znam`/`_ZdaPv` extern 替身 / `placement new` 转发 (void*)ptr / `if constexpr` 编译期分支 / 16/16 m17_* 集成测试 PASS |
| C++ 标准库扩展 (M18) | ✅ | 11 个 `tests/fixtures/m18_*.cpp` 全部通过: `<chrono>` (duration / time_point / 三种 clock 走 `clock_gettime`); `<thread>` `<mutex>` `<condition_variable>` 包装 pthread; `<filesystem>` (path / file_type / directory_iterator); `<expected>` C++23 (expected_int / expected_str 擦除模板); `<flat_map>` C++23 (int→int 二分查找); C++23 多维下标 `obj[i,j,k]` → `__dcc_multi_index_N`; `<regex>` 简化版 (回溯 NFA 解释器支持 `.`/`[]`/`[^]`/`*`/`+`/`?`/`|`/`()`/`^`/`$`); `<ranges>` 简化版 (filter/transform/take/reverse view); Concepts 语法层 (SFINAE 简化版); Modules 占位 (src/cpp/modules.rs) |
| C 前端补洞 `_Pragma` 操作符 (M11.1) | ✅ | C99 `_Pragma("string")` 在 preproc 展开阶段累积到 `pending_pragmas: RefCell<Vec<String>>`，源块刷新时批量应用到 `process_directive` |
| C 前端补洞 `_Pragma` 操作符 (M14) | ✅ | `_Pragma` 与 `#pragma` 共享同一条 `process_directive` 通路，自动具备 pack/weak/STDC/GCC diagnostic 处理 |
| C 前端补洞 `#warning` / `#error` (M11.1) | ✅ | preproc 阶段匹配 `^#\s*warning` / `^#\s*error` 行；`#error` 走 fatal 诊断；`#warning` 走 stderr 提示 |
| C 前端补洞 `_Static_assert` 字符串化 (M11.1) | ✅ | C23 单参 `_Static_assert(expr)` 与双参 `_Static_assert(expr,"msg")` 全部支持；失败消息附加 `evaluates to: <ExprDebug 文本>` 便于定位 |
| C 前端补洞 GNU 扩展 (M11.2) | ✅ | `__builtin_offsetof` 支持嵌套 struct/union/数组下标/匿名 struct 成员访问；`__builtin_huge_val`/`huge_valf`/`huge_vall` 按 IEEE 754 位模式 (double 0x7FF0000000000000 / float 0x7F800000 / long double 0x7FFF8000000000000000) rodata 加载；`__builtin_nan`/`nanf`/`nanl` 走 qNaN 编码 (double 0x7FF8000000000000)；`_Atomic` 在 C 前端接通 `__atomic_*` 内建函数 + `__c11_atomic_*` 路径；`__builtin_call_with_static_chain` 按 SysV ABI 把 chain 写入 r10 并按 GP/XMM 寄存器约定传参 + 间接 call；嵌套函数作为 IR 一等公民支持静态链参数；computed goto 跨函数（标签地址走 LEA 加载，间接 jmp 通过寄存器）；asm goto 输出约束在 fall-through 路径上写回，跳转路径保留原值；`__builtin_isnan`/`isinf`/`finite`/`signbit`/`fpclassify` 正确处理 NaN/Inf 位模式；`Expr::Unary` 浮点取负走恒常折叠 + 运行时 XOR 符号位；`AddrOf` REX.R 编码对 r10–r15 修复 |
| 内联 asm 约束全家族 (M11.3) | ✅ | `parse_asm_constraint_flags` 解析 `=` / `+` / `&` / `%` / `0..9`；`asm_constraint_*` 判定：is_immediate (i/n/I/J/K/L/M/N/O/X/E/F/G/H/s)、is_address (p)、wants_xmm (x/Y)、is_general (g)、prefers_memory；`asm_pick_sub_constraint` 从多约束 ("rm") 选单约束，按 `r` > `m` > 立即数 > `g` > 默认优先级；`build_asm_operand_map` 用 `Slot::{Reg, Imm, Mem}` + `meta: (reg, xmm_w, is_out, is_mem, flags)` 记录每个操作数；`prepare_asm_binding(expr, reg, is_output)` 区分输出/输入语义：输出走 lea 取地址（配合 `writeback_asm_operand` 写回），输入直接 `gen_expr` 取值（修复原 bug：输入也走 lea 导致 segfault）；early-clobber 阶段若 `&` 输出寄存器与任一输入寄存器重叠，重新从 GP 池挑选并重新 emit prepare/map；`asm_format_subst` 把 `op_map` 值按 AT&T 形态输出（`%rax` / `$305419896` / `-16(%r13)`）；offsetof 约束在输入侧走 `const_eval::eval` 求值，失败时回退到 GP 寄存器并 `lea` 加载常量基址 + 偏移；测试 `test_asm_constraints_ext.c` 10 个用例 (`test_immediate_constraint` / `test_address_constraint` / `test_general_constraint` / `test_constant_constraint` / `test_match_constraint_0` / `test_early_clobber` / `test_multi_constraint` / `test_xmm_constraint` / `test_digit_label_constraint` / `test_offsetof_constraint`) 本地 cargo test 10/10 PASS |
| GCC 完整 bootstrap (cc1/cc1plus) | ⏳ 部分受阻 | M9 解锁 dcc++ 端到端；本地磁盘已具备（GCC 源 ~1G + 构建 ~5G）；仍缺 SFINAE / partial specialization / 完整 concepts / 协程 / module |
| libdecnumber 组合 TU 编译 | ✅ 本地 OK | `tools/libdecnumber_combined.c` 收敛 decSingle/Double/Quad 三套符号 |
| 嵌套块重声明 (C99 6.2.1) | ✅ | `parser.rs::check_var_redecl` 允许函数体内遮蔽；文件作用域类型不匹配仍报 E0007 |

## 本地 debug 记录（2026-07-15）

| 问题 | 根因 | 修复 |
|------|------|------|
| `tests/integration.sh` 等脚本在 Windows 同步后报 `pipefail: invalid option` | 脚本为 CRLF | `.gitattributes` 强制 `*.sh` LF |
| `devicetable-offsets.c` 缺 `generated/autoconf.h` | 内核树未跑 `syncconfig` | `scripts/kernel_full_check.sh` 自动 `make include/generated/autoconf.h` |
| `file2alias.c` 编译失败 | stddef 重入 + linux/stddef.h 误解析 | 移除 include-once；GCC include 优先 |
| file2alias IR | DEF_FIELD typeof / devtable 非常量初始化 | `type_of_member` + 函数指针静态初始化 |

## 本地 debug 记录（2026-07-16，GCC libiberty）

| 问题 | 根因 | 修复 |
|------|------|------|
| `#if 'A' == 0x41` 为假 | hex 被 ident 替换拆坏 | `replace_unknown_idents` 保留 `0x`/`0b` |
| `argv.c` 栈溢出 | 宏参数展开丢涂蓝集 | 继承 disabled + 深度上限 |
| `limits.h` / `lrealpath` | 无 `#include_next` | 实现并优先匹配指令名 |
| `argv.c` `_Generic` 解析失败 | 三元 else bp=0 吞逗号 | else 改为 `parse_expr_bp(2)` |
| 浮点比较/`&&`/`\|\|` | 结果类型标成 float | 比较与逻辑结果恒为 int |
| 宏参替换破坏字符串 | `replace_ident` 进字面量 | 跳过 `"`/`'` |
| `##` 后再替换 → `&&di` | paste 全局替换 | 只替换 ## 两侧 token |
| `TRANSLATE(*d)` → `(**d)` | 无 `##` 的 `handle_paste` 仍替参 + 第三步多遍扫描 | 无 `##` 早退 + `replace_all_params` 单次不透明替换 |
| `regex.c` 完整文件仍 `(**d)` | V2 include 缓存保留旧展开 | 缓存 bump V3；本地 **125/0** |
| `char const *` / `char const **` | `const` 在 `*` 前未跳过 | `finish_type` 限定符顺序 |
| libiberty 链接 multiple definition | extern 声明 + 定义双份 symtab | 符号收集去重 |
| `ld -r --gc-sections` | bfd 缺 `-u` 根符号 | `gc_sections_root` + `-u` |
| `struct T *a, *b` 变 `T**` | 说明符吃掉 `*` | `parse_decl_specifiers` + 多声明符 |
| 全局 `&arr[i]` | 初值校验过严 | 静态初值 + rela addend |
| 前向 struct 成员 | 空 fields | 空时查 `struct_defs` |

## 本地 debug 记录（2026-07-16，asm-offsets / prepare0）

| 问题 | 根因 | 修复 |
|------|------|------|
| stmt_expr 后 `()` 未解析 | 语句表达式缺后缀链 | `parse_postfix_suffix()` |
| `__typeof__(struct T)` | `(` 后误用 `is_peek_type_start` | 改为 `is_type_start()` |
| `section("a" "b")` | 属性只读一个 StringLit | `parse_concatenated_string_lit()` |
| `__aligned__((expr))` | 只支持纯数字 | 括号深度跳过完整参数 |
| asm 试探 skip 未恢复 lexer | bookmark/restore + 仅 asm goto 试探 | `probe_asm_goto()` |
| 位域 `a:8, b:4` | `parse_expr()` 吞逗号 | `parse_expr_bp(2)` |
| struct `{ lock; ; }` | 空字段非法 | `parse_fields` 跳过 lone `;` |
| `__auto_type` / typedef `(fn)(...)` | 未支持 GNU 分支 | 新增解析路径 |
| C11 atomic 预置宏 | 与内核 atomic-instrumented 冲突 | freestanding 下不预置；hosted 预置完整 `atomic_*` 宏族 |
| `_Static_assert(offsetof(...))` | const_eval 无 Offsetof | `offsetof_member()` + find_field |
| `DIV_ROUND_UP` 未展开 | object-like 别名后 `(args)` 未重扫描 | preproc 条件 tail_call 重扫描; 缓存 V5 |
| `tags[3][EXPR]` | 结构体字段只解析一维 `[...]` | while LBracket + const_eval 数组界 |
| `void __attr__ *fn()` | void 后属性残留 | `skip_attributes` 后再 `parse_ptr_suffix` |
| 语句表达式 `q * n_` | `is_decl_start` 误判 | ident 须为类型名；补 `__auto_type` |
| `_DECLARE_STATIC_KEY_0(x)` | 预处理二次宏残留 | `parse_static_key_decl()` 语义补全 |
| `ret ?: -7` | 未实现 GNU 省略三元 | 中间缺省则 then_=cond |
| `current_thread_info()` 未展开 | include 缓存零参宏误读为 `[""]` | load 过滤空 param；缓存 V6 |
| percpu 三元 `*` 解引用 E1004 | `Cond` 固定 int 类型 | 分支为指针时保留指针类型 |
| codegen panic `6f` | 内联 asm 缺 GNU 局部数字标签 | `AsmLocalLabels` N:/Nf/Nb |
| `-O2` 后 `goto found` panic | DCE 删除 `return` 后 C 标签 | `opt.rs` 保留 `Stmt::Label` |
| 同行 `1:jmp` / `found: ret` | 整行当助记符 `1:jmp` | `expand_label_insn_lines` 拆分 |
| `btl`/`shld` 等 nop 降级 | mini-asm 缺编码 | `emit_asm_bit` / `emit_asm_shld_shrd` |
| `btsq`/`bsf`/`popcnt`/`bswap`/`movnt*` nop | mini-asm 缺编码 | `emit_asm_scan`/`popcnt`/`bswap`/`movnt` |
| GAS 伪指令 `.long`/`.balign` nop | 未识别 `.` 行 | `emit_asm_directive` 静默填充 |
| `push`/`pop`/`cmov` 内存操作数 nop | 操作数模式不全 | 扩展 `emit_asm_*` mem/RIP 分支 |
| duplicate typedef（modpost 宿主头） | 多 TU 重复 `#include` 相同 typedef | `register_typedef` 允许 `types_compatible` 重定义 |
| `index`/`basename` E0007 | 局部变量与 libc 函数原型同名 | 函数体内局部声明不查 `globals` |
| `typeof(_Generic(...))` | `_Generic` 被当作类型名 | `typeof_operand_is_type_form` 排除 `_Generic` |
| 声明初始化 `sizeof(*v)` | 变量未先入 locals | `=` 前 `locals.insert` |
| asm goto `jmp %l[x] # cmt` panic | `#` 后注释被当作标签名 | `strip_asm_comment` 剥离 AT&T 注释 |
| 向量 `e = e + b` E1007 | binop 结果误标 int | parser 保留 Vector；validate 允许 Binary 赋值 |
| 向量 binop 非 Ident 操作数 | `gen_scalar_vector_load` 从 rax 误读 | `gen_vector_to_temp` 物化到 slot |

**当前 asm-offsets 状态**：解析 **0**、语义 **0**；`-O2` 不再 panic `unresolved label: found`；
mini-asm 位测试/扫描/popcnt/bswap/movnt/伪指令/lock+rep 等已编码（`tests/fixtures/asm_extended.c`）；
全树 `-O2 -c` 仍可能遇 paravirt 类型冲突等后续阻塞。验收脚本：
`scripts/test_asm_offsets.py`。

## 已实现模块（2026-07-16 扩展）

| 特性 | 说明 |
|------|------|
| jump_label 静态 key 声明 | `parse_static_key_decl()`：`_DECLARE_STATIC_KEY_0/1` 及 `IS_ENABLED` 残留 → `extern struct static_key_{false,true}` |
| Typed IR 语义校验 | ✅ `ir.rs`：`lower`/`validate`/`resync`；float/vector/complex 二元/一元/赋值/条件/聚合/转换与 codegen 对齐 |
| BTF struct 探测 | `build_btf_probe()`：int + `dcc_probe` 结构体 |
| eh_frame FDE | 每函数 PC+size 生成 FDE；ORC 节写入 PC 列表 |
| LTO 链接 + gc | `src/link/legacy.rs`：`ld -r --gc-sections` + `.llvm.lto` 注入 + dead strip |
| per-function section | `-ffunction-sections` / `-flto` 默认 `.text.<name>` |
| rela 引用标记 | `elf::referenced_symbol_names` → `lto::mark_references_from_objects` |
| CLI 多输入 | `-r a.o [b.o…] -o merged.o` 链接模式；多 `.c` 须 `-c` 后用 gcc 或 `-r` |
| 链接/LTO 模块（自研多 TU + 旧 `-r`） | `src/link/`：object 解析 / 跨 TU 解析 / 节合并 / 重定位 / PLT-GOT / ET_EXEC+ET_DYN / DBC 位码 LTO；`-r` 沿用 host `ld -r` + objcopy |
| LTO 跨函数内联 | `run_cross_tu_lto` cost model：≤4 语句 / 4 表达式 / 无递归，含同 TU 与跨 TU 调用 |
| LTO 跨 TU 常量传播 | 跨 TU `global constant` 初始化值合并 + 引用替换 |
| LTO 跨 TU 全局优化 | DCE + DAE（消除 `global` 中无副作用 store） |
| `.gnu.hash` TIE 压缩 | `link/gnu_hash.rs` 写 ELF 64-bit GNU hash（含 bloom filter + buckets + chain） |
| `.eh_frame_hdr` 搜索表 | `link/eh_frame.rs` 二分搜索表（按 initial_location 排序） |
| IFUNC 解析 | `link/ifunc.rs`：`STT_GNU_IFUNC` → `R_X86_64_IRELATIVE` 重定位生成 |
| `--exclude-libs` 隐藏静态库符号 | `LinkConfig.exclude_libs` → 被排除归档库中全局符号打 `STV_HIDDEN` |
| `--build-id=sha1/uuid/md5/none` | `link/build_id.rs`：内置 SHA-1 / MD5 / UUID v4 写 `.note.gnu.build-id` |
| `--no-undefined-version` | `LinkConfig.no_undefined_version`：缺版本 `.symver` 引用 → 错误 |
| `.symver` 完整版本表 (M12) | `link/symver.rs::VersionTable` 收集 `.symver name,alias@ver` / `@@ver`，生成 `.gnu.version` 16-bit 索引表与 `.gnu.version_d` VERDEF 结构 |
| `-z relro[=partial\|full]` (M12) | 写 `PT_GNU_RELRO` 段，覆盖 `.got` / `.got.plt` / `.init_array` / `.fini_array` / `.data.rel.ro` |
| `-z now` / `BIND_NOW` (M12) | `.dynamic` 写 `DT_FLAGS=DF_BIND_NOW` (0x8) 与 `DT_FLAGS_1=DF_1_NOW` (0x1)，关闭延迟绑定 |
| `-z lazy` (M12) | 恢复延迟绑定（默认行为） |
| `-z execstack` / `-z noexecstack` (M12) | 控制 `PT_GNU_STACK` 的 `PF_X` 标志 |
| `-z max-page-size=N` / `-z common-page-size=N` (M12) | 解析 K/M/G 后缀与 `0x` 前缀，控制 `PT_LOAD` align |
| `-z pack-relative-relocs` (M12) | 生成 `.gnu.pack.relocs` 段（SHT_GNU_RELR，内核/RTOS 友好的 RELR 压缩） |
| `-rpath` / `-rpath-link` / `-runpath` (M12) | `.dynamic` 写 `DT_RPATH` (23, deprecated) / `DT_RUNPATH` (29, glibc 推荐) |
| `-fuse-ld=<name>` (M12) | `LinkOpts.fuse_ld` 字段 + `DCC_FUSE_LD` 环境变量覆盖 |
| `-fno-semantic-interposition` / `-fsemantic-interposition` (M12) | LTO 阶段控制函数符号插入语义 |
| `--gdb-index` (M12, 占位) | 生成最小 `.gdb_index` 节（GDB 识别，内容仅占位） |
| `--compress-debug-sections=zlib\|zstd` (M12, 占位) | 当前忽略压缩，保留 CLI 兼容 |
| `SHF_GNU_RETAIN` 节标志 (M12) | 对 `.init` / `.fini` / `.init_array` / `.fini_array` / `.text.startup` 等元数据节自动打 `0x200000` |
| `gc_sections_root` 自动选根 (M12) | 接受 `-u <sym>` / `-Wl,-u,<sym>` 显式根符号，从 `entry + -u` 集合做引用图 BFS，软删除未被引用节 |
| IFUNC `target_clones` 多版本 (M12) | 解析 `__attribute__((target_clones("sse4.2,default")))`，生成 STT_GNU_IFUNC 符号 + R_X86_64_IRELATIVE 重定位，resolver 简化返回 default 变体 |
| `__dso_handle` / `.fini_array` 调度 (M12) | `__dso_handle` 指向 `.data` 起始；`FiniArrayBuilder` 收集 destructor 符号按 priority 排序生成 `.fini_array` 数据 |
| `-r` 自研路径 (M12, 简化) | `OutputKind::Relocatable` + `write_elf_relocatable` 生成 ET_REL；当前复用 ET_EXEC 写盘改 e_type |
| AArch64 重定位（22 种） | `ABS64` / `PREL64` / `CALL26` / `JUMP26` / `ADR_PREL_LO21` / `ADR_PREL_PG_HI21` / `ADD_ABS_LO12_NC` / `LDST*_ABS_LO12_NC` / `MOVW_UABS_G0..G3` |
| RISC-V 重定位（15 种） | `64` / `32` / `32_PCREL` / `CALL` / `CALL_PLT` / `PCREL_HI20` / `PCREL_LO12_I` / `PCREL_LO12_S` / `HI20` / `LO12_I` / `LO12_S` / `TPOFF64` / `TPREL_HI20` / `TPREL_LO12_I` / `TPREL_LO12_S` |
| GOT64 大条目 | `R_X86_64_GOT64` / `R_X86_64_GOTOFF64` 重定位支持 |
| TLS 重定位 | `R_X86_64_TPOFF64` / `R_X86_64_TPOFF32` 重定位支持 |
| 验收脚本 | prepare0 / pahole / objtool / modpost / vmlinux / kernel_full_check |
| TLS codegen | `_Thread_local` → `.tdata`/`.tbss`、`STT_TLS`；ET_EXEC 经 `arch_prctl` 设 FS |
| ET_EXEC symtab | `build_elf_exec_x86_64` 输出 `.symtab`/`.strtab` |
| format 属性 | `format_check.rs`：`-Wformat` / `W0003` arity 检查 |
| 严格类型 / permissive | 默认未知 typedef → `E0009`；`-fpermissive` → `W0002`+int |
| K&R 函数 | 解析 + `is_old_style` 标志 + codegen 测试 |
| float 取模 (M13.1) | 软件 fmod = l - trunc(l/r) * r，divss/divsd + cvttsX2si + cvtsi2sX + mul + sub，零 libc 依赖；`ir.rs::FLOAT_BINOPS` 含 `Mod` |
| int128 `&&`/`\|\|` (M13.1) | `or rdx, dst` 合并高 64 位后再 test，避免「高 1 低 0」误判 0 |
| asm `x` 约束 XMM (M13.1) | 独立 `input_xmm_set` + `prepare_asm_xmm_binding` 输入加载 + `writeback_asm_operand` 按 `xmm_width` 写回 xmm8–15；early-clobber 在 `xmm_pool` 内避让 |
| 全局数组 RIP+SIB (M13.1) | `emit_load_global_indexed`：`[rip+disp32+idx*scale]` 单指令加载 int/float/double |
| mini-asm SIMD 扩展 (M13/M13.2) | VEX/EVEX 前缀 + AVX/AVX2/AVX-512 + SSE3–4.2 + AES-NI/SHA + FMA/F16C + TSX + AMX + AVX-VNNI；`pinsr*`/`pextr*` GPR 操作数 + `tmm`/`ymm`/`zmm`/`k` 寄存器名；`asm_avx_aes_fma_tsx.c` 集成测试 PASS |

## Sanitizer 插桩（AST 层全部通道已交付）

| 通道 | 能力 | 已知遗留 |
|------|------|------|
| KASAN / HWASAN | TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 `Deref/Index/ArrowMember` 与对应 `Assign` LHS 之前插入 | 16B 以上 / 非整型 load/store 走 `__asan_load`；MTE 标签未实现 |
| UBSAN | `Div/Mod` / `Shl/Shr` / 有符号 `Add/Sub/Mul` 全部前置 `__ubsan_handle_*`；签名整数 overflow 在常量折叠时跳过 | VLA bound、null deref、float cast 单独 trap 仍缺 |
| KCSAN | TU init + 每函数 `__kcsan_func_entry`；内存访问 watchpoint 与 KASAN 共享 load/store 通道 | 专用 `__kcsan_check_access` 路径未独立发出 |
| KCOV | 每函数 `__dcc_cov_guards_N[16]` 计数器 + 入口 `__sanitizer_cov_trace_pc` + 边覆盖 `__sanitizer_cov_trace_pc_guard`（if / while / do / for / case / label / goto）| 跨函数 inline 合并未做 |
| Fuzzer | KCOV 全部 + 比较 `EqEq/Ne/Lt/Gt/Le/Ge/AndAnd/OrOr` 前 `__sanitizer_cov_trace_cmp1/2/4/8` + switch dispatch 前 `__sanitizer_cov_trace_switch` | libFuzzer 协议 / 拦截器接入 |
| no_sanitize | `address` / `kernel-address` / `hwaddress` / `undefined` / `thread` / `kernel-kcsan` / `all` 按通道关闭；KCOV / Fuzzer 不受 `no_sanitize` 影响 |  |

实现：`src/sanitizer.rs` `instrument_program()`，所有通道在 driver parse 之后、
IR lower 之前通过 `Expr::Comma` 与 `Stmt::Block` 注入 side-effect 调用，
不需要修改 IR / codegen。`.dcc.san_info` 节版本 v2，载荷为
`DCC_SAN\0 + v2 + kasan/hwasan/ubsan/kcsan/kcov/fuzzer` 各 1 字节。
回归：`src/sanitizer.rs` 12 个单元测试（sanitizer 节 + 汇编桩计数）。

## 性能分析子系统（M20 已完成）

| 通道 | 能力 | 已知遗留 |
|------|------|----------|
| PGO instrument（`-fprofile-generate` / `-fprofile-use`） | `ProfileConfig.pgo_generate` / `pgo_use` 开关齐；`Profraw` 二进制 roundtrip（PROF magic + version 1 + sites + u64 计数器）；`pgo::instrument_program` AST 改写 FuncDecl 顶部插入 `__dcc_pgo_count_fn(idx)`；`PgoWeights` 聚合 func 入口计数 | codegen 阶段尚未自动 emit 计数器；下游权重传播 stub |
| GCOV（`-ftest-coverage` / `-fprofile-arcs`） | `.gcno` notes + `.gcda` data 二进制（与 GCC gcov-9 layout 对齐）；`GcovUnit` / `GcovFunction` / `GcovBlock` 数据结构；`write_notes` / `write_data_with_counters` 写盘入口 | cfg_checksum 简化公式；非 x86_64 arch 未验证 |
| AutoFDO（`-fauto-profile=` / `-fprofile-sample-use=`） | 简化 text parser（`func <name> <total>` / `line <name> <line> <col> <samples>`）；`bucketize` 按 >10% / >1% 阈值分 Hot/Warm/Cold | 权重不下发到 codegen |
| LBR（`-flbr-profile=`） | 解析 `perf script -F brstack` 文本；`hot_entries` 按 `unknown -> func` 入度排序 | 权重不下发到 codegen |
| MemProf（`-fmemory-profile` / `-fmemory-profile-use=`） | 简化 parser（`alloc` / `lifetime` 行式）；`is_cold(stack, threshold)` 启发 | 未在 libc malloc 上插桩；无 runtime 库 |
| Perf 事件 / TopdownL1（`-ftopdownmetrics`） | `TopdownL1` 枚举（Retiring / BadSpeculation / FrontendBound / BackendBound）；`PerfRecord` 容器；`topdown_percentages()` 把四 slot 规整为百分比（sum = 1.0 校验） | 未读 perf.data 二进制 |
| Offloading（`-fopenmp` / `-fcuda` / `-fhip` / `-fopenmp-targets=`） | `OffloadConfig` / `OffloadSection` / `OffloadEntry` / `OffloadKind` 数据结构；`to_json()` 序列化 | 无 device codegen 后端；runtime loader 未实装 |

CLI 矩阵：
`-fprofile-generate[=dir]` / `-fprofile-instr-generate[=dir]` /
`-fprofile-use=file` / `-fprofile-instr-use=file` /
`-fauto-profile=file` / `-fprofile-sample-use=file` /
`-flbr-profile=file` / `-ftest-coverage` / `-fprofile-arcs` /
`-ftopdownmetrics` / `-frecord-profile=dir` /
`-fmemory-profile` / `-fmemory-profile-use=file` / `-fmemprof` /
`-fopenmp` / `-fopenmp=simd` / `-fcuda` / `-fhip` /
`-fopenmp-targets=triple,...` / `--offload-arch=triple` /
`-fbranch-probabilities` / `-fprofile-values` / `-fprofile-list[=file]`。

回归：`src/profile/{mod,gcov,pgo,autofdo,lbr,memprof,perf_event,offload}.rs`
10 个模块单元测试 + `tests/profile.rs` 11 个集成测试
（CLI 接受 / 缺路径拒绝 / 编译出 .o）。
详见 `docs/profiling.md`。

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-6.6`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
