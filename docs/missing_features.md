# DCC 缺失特性与已知限制

> 最后更新：2026-08-24 — 基于当前源码、测试与脚本审计
>
> 已支持特性详见 `docs/support_features.md`。本文件只记录仍不支持、
> 明确降级、仍为 stub/no-op，或缺少测试契约的项目。

## 后端 codegen 限制

### AArch64 / RISC-V / MIPS / ARM

| 限制 | 说明 |
|------|------|
| 非 x86 `-c` 路径 mini-assembler | `-S` 路径已完整支持（详见 `support_features.md` 非 x86_64 后端 inline asm 条目）；`-c` 路径无完整 mini-assembler 机器码编码，inline asm 文本不生成目标机器码，仅发出警告 |

## 链接器 / 对象文件限制

| 限制 | 说明 |
|------|------|
| Windows COFF 链接路径 | Windows MSVC triple 可解析并生成 ELF64 目标文件，object reader 可接受；COFF 格式 codegen/link 与原生 Windows 链接尚未实现 |

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

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（默认保留缺产物 SKIP；
  `STRICT_FULL_BUILD=1` 要求 vmlinux / bzImage / modpost / pahole / objtool
  全部硬通过）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
