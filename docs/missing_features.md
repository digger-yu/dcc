# DCC 缺失特性与已知限制

> 最后更新：2026-08-07 — 基于 `cargo test` 287 integration tests + 554 unit tests 全部通过
> 
> 已支持特性详见 `docs/support_features.md`

## 阻塞级（Linux 6.6 全树）

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
| TLS 重定位 (`R_AARCH64_TLSDESC_*`) | 仅基础 `tpoff` 模型 |

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
| KASAN / HWASAN | 16B 以上 / 非整型 load/store 走 `__asan_load`；MTE 标签插入未完全集成到 sanitizer 流程（AArch64 指令已实现） |
| UBSAN | VLA bound、null deref、float cast 单独 trap 仍缺 |
| KCSAN | 专用 `__kcsan_check_access` 路径未独立发出 |

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

| 限制 | 说明 |
|------|------|
| 动态 RTTI | 跨多态指针动态 RTTI 仍依赖 C 后端在 vtable 中写 typeinfo 字段，多文件链接场景受限 |
| 继承成员访问 | C++ 多文件链接场景受 dcc C 后端 `__base_Base1` 字段访问 pre-existing 限制 |
| `__dcc_mbr` 辅助定义 | 继承成员访问改写需要 C 后端支持 `__base_*` 字段访问 |

## ABI 限制

| 限制 | 说明 |
|------|------|
| 浮点变参 | v0–7 / fa0–fa7 未保存 |
| 栈溢出变参 | >8 个未实装 |

## 链接器限制

| 限制 | 说明 |
|------|------|
| `ptr_to_member` 多文件链接 | vtable 符号多文件链接场景重复定义 |
| Windows 链接路径验证 | Windows 平台链接路径未实际验证 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-6.6`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
