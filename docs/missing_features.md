# DCC 缺失特性与已知限制

> 最后更新：2026-08-16 — 基于源码、测试与脚本的只读审计
>
> 已支持特性详见 `docs/support_features.md`。本文件只记录仍不支持、
> 明确降级、仍为 stub/no-op，或缺少测试契约的项目。

## Linux 7.1 全树编译状态

| 指标 | 状态 |
|------|------|
| dcc 编译器错误 | ✅ 零错误 (E0001/E0102/E0300/E0301 已清零) |
| 成功产出 .o 文件 | 133+ 个 (defconfig x86_64) |
| 剩余阻塞 | objtool 兼容性与严格内核 E2E 门禁 |

### objtool 兼容性（核心阻塞）

| 问题 | 说明 |
|------|------|
| 指令解码失败 | objtool 无法解码 dcc 生成的部分指令序列 (`can't decode instruction`) |
| 函数起始定位失败 | `_context_unsafe_alias()` 等空函数/内联函数 objtool 无法定位 (`can't find starting instruction`) |
| 跳转目标缺失 | 个别函数内跳转目标地址 objtool 无法解析 (`can't find jump dest instruction`) |

根因：dcc 的 x86 编码、函数边界和跳转目标信息仍未与内核
objtool 的解码/验证模型完全一致。

## C 前端 / IR 语义限制

| 限制 | 说明 |
|------|------|
| complex/vector cast 边界 | 支持同尺寸 vector/scalar、vector/vector、complex/vector bitcast；不同尺寸或未枚举转换仍由 validator 拒绝 |
| unknown `__builtin_*` explicit fallback | 未识别 builtin 明确降级：有返回值时返回 0，`void` 返回时发 `ud2` 并 warning；表达式/语句上下文均有测试契约 |

## 后端 codegen 限制

### x86_64

| 限制 | 说明 |
|------|------|
| mini-asm 未知指令 | 未识别 mnemonic 或已识别指令的不合法操作数仍 warning 后发 `nop`，不是 hard error |
| objtool consumer compatibility | 部分可执行对象仍无法通过 objtool 解码、函数边界和 ret 模式验证 |
| sanitizer runtime 依赖 | ASAN/UBSAN/KCSAN 等主要只负责插桩；运行时库与实际 report 行为不由 dcc 完整提供 |

### AArch64 / RISC-V / MIPS / ARM

| 限制 | 说明 |
|------|------|
| 浮点函数调用 | AArch64/RISC-V 的浮点表达式 codegen 遇到 `ExprKind::Call` 仍 warning 并返回 `0.0` |
| 非 x86 inline asm | 非 x86 后端可在 `-S` 保留 asm 文本和简化占位符替换；`-c` 路径没有完整 mini-assembler 机器码语义 |
| AArch64 vector 表达式 | 向量 codegen 仍主要覆盖二元/常量路径，其他形式可能落到零值 fallback |
| 多架构运行验证 | aarch64/riscv64/mips/mips64/arm 多数测试仍是 compile/ELF 产物验证，缺 QEMU runtime 语义门禁 |

## 预处理 / CLI / 内核 wrapper 限制

| 限制 | 说明 |
|------|------|
| `#pragma STDC FP_CONTRACT` / `CX_LIMITED_RANGE` | 当前仅接受为 no-op，未影响浮点收缩或 complex range 语义 |
| PCH 降级 | 宏状态 hash 不一致、写入失败或有预处理错误时 warning 后回退，不作为 hard failure |
| `-Wp,*` 子选项 | 仅白名单支持；未知预处理器透传选项会报 unsupported |
| `-x` 语言集 | 仅支持 assembler/assembler-with-cpp/C/C++ 等少数输入语言 |
| unknown sanitizer name | 未知 `-fsanitize=` 名称未形成严格诊断契约，存在被忽略的路径 |
| 内核 wrapper 过滤 flags | `arch_x86_compile.sh` 仍过滤 dcc 未实现的内核 flags，如 `-fstack-clash-protection`、`-ftrivial-auto-var-init`、`-mrecord-mcount`、`-mfentry`、`-pg` 等 |
| host wrapper stub depfile | `dcc-host-wrapper.sh` 在 fixdep 依赖生成失败时会写 stub `.d`，可能隐藏 host C gap |

## 链接器 / 对象文件限制

| 限制 | 说明 |
|------|------|
| 输入对象架构 | object reader 只接受 ELF64 且 machine 为 x86_64/AArch64/RISC-V；MIPS/ARM 等对象链接路径仍不完整 |
| relocation 覆盖 | 未枚举 relocation type 仍返回 `UnsupportedType` |
| `-z` 选项 | 部分 `-z` 选项仅静默接受或忽略；未知 `-z` 未形成严格错误 |
| LTO/offload/profile 格式版本 | `.llvm.lto`、`.dcc.offload`、profraw/profdata/autofdo 等只支持当前固定版本 |
| Windows 链接路径 | Windows 平台链接路径未实际验证 |
| dynamic loader | 生成 ELF 依赖 host `ld.so`（设计决策）；dcc 自身可 musl 静态链接 |

## 性能分析 / Offload 限制

| 通道 | 已知遗留 |
|------|----------|
| `-fprofile-values` | CLI 接受但当前为 no-op，缺块级 value sampling |
| GCOV runtime | `.gcno/.gcda` 读写与报告结构存在，但运行后真实 counter 更新的端到端路径仍未形成硬门禁 |
| MemProf runtime | `MemProfRuntime` 仍生成 C stub；alloc/free/flush 中 callstack 记录与 `.memprofdata` 写出为 TODO |
| Offload device codegen | CUDA/HIP/OpenMP target 设备端 PTX/AMDGCN 仍是 kernel stub (`ret` / `s_endpgm`) |
| Offload runtime dispatch | `OffloadRuntimeLoader::check_runtime` 总是 `NotFound`，kernel launch 返回 `RuntimeNotAvailable`，未真正调 CUDA/ROCm driver |
| unsupported device triple | 未知 device triple warning 后跳过 device codegen |

## C++ 前端 / GCC bootstrap 限制

| 限制 | 说明 |
|------|------|
| GCC 完整 bootstrap | cc1/cc1plus 未完成真实 bootstrap 验证；当前只覆盖可选 GCC source 子集测试 |
| C++20 modules | `import` 仍等价 include；无 BMIP 二进制缓存、真实跨 TU module import、private fragment 或 partitions |
| C++ ODR 集成 | `OdrChecker` 存在，但不等于完整 module/link pipeline ODR enforcement |
| C++ enum lowering | enum 仍降级为 `typedef int`，不保留完整枚举类型语义 |
| `auto` / `decltype(auto)` | 类型推导对调用、成员调用和复杂表达式仍保守降级为 int/Unknown |
| class member parser | 部分类成员、引用成员、operator name 和表达式形式仍返回 `Unsupported` |
| coroutine 语义 | parser/lower 存在状态机路径，但仍有占位和语义覆盖缺口 |
| virtual inheritance | 菱形/虚基复杂布局仍以 warning/注释降级，不是完整 Itanium ABI 虚继承布局 |
| `noexcept(expr)` | 当前直接降级为常量 true，不分析表达式抛异常可能性 |
| template depth | 默认深度 128，低于 GCC/Clang 常见上限 |

## 交叉编译 / 缺失平台

| 缺失平台 | 说明 |
|----------|------|
| Windows MSVC | 无 COFF/MSVC ABI codegen/link 验证 |
| Darwin / Mach-O | 无 Mach-O 对象与链接支持 |
| WASM | 无 wasm32/wasm64 codegen |
| AVR / MSP430 / Xtensa | 无 embedded backend |
| PowerPC / LoongArch64 | 无对应 codegen/link 路径 |

## 自举

| 步骤 | 状态 |
|------|------|
| 步骤 2 (`dcc0 → dcc1`) | 未开始，需 dcc0 完整支持自身依赖（std 子集） |
| 步骤 3 (`dcc1 → dcc2`) | 未开始 |

## 缺失测试用例

### Linux 内核全树 / CI 门禁

| 缺口 | 建议测试 |
|------|----------|
| strict full build | 增加显式 strict 模式，要求 `vmlinux`、`bzImage`、modpost、pahole、objtool 全部失败即失败，不允许 SKIP |
| QEMU boot | 在产物存在时把 `qemu_boot_matrix.sh` 接入硬门禁，并断言 boot log 关键字符串 |
| kernel object corpus | 固化若干真实内核 `.o` 样本，覆盖 `arch/x86/kernel`、`kernel/sched`、`lib/` |
| stale CI paths | 校验 CI/examples 使用当前存在的 examples 路径，避免引用历史根目录 fixture |

### objtool / ORC / x86 mini-asm

| 缺口 | 建议测试 |
|------|----------|
| objtool known failures | 为 `can't decode instruction`、`can't find starting instruction`、`can't find jump dest instruction`、rethunk naked return 增加最小 xfail fixture |
| byte-level asm | 对 Linux legacy mini-asm 指令增加反汇编/字节级断言，而不只检查 “未 emitted as nop” |
| ORC consumer | 增加 objtool strict 单对象测试，验证 ORC/函数边界可被 objtool 消费 |

### Sanitizer

| 缺口 | 建议测试 |
|------|----------|
| runtime negative tests | 增加 KASAN 越界、UBSAN div/mod/shift/VLA/null/float-cast、KCSAN 竞态的运行时/链接后测试 |
| HWASAN/MTE | 增加 AArch64 `irg/addg/stg` 汇编断言和软件 tag 路径测试 |
| KCOV/Fuzzer | 覆盖 `trace_cmp*`、`trace_switch`、8-bit counters、PC table、`no_sanitize` 与 KCOV/Fuzzer 交互 |
| unknown sanitizer | 增加未知 sanitizer 名称的负向诊断测试 |

### Profile / GCOV / Offload

| 缺口 | 建议测试 |
|------|----------|
| PGO E2E | compile → run → profraw → merge/profdata → profile-use，并验证优化或 section/layout 可观测变化 |
| GCOV E2E | 运行程序后校验 `.gcda` counter 非零与报告行覆盖率 |
| AutoFDO/LBR/MemProf/Topdown | 增加文本+二进制 profile 输入的 CLI 集成与权重影响测试 |
| Offload driver/link | 覆盖 nvcc/clang++ 缺失、runtime 查找、host link flags/rpath、device-link 命令与错误路径 |
| Offload runtime | 增加 expected-fail 测试，锁定当前 `RuntimeNotAvailable` 行为直到真正实现 |

### 预处理 / PCH / 依赖文件

| 缺口 | 建议测试 |
|------|----------|
| Linux header graph | 用 `compiler_types.h`、`rwonce.h`、generated `autoconf.h` 等真实内核头做预处理 E2E |
| PCH negative | 覆盖 hash mismatch、版本不兼容、损坏 PCH、写入失败的 snapshot |
| depfile/fixdep | 增加空格、反斜杠、转义路径、`-MP` phony 与 fixdep 反解析联动测试 |
| `#error/#warning` | 增加多格式诊断 snapshot 和宏展开后消息测试 |

### Builtin / 属性 / C 前端

| 缺口 | 建议测试 |
|------|----------|
| Linux FORTIFY | 覆盖真实 `__builtin___memcpy_chk` 等 FORTIFY 宏族展开与链接行为 |
| low-level builtins | 对 `trap`、`debugtrap`、`unreachable`、`assume_aligned`、`alloca`、`static_chain`、`frame_address`、`return_address` 增加汇编或运行时断言 |
| attributes | 对 alias/ifunc/retain/used/error/warning/deprecated/constructor priority 增加负向与链接后符号测试 |

### 多架构

| 缺口 | 建议测试 |
|------|----------|
| QEMU runtime matrix | aarch64/riscv64/arm/mips/mips64 至少覆盖 hello、control-flow、aggregate ABI、va_arg |
| aggregate/HFA ABI | 增加 AArch64 HFA 1–8 元素、>8 fallback，以及 RISC-V HFA 1–4、>4 fallback 测试 |
| non-x86 inline asm | 增加 `-S` 文本保留测试与 `-c` expected-fail/diagnostic 测试 |
| unsupported targets | 对 Windows MSVC、Darwin、WASM、AVR、MSP430、Xtensa、PowerPC、LoongArch64 增加稳定负向测试 |

### C++ / GCC bootstrap

| 缺口 | 建议测试 |
|------|----------|
| semantic run tests | SFINAE 失败路径、partial specialization 优先级、concept reject、dynamic_cast 虚继承负向应 compile+link+run |
| modules | BMF 导入、真实跨 TU import、ODR 冲突、private fragment/partition expected-fail |
| coroutine | 覆盖 `co_await`/`co_yield`/`co_return` 状态机运行语义和错误路径 |
| GCC bootstrap | 增加可复现的 cc1/cc1plus configure/build xfail 或 ignore 门禁脚本 |

### Linker / ELF / debug info

| 缺口 | 建议测试 |
|------|----------|
| GNU hash / EH frame hdr | 校验 bloom/bucket/chain 与 `.eh_frame_hdr` 搜索表内容，不只看 section 存在 |
| IFUNC / symver / RELRO | 增加 resolver 运行、`--exclude-libs` 可见性、build-id、symver、RELRO/NOW/LAZY/RPATH/RUNPATH 测试 |
| debug consumers | 用 `addr2line`、`readelf --debug-dump`、`pahole` strict 模式验证 DWARF/BTF/bitfield/split-dwarf/gpubnames |
| compressed debug | 校验 zlib/zstd 压缩节可被工具解压和消费 |

### 优化 / 诊断契约

| 缺口 | 建议测试 |
|------|----------|
| kernel-sensitive opt | 增加 volatile、barrier、inline asm memory clobber 下的 O2/O3 误优化回归 |
| SSA pass coverage | 对 alias、SCCP、TCO、DAE、IPA-SRA、PGO weights 增加可观测正/负测试 |
| xfail harness | 为本文件每个未支持项建立 expected-fail，校验错误码/诊断文本 |
| skipped diagnostics | 将 `tests/errors.rs` 中 skip 的 known compiler issue 改为显式 known-fail 契约 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物默认 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
