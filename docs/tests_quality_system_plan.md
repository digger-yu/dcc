# DCC 测试体系全局改造方案

> 版本：**v1.0**（从「`tests/` 目录整理」升级为**全局面测试体系**）  
> 状态：方案稿 — 尚未按阶段开工。  
> 摸底：2026-09 · `tests/**`、`scripts/*test*`、`.github/workflows`、`docs/roadmap.md`  
> 关联：  
> - `docs/cpp_test_naming_unification_plan.md`（C++ 测试改名，**错峰**）  
> - `docs/backend_refactor_plan.md`（`backend_parity` / 探针矩阵，**错峰**）  
> - `docs/next_steps.md`（内核 P0 — 本方案定级 **P1/P2**）  
> 方案路径：**`docs/tests_quality_system_plan.md`**

---

## 0. 定位与诚实边界

### 0.1 本方案要解决什么

| 层次 | 目标 |
|------|------|
| **L-A 结构** | 消灭巨石文件、死 fixture、无公共 harness、根目录杂物 |
| **L-B 规则** | 命名/目录/「如何加一条测试」有唯一规则与 README |
| **L-C 度量** | 有测试总图：每类测什么、何时跑、谁负责 |
| **L-D 门禁** | 关键规则进 CI（硬失败），避免整理完回潮 |
| **L-E 索引** | 特性 → fixture → 测试 的可检索覆盖图（防「全局仍是黑盒」） |

### 0.2 不承诺什么（Out of scope）

- 不重写为 libtest-mimic / criterion 等测试框架（保持零额外依赖）  
- 不把 C++ 测试 `m17_*` 改名在本方案内做完（见关联案，只留接口与错峰）  
- 不重写 gcc-dg / torture / 内核套件本身  
- 不承诺「测完 = 语义与 GCC 全等」  
- 不替代 `docs/backend_refactor` 的架构一致性工程  

### 0.3 成功定义（三级，禁止混谈）

| 级 | 名称 | 含义 | 何时算过 |
|----|------|------|----------|
| **L-A** | 结构可导航 | 无 20k+ 行单测文件；无零引用 fixture 目录；harness 单一入口 | 阶段 1–3 |
| **L-B** | 规则可执行 | README + `check_fixtures` + PR 模板；新增用例有路径可循 | 阶段 0–1 + 4 |
| **L-C** | 体系可运营 | 总图与 `test_all`/CI 对齐；未引用 fixture 硬门禁；覆盖索引可用 | 阶段 4–5 |

对外/对内报告必须标明级别；**禁止**「目录搬完」当成「测试体系完成」。

---

## 1. 全局测试总图（现状 → 目标）

### 1.1 现状盘点

```text
                    ┌─────────────────────────────────────┐
                    │  质量入口（现状分散）                 │
                    └──────────────┬──────────────────────┘
       ┌──────────────┬───────────┼───────────┬──────────────┐
       ▼              ▼           ▼           ▼              ▼
  cargo test     errors snap   scripts/    workflows     手工/文档
  ~2416 #[test]  25+46 snap    test_all    e2e/gdg/      roadmap
  (unit≈1498     fixtures/     bootstrap   torture/      引用旧路径
   int≈916)      errors/       kernel_*    kernel-bench
       │              │           │           │
       └──────────────┴───────────┴───────────┘
                    无统一总图 / 无统一未引用检查
```

| 层 | 资产 | 规模（约） | 健康度 |
|----|------|------------|--------|
| 单元 | `src/**` `#[cfg(test)]` | ~1498 test | 良（分散但正常） |
| 集成黑盒 | `tests/integration.rs` | **21471 行 / 834 test / 776KB** | **差 — 巨石** |
| 错误快照 | `tests/errors.rs` + `fixtures/errors/` | 25 test + 46 snap | **优 — 范本** |
| Profile | `tests/profile.rs` | 53 test / 2141 行 | 中（依赖 `fixtures/c/profile.c`） |
| 性能 | `tests/perf_regression.rs` | 4 test | **优** |
| Fixture | `fixtures/{c,cpp,dev,errors,opt,gcc_tools,corpus}` | c346 / cpp54 / **dev88(0引用)** / … | **差 — dev 死肉 + c 平铺** |
| 外部套件 | gcc-dg、torture（`*_skip.txt` 在 tests 根） | 脚本驱动 | 中 — 与 cargo 文化分裂 |
| 自举 | `scripts/bootstrap.sh` + `fixtures/c/bootstrap_selftest.c` | 脚本 | 良 |
| 内核 | `kernel_full_check.sh` / qemu / prepare0… | 脚本 + CI 可选 | 良 — **产品 P0** |
| CI | e2e / gcc-dg / torture / kernel-bench | 4 workflow | 中 — 分层未文档化 |
| 杂项 | `tests/minimal_bar_test.c` | 几乎无引用 | **差** |

### 1.2 目标总图

```text
Tier 0  PR 门禁（必绿，Windows 也可跑）
        ├─ cargo test --lib
        ├─ cargo test --test integration --test errors --test profile
        ├─ cargo test --test perf_regression
        ├─ scripts/check_fixtures.sh     ← 升级：存在性 + 未引用 + 目录规则
        └─ （可选）rustfmt/clippy 若仓库已有约定

Tier 1  Linux CI（e2e.yml 扩展或保持 + 明确分层文档）
        ├─ Tier 0 全量
        ├─ gcc_dg（workflow 已有）
        ├─ torture（workflow 已有）
        └─ bootstrap 冒烟（可迁入 e2e 或单独 job）

Tier 2  内核 / 性能（产品 P0 与 benchmark）
        ├─ kernel_full_check / qemu_boot_matrix   ← next_steps P0
        └─ kernel-bench.yml

Tier 3  可选深测（本地或 scheduled）
        ├─ libiberty / gcc 源树相关
        └─ 全量 kernel STRICT_FULL_BUILD
```

**总图文档**：本方案 §1 即权威草稿；落地时同步进 `docs/ci_guide.md` 一节「测试分层」，并避免与 `scripts/README.md` 再各写一套。

### 1.3 与 `scripts/test_all.sh` 的关系

现状 `test_all.sh` 偏 examples 冒烟（arith/hello…），**不等于** cargo 全量。

**目标语义（改文档即可，改脚本为阶段 4）**：

| 入口 | 职责 |
|------|------|
| `cargo test` | Tier 0 程序化测试 |
| `bash scripts/check_fixtures.sh` | fixture 引用完整性（升级后含未引用） |
| `bash scripts/test_all.sh` | examples + 可选串起 cargo（薄编排，**不**再复制一份断言逻辑） |
| workflows | Tier 1/2 显式 job 名对应 §1.2 |

---

## 2. 已定决策（三项 + 扩展）

以下为**默认已选**，执行不再反复询问；若要改请在开工前书面推翻。

| # | 决策 | 结论 | 理由 |
|---|------|------|------|
| D1 | `fixtures/dev` | **`git mv` → `fixtures/_archive/dev/`**，修 roadmap 路径；1–2 个里程碑后再删 | 88 文件 0 个 rs 引用；可逆 |
| D2 | 拆 `integration.rs` | **单 target + `include!` 子文件** + 先抽 `tests/common/harness.rs` | 不改 CI 命令；可演进多 crate |
| D3 | fixture 主题子目录 | **一期只迁 `asm_*` → `fixtures/asm/`**；kernel/cross/m27 **二期** | 映射简单、收益最大；避免分类学膨胀 |
| D4 | `minimal_bar_test.c` | **删除**（无引用）或归档；默认删 | 游离文件 |
| D5 | `*_skip.txt` | → `tests/skip/` + 改 4 个 shell | 根目录减负 |
| D6 | `fixtures/c/comdat_test.cpp` | 移到 `fixtures/cpp/`（或改扩展名，看内容） | 目录=语言 |
| D7 | C++ `mNN_` 测试改名 | **不在本方案实施**；命名规范见关联案 | 避免双改冲突 |
| D8 | `m27_*.c` fixture 改名 | **二期**，与 C 测试改名一并 | 与 D3 边界一致 |
| D9 | 多 `tests/*.rs` crate | **一期不做**；终点未锁死，T3 用 include 已可迁 | |

---

## 3. 目标目录与 Rust 布局

### 3.1 目标树

```text
tests/
├── README.md                      # T0 必交付：如何跑、如何加测试、目录约定
├── common/
│   └── harness.rs                 # dcc / run_* / temp / 常用 assert（自 integration 抽出）
│
├── integration.rs                 # 壳：声明 harness + include! 子文件
├── integration/                   # 主题块（见 3.2）
│   ├── c_lang.rs
│   ├── c_asm.rs
│   ├── c_kernel.rs
│   ├── c_attr_builtin.rs
│   ├── c_pp_c23.rs
│   ├── c_link_debug.rs
│   ├── c_cross.rs
│   ├── c_sanitizer_profileish.rs  # 若 profile 已独立可减
│   ├── cpp_lang.rs                # 与 cpp 改名 PR 协调
│   ├── cpp_stdlib.rs
│   ├── cpp_runtime.rs
│   ├── opt.rs
│   ├── xfail.rs
│   └── glue_misc.rs               # 收尾未归类（有上限，防新杂物堆）
│
├── errors.rs
├── profile.rs
├── perf_regression.rs
├── backend_parity.rs              # 二期/并行：backend 方案引入；本方案只留位
│
├── skip/
│   ├── gcc_dg_skip.txt
│   └── torture_skip.txt
│
└── fixtures/
    ├── README.md                  # 分类规则 + 命名 + 禁止项
    ├── c/                         # 通用 C 正例（一期仍大平铺，除已迁 asm）
    │   └── …                      # 二期：lang/ abi/ attr/ kernel/ cross/ …
    ├── asm/                       # D3：全部 asm_*
    ├── cpp/                       # 保持功能名
    ├── errors/                    # 保持 .c + .stderr.snap
    ├── opt/                       # 保持
    ├── profile/                   # 可选：profile.c 迁出
    ├── gcc_tools/
    ├── corpus/
    └── _archive/                  # 死 fixture；禁止被 include_str!
        └── README.md
```

### 3.2 `include!` 约定

- 仅 `tests/integration.rs` 是 Cargo integration target；子文件**不要**再放在会被 cargo 当成独立测试的地方（放在 `tests/integration/` 目录下，**不**用 `tests/xxx.rs` 顶层重复 target）。  
- 文件头统一：

```rust
// included from tests/integration.rs — not a standalone cargo target
```

- `#[test]`、`#[cfg(unix)]` 保持原样；**不**在子文件写 `fn main` 或第二个 harness。  
- 子文件内 `include_str!("…")` 路径仍相对 **`integration.rs` 所在目录**（`tests/`），与现在一致——**迁文件时勿改成相对子目录**（Rust `include_str!` 相对当前文件，需统一策略）：

**路径策略（强制一种）**：

| 策略 | 做法 | 推荐 |
|------|------|------|
| P-a | 子文件仍在 `tests/integration/foo.rs`，`include_str!("../fixtures/…")` | 迁移时全改，清晰 |
| **P-b** | 子文件用 `include_str!("fixtures/…")` 会错（相对 integration/）→ **必须** `../fixtures/` | **采用 P-b：子文件一律 `../fixtures/…`** |
| P-c | 壳再 `include_str` 不现实 | 否 |

落地时：**T3 每搬一个块，同步改该块全部 `include_str!` 前缀为 `../fixtures/`**，并靠 `check_fixtures` 验证。

### 3.3 公共 harness 抽取清单（自 `integration.rs` 顶部）

迁到 `common/harness.rs`（路径属性或 `#[path]` 从 `integration.rs` 挂载）：

- `run_syntax_test` / `run_object_test` / `run_object_test_flags`  
- `run_asm_output_test`  
- `run_compile_and_run` / `run_compile_and_run_flags` / `run_exe`  
- `run_sanitized_binary`  
- `run_cpp_object_test`  
- `run_host_link_and_run` / `run_host_link_and_run_with`  
- `run_compile_o`（及仅一处使用的辅助）  

`profile.rs`：改为 `#[path = "common/harness.rs"]` 或保留薄封装，**删除重复的 `dcc()` 若 harness 已提供**。

---

## 4. Fixture 规则（权威）

### 4.1 目录=语言/角色

| 目录 | 放什么 | 禁止 |
|------|--------|------|
| `c/` | 通用 C 正例（一期可平铺） | `.cpp`、错误码主用例、死文件 |
| `asm/` | `asm_*.c/h` 及 asm 专用头 | 非 asm 的 lang 测试 |
| `cpp/` | `.cpp` C++ 正例 | `.c` |
| `errors/` | `eNNNN.c` + `eNNNN.stderr.snap`（及 W 码） | 正例功能测试 |
| `opt/` | 优化 pass 样例 | 散落进 `c/` 再开 opt |
| `profile/`（可选） | profile/offload 源 | |
| `gcc_tools/` | libiberty 探测组合 | |
| `corpus/` | 真实 `.o` 样本 | 源码 |
| `_archive/` | 无测试引用的历史件 | **任何 `include_str!`** |

### 4.2 命名

```text
<domain>_<feature>[_<qualifier>].c
```

- 允许：`asm_avx.c`、`attr_alias_var.c`、`generic_ptr.c`  
- **禁止新增**：`m\d+_`、`test_` 前缀（存量白名单：`_archive` 内全量；`c/m27_*` 二期清）  
- 错误码例外：仅 `errors/` 使用 `e\d{4}` / `w\d{4}`  

### 4.3 引用完整性

任一 `fixtures/**` 正式文件应满足 **至少一条**：

1. 某 `.rs` `include_str!` / `Path` 引用  
2. 某 `scripts/*.sh` 引用  
3. 外部套件清单（gcc-dg 等，白名单）  
4. 明确标记为 corpus 二进制  

否则 → 阶段 4 起 **CI 失败**（先 warn 一个周期）。

---

## 5. 分阶段路线图（比原 T0–T5 更硬）

### 阶段 0 — 总图与规则（文档，0.5–1 人日）

| 交付 | 说明 |
|------|------|
| `tests/README.md` | 分层（§1）、如何加测试、harness 用法、skip 位置 |
| `tests/fixtures/README.md` | §4 全文 |
| `docs/ci_guide.md` 增补 | 「测试分层」指向本方案 §1，**删除重复叙述** |
| 本文件定稿 | 决策 D1–D9 锁定 |

**DoD**：新贡献者只读 `tests/README` 能加一条 fixture+test 并知道跑什么命令。

### 阶段 1 — 去死肉与根目录（0.5–1 人日，行为零变）

| 步骤 | 动作 |
|------|------|
| 1.1 | D1：`fixtures/dev` → `_archive/dev` + `_archive/README.md` |
| 1.2 | D4：删 `minimal_bar_test.c`（`rg minimal_bar` 确认） |
| 1.3 | D5：`tests/skip/{gcc_dg,torture}_skip.txt`；改 `gcc_dg_test.sh`、`gcc_dg_summary.sh`、`torture_test.sh`、`torture_compile_test.sh`、`torture_execute_test.sh`、`torture_summary.sh` |
| 1.4 | D6：`comdat_test.cpp` → `fixtures/cpp/`（改引用若有） |
| 1.5 | 修 `docs/roadmap.md` 中 `fixtures/dev/builtins.c` → `fixtures/c/builtins.c` |
| 1.6 | 全库 `rg 'fixtures/dev\|minimal_bar'` 仅允许 `_archive` 说明 |

**DoD**：`cargo test` + `check_fixtures` + skip 脚本 `SKIP_FILE` 存在性检查通过；`rg fixtures/dev` 在 `tests/*.rs` 为 0。

### 阶段 2 — 抽 harness（1 人日）

| 步骤 | 动作 |
|------|------|
| 2.1 | 新增 `tests/common/harness.rs`（内容自 integration 顶） |
| 2.2 | `integration.rs`：`#[path = "common/harness.rs"] mod harness;` + `use harness::*`（或等价） |
| 2.3 | `profile.rs` 收敛重复 |
| 2.4 | **禁止**同时搬测试体或改 fixture 路径 |

**DoD**：全量 `cargo test` 与改前同级通过；`integration.rs` 行数下降约数百行；无测试逻辑变更。

### 阶段 3 — 拆 `integration.rs`（3–6 人日，多 PR）

**顺序（每 PR 只动一块）**：

1. `c_cross.rs` — `*_cross_compile`、预定义宏交叉  
2. `c_link_debug.rs` — gnu hash / eh_frame / dwarf / ifunc / vdso / build-id  
3. `c_sanitizer.rs` — sanitizer 相关（若有）  
4. `c_asm.rs` — 全部 asm 主题（大块）  
5. `c_kernel.rs` — percpu/vfs/kernel 风格  
6. `c_attr_builtin.rs` + `c_pp_c23.rs`  
7. `opt.rs`（m16 等）  
8. `xfail.rs`  
9. **C++ 块** `cpp_*.rs` — **与 `cpp_test_naming` 错峰或合并为单主题 PR**  
10. `c_lang.rs` + `glue_misc.rs` 收尾  

**规则**：

- 每 PR：纯移动 + 必要的 `../fixtures/` 前缀；**零语义改动**  
- 门禁：`cargo test --test integration` + `check_fixtures`  
- 目标：`integration.rs` 壳 **< 300 行**；无子文件 > ~8k 行（asm 可临时豁免并记 TODO 再切）  

**DoD**：无单文件 integration 源 > 8k（或例外清单 ≤1 且有拆分 issue）；壳仅 include 列表。

### 阶段 4 — 硬门禁 + 有限 fixture 主题化（1–2 人日）

| 步骤 | 动作 |
|------|------|
| 4.1 | **D3**：`fixtures/c/asm_*` → `fixtures/asm/`；批量改 rs/scripts/docs |
| 4.2 | 升级 `scripts/check_fixtures.sh`：① 引用存在 ② **未引用 → exit 1**（先 `CHECK_FIXTURES_ALLOW_UNREFERENCED=1` 过渡一个 PR）③ `_archive` 被引用 → 失败 ④ 新增 `m\d+_` fixture → 失败（白名单存量） |
| 4.3 | `scripts/test_all.sh` 顶部注释改为调用/指向 Tier 0 清单；或增加 `--cargo` 薄封装 |
| 4.4 | `docs/ci_guide.md` 与 §1.2 对齐 job 列表 |
| 4.5 | `.github/PULL_REQUEST_TEMPLATE.md` 勾选：新测试已挂 fixture、已进主题子文件、已跑 `check_fixtures` |

**DoD**：CI 或本地必跑命令中 `check_fixtures` 非 0 会挡合并（至少 e2e job）；`rg 'fixtures/c/asm_'` 为空。

### 阶段 5 — 覆盖索引与运营（1–2 人日 + 持续）

| 交付 | 说明 |
|------|------|
| `docs/test_coverage.md` | **能力 → fixture → 测试函数** 表；优先：C23、内核构造、C++ 核心、优化 pass |
| 生成方式 | 先手维护 1 屏子集；中期 `scripts/gen_coverage.sh` 扫 `include_str!` + `fn` 名 |
| 与 backend 矩阵 | 不重复：语言/行为用本索引；**arch 能力**用 `backend_feature_matrix` |
| 运营 | 每合并「新语言特性」必须触达索引一行（PR 模板第 5 项） |

**DoD**：抽 10 个对外宣称的特性，能在 1 分钟内从索引找到测试名。

### 阶段 6 — 二期（明确不在一期 DoD）

- [ ] `fixtures/c` 其余主题化：`lang/` `abi/` `attr/` `kernel/` `cross/` `builtin/`  
- [ ] `m27_*` fixture 改名（联动 C 侧测试改名）  
- [ ] `integration` 真·多 crate（若 include 痛感强）  
- [ ] `dev` 归档删除  
- [ ] gcc-dg/torture 与 cargo 结果汇总到同一 JSON/summary（可选）  
- [ ] 覆盖索引全量自动  

---

## 6. 阶段依赖与项目 P0 / 关联案

```text
阶段0 文档 ──┬── 阶段1 死肉 ── 阶段2 harness ── 阶段3 拆文件 ── 阶段4 门禁+asm
              │                                              │
              └── check_fixtures 增强可与阶段1并行草稿        └── 阶段5 索引
```

| 外部 | 规则 |
|------|------|
| 内核 P0 | 红灯排障周 **不开** 阶段 3 大拆；阶段 1–2 可低风险合 |
| `cpp_test_naming` | 改 `fn`/label 与 T3 cpp 块 **同 PR 或严格一周互斥** |
| `backend_refactor` | 新增 `backend_parity.rs`、探针 — **不**塞进 integration 壳乱放；与阶段 4 CI 可共用 check 风格 |
| freeze | 任一「纯移动」PR 合并前，不开同文件功能 PR |

**优先级**：本方案 **P1/P2**；不得压过 `next_steps` 内核 P0。

---

## 7. 门禁与命令（权威）

### 7.1 本地 / PR Tier 0

```bash
cargo build --tests
cargo test
bash scripts/check_fixtures.sh
```

### 7.2 阶段 4 后 check_fixtures 能力

```text
[必] 引用的 fixture 存在
[必] 非 _archive 正式文件至少一处引用（或白名单）
[必] _archive 不可被 tests/scripts 新引用
[必] 禁止新增 fixtures/**/m\d+_*.c（白名单存量）
[必] 语言目录与扩展名（c/*.cpp 等）
```

### 7.3 CI 对齐建议（改 workflow 为阶段 4）

| Job | 内容 |
|-----|------|
| e2e | 现有 cargo test **+ `check_fixtures.sh`** |
| gcc-dg / torture | 路径改为 `tests/skip/…` |
| kernel-bench | 不变（Tier 2） |

### 7.4 单测过滤习惯（写入 README）

```bash
cargo test --test integration asm_        # 主题子文件后仍可用 fn 名子串
cargo test --test errors
cargo test --lib
bash scripts/check_fixtures.sh
```

---

## 8. 风险与缓解

| 风险 | 缓解 |
|------|------|
| `include_str!` 相对路径在拆分后全错 | **P-b**：子文件统一 `../fixtures/`；每 PR `check_fixtures` |
| `include!` 与 rustfmt/RA | 文件头注释；壳是唯一 target |
| 与 C++ 改名撞车 | 阶段 3 cpp 块错峰；映射表见关联案 |
| 未引用规则误伤 corpus/gcc_tools | 白名单 + 目录豁免（`corpus/` `gcc_tools/`） |
| asm 大搬迁漏 docs | `rg 'fixtures/c/asm_'` 为门禁清单 |
| skip 路径改漏 shell | 阶段 1 用 `rg torture_skip\|gcc_dg_skip` 清零 |
| 范围膨胀（想一次分类 331 文件） | D3 锁定；阶段 6 才主题化 |
| 只搬目录不建索引，仍「全局黑盒」 | 阶段 5 索引 + PR 模板强制触达 |
| 归档当永久垃圾场 | `_archive` README + 阶段 6 删除条件（无 docs 引用） |

---

## 9. 验收清单（总）

### 9.1 阶段 0–2（结构）

- [ ] `tests/README.md` + `fixtures/README.md` 存在且与本方案一致  
- [ ] `fixtures/dev` 不在活跃路径；roadmap 无错误 `dev/` 引用  
- [ ] 无 `tests/minimal_bar_test.c`  
- [ ] skip 表在 `tests/skip/`；shell 路径已改  
- [ ] `common/harness.rs` 承载 `run_*`；profile 不再整段复制  

### 9.2 阶段 3（拆分）

- [ ] `integration.rs` 为壳；主题文件列表完整  
- [ ] 全量 `cargo test --test integration` 绿  
- [ ] 子文件 `include_str!` 均 `../fixtures/…`  
- [ ] C++ 块与改名案无冲突记录  

### 9.3 阶段 4–5（彻底性）

- [ ] `fixtures/asm/` 存在；`fixtures/c/asm_*` 为空  
- [ ] `check_fixtures` 未引用 = 失败（或记录中的过渡开关）  
- [ ] PR 模板含 fixture/主题/索引勾选  
- [ ] `docs/ci_guide.md` 分层与 §1.2 一致  
- [ ] `docs/test_coverage.md` 至少 10 个特性可导航  

### 9.4 三级成功定义

- [ ] **L-A** 结构可导航 — 阶段 3 DoD  
- [ ] **L-B** 规则可执行 — 阶段 0+1+4 DoD  
- [ ] **L-C** 体系可运营 — 阶段 4–5 DoD  

---

## 10. 与「中等方案」的差异（为何这是更全局、更彻底）

| 维度 | 原 T0–T5（目录整理） | 本 v1.0 全局方案 |
|------|----------------------|------------------|
| 范围 | tests 树怎么搬 | **总图 + 搬 + 门禁 + 索引 + 运营** |
| 决策 | 3 项口头 | **D1–D9 书面锁定** |
| 门禁 | 建议 warn | **阶段 4 起硬失败 + 白名单** |
| `include_str!` 路径 | 未写死 | **P-b 强制** |
| 与 test_all/CI | 略提 | **§1.2 Tier 0–3 权威** |
| 覆盖可见性 | 无 | **阶段 5 索引** |
| 与三案关系 | 口头错峰 | **§6 表** |
| 二期边界 | 模糊 | **阶段 6 清单** |
| 成功定义 | 感觉不乱了 | **L-A/B/C 分级验收** |

仍未做、且不应假装已做：机器码/ABI 级语义等价、多 arch 执行全覆盖（归 backend parity）、内核全树本身（P0）。

---

## 11. 建议执行序（批准后）

```text
P0  阶段 0 文档（0.5–1d）
P1  阶段 1 死肉 + skip + roadmap 路径（0.5–1d）
P2  阶段 2 harness（1d）
P3  阶段 3 多 PR 拆 integration（3–6d，cpp 块错峰）
P4  阶段 4 asm 搬迁 + check_fixtures 硬门禁 + CI + 模板（1–2d）
P5  阶段 5 覆盖索引子集（1d）→ 持续
—— 以上为一期 ——
P6  阶段 6 二期按需
```

**合并策略**：只合 **纯移动 / 纯文档 / 单主题行为**；不与功能修 bug 混 PR。

---

## 12. 提交信息示例

```text
docs(tests): add tests README and fixture rules; lock D1–D9
chore(tests): archive fixtures/dev; drop minimal_bar; move skip lists
refactor(tests): extract common harness from integration.rs
refactor(tests): split integration into include! theme modules
refactor(fixtures): move asm_* under fixtures/asm
feat(ci): hard-fail check_fixtures on unreferenced fixtures
docs: test tier map in ci_guide; test_coverage seed table
```

---

## 13. 一句话结论

> **一期做「可导航 + 可回滚 + 不回潮的门禁」；二期做「分类学与多 crate」；始终用 Tier 总图和覆盖索引把 cargo、snap、外部套件、内核脚本收成一张全局地图——目录整理只是其中一层，不是终点。**

---

*方案版本：v1.0 — 全局测试体系定稿。执行阶段 0 后回填 README 链接与 check_fixtures 行为差异说明。*
