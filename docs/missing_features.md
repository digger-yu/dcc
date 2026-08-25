# DCC 缺失特性与已知限制

> 最后更新：2026-08-25 — 基于当前源码、测试与脚本审计
>
> 已支持特性详见 `docs/support_features.md`。本文件只记录仍不支持、
> 明确降级、仍为 stub/no-op，或缺少测试契约的项目。

## 后端 codegen 限制

> 非 x86 后端 mini-assembler 已支持常见指令 (-c 路径编码机器码), 详见 `support_features.md`

## 链接器 / 对象文件限制

| 限制 | 说明 |
|------|------|
| COFF 动态链接 | COFF 目标文件可生成、解析并链接为 PE 可执行文件；但不支持 DLL 导入表、动态链接等高级 PE 特性 |

## 交叉编译 / 缺失平台

| 缺失平台 | 说明 |
|----------|------|
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
