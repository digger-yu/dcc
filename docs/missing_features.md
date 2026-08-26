# DCC 缺失特性与已知限制

> 最后更新：2026-08-27 — 基于当前源码、测试与脚本审计
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
| 优化器 `-fno-strict-aliasing` / `-fno-strict-overflow` | 当前优化器已保守处理（不做 TBAA、不假设有符号溢出 UB），但未显式检查 target 标志；未来添加激进优化时需实现标志检查 |
| `__seg_fs` / `__seg_gs` 段寄存器地址空间限定符 | 解析器接受（在 decl-specifier / 类型与 `*` 之间 / `*` 与标识符之间 / cast / typedef / typeof / struct 字段 / 函数指针声明符 / 函数指针返回类型 / 函数指针字段声明符 / sizeof / _Alignof 等所有合法位置跳过这些关键字，见 `support_features.md`），但不存储在类型系统中，也不在汇编输出生成 `%fs:` / `%gs:` 段前缀。因此 Linux 内核 per-cpu 变量访问会落到默认数据段，运行时读到错误的地址。完整段寄存器 codegen 需要：(1) 在 `Type`/`Pointer` 中携带 address space 标识；(2) `x86_64.rs` / `aarch64.rs` 等后端在加载/存储指令中插入段前缀或选择 `gs:`/`fs:` 寻址模式。当前实现只满足让内核 per-cpu 声明可编译，运行时会出错 |


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
- 内核树：`~/linux-7.1`，需已有 `.config`（QEMU 启动需启用
  `CONFIG_SERIAL_8250_CONSOLE`）
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`
  （由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（默认保留缺产物 SKIP；
  `STRICT_FULL_BUILD=1` 要求 vmlinux / bzImage / modpost / pahole /
  objtool / QEMU boot 全部硬通过）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` +
  bzImage；2026-08-27 验证通过，串口启动日志正常）
