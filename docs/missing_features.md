# DCC 缺失特性与已知限制

> 最后更新：2026-08-22 — 基于源码、测试与脚本的只读审计
>
> 已支持特性详见 `docs/support_features.md`。本文件只记录仍不支持、
> 明确降级、仍为 stub/no-op，或缺少测试契约的项目。

## 后端 codegen 限制

### AArch64 / RISC-V / MIPS / ARM

| 限制 | 说明 |
|------|------|
| 非 x86 inline asm | 非 x86 后端可在 `-S` 保留 asm 文本和简化占位符替换；`-c` 路径没有完整 mini-assembler 机器码语义 |

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
| `-fprofile-values` | 完整运行时 value histogram 仍需外部 runtime 支持 |
| GCOV runtime | `.gcno/.gcda` 读写与报告结构存在，但运行后真实 counter 更新的端到端路径仍未形成硬门禁 |
| MemProf runtime | `MemProfRuntime` 仍生成 C stub；alloc/free/flush 中 callstack 记录与 `.memprofdata` 写出为 TODO |
| Offload device codegen | CUDA/HIP/OpenMP target 设备端 PTX/AMDGCN 仍是 kernel stub (`ret` / `s_endpgm`) |
| Offload runtime dispatch | `OffloadRuntimeLoader::check_runtime` 总是 `NotFound`，kernel launch 返回 `RuntimeNotAvailable`，未真正调 CUDA/ROCm driver |
| unsupported device triple | 未知 device triple warning 后跳过 device codegen |

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
| QEMU boot | 在产物存在时把 `qemu_boot_matrix.sh` 接入硬门禁，并断言 boot log 关键字符串 |
| kernel object corpus | 固化若干真实内核 `.o` 样本，覆盖 `arch/x86/kernel`、`kernel/sched`、`lib/` |
| stale CI paths | 校验 CI/examples 使用当前存在的 examples 路径，避免引用历史根目录 fixture |

### x86 mini-asm

| 缺口 | 建议测试 |
|------|----------|
| byte-level asm | 对 Linux legacy mini-asm 指令增加反汇编/字节级断言，而不只检查 “未 emitted as nop” |

### Sanitizer

| 缺口 | 建议测试 |
|------|----------|
| runtime negative tests | 增加 KASAN 越界、UBSAN div/mod/shift/VLA/null/float-cast、KCSAN 竞态的运行时/链接后测试 |
| HWASAN/MTE | 增加 AArch64 `irg/addg/stg` 汇编断言和软件 tag 路径测试 |
| KCOV/Fuzzer | 覆盖 `trace_cmp*`、`trace_switch`、8-bit counters、PC table、`no_sanitize` 与 KCOV/Fuzzer 交互 |
| unknown sanitizer | 增加未知 sanitizer 名称的负向诊断测试 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（默认保留缺产物 SKIP；
  `STRICT_FULL_BUILD=1` 要求 vmlinux / bzImage / modpost / pahole / objtool
  全部硬通过）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
