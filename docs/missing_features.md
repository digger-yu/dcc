# DCC 缺失特性与已知限制

> 最后更新：2026-08-16 — 基于 Linux 7.1 全树编译 + `cargo test` 全量通过
>
> 已支持特性详见 `docs/support_features.md`

## Linux 7.1 全树编译状态

| 指标 | 状态 |
|------|------|
| dcc 编译器错误 | ✅ 零错误 (E0001/E0102/E0300/E0301 全部清零) |
| 成功产出 .o 文件 | 133+ 个 (defconfig x86_64) |
| 剩余阻塞 | objtool 兼容性 (33 个 .o 因 objtool 解码失败被删除) |

### objtool 兼容性（核心阻塞）

| 问题 | 说明 |
|------|------|
| 指令解码失败 | objtool 无法解码 dcc 生成的部分指令序列 (`can't decode instruction`) |
| 函数起始定位失败 | `_context_unsafe_alias()` 等空函数/内联函数 objtool 无法定位 (`can't find starting instruction`) |
| 跳转目标缺失 | 个别函数内跳转目标地址 objtool 无法解析 (`can't find jump dest instruction`) |
| MITIGATION_RETHUNK | objtool 检测到非标准 ret 模式 (`'naked' return found in MITIGATION_RETHUNK build`) |

根因：dcc 的指令编码与 GCC/LLVM 不完全一致，objtool 的 x86 解码器无法识别某些编码模式。

## 后端 codegen 遗留

### x86_64 mini-asm 缺失指令

| 缺失指令 | 内核使用场景 | 说明 |
|----------|------------|------|
| `cmpxchg8b` | 32 位原子操作 (`asm/cmpxchg_32.h`) | 仅支持 `cmpxchg` 和 `cmpxchg16b` |
| `pushfl` / `popfl` | 标志寄存器操作 (32 位后缀形式) | 仅支持 `pushf`/`pushfq`/`popf`/`popfq` |
| `iret` / `iretl` | 中断返回 (无 q 后缀) | 仅支持 `iretq` |
| `cmovb`/`cmovae`/`cmovbe`/`cmovs`/`cmovo`/`cmovp` 等 | 条件传送 (约 10 种条件码) | 仅支持 7 种: `cmovz`/`cmovnz`/`cmova`/`cmovl`/`cmovg`/`cmovle`/`cmovge` |
| `cmovzl` 等带后缀形式 | GNU as 允许 cmov 加 `l`/`w`/`q` 后缀 | 当前不识别带宽度后缀的 cmov 别名 |
| 裸 `in` / `out` (无 b/w/l 后缀) | 某些头文件中的 I/O 操作 | 仅支持 `inb`/`inw`/`inl`/`outb`/`outw`/`outl` |
| `in`/`out` 非标准操作数模式 | 端口为内存间接形式 | 仅支持立即数端口和 DX 端口 |
| `shldw` / `shrdw` | 16 位双精度移位 | 仅支持 `shld`/`shldq`/`shldl` 和 `shrd`/`shrdq`/`shrdl` |
| 裸 `ds` 段前缀作为独立指令 | 旧式汇编风格 | 仅支持 `ds` 作为其他指令的前缀修饰 |

### AArch64

| 模块 | 现状 |
|------|------|
| NEON 浮点聚合传参 > 4 元素 | 仅支持 1–4 元素 HFA |

### x86_64 mini-asm 已知限制

| 类别 | 缺失模式 | 状态 |
|------|----------|------|
| `bswap` | 16 位操作数 | 🟡 不支持（gcc 同，x86_64 架构限制） |

## GCC bootstrap

| 任务 | 状态 | 说明 |
|------|------|------|
| GCC 完整 bootstrap (cc1/cc1plus) | ⏳ 部分受阻 | 本地磁盘已具备（GCC 源 ~1G + 构建 ~5G）；SFINAE / partial specialization / 完整 concepts / 协程 / module 均已实装；GCC 兼容运行时已实装；待 GCC 源树实际构建验证 |

## Sanitizer 遗留

| 通道 | 已知遗留 |
|------|----------|
| ~~HWASAN (`-fsanitize=hwaddress`)~~ | ✅ 软件 tag 模式完整；AArch64 MTE 硬件 tag 已集成: prologue tag 栈帧 (IRG/ADDG/STG 逐 granule 打 tag) + 指针 tag 流程 (sp 携带本帧随机 tag, 局部地址自动继承; 调用点 untag/retag, epilogue 恢复 x19 并清除 tag) |

## 性能分析子系统遗留

| 通道 | 已知遗留 |
|------|----------|
| ~~PGO~~ | ✅ v2 格式完整支持: value profiling + FNV-1a checksum + merge + profdata 合并格式 |
| ~~GCOV~~ | ✅ 完整格式支持: read_notes/read_data 读取器 + 行级覆盖率报告 (GcovReport) + GCC gcov-9 layout 兼容 |
| ~~AutoFDO~~ | ✅ 二进制格式 + discriminator 支持: write_binary/read_binary + disc 记录 + 自动检测文本/二进制 |
| ~~LBR~~ | ✅ perf.data 二进制 LBR 记录解析: read_perf_data_binary + PERF_RECORD_SAMPLE branch_stack + 自动检测文本/二进制 + serialize |
| ~~MemProf~~ | ✅ malloc 插桩 + runtime 库接口: MemProfInstrumentor + MemProfRuntime + 二进制 .memprofdata 格式 + 自动检测 |
| ~~Perf 事件~~ | ✅ perf.data 二进制样本解析: `parse_samples` (PERF_RECORD_SAMPLE) + `correlate_events` 事件关联 + `aggregate_by_ip` IP 聚合 + 自动检测文本/二进制 |
| ~~Offloading~~ | ✅ CUDA/ROCm driver 链接实装: `OffloadDriverLinker` + runtime 库解析 (libcudart/libamdhip64) + host link flags + device link commands (nvcc/clang++) + fat binary 描述 + link script 生成 |

## 链接器限制

| 限制 | 说明 |
|------|------|
| Windows 链接路径验证 | Windows 平台链接路径未实际验证 |
| 运行时 dynamic loader | 生成 ELF 依赖 host `ld.so`（设计决策）；dcc 自身可 musl 静态链接 |

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
| dynamic loader | 生成 ELF 依赖 host `ld.so`；dcc 自身可 musl 静态链接（见 `docs/static_build.md`） |
| fenv 运行时调用 | `fesetround` / `fegetround` 等未提供，`FENV_ACCESS` 仍为纯语义状态 |
| `double complex` 传值 ABI | `_Complex` 字面量/算术已实装，`double complex` 按值传 ABI 待后续 |

## 内联 asm 诊断与语义校验

> 以下诊断 gap 为已知限制，当前不计划实现完整校验。

| 缺失校验 | 当前行为 | GCC 行为 |
|-----------|----------|----------|
| 约束与表达式类型不匹配（如 `"r"` 绑浮点） | `W0005` 警告 | `error: impossible constraint` |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
