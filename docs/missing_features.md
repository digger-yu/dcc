# DCC 缺失特性与已知限制

> 最后更新：2026-08-08 — 基于 `cargo test` 326 integration tests + 685 unit tests 全部通过
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

### RISC-V64

| 模块 | 现状 |
|------|------|
| RVV 完整指令集 | 仅基础 VSETVLI/LOAD/STORE/ADD/MUL/REDUCE，缺宽化/窄化/掩码/浮点向量 |

### x86_64 mini-asm

| 类别 | 缺失模式 | 状态 |
|------|----------|------|
| `bswap` | 16 位操作数 | 🟡 不支持（gcc 同，x86_64 架构限制） |

## Sanitizer 遗留

| 通道 | 已知遗留 | 状态 |
|------|----------|------|
| KASAN (`-fsanitize=address`) | codegen 已支持 Deref/Index/Member/Ident LHS (Comma 解包)；栈红区 + 全局红区已实装 | ✅ 完整 |
| HWASAN (`-fsanitize=hwaddress`) | 软件 tag 模式完整 (`__hwasan_loadN`/`__hwasan_storeN`)；AArch64 MTE 硬件指令编码已实现但未集成到 prologue tag 栈帧 + 指针 tag 流程 | ⚠️ 软件 tag 完整，硬件 MTE 待集成 |
| KASAN-kernel (`-fsanitize=kernel-address`) | x86_64 内联 shadow 地址计算：`shadow = (addr >> 3) + 0xdffffc0000000000`；shadow byte < 0 时调用 `__asan_report_error` | ✅ 完整 |
| UBSAN (`-fsanitize=undefined`) | Div/Mod/Shl/Shr/有符号 Add/Sub/Mul/VLA bound/null deref/float cast overflow 全部前置 `__ubsan_handle_*` | ✅ 完整 |
| KCSAN (`-fsanitize=thread`) | TU init + 每函数 `__kcsan_func_entry`；KCSAN-only 模式走 `__kcsan_check_access(addr, size, is_write)` 三参调用 | ✅ 完整 |
| KCOV (`-fsanitize-coverage`) | 每函数 `__dcc_cov_guards_N[16]` + 入口 `__sanitizer_cov_trace_pc` + 边覆盖 `__sanitizer_cov_trace_pc_guard`；跨函数 inline 合并由 `profile::kcov_merge` 在 LTO 阶段完成 | ✅ 完整 |
| Fuzzer (`-fsanitize=fuzzer`) | KCOV 全部 + `__sanitizer_cov_trace_cmp1/2/4/8` + `__sanitizer_cov_trace_switch`；libFuzzer 协议由 `profile::fuzzer_protocol` 提供（`LLVMFuzzerTestOneInput` 入口包装、8-bit counters、PC table、malloc/free 拦截器） | ✅ 完整 |

## 性能分析子系统遗留

| 通道 | 已知遗留 |
|------|----------|
| PGO | 下游权重传播已实装 (`propagate_edge_weights` 生成 `BranchProbMap`, `propagate_loop_trip_counts` 推算循环 trip count)；`.profraw` 二进制格式仅支持子集 |
| GCOV | 已验证跨架构字节序一致性 (显式 little-endian 编码, 测试覆盖 `.gcno`/`.gcda` 格式); 仅支持 dcc 简化格式, 不兼容 GCC gcov 完整格式 |
| AutoFDO | 已接入 codegen (`.text.hot`/`.text.unlikely`)；仅支持 dcc 简化文本格式，不兼容 LLVM extbinary 格式 |
| LBR | 已接入 codegen；仅解析 `perf script -F brstack` 文本，不直接读 `perf.data` 二进制 |
| MemProf | 已接入 codegen + AllocClass 分类；未在 libc malloc 上插桩；无 runtime 库 |
| Perf 事件 | L1 四 slot + L2 八子分类已接入；仅支持 `perf stat` 文本输出，不读 `perf.data` 二进制 |
| Offloading | device codegen (PTX/AMDGCN) stub 已实装; runtime loader 已实装 (`OffloadRuntimeLoader` 读 `.dcc.offload` 节 + kernel launch 接口 + 诊断输出); 实际 CUDA/ROCm driver 链接未实装 |

## C++ 前端限制

无已知限制。

## 链接器限制

| 限制 | 说明 |
|------|------|
| Windows 链接路径验证 | Windows 平台链接路径未实际验证 |
| DSO symbol caching | `DsoCache` 已实现，链接器侧未接入（需先补 `-lxxx → libc.so` 解析） |
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

> 以下诊断 gap 为已知限制，当前不计划实现完整校验。`validate_asm_semantics` 已覆盖部分场景（E0014/E0015/W0005-W0007），但以下情况仍未严格处理。

| 缺失校验 | 当前行为 | GCC 行为 | 说明 |
|-----------|----------|----------|------|
| 约束与表达式类型不匹配（如 `"r"` 绑浮点） | `W0005` 警告 | `error: impossible constraint` | dcc 仅发 warning 不阻塞编译；GCC 报 hard error |
| 模板占位符数量与操作数数量不匹配 | `E0015` 仅检测索引越界 | 编译期 error | dcc 不检测 `%0..%N` 总数是否与实际操作数数量一致 |
| 输出约束未配合 `=`/`+` 前缀 | `E0014` error | 编译期 error | 已实装但仅在前端校验；SSA/IR 层不重复检查 |
| 命名操作数与位置操作数混用 | `W0007` warning | GCC 允许但有限制 | dcc 仅警告不强制；GCC 在混用 `[name]` 与 `%N` 时有更严格的上下文规则 |
| 重复约束名 | `W0006` warning | 编译期 error | dcc 仅警告；GCC 对同名操作数报 hard error |

## 内联 asm 优化器集成

| 缺失能力 | 当前行为 | 说明 |
|-----------|----------|------|
| asm 周围指令重排别名分析 | 仅 memory clobber 产生调度屏障 | 无 memory clobber 的 asm 不参与别名分析，可能导致不安全的重排 |
| DCE 不消除无副作用无输出 asm | ✅ 已按 GCC 兼容行为保留 | 所有 inline asm 视为潜在副作用，DCE 不消除 |
| 寄存器分配器 asm 统一规划 | 依赖硬编码 GP/XMM 池 | asm 输入输出占用的物理寄存器未与 SSA 寄存器分配统一建模 |

## 内联 asm 调试信息

| 缺失能力 | 当前行为 | 说明 |
|-----------|----------|------|
| DWARF 行号精度 | asm 块整体映射到源行 | 内联 asm 内部指令无独立行号条目，`addr2line` 只能定位到 asm 语句起始行 |
| GDB step-into asm 体验 | mini-asm 不发射 `.debug_insn` 等扩展 | GDB 无法单步进入 asm 块内部；与 GCC 行为一致（GCC 也不提供 asm 内部行号） |

## 内联 asm 测试覆盖空白

| 缺失场景 | 说明 |
|-----------|------|
| 多 TU 跨文件 asm goto 跳转 | 需链接两个 `.o` 文件验证 asm goto 标签解析 |
| asm goto 输出约束跨 fall-through / jump 路径写回 | 验证跳转路径不写回、fall-through 路径正确写回 |
| 大规模 AVX-512 EVEX 掩码指令端到端运行 | `asm_avx.c` 仅编译通过，未运行验证结果正确性 |
| asm goto 与 `&&label` 混合 | 混合使用 computed goto 地址与 asm goto 标签 |
| 嵌套 asm 字符串里的 `.macro` / `.rept` 块 | 深层嵌套宏展开的边界情况 |
| memory clobber + 优化器重排验证 | `-O2` 下 memory clobber 阻止指令重排的端到端验证 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`（由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
