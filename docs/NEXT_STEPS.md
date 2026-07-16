# 后续步骤：20 步路线图 Checklist

> 最后更新：2026-07-16 — 以 `docs/ROADMAP.md`、`docs/MISSING_FEATURES.md`
> 与远程 `ds@192.168.148.128` 自动化日志为准。
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
  - 验证：`git status` 干净；远程 `~/dcc` 与本地一致
  - 命令：`bash scripts/sync_remote_build.sh`

- [x] **2. 远程 `sync_remote_build.sh` 全链路回归**
  - 验证：`cargo test` 38/38 + `remote_feature_check` 17/17 + prepare0/pahole/objtool PASS
  - 注意：`.sh` 文件须为 LF 行尾

- [x] **3. libiberty / fixincludes 回归锁定基线**
  - 验证：`bash scripts/remote_gcc_c_probe.sh` → libiberty **125/0/4**，fixincludes **6/0/1**
  - 附加：`bash scripts/test_tr2.sh`、`bash scripts/test_v3_cache.sh`

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

- [ ] **7. vmlinux 工具链验收**
  - 验证：`bash scripts/vmlinux_regress.sh` PASS
  - 验证：pahole 见 BTF struct；objtool 零 CRITICAL WARN

- [ ] **8. modpost 全 dcc 化（fixdep 替代）**
  - 验证：`scripts/modpost_link.sh` 全程 `HOSTCC=dcc`（fixdep 不再依赖 gcc）
  - 方案：Rust mini-fixdep 或预生成 fixdep

- [ ] **9. prepare0 全链路 vs 窄目标对齐**
  - 验证：`make prepare0 CC=dcc` PASS，或文档明确豁免项
  - 窄目标已通过：`bash scripts/prepare0_check.sh`

- [ ] **10. 内核 smoke 测试矩阵**
  - 验证：`qemu_boot_matrix.sh` 多配置（defconfig / 串口）非 SKIP
  - 验证：内核能打印 boot log 不 panic

---

## 阶段 C：CI 与回归体系（P1，步骤 11–14）

- [ ] **11. 扩展 GitHub CI：`cargo test` + integration**
  - 验证：`.github/workflows/e2e.yml` 增加 `cargo test --release`（当前仅 4 个 example）
  - 目标：PR 自动跑 38 项 integration

- [ ] **12. 远程回归纳入 CI 或 scheduled workflow**
  - 验证：self-hosted runner 或 SSH action 定期跑 `sync_remote_build.sh`
  - 目标：libiberty 125/0 不 regression

- [ ] **13. 首次 kernel-benchmark 基线**
  - 验证：手动触发 `kernel-benchmark` workflow；GitHub Step Summary 有 GCC vs dcc 对比
  - 目标：dcc warm < GCC warm（或记录差距与原因）

- [ ] **14. 宏展开 / include 缓存回归**
  - 验证：`test_tr2.sh`、`test_v3_cache.sh` 纳入 CI 或 `sync_remote_build.sh`
  - 目标：regex/TRANSLATE 类 bug 不因缓存版本回退

---

## 阶段 D：编译器能力补洞（P1–P2，步骤 15–17）✅

- [x] **15. `_Generic` 指针精确匹配**
  - 实现：association 按 pointee 匹配；`void*` 与对象指针兼容
  - 验证：`tests/fixtures/generic_pointer.c`；libiberty **125/0/4**

- [x] **16. 内联 asm 从 no-op 到最小可用**
  - 实现：约束 `"r"`/`"m"`/`"=r"`/`"+m"`、`memory` clobber；文件级 asm 原样输出
  - 验证：`examples/test_asm_constraints.c` 编译 + 运行 exit 0

- [x] **17. 完整 SysV `va_list`（含 SSE 参数区）**
  - 实现：XMM0–7 保存区；`fp_offset`/`cmp $176` 修复；命名 float 参数从 xmm 入栈
  - 验证：`examples/test_p4_va_list.c` 编译 + 运行 exit 0

---

## 阶段 E：性能与架构扩展（P3，步骤 18–20）✅

- [x] **18. 性能基础设施：mmap + 函数级分块 codegen**
  - 实现：`src/mmap_io.rs` ADR-006 零拷贝读源；`-O2+` 多函数 TU 走分块 emit/merge
  - 说明：真·多线程并行待 AST `Rc`→`Arc` 迁移；可用 `DCC_NO_PARALLEL=1` 关闭分块路径
  - 验证：`bash scripts/bench_phase_e.sh`；`remote_feature_check` 仍 PASS

- [x] **19. 编译期优化 pass 扩展**
  - 实现：`-O2` 常量分支 DCE + return 后不可达删除；`-O3` 小 static 函数内联
  - 验证：`remote_feature_check` **O1_fold** + **O2_branch_fold** PASS

- [x] **20. 决策点：M7 多架构 vs M5 自举 → 选定 M7**
  - 路径：**M7**（内核+速度优先）；M5 自举维持 backlog
  - 实现：`src/codegen/aarch64.rs` hello world；`build_elf_aarch64` / `build_elf_rel_aarch64`
  - 验证：`dcc --target aarch64-linux-gnu -c hello.c`；`remote_feature_check aarch64_hello`
  - 文档：`docs/ROADMAP.md` M7 进行中；README 里程碑已同步

---

## 明确不做（避免 scope creep）

- [ ] ~~完整 GCC bootstrap~~ — GCC 主体为 C++，无 `dcc++` 前端
- [ ] ~~libdecnumber Rust 移植~~ — Makefile 组合 TU 即可（可选）
- [ ] ~~gen\* 工具重写~~ — 服务于 GCC 构建管线，与 dcc 无关

---

## 一键验证清单

每轮 push 或远程同步后建议依次执行：

```bash
bash scripts/sync_remote_build.sh          # 含 cargo test + feature + prepare0 + kernel_full_check
bash scripts/remote_gcc_c_probe.sh         # libiberty + fixincludes + 链接
bash scripts/kernel_full_check.sh          # modpost / QEMU / vmlinux（缺产物 SKIP）
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
- [x] 远程 libiberty：**125 OK / 0 FAIL**（含 `regex.c`）
- [x] libiberty 整库 `dcc -r` 链接 + fixincludes **6/6**
- [x] `char const *`、`ld -r --gc-sections -u`、symtab 去重

### 2026-07-16（内核脚本 + LTO gc）

- [x] `scripts/modpost_link.sh` — HOSTCC=dcc（fixdep 仍用 gcc）
- [x] `scripts/qemu_boot_matrix.sh` + `scripts/kernel_full_check.sh`
- [x] `scripts/vmlinux_regress.sh`
- [x] LTO dead symbol：`-ffunction-sections` + `ld --gc-sections` + objcopy strip
- [x] `sync_remote_build.sh` 接入 `kernel_full_check.sh`

### 2026-07-16（阶段 D + 文档）

- [x] 步骤 15–17：`_Generic` 指针 / 内联 asm 约束 / SysV va_list SSE
- [x] README 里程碑 M3/M6 与 ROADMAP 对齐

</details>
