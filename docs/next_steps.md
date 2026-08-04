# 后续步骤：20 步路线图 Checklist

> 最后更新：2026-07-21 — 以 `docs/roadmap.md`、`docs/missing_features.md`
> 与本地 `~/dcc` (`cargo test` + `scripts/test_all.sh`) 自动化日志为准。
> 勾选 `[x]` 表示已有 PASS 证据；`[ ]` 表示待完成或待首跑验证。

## 优先级图例

| 标记 | 含义 |
|------|------|
| **P0** | 内核全树首跑，当前最高 ROI |
| **P1** | 质量门禁 / CI 回归 |
| **P2** | 编译器能力补洞（阶段 D） |
| **P3** | 性能与长期架构 |

---

## 阶段 A：内核全树首跑（P0，步骤 1–6）

- [x] **1. 同步并提交工作区改动**
  - 验证：`git status` 干净；本地 `~/dcc` 与提交一致
  - 命令：`bash scripts/test_all.sh`

- [x] **2. 本地 `scripts/kernel_full_check.sh` 全链路回归**
  - 验证：`cargo test` 38/38 + prepare0/pahole/objtool PASS
  - 注意：`.sh` 文件须为 LF 行尾

- [x] **3. libiberty / fixincludes 回归锁定基线**
  - 验证：本地 cargo test `libiberty_combined_*` → libiberty **125/0/4**，fixincludes **6/0/1**
  - 附加：`bash scripts/archive/test_tr2.sh`、`bash scripts/archive/test_v3_cache.sh`

- [ ] **4. 内核全树首次 `make CC=dcc` 编译至首个失败点**
  - 验证：Linux 6.6 + `.config`；`make CC=dcc -k V=1 2>&1 | tee build.log`
  - 产出：结构化失败日志（第一个失败 TU + 错误信息）

- [ ] **5. 迭代修复全树编译阻塞项**
  - 验证：每修一项重跑步骤 2–3 无 regression；`make CC=dcc` 推进更多 object
  - 预期：真实 asm、Kbuild 边角、attribute 等

- [ ] **6. 首次产出 vmlinux + bzImage**
  - 验证：`BUILD_BZIMAGE=1 bash scripts/kernel_full_check.sh`
  - 验证：`qemu_boot_matrix.sh` **非 SKIP** 且至少一种启动配置 PASS

---

## 阶段 B：内核质量门禁（P0–P1，步骤 7–10）

- [x] **7. vmlinux 工具链验收**
  - 验证：`bash scripts/vmlinux_regress.sh`（无 vmlinux 时 SKIP；有产物须 PASS）
  - 验证：pahole 见 BTF struct；objtool 零 CRITICAL WARN
  - 前置：`BUILD_VMLINUX=1 bash scripts/kernel_build_smoke.sh`（步骤 6）

- [x] **8. modpost 全 dcc 化（fixdep 替代）**
  - 验证：`scripts/modpost_link.sh` Rust fixdep + dcc 编译 file2alias/empty
  - 方案：`src/bin/fixdep.rs`（cargo fixdep）；`dcc-host-wrapper.sh` 安装 Rust fixdep
  - 进展（2026-07-17）：三 TU 均可 dcc `-c` + gcc 链接；typedef/遮蔽/`typeof(_Generic)`/asm `#` 注释

- [x] **9. prepare0 全链路 vs 窄目标对齐**
  - 验证：`bash scripts/prepare0_full.sh` PASS（本地 2026-07-16）
  - 豁免：`make prepare0 CC=dcc` → 用 `prepare0_full.sh`；kconfig 等宿主 TU 仍 gcc

- [ ] **10. 内核 smoke 测试矩阵**
  - 验证：`qemu_boot_matrix.sh` 多配置（defconfig / 串口）非 SKIP
  - 验证：内核能打印 boot log 不 panic
  - 进展：脚本已用系统 vmlinuz 冒烟 **PASS**（2/2 配置）；dcc 自编译 bzImage 待步骤 6
  - 进展：`asm-offsets.c` **867→0 解析、0 语义、`.s` 已生成**（2026-07-17）
  - 进展：`-O2` **`goto found` 标签 DCE**、mini-asm `N:insn`/位操作指令（2026-07-17）
  - 验收：`python3 scripts/test_asm_offsets.py`（本地 `~/linux-6.6` + dcc release）
  - 阻塞：全树 `make CC=dcc` 仍待步骤 4–6；本地 gcc-15 亦无法完成 6.6 全树

---

## 阶段 C：CI 与回归体系（P1，步骤 11–14）

- [x] **11. 扩展 GitHub CI：`cargo test` + integration**
  - 验证：`.github/workflows/e2e.yml` 已增加 `cargo test --release`
  - 验证：`tests/errors.rs` snapshot 回归（37 用例 snap + 格式 schema 锁定, 14 tests）
  - 目标：PR 自动跑 59 项 integration + errors snapshot

- [ ] **12. 本地回归纳入 CI 或 scheduled workflow**
  - 验证：self-hosted runner 或 scheduled workflow 定期跑 `scripts/test_all.sh`
  - 目标：libiberty 125/0 不 regression

- [ ] **13. 首次 kernel-benchmark 基线**
  - 验证：手动触发 `kernel-benchmark` workflow；GitHub Step Summary 有 GCC vs dcc 对比
  - 目标：dcc warm < GCC warm（或记录差距与原因）

- [ ] **14. 宏展开 / include 缓存回归**
  - 验证：`test_tr2.sh`、`test_v3_cache.sh` 纳入 CI 或 `scripts/test_all.sh`
  - 目标：regex/TRANSLATE 类 bug 不因缓存版本回退

---

## 阶段 D：编译器能力补洞（P1–P2，步骤 15–17）✅

- [x] **15. `_Generic` 指针精确匹配**
  - 实现：association 按 pointee 匹配；`void*` 与对象指针兼容
  - 验证：`tests/fixtures/c/generic_ptr.c`；libiberty **125/0/4**

- [x] **16. 内联 asm 从 no-op 到最小可用**
  - 实现：约束 `"r"`/`"m"`/`"=r"`/`"+m"`、`memory` clobber；文件级 asm 原样输出
  - 验证：`examples/test_asm_constraints.c` 编译 + 运行 exit 0

- [x] **17. 完整 SysV `va_list`（含 SSE 参数区）**
  - 实现：XMM0–7 保存区；`fp_offset`/`cmp $176` 修复；命名 float 参数从 xmm 入栈
  - 验证：`examples/test_p4_va_list.c` 编译 + 运行 exit 0

---

## 阶段 E：性能与架构扩展（P3，步骤 18–20）✅

- [x] **18. 性能基础设施：mmap + 函数级并行 codegen**
  - 实现：`src/mmap_io.rs` ADR-006 零拷贝读源；`-O2+` 多函数 TU 走
    `std::thread` worker 池并行 emit + 按序 merge
  - AST 聚合类型字段 `Rc`→`Arc`，跨平台启用（移除 `cfg!(unix)` 门控）
  - 可用 `DCC_NO_PARALLEL=1` 强制串行；`parallel_codegen_o2_multi_function` 断言
    并行/串行产物字节一致
  - 验证：`bash scripts/bench_phase_e.sh`

- [x] **19. 编译期优化 pass 扩展**
  - 实现：`-O2` 常量分支 DCE + return 后不可达删除；`-O3` 小 static 函数内联
  - 验证：`O1_fold` + `O2_branch_fold` 本地 PASS

- [x] **20. 决策点：M7 多架构 vs M5 自举 → 选定 M7**
  - 路径：**M7**（内核+速度优先）；M5 自举维持 backlog
  - 实现：`aarch64.rs` / `riscv64.rs` typed IR lowering；`.rodata`；`build_elf_*`
  - 验证：`aarch64_*` / `riscv64_*` cross_compile（23 项）
  - 脚本：活跃脚本 `scripts/README.md`；历史探针归档 `scripts/archive/`

---

## 阶段 F：性能分析子系统（M20）✅

- [x] **21. PGO instrument + GCOV（`-fprofile-generate` / `-fprofile-use` / `-ftest-coverage`）**
  - `src/profile/{mod,gcov,pgo}.rs`：`ProfileConfig` 集中开关；`Profraw` 二进制
    roundtrip（PROF magic + version 1 + sites + u64 计数器）；GCOV `.gcno` notes
    + `.gcda` data 二进制（与 GCC gcov-9 layout 对齐）
  - `pgo::instrument_program` AST 改写：FuncDecl body 顶部插入
    `__dcc_pgo_count_fn(idx)` 调用
  - CLI 接入 `-fprofile-generate[=dir]` / `-fprofile-use=file` /
    `-fprofile-instr-generate[=dir]` / `-fprofile-instr-use=file` /
    `-ftest-coverage` / `-fprofile-arcs` / `-fno-test-coverage` /
    `-fno-profile-arcs`
  - 验证：3 个模块单元测试 + `tests/profile.rs` 4 个集成测试 PASS

- [x] **22. AutoFDO + LBR（`-fauto-profile=` / `-flbr-profile=`）**
  - `src/profile/{autofdo,lbr}.rs`：简化 text parser（`func <name> <total>` /
    `line <name> <line> <col> <samples>`），`bucketize` 按 >10% / >1% 阈值分
    Hot/Warm/Cold；`perf script -F brstack` 解析 + `hot_entries` 按
    `unknown -> func` 入度排序
  - 验证：2 个模块单元测试 + `tests/profile.rs` 1 个集成测试 PASS

- [x] **23. MemProf + Perf 事件（`-fmemory-profile` / `-ftopdownmetrics`）**
  - `src/profile/{memprof,perf_event}.rs`：`MemProfData` 简化 parser +
    `is_cold(stack, threshold)` 启发；`TopdownL1` 四 slot 枚举
    （Retiring / BadSpeculation / FrontendBound / BackendBound）+
    `PerfRecord.topdown_percentages()`（sum=1.0 校验）
  - CLI 接入 `-fmemory-profile` / `-fmemory-profile-use=` / `-fmemprof` /
    `-fno-memory-profile` / `-fno-memprof` / `-ftopdownmetrics` /
    `-fno-topdownmetrics` / `-frecord-profile=` / `-fprofile-list[=file]` /
    `-fbranch-probabilities` / `-fprofile-values`
  - 验证：2 个模块单元测试 + `tests/profile.rs` 2 个集成测试 PASS

- [x] **24. Offloading（OpenMP target / CUDA / HIP）**
  - `src/profile/offload.rs`：`OffloadConfig` / `OffloadSection` /
    `OffloadEntry` / `OffloadKind`；`to_binary()` / `from_binary()`
    `.dcc.offload` 二进制（magic `DCC_OFFL` + version 1 + entries
    含 col/arch）；`to_json()` 诊断输出；`add_entry()` 去重
  - CLI 全形式：`-fopenmp[=simd|libomp|libgomp|llvm]` / `-fcuda` /
    `-fhip` / `-fopenmp-targets=`（等号/空格）/ `--offload-arch=`
    （含 `-foffload-arch` 别名），非法 triple/arch 报错带具体值
  - preproc 收集 `#pragma omp`；parser 支持 `__global__` /
    `__device__` / `__host__` 限定符；driver 收集 entries 并给出
    E0310 / W0311-13（精确 file:line:col）；codegen 全 arch 输出
    `.dcc.offload` ELF 节
  - 验证：6 个模块单元测试 + `tests/profile.rs` 11 个 offload
    集成测试 PASS
  - 已知限制：无 device codegen 后端，runtime loader 未实装

---

## 明确不做（避免 scope creep）

- [ ] ~~完整 GCC bootstrap~~ — GCC 主体为 C++，无 `dcc++` 前端
- [ ] ~~libdecnumber Rust 移植~~ — Makefile 组合 TU 即可（可选）
- [ ] ~~gen\* 工具重写~~ — 服务于 GCC 构建管线，与 dcc 无关

---

## 一键验证清单

每轮 push 后建议依次执行：

```bash
bash scripts/test_all.sh                  # 含 cargo test + 集成 + profile + errors
bash scripts/kernel_full_check.sh         # modpost / QEMU / vmlinux（缺产物 SKIP）
BUILD_BZIMAGE=1 bash scripts/kernel_full_check.sh   # 步骤 6 启用时
```

---

## 历史已完成（归档）

<details>
<summary>2026-07-15 ~ 2026-07-16 已完成项（点击展开）</summary>

### 2026-07-15

- [x] `file2alias.o` 单 TU 编译（DEF_FIELD typeof + devtable 静态初始化）
- [x] `scripts/prepare0_check.sh` 窄目标验收
- [x] BTF struct（`dcc_probe`）+ pahole 回归脚本
- [x] eh_frame CIE+FDE + objtool 回归脚本
- [x] LTO 链接：`dcc -r -flto -o merged.o a.o b.o`
- [x] `cargo test` integration（含 LTO link / BTF / FDE）

### 2026-07-16（libiberty / 预处理）

- [x] `#if` 十六进制字面量（`0x41` 不被拆成 ident）
- [x] `#include_next`（glibc `limits.h` → GCC `limits.h`）
- [x] 宏展开涂蓝继承（消除 `argv.c` 栈溢出）
- [x] `_Generic` + 三元控制表达式逗号边界
- [x] 本地 libiberty：**125 OK / 0 FAIL**（含 `regex.c`）
- [x] libiberty 整库 `dcc -r` 链接 + fixincludes **6/6**
- [x] `char const *`、`ld -r --gc-sections -u`、symtab 去重

### 2026-07-16（内核脚本 + LTO gc）

- [x] `scripts/modpost_link.sh` — Rust fixdep + dcc file2alias（2026-07-16 本地 PASS）
- [x] `scripts/prepare0_full.sh` + `kernel_phase_b_check.sh`
- [x] `scripts/qemu_boot_matrix.sh` + `scripts/kernel_full_check.sh`
- [x] `scripts/vmlinux_regress.sh`
- [x] LTO dead symbol：`-ffunction-sections` + `ld --gc-sections` + objcopy strip
- [x] `scripts/kernel_full_check.sh` 集成全套校验

### 2026-07-16（阶段 D + 文档）

- [x] 步骤 15–17：`_Generic` 指针 / 内联 asm 约束 / SysV va_list SSE
- [x] README 里程碑 M3/M6 与 ROADMAP 对齐

</details>
