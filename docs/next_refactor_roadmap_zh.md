# DCC 后端重构：原方案完成度审计与下一步操作路线图

> 审计基线：分支 `part-a-pr-a2-asm-extraction`，HEAD = `bbad2fe`（领先 `main` `3866db6` 共 5 个 commit）。
> 审计对象：`report/backend_refactor_plan.md` **v1.2**（768 行，与 `docs/backend_refactor_plan.md` 逐字节相同，md5 `a05d8afd…`）。
> 取证方式：全部结论均由 `wc -l` / `awk` / `grep` / `diff` / `git show` / `cargo check` / `cargo test` 实地复算，
> **不采信** `BIG_BANG_MIGRATION_REPORT.md`、`BIG_BANG_MIGRATION_STATUS.md` 中写于 41k 行时代的数字。
> 本文档为只读分析产物：未修改任何代码、未 commit、未删除任何文件。

---

## 1. 执行摘要

**原方案 v1.2 的落地完成度约 33%（加权，区间 30%–35%）。** 具体分布是：**"契约门面"已基本立起来，"拆分实体"和"一致性证明"两条腿都还没落地。**

- 已经真实做到的：`codegen/mod.rs` 的巨型 `match` 被 `registry::backend_for()` 派发取代（1,266 行文件、`emit()` 仅 31 行、`panic!` 兜底已改 `Err(E0399)`）、`Backend` trait 含 `compile/render_asm/build_object` 且 **6 个架构全部 impl**、`RiscV32` 不再谎报 `has_backend=true`、诊断失败策略收敬为单一 `has_fatal_diagnostics()`、x86_64 目录骨架（41 个 `.rs`）已投产且 `unimplemented!()/todo!` 为 0。
- 尚未做到的（也是剩余工作的全部性质）：**纯粹的"逐字搬迁 + 一致性证明"**，不含任何新语义设计。`x86_64/mod.rs` 仍有 **35,077 行（占整个后端 78.8%）**，其中 asm 编码家族 10,657 行、builtin 4,144 行、`compile_*` 3,293 行是三块边界清晰、可机械切分的硬骨头；`FeatureSet` 是**没有任何调用点的装饰品**且对 5 个交叉架构**系统性虚报能力**；行为探针脚本**指向不存在的 `tests/input/`，实跑 0 通过 50 失败却仍返回 exit 0**；`check_feature_matrix` CI job 的 registry 校验步骤本地复算 `22 != 6`，**按当前代码永远红**；`tests/backend_parity.rs` 与 L0/L1/L2 分层表**完全不存在**；§3.7 文档行号清单**一处未改**（29 处旧路径 + 22 处 `#L`）。
- 一句话：**方案 §1.3 七条成功标准里，只有 "RV32 不说谎" 一条完全达成，"契约有 trait" 一条大体达成，其余五条（文件规模 / 能力可见 / 同源测试 / 诊断对照 / 卸载闭环）均未达成或仅半达成。**

剩余工作量的合理预期是 **11 个步骤、约 40±8 个 commit**，其中 60% 是可自动化的搬迁与门禁修复，不涉及编译器语义新增。

---

## 2. 实地核验的事实基线

### 2.1 规模（截至 `bbad2fe`）

| 对象 | 行数 | 备注 |
|------|------|------|
| `src/codegen/x86_64/mod.rs` | **35,077** | 后端总 44,511 行中的 **78.8%**；`main` 上单文件 `x86_64.rs` 为 41,658 行 |
| `src/codegen/x86_64/` 子模块合计（40 个文件） | 9,434 | 21.2%；峰值搬迁后净减 6,581 行（−15.8%） |
| `src/codegen/aarch64.rs` | 7,003 | 277 个 `fn`，最大 `gen_atomic_builtin` 510 行 |
| `src/codegen/riscv64.rs` | 6,803 | 313 个 `fn`，最大 453 行 |
| `src/codegen/arm.rs` | 2,870 | 120 个 `fn` |
| `src/codegen/mips.rs` | 2,467 | 104 个 `fn` |
| `src/codegen/mips64.rs` | 222 | `Mips64Codegen { inner: MipsCodegen }` 薄封装（§3.5 模式已实现 ✅） |
| `src/codegen/mod.rs` | 1,266 | 方案 §4.1.2 目标 900–1,200，**接近达标** |
| `src/codegen/backend.rs` / `features.rs` / `registry.rs` | 130 / 171 / 152 | trait + FeatureSet + AnyBackend |

`mod.rs` 行数轨迹（`git show <c>:src/codegen/x86_64/mod.rs | wc -l`）：`359259d` 41,658 → `ccdb1b8` 36,813 → `bbad2fe` 35,077。

### 2.2 门禁实测（本文档撰写时逐条跑过）

| 命令 | 实测结果 |
|------|----------|
| `cargo check --all-targets` | **0 error**；仅 warning（`src/codegen/x86_64/asm/mod.rs:16/19/22` 三条未使用 `pub use` + `asm_event.rs` 若干） |
| `cargo test --lib` | **1591 passed / 1 failed**，失败项 `cpp::sema::tests::build_hir_unsupported_decl_returns_err`（与 codegen 无关，属既有失败） |
| `cargo test --test integration` | **806 passed / 7 failed / 25 ignored**，7 个失败全为 `cpp_*`，报 `E0500 C++ native backend`；`git diff --name-only main..HEAD -- src/cpp` = **0 文件**，可判定为既有失败 |
| `./scripts/probe_backend_features.sh -o md` | **`Probe complete: 0 passed, 50 failed`**，全部格为 `❓`(MISSING_FIXTURE)，md 表格被 `_` 分列破坏；**exit code = 0** |
| CI registry 校验步骤（本地复算） | `expected_backends=22`、`actual_enabled=6` → **`exit 1`，该 job 结构性不可通过** |

结论：**测试基线 = `--lib 1591/1`、`integration 806/7/25`、`cargo check` 0 error**，后续每步都必须以此或更高为门槛。

### 2.3 "文档宣称 vs 代码事实" 偏差（审计重点，共 9 条）

| # | 文档宣称 | 实地事实 |
|---|----------|----------|
| 1 | `BIG_BANG_MIGRATION_STATUS.md` §Phase3："`x86_64.rs` ~39,562 行待拆" | 现 `mod.rs` 35,077 行，`main` 上实为 41,658 行；两个数字都不是当前值 |
| 2 | `BIG_BANG_MIGRATION_REPORT.md`：Part A 目录含 `elf_side/`、`tests/` ✅ | `src/codegen/x86_64/elf_side/`、`src/codegen/x86_64/tests/` **是空目录**（`find -type d -empty` 证实），零文件 |
| 3 | `registry.rs:126` 注释"与 backend_for 的 match 分支保持一致"、`BIG_BANG_MIGRATION_REPORT.md`"has_backend delegates to registry, no hardcoded list" | **驱动侧真正生效的是 `src/cli.rs:378` 的手写 `matches!` 列表**；`registry::has_backend()` 全仓 **0 个调用点**（仅出现在自身 doc 注释里）。两份清单并存，无机械绑定 |
| 4 | "`FeatureSet` makes capabilities explicit and measurable" | `grep -rn "\.features()" src/` → **0 个调用点**。`features()` 从未被读；不存在 §4.2 要求的"缺能力 E0xxx"门禁 |
| 5 | 矩阵文档标 arm/mips `Lto ❌ / Tls ❌ / Vector ❌` | 5 个交叉架构 `features()` 一律返回 `FeatureSet::elf_only()` = `full()` 减 COFF，即**同时声称支持** `Int128/Vector/Tls/Atomics/InlineAsm(Full)/Sanitizer/CfiUnwind/StackProtect/Lto/BoundsCheck/Trapv/ParallelCodegen/RiscVBorrowedRegs`。而实测：`.dcc.lto` / `.llvm.lto` **只在 `x86_64/backend.rs` 出现**（其余 5 arch 各 0 命中），`parallel` 在非 x86 backend **0 命中** → FeatureSet 与探针、与代码三方互斥 |
| 6 | "`check_feature_matrix` 强制 diff，防腐" | workflow 只做 `>> docs/backend_feature_matrix.md` **追加** + `[ -f 文件 ]` 存在性判断；无 diff 断言；探针自身 exit 0 恒真；registry 步骤恒失败 |
| 7 | `docs/backend_feature_matrix.md` 头写 `generated-or-verified-by: probe @ $(date -Iseconds)` | 该 `$(…)` **字面未展开**，证明该文件从未经探针生成；文件 74 行全为手写 |
| 8 | "golden 回归基线已建立" | `golden_baseline/` 确有 7 个 `.o` + `hashes.txt`（7 条 sha256），但 `grep -rn "golden_baseline" Makefile scripts/ .github/` **无任何引用** → 没有比对脚本、没有 CI 挂点。`scripts/CREATE_GOLDEN_BASELINES.sh` 的 `golden_hashes/*.sha256` 打的是 **`.rs` 源文件** 的哈希（对重构毫无意义），`check_status.txt` 存的是一份 cargo warning 快照 |
| 9 | "Progress Metrics：x86_64 split ~21%，Overall ~25%" | 21% 这个比例数字与实测（9,434/44,511 = 21.2%）吻合，是两份报告里**唯一**经得起复算的量化结论 |

另有 3 处"影子/孤儿"残留（PR-A2 未收尾的直接证据）：

- `asm/constraint/parsing.rs`、`asm/reg/mapping.rs`、`asm/template/subst.rs` 是**孤立文件**：无 `mod constraint` / `mod reg` / `mod subst` 声明（`asm/template.rs`、`asm/directive.rs`、`asm/label.rs` 为同级 `.rs`，目录里的 `*.rs` 不参与编译），**从未被编译**。另有空目录 `asm/label/`、`asm/directive/`、`src/codegen/mips/`。
- `AsmDirectiveExpander` 定义 **3 份**（`mod.rs:911` 生产 + `asm/directive.rs:9` + `asm/expansion.rs:6`）、`AsmLocalLabels` **3 份**（`mod.rs:748` + `asm/label.rs:6` + `asm/template.rs:8`）、`AsmRegisterPlanner` **2 份**（`mod.rs:536` + `asm/planner/mod.rs:18`）；生产路径走 `mod.rs` 版本（子模块以 `use crate::codegen::x86_64::AsmDirectiveExpander` 反向引用父模块私有项），子模块里的同名类型是死码。
- `x86_64/mod.rs:36–56` 的注释仍写"模块化提取层（**影子实现，尚未接入 gen_expr 生产路径**）"并给 `mod abi/emit/mem/parallel/special` 挂 `#[allow(unused_imports)]` —— 与"全部接线投产"的现状不符，且 `allow` 用错位置（模块声明上无意义）。

---

## 3. 完成度审计表（对照 v1.2 逐条打分）

### 3.1 §1.3 成功标准七条

| 标准 | 判定 | 代码证据 |
|------|------|----------|
| 文件规模：`x86_64/` 常规模块 < 5k 行 | ❌ | `mod.rs` 35,077 行，超阈值 **7 倍**；且方案要求的"例外须在附录列明"也未做（无附录） |
| 契约：`trait Backend` + Tier-1 impl + 包装期行为一致 | 🟡 | trait 与 6 arch impl 齐备（`x86_64/backend.rs`、`aarch64/backend.rs`、`arm.rs:2765`、`mips.rs:2355`、`mips64.rs:109`、`riscv64.rs:6659`）✅；"行为一致"**未被证明**（golden 无自动化、parity 无测试）❌ |
| 能力可见：`matrix = 探针 = FeatureSet` | ❌ | 三者互相矛盾（见 §2.3 表第 5–7 行）；探针 0 通过 |
| 同源测试：`tests/backend_parity.rs` L0/L1/L2 | ❌ | 文件不存在。`tests/` 只有 `integration.rs`(23,011 行) / `errors.rs` / `profile.rs` / `perf_regression.rs`；cross 相关 fn **41 个**，qemu L1 仅 4 个 fixture（`non_x86_qemu_runtime_{hello,control_flow,aggregate_abi,va_arg}`），零散而非表驱动 |
| 诊断一致：单一策略 + **对照测试** | 🟡 | `codegen/mod.rs:31 has_fatal_diagnostics()` 为唯一策略、`emit()` 全 arch 共用、旧 `!diagnostics.is_empty() 即 Err` 模式在 `src/codegen/**` **0 命中** ✅；但"同一 warning 程序 x86 与 aarch64 同过同挂"的对照测试 **不存在** ❌ |
| RV32 禁止 true + E0399 并存 | ✅ | `cli.rs:378` `Arch::RiscV32 => false`；`registry.rs:105` `Arch::RiscV32 => None`；`cli.rs:3707` 有断言；`emit()` 走 `Err(E0399)` 而非 panic |
| 卸载：registry 摘除 → has_backend=false → emit 臂删 → CI/文档同步 | 🟡 | `emit()` 已无 arch 臂（**无死臂风险** ✅，这是 §0.2 硬伤的真正解药）；但"改一处即全同步"未成立：仍需手改 `cli.rs` 清单 + `enabled_backends()` 硬编码 Vec + CI 文档三处 |

### 3.2 Part A（§3 拆分 x86_64）

| 方案条目 | 判定 | 精确事实 |
|----------|------|----------|
| §3.4 PR-A1 建目录、整文件进子模块壳 | ✅ | `359259d` 一次性把 `x86_64.rs` → `x86_64/mod.rs`（41,658 行原样），行为零变 |
| §3.4 PR-A2 抽出 `asm/*` | 🟡 **≈13%** | `asm/` 子树仅 1,568 行且含大量影子/孤儿；`mod.rs` 内 asm 家族仍 **134 个 `fn` / 10,657 行**（`emit_asm_instr` 27142+2,961、`build_asm_operand_map` 23989+647、`emit_resolved_asm_lines` 25557+377、`emit_asm_mov_inner` 30262+371、`parse_asm_reg` 26108+273、`build_asm_output` 24956+240…）+ 三个支持类型 ~900 行（`mod.rs:536–1452`） |
| §3.4 PR-A3 抽出 `abi` + call/frame | 🟡 | `abi/` 1,147 行（`call.rs` 738 = `emit_sysv_call` 族、`classes.rs` 298、`frame.rs` 91）✅；但 **`compile_func`(6839+951) / `compile_rel`(4550+1923) / `compile_exec`(3675+223) 全部仍在 `mod.rs`** → 帧与调用编排未动 |
| §3.4 PR-A4 抽出 `mem/addr` | 🟡 | `mem/` 仅 511 行 7 个 `fn`（`addr.rs` 422、`global.rs` 28、`load_store.rs` 51）；`mod.rs` 内 addr/load/store 相关仍有 **79 个 `fn` / 2,430 行**（`materialize_address` 13257+170、`emit_address_descriptor_with_index` 13113+117…）。§3.3 期望 5–6k，实际搬出 ~2% |
| §3.4 PR-A5 抽出 `special` | 🟡 | `special/` 1,010 行（`int128.rs` 507、`x87_floating_point.rs` 265、`sanitizer.rs` 197、`tls.rs` 31），`int128`/`tls` 已基本搬空（`mod.rs` 内 int128 仅剩 2 fn/50 行、tls 0 fn）✅；但 **vector 15 fn/909 行、float+x87 25 fn/1,051 行、complex 22 fn/1,463 行、sanitizer 相关 atomic 3 fn/582 行仍在 `mod.rs`** |
| §3.4 PR-A6 抽出 `parallel` + elf 收集 | 🟡 | `parallel/` 308 行 ✅（真实投产）；`elf_side/` **空目录**，reloc/符号收集仍分散在 `compile_rel`/`emit_pushsection_data`(2767+217)/`collect_exec_symbols`(3898+157) |
| §3.4 PR-A7 expr 内部再拆 | ✅ | `emit/expr/` 3,857 行分 9 个文件；`gen_expr` 已是 **103 行纯分发器**（`mod.rs:8154–8256`，逐 arm 委托，含 `_ =>` ud2 兜底）；`gen_stmt` **全量**搬至 `emit/stmt.rs`(845)，`mod.rs` 内已无 `fn gen_stmt`；树内 `unimplemented!/todo!` = **0**（5 处命中全为注释） |
| §3.4 PR-A8 测试分模块 + 文档行号→符号 | ❌ | `x86_64/tests/` 空目录，`mod.rs:35014–35077` 仍是唯一 `mod tests`（63 行）；文档 0 处修订（详见 §3.5 之 §3.7 行） |
| §3.6 可见性与死代码清理 | ❌ | `#[allow(dead_code)]/#[allow(unused*)]` 在 `src/codegen/` 共 13 处，其中 **7 处集中在 `x86_64/mod.rs:27–56` 的模块声明上**（对 `mod` 条目挂 `allow(unused_imports)` 属误用）；影子 struct/孤儿文件未清（§2.3） |
| §1.2/§4.5 非目标："一期不上 7 个 backend-* cargo feature" | ✅ | `Cargo.toml [features]` 只有 `gcc_src`。**未提前做不该做的事**（唯一例外：`features.rs` 已建但未接线，属"半成品"而非"越界"） |
| §3.5 mips64 组合复用模式 | ✅ | `Mips64Codegen { inner: MipsCodegen }` + `set_64bit()`，未抄 2k 行 |

**§3.2 目标目录完整性核对**（还缺哪些子模块）：

| 方案 §3.2 条目 | 现状 |
|----------------|------|
| `emit/frame.rs` | ❌ 缺（帧编排在 `mod.rs` 的 `compile_rel`/`compile_func`） |
| `emit/call.rs` | ❌ 缺（等价物在 `abi/call.rs`，命名不符） |
| `emit/builtin.rs` | ❌ 缺（`gen_builtin` 3,134 行 + `gen_fenv_builtin` 381 行无处可去） |
| `emit/instrument.rs` | ❌ 缺 |
| `emit/vector.rs` / `int128.rs` / `float_x87.rs` / `init.rs` / `sanitizer.rs` / `tls.rs` | 🟡 内容存在但**换名落在 `special/`、`emit/expr/init.rs`**，可接受（需在附录记录命名偏离） |
| `emit/program.rs`（`compile` 总编排） | ❌ 方案未列，但实际是 `mod.rs` 剩下的主体，需新增命名 |
| `asm/reg.rs` / `constraint.rs` / `plan.rs` | 🟡 仅有**未编译的孤儿文件** `asm/reg/mapping.rs`、`asm/constraint/parsing.rs`、`asm/planner/mod.rs`（影子） |
| `elf_side/reloc_collect.rs` / `parallel.rs` | ❌ 空目录 |
| `abi/win64.rs` | ❌ 缺（COFF 逻辑在 `x86_64/backend.rs:84` 直接调 `coff::build_coff_rel_x86_64`） |
| `tests/` | ❌ 空目录 |

### 3.3 Part B（§4 多架构功能一致）

| 方案条目 | 判定 | 精确事实 |
|----------|------|----------|
| §4.1 一期"适配"：`EmitOptions`/`CompileInput`/`CommonImage` | 🟡 | 三个类型齐备且 `EmitOptions` 与旧 `emit()` 17 参逐一对齐 ✅；**但 `CommonImage` 是 write-only 装饰品**：`emit()` 只用 `image.diagnostics`，随后调 `backend.build_object()` 从 `Codegen` **内部字段**重新组装 ELF/COFF（`x86_64/backend.rs:82–138`）。`compile()` 里那 34 行 `CommonImage{…}` 映射（含 3 个 `// TODO:` 未跟踪 local/imports/TLS offset）产出物无人消费 |
| §4.1 关键约束 2"打包点统一、ELF/COFF 一律在共享 `package()`" | ❌ **反向实现** | 独立 `package_image()` 未引入，结果不是"上收"而是**下沉到 6 份 `build_object()`**。同时 `build_aux_sections()` 被复制 **6 份**（arm/mips/mips64 各 72 行，`diff` 实测仅 **6 行不同**：形参类型名 + `ArchKind::Arm`/`Mips`；riscv64 107 行、aarch64 75 行、x86_64 122 行）→ §4.6 的 1c(LTO 上收)/1d(debug 钩子)/1e(package 上收) 三个子 PR **实质未做**，去重目标变成了增重 |
| §4.1.1 单一 `has_fatal()` + 对照测试 | 🟡 | 策略 ✅（`codegen/mod.rs:31,1260`）；对照测试 ❌ |
| §4.1.2 `emit()` 目标形态（registry 校验→compile→has_fatal→package→字节） | ✅ | `codegen/mod.rs:1236–1266` 31 行完全按序；缺后端 `Err(E0399)` 不 panic；`-S` 走 `render_asm()` |
| §4.2 `Feature`/`FeatureSet` + Tier + "宁可 E0xxx 不可静默 no-op" | ❌ | 枚举形状与方案一致 ✅，但：`features()` **0 调用点**、无 Tier 字段、无 E0xxx 门禁、5 arch 共用 `elf_only()` 虚报（含 `RiscVBorrowedRegs` 出现在 ARM/MIPS 集合里） |
| §4.2 `has_backend` 改 registry 同源 | 🟡 | `registry::has_backend()` 写好了但**是死码**；驱动仍读 `cli.rs:378` 手写清单（今天两边恰好一致，靠人肉） |
| §4.3 L0/L1/L2 同源测试 | ❌ | 无 `tests/backend_parity.rs`；无 `Feature × arch × fixture` 表；无"arch 过滤读同一 FeatureSet"；现有 41 个 `*_cross*` + 4 个 qemu fn 属"手工散点" |
| §4.3 黄金字节（10–20 TU 的 `.o`/`.text` sha256） | 🟡 | `golden_baseline/` 有 **7** 个 `.o`（下限的一半），且方案要的是 `.text` 级哈希而实为整文件哈希；**无生成/比对脚本挂点**（`hashes.txt` 手写，无脚本引用） |
| §4.5 注册表静态 match + 卸载清单 | 🟡 | `backend_for()` ✅、卸载清单只写在注释里（`registry.rs:87–93`），无脚本/测试保障 |
| §12.1 防腐（探针=真相源 + CI diff + PR checklist + 文档单一真相） | ❌ | 探针指向**不存在的 `tests/input/`**（实际 fixtures 在 `tests/fixtures/c/` 共 **340** 个 `.c`）→ 0 通过；md 输出格式损坏（`| x86 \| 64-unknown-linux-gnu_Atomics |`）；探针恒 exit 0；CI 无 diff 断言且 registry 步骤恒失败；`.github/PULL_REQUEST_TEMPLATE.md` **不存在**；`docs/backend_feature_matrix.md` 手写且与代码矛盾 |
| §12.2 语义单源试点（≥2 arch 移出私有 `gen_*`） | ❌ | `src/codegen/semantic/` 不存在。**但基础设施已在**：`src/semantics.rs`（519 行，`compute_lvalue_address` / `AddressDescriptor` / `MaterializeAction`）已被 x86_64 的 4 个文件消费（`mod.rs`、`emit/expr/{init,lvalue}.rs`、`mem/addr.rs`）—— **aarch64/riscv64/arm/mips 4 者 0 消费**。试点的"共享一半、接线零"，恰好差方案要求的那一步 |
| §12.4 Tier-1 产品清单 `docs/backend_tier1.md` | ❌ | 文件不存在（`ls` 证实） |
| §12.5 三级验收区分 | 🟡 | `BIG_BANG_MIGRATION_STATUS.md` 确实写了 Level A/B/C 清单（唯一有价值产出），但 **A 的两个未勾项（matrix=probe、parity L0）从未验证**，对外文档（README）仍按"全部完整"宣传 |
| §5.1 与内核 P0 错峰 / freeze 窗口 | ❌ 违反 | 5 个 commit 中有 2 个是巨型混提：`ccdb1b8` **85 文件 / +15,995 / −8,076**，把 `src/diagnostics.rs` 新增 `Diagnostic::new`（硬编码 `code:"E0500"`，且**全仓 0 调用点**）与 `src/link/lto_call_graph_stubs.rs` **525 行新特性文件**混进 "refactor(codegen)"；`359259d` **+44,855/−41,706**。方案 §3.1/§7.2 "纯移动与行为修改绝不混提" 未执行 |

### 3.4 §3.7 文档行号清单（PR-A8）

| 方案点名的文件 | 现状 | 判定 |
|----------------|------|------|
| `docs/architecture.md` | 第 **979** 行 `gen_stmt (codegen/x86_64.rs:7207)`、第 **994** 行 `gen_expr (codegen/x86_64.rs:7851)` —— 文件已不存在为 `.rs`，且 `gen_stmt` 实际在 `x86_64/emit/stmt.rs:11`、`gen_expr` 在 `x86_64/mod.rs:8154`；另有 449/628 行仍称"（`codegen/x86_64.rs`）" | ❌ 全失效 |
| `docs/array_index_addressing_refactor_plan.md` | **13** 处 `#L<数字>` | ❌ |
| `README.md` | 第 30 行"6 后端**全部完整**"、198 行"`has_backend()` 当前对全部 6 个 arch 返回 true"、306 行 `src/codegen/x86_64.rs::builtin_call`；1 处 `#L` | ❌ 且第 30/198 行与 §12.5"宣传不得掺水"直接冲突 |
| `docs/support_features.md` | 无 `x86_64.rs#` 引用 | ✅ 免改 |
| `docs/diagnostics.md` | 含旧 `x86_64.rs` 路径引用 | ❌ |
| 其它 `rg 'x86_64\.rs#' docs` | 合计 **29 处**旧路径引用分布在 **9 个**文件（architecture / array_index / backend_refactor_plan / cpp_native_backend / dcc-dual-arch / diagnostics / lua_compilation_status / va_arg_longdouble / README）；`#L` 合计 **22 处** | ❌ 0 处修订 |

### 3.5 加权完成度

| 维度 | 权重 | 自评 | 贡献 |
|------|------|------|------|
| §1.3 文件规模达标 | 15% | 21%（子模块占比） | 3.2 |
| §4.1 Backend 契约两阶段 + 打包上收 | 15% | 60%（trait 齐、package 反向下沉） | 9.0 |
| §4.1.2 `emit()` 形态 | 5% | 90% | 4.5 |
| §4.1.1 诊断统一 + 对照测试 | 5% | 70%（缺测试） | 3.5 |
| §4.2/§4.5 FeatureSet + registry 闭环 | 10% | 35% | 3.5 |
| §4.3 L0/L1/L2 parity + golden | 15% | 15% | 2.3 |
| §12.1 防腐 CI | 10% | 10% | 1.0 |
| §12.2 语义单源试点 | 5% | 0%（基础设施在、跨 arch 0） | 0 |
| §12.4 Tier-1 产品清单 | 5% | 0% | 0 |
| §3.7 文档行号 | 5% | 0% | 0 |
| §1.2 非目标边界守住 | 5% | 100% | 5.0 |
| §5.1/§7 提交卫生与 PR 切片 | 5% | 20% | 1.0 |
| **合计** | 100% | — | **≈ 33%** |

---
