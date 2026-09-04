# DCC 缺失特性与已知限制

> 最后更新：2026-09-02 — 基于当前源码、测试与脚本审计
>
> 已支持特性详见 `docs/support_features.md`。本文件只记录仍不支持、
> 明确降级、仍为 stub/no-op，或缺少测试契约的项目。

## 后端 codegen 限制

> 非 x86 后端 mini-assembler 已支持常见指令 (-c 路径编码机器码), 详见 `support_features.md`

| 限制 | 说明 |
|------|------|
| `-falign-jumps=N` / `-falign-loops=N` (N>1) | DCC 不执行代码段对齐；`=1` 等价于不对齐，静默接受；其他值发出警告 |
| `-mpreferred-stack-boundary=N` (N≠2) | DCC 不调整栈对齐；`=2` 为内核默认，静默接受；其他值发出警告 |
| `-fno-plt` | 接受但无操作；内核不使用 PLT，DCC codegen 不生成 PLT 入口 |
| `-ftrampolines` / `-fno-trampolines` | 接受但无操作；嵌套函数直接调用通过 `__builtin_call_with_static_chain` 支持；嵌套函数取地址时生成栈上 trampoline (设置 r10=父帧指针后跳转)，用户需确保栈可执行 (mprotect PROT_EXEC) 或通过 `-z execstack` 链接 |
| 优化器 `-fno-strict-aliasing` | 已实现：`-fno-strict-aliasing` 时别名分析合并所有指针到同一 alias set (保守)；默认 `-fstrict-aliasing` 按类型标签区分 alias set (C 标准语义) |


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
| 步骤 1: 自举基础设施 | ✅ 完成 — `src/bootstrap.rs` 三层对比 (byte/struct/semantic)，`--bootstrap-compare=<ref.o>` CLI |
| 步骤 2: `dcc0 → dcc1` 编译验证 | ✅ 完成 — `bootstrap_selftest.c` (14 组测试) + `scripts/bootstrap.sh` 4 阶段流程 |
| 步骤 3: `dcc1 → dcc2` 确定性验证 | ✅ 完成 — 字节级 + 语义级对比通过；多文件编译一致性验证通过 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`（QEMU 启动需启用
  `CONFIG_SERIAL_8250_CONSOLE`）
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`
  （由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（默认保留缺产物 SKIP；
  `STRICT_FULL_BUILD=1` 要求 vmlinux / bzImage / modpost / pahole /
  objtool / QEMU boot 全部硬通过）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` +
  bzImage；2026-08-27 验证通过，串口启动日志正常）
