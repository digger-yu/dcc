# DCC 缺失特性与已知限制

> 最后更新：2026-08-07 — 基于 `cargo test` 287 integration tests + 554 unit tests 全部通过
> 
> 已支持特性详见 `docs/support_features.md`

## 阻塞级（Linux 7.1 全树）

| 特性 | 当前状态 | 说明 |
|------|----------|------|
| 全树 `make CC=dcc` | ⏳ 待本地下一步 | 推荐 `HOSTCC=gcc` 混合；Rust fixdep 已替代 C fixdep |
| QEMU 启动矩阵 | ⚠️ 脚本就绪 | 无 bzImage 时 SKIP；需完整内核构建产物 |
| vmlinux pahole / objtool | ⚠️ 脚本就绪 | 无 vmlinux 时 SKIP；单 TU 回归已通过 |

## GCC bootstrap

| 任务 | 状态 | 说明 |
|------|------|------|
| GCC 完整 bootstrap (cc1/cc1plus) | ⏳ 部分受阻 | 本地磁盘已具备（GCC 源 ~1G + 构建 ~5G）；SFINAE / partial specialization / 完整 concepts / 协程 / module 均已实装；GCC 兼容运行时已实装；待 GCC 源树实际构建验证 |

## 后端 codegen 遗留

### AArch64

| 模块 | 现状 |
|------|------|
| NEON 浮点聚合传参 > 4 元素 | 仅支持 1–4 元素 HFA |
| 大结构体 > 16B 嵌套返回 | 仅单层 sret，未验证多层嵌套调用链 |

### RISC-V64

| 模块 | 现状 |
|------|------|
| RVV 完整指令集 | 仅基础 VSETVLI/LOAD/STORE/ADD/MUL/REDUCE，缺宽化/窄化/掩码/浮点向量 |
| 压缩指令自动选择 | 手动发射，未实现自动 4→2 字节压缩优化 |

### x86_64 mini-asm

| 类别 | 缺失模式 | 状态 |
|------|----------|------|
| `bswap` | 16 位操作数 | 🟡 不支持（gcc 同，x86_64 架构限制） |

## Sanitizer 遗留

| 通道 | 已知遗留 |
|------|----------|
| HWASAN MTE | AArch64 MTE 指令编码已实现，但未集成到 sanitizer 流程（需函数 prologue tag 栈帧、指针标记、软件 tag 检查） |

## 性能分析子系统遗留

| 通道 | 已知遗留 |
|------|----------|
| PGO | 下游权重传播 stub |
| GCOV | 非 x86_64 arch 未验证 |
| AutoFDO | 权重不下发到 codegen |
| LBR | 权重不下发到 codegen |
| MemProf | 未在 libc malloc 上插桩；无 runtime 库 |
| Perf 事件 | 未读 perf.data 二进制 |
| Offloading | runtime loader 未实装 |

## C++ 前端限制

无已知限制。

## 链接器限制

| 限制 | 说明 |
|------|------|
| Windows 链接路径验证 | Windows 平台链接路径未实际验证 |
| DSO symbol caching | `DsoCache` 已实现，链接器侧未接入（需先补 `-lxxx → libc.so` 解析） |
| 运行时 dynamic loader | 完全依赖 host `ld.so`（设计决策，非缺失） |

## 交叉编译

| 限制 | 说明 |
|------|------|
| 缺失平台 | Windows MSVC / Darwin (Mach-O) / WASM / AVR / MSP430 / Xtensa / PowerPC / LoongArch64 无 codegen |

## 自举

| 步骤 | 状态 |
|------|------|
| 步骤 2 (`dcc0 → dcc1`) | 未开始，需 dcc0 完整支持自身依赖（std 子集） |
| 步骤 3 (`dcc1 → dcc2`) | 未开始 |

## 运行时限制

| 限制 | 说明 |
|------|------|
| dynamic loader | 完全依赖 host `ld.so`（设计决策） |
| fenv 运行时调用 | `fesetround` / `fegetround` 等未提供，`FENV_ACCESS` 仍为纯语义状态 |
| `double complex` 传值 ABI | `_Complex` 字面量/算术已实装，`double complex` 按值传 ABI 待后续 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
