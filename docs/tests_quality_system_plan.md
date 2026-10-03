# DCC 测试体系终态方案 v2.0（一次性彻底落地）

> 版本：**v2.0**（**废除阶段制**：不再有阶段 0–6 的渐进路线图）
> 状态：**方案定稿，待一次性实施**（本文件即实施契约；实施=单次提交，同时满足 L-A/L-B/L-C）
> 摸底：2026-09-28 · 全部数据为**实测值**（命令与行号可复核），非估算
> 关联：
> - `docs/cpp_test_naming_unification_plan.md`（C++ 测试改名，**错峰接口见 §12**）
> - `docs/backend_refactor_plan.md`（`backend_parity` / 探针矩阵，**错峰接口见 §12**）
> - `docs/next_steps.md`（内核 P0 — 本方案定级 **P1/P2**）
> 方案路径：**`docs/tests_quality_system_plan.md`**

---

## 0. 一页结论

### 0.1 v2.0 相对 v1.0 的根本变化

| 维度 | v1.0 | **v2.0** |
|------|------|----------|
| 时间结构 | 阶段 0–5 + 二期阶段 6 | **无阶段**。终态 = 单次提交的全部必要条件 |
| 验收 | L-A 在阶段 1–3、L-B 在阶段 0+1+4、L-C 在阶段 4–5（分批达成） | **L-A / L-B / L-C 同批达成**，缺一即视为未完成 |
| 原二期事项 | `fixtures/c` 其余主题化、`m27_*` 改名、归档删除、覆盖自动化 → 推到阶段 6 | **全部纳入本次**。这些是"半途"的来源，推到二期就等于永远不到来 |
| 拆分范围 | 只拆 `integration.rs` 的 C 主题，C++ 专项 `.rs` 另议 | **24k 行完整拆分** + 4 个 C++ 专项顶层 target 并入主题文件 |
| 门禁 | 先 warn 一个周期，`CHECK_FIXTURES_ALLOW_UNREFERENCED` 过渡开关 | **直接 exit 1**，无过渡开关；存量债在本次逐个判定并清零（§7.4） |
| 现状描述 | "已完成"勾选块（14 项打勾） | **勾选块整段作废**，替换为实测终态差距表（§1） |
| 覆盖索引 | 手维护 10 行子集 + "future work" | **脚本全量生成**（`scripts/gen_coverage_index.sh`）+ 人工校订 |

**为什么敢一次性**：本方案的全部改动都是**可机械复核的移动**（文本搬迁 + 路径前缀 + 脚本），不含语义设计；`git revert` 单提交即完整回滚。分阶段反而制造了 v1.0 里已经出现的那个后果——**半截状态被当成完成状态写进文档**。

### 0.2 本方案解决什么 / 不解决什么

解决（同批）：

| 级 | 名称 | 终态判据（详版见 §2） |
|----|------|----------------------|
| **L-A** | 结构可导航 | `tests/integration.rs` 是 < 300 行的壳；**932** 个 `#[test]`（= 906 现存 + 4 丢失回纳 + 22 顶层并入）全部位于 15 个主题子文件——此数是 **守恒下界**（一条不许丢）；S5（§7.4 零引用夹具判定）补写的真实用例会推高实测总数（2026-09-28 实测 = 970 = 932 + 38）；harness 单一入口且**可见性正确** |
| **L-B** | 规则可执行 | README/PR 模板/`check_fixtures.sh` 三者描述一致且**门禁真的会红**（5 类反向注入全部 exit 1） |
| **L-C** | 体系可运营 | 覆盖索引全量生成并含 C++ 5 主题；CI 有 `check_fixtures` 步骤；未引用 fixture = 0（非白名单） |

不承诺（Out of scope，且**不假装已做**）：

- 不重写为 libtest-mimic / criterion；保持零额外测试框架依赖
- 不做多 crate 拆分（保持单 integration target + `include!`）
- 不重写 gcc-dg / torture / 内核套件本身
- 不承诺"测完 = 与 GCC 语义全等"（机器码/ABI 级等价归 `backend_parity`）
- `tests/fixtures/probe/`（19 文件）归 backend 案，不在本方案的 fixture 主题化范围
- C++ `mNN_` **测试函数**改名归 `cpp_test_naming` 案（本方案只搬文件、只改 **fixture 文件**名）

---

## 1. 现状真实性校正（实测）

v1.0 的「现状快照 — 已完成」勾选块与仓库实际不符。以下 9 条为**复核过的实测事实**，v2.0 的一切动作都从这里出发。

### 1.1 七条假完成（必须修）

**(1) `tests/integration` target 当前不能编译 —— 744 个错误**

```text
$ cargo check --test integration --message-format short
error: could not compile `dcc` (test "integration") due to 808 previous errors; 2 warnings emitted
  按错误码：E0425 × 743，E0308 × 1
  警告：tests/common/harness.rs:2:29 unused import: `Stdio`
        tests/integration.rs:6:5  unused import: `harness::*`      ← glob 什么都没导进来
```

根因：`tests/common/harness.rs`（469 行）里 19 个函数**全部是私有 `fn`**（`grep -c pub tests/common/harness.rs` = 0），而 `tests/integration.rs:4-6` 是

```rust
#[path = "common/harness.rs"]
mod harness;
use harness::*;
```

Rust 的 glob import 只能导入**对导入方可见**的项；`harness` 模块内的私有项对父模块不可见 → `run_exe`/`run_compile_and_run`/`run_object_test`/… 在 scope 中不存在 → E0425。缺失符号分布（次数=调用点）：

| 符号 | E0425 次数 | 符号 | E0425 次数 |
|------|-----------|------|-----------|
| `run_exe` | 242 | `run_asm_output_test` | 9 |
| `run_compile_and_run` | 206 | `assert_exit_code` | 9 |
| `run_object_test` | 95 | `elf64_sections` | 6 |
| `run_compile_and_run_flags` | 35 | `qemu_runtime_target` | 5 |
| `elf64_section_data` | 17 | `compile_legacy_asm_object` | 5 |
| `run_gcc_cross_check` | 14 | `tool_exists` | 3 |
| `run_object_test_flags` | 11 | `cpp_gcc_cross_check` | 2 |
| `run_sanitized_binary` | 10 | `run_gcc_two_tu_cross_check` | 1 |
| `readelf_relocations` | 10 | `assert_runtime_fails_with` | 1 |
| `cpp_coroutine_cross_check` | 1 | | |

**(2) harness 搬迁不仅丢了可见性，还搬坏了 1 个函数**

`tests/common/harness.rs:451-468` 的 `compile_legacy_asm_object` 声明返回 `(Vec<u8>, String)`，但函数体以 `assert!(dump.status.success(), …)` 结尾——**丢了 `disasm` 的绑定和元组返回表达式**，因此 E0308 at `harness.rs:451:59`。调用侧（`tests/integration.rs:3422/3431/3459/3477/3492`）都在 `let (text, disasm) = …`。

**(3) asm fixture 搬了文件、没搬引用 —— 67 处悬挂**

`tests/fixtures/c/asm_*` 现存 0 个文件、`tests/fixtures/asm/` 已有 64 个，但引用面：

| 文件 | 残留 `fixtures/c/asm` 处数 |
|------|---------------------------|
| `tests/integration.rs` | **67** |
| `docs/test_coverage.md` | 7 |
| `docs/tests_quality_system_plan.md` | 6 |
| `scripts`/`tests/fixtures/README.md` | 1 |
| `docs/support_features.md` | 3 |
| `docs/roadmap.md` | 2 |

**(4) `scripts/check_fixtures.sh` 是假绿门禁**

实测本地直接跑：

```text
$ bash scripts/check_fixtures.sh
=== 扫描 tests/*.rs 中的 fixture 引用 ===
=== 扫描 CI/scripts 中的 examples 引用 ===
find: ‘/tests/fixtures’: 没有那个文件或目录
✅ 所有 fixture 检查通过。          ← 一个文件都没扫，仍然 exit 0
```

缺陷逐条（行号见 §8.1）：`$REPO_ROOT` 全文未赋值、`$errors` 未初始化、`referenced_fixtures`/`INTENTIONALLY_MISSING` 未 `declare -A`、Pattern 3 使用未定义的 `$source_ref`、只遍历 `tests/*.rs` 顶层、`include_str!("../fixtures/…")` 形式匹配不到、任何前置失败都走到"✅ 通过"分支。**在这个脚本被重写之前，v1.0 里任何"硬门禁"声称都没有意义。**

**(5) `tests/integration/` 下 3 个文件是死文件**

`asm_lang.rs`(3 行)、`cpp_lang.rs`(3 行)、`c_lang.rs`(74 行 / 4 个 `#[test]`)。全库 `grep -rn 'include!' tests/` = **0 处**，即没有任何文件包含它们；Cargo 也不会把 `tests/integration/` 当成 target（只有 `tests/*.rs` 是 target）。它们写的 `use crate::common::harness::*;` 路径也是错的（按现有 `#[path]` 挂载应为 `crate::harness`）。

更严重：`c_lang.rs` 里那 4 个测试在 `integration.rs` 中**已不存在同名 `fn`**（实测：`arch_x86_wrapper_preserves_supported_kernel_flags`、`constant_builtin_and_static_assert_pass_through_typed_ir`、`failed_static_assert_is_a_hard_error`、`unknown_typedef_is_not_coerced_to_int` 在 `integration.rs` 均为 0 命中）——它们不是副本，而是**已被静默搬出编译范围的真实丢失用例**。S0 必须把它们重新纳入活跃路径（分别归 `c_kernel.rs` 与 `c_pp_c23.rs`/`c_lang.rs`）。

**(6) 拆分根本没开始**

`tests/integration.rs` 仍是 **24011 行 / 906 个 `#[test]` / 438 处 `include_str!`**（`fixtures/c/` 338、`fixtures/cpp/` 88、`fixtures/opt/` 10、`../examples/` 2）+ 30 处 `env!("CARGO_MANIFEST_DIR")` 运行时路径。

**(7) C++ 专项 target 没并进来；CI/文档/模板三件套不存在**

v1.0 声称"已整合进 `tests/integration/{cpp_abi,cpp_hir,cpp_cli,legacy_strip,g12_variadic}.rs`"，实际 5 个文件仍在顶层：`cpp_abi_gcc_cross_check.rs`(252)、`cpp_hir_legacy_correspondence.rs`(408)、`driver_cxx_native_cli_contract.rs`(194)、`legacy_strip_not_in_main_path.rs`(40)、`test_g12_variadic_templates.rs`(262)。
另：`.github/PULL_REQUEST_TEMPLATE.md` **不存在**；`docs/ci_guide.md` 只有 §1–§5 推送/查看操作，**无「测试分层」**；`scripts/gen_coverage_index.sh` **不存在**；`.github/workflows/*.yml` 中 `check_fixtures` **0 处**（仅 `Makefile:18` 的 `test-all` 调用，而 e2e job 跑的是 `cargo test --release`）。

### 1.2 两条真完成（保留为终态，不重做）

- `tests/fixtures/_archive/dev/`（88 文件 + `_archive/README.md`）已就位；`tests/`+`scripts/`+`src/` 中 `fixtures/dev` 引用 = 0（仅 `_archive/README.md` 与本方案的说明性文字）
- `tests/minimal_bar_test.c` 已删除；`tests/skip/{gcc_dg_skip,torture_skip}.txt` 已就位且 6 个 shell 路径已改（`gcc_dg_test.sh:32`、`gcc_dg_summary.sh:170`、`torture_test.sh:21`、`torture_compile_test.sh:21`、`torture_execute_test.sh:21`、`torture_summary.sh:132`）

### 1.3 存量债规模（决定"一次性"到底要不要处理它们）

| 债项 | 实测 | 本次处置 |
|------|------|----------|
| `fixtures/c/` 零引用 fixture | **24** | 逐个判定：补断言 or 归档，不留 TODO（§7.4-A） |
| `fixtures/cpp/` 零引用 fixture | **2**（`enhanced_sfinae.cpp`、`g11_template_templates.cpp`） | 同上 |
| `fixtures/c/mNN_*` 前缀 fixture | **16**（全部 `m27_*`，`integration.rs` 内 40 处引用） | 改名 + 同步引用（§7.4-B） |
| 顶层独立 cargo target | 10 个 `.rs`（`integration` 24011 / `profile` 2297 / `errors` 1044 / …） | 4 个 C++ 专项并入主题（§S7） |
| 主题目录 | `c`291 / `asm`64 / `cpp`89 / `errors`92 / `opt`10 / `probe`19 / `gcc_tools`4 / `corpus`3 / `c_xfail`2 / `cpp_abi_*` 2-6-2-4-4-11 | 终态见 §4 |

---

## 2. 终态定义（L-A / L-B / L-C 同批）

**同一个提交内**必须同时成立。任何"L-A 先合、L-B 后补"的做法就是把 v1.0 的失败重演一遍。

| 级 | 机器判据（可直接执行，见 §14 汇总） |
|----|--------------------------------------|
| L-A | `wc -l < tests/integration.rs` < 300；`ls tests/integration/*.rs \| wc -l` = **15**；`grep -c '#\[test\]' tests/integration/*.rs` 之和给出**两个量**：**守恒下界 = 932**（= 906（现存量）+ 4（§1.1(5) 丢失用例）+ 22（S7 并入：g12 8 + hir 6 + cli 6 + legacy_strip 2），一条测试都不许丢，低于即 FAIL）与**实测总数 = 932 + S5（§7.4 零引用夹具判定后新增的真实用例）数**（932 只是「S5 之前」的守恒值，故 S5 完成后实测总数必然 > 932；2026-09-28 磁盘实测 = 970 = 932 + 38）；`grep -c 'include!' tests/integration.rs` = 15；`cargo check --test integration` 0 error |
| L-B | `check_fixtures.sh` 5 类反向注入全部 exit 1（§8.3）；`grep -rn 'fixtures/c/asm' .` = 0；`grep -rn 'crate::common::harness' tests/` = 0；`ls tests/fixtures/ \| grep -x dev` 为空；`Makefile`+`e2e.yml` 都出现在 `check_fixtures` 调用点 |
| L-C | `docs/test_coverage.md` 由 `scripts/gen_coverage_index.sh` 生成、含全部主题归属与 C++ 5 主题小节；非白名单目录零引用 fixture = 0；`docs/ci_guide.md` 有「测试分层」；PR 模板 4 项勾选存在 |

**报告纪律**：对外/对内汇报必须给出上面三条判据的**实测输出**，不接受"目录已整理"这类描述性结论。

---

## 3. 一次性落地清单（S0–S13）

> 编号只是**同一提交内的操作序**（先修绿灯再搬，才能区分"搬运引入的失败"与"原本就有的失败"），**不是可分批合入的阶段**。任何一条未完成 = 整个方案未完成。

| # | 动作 | 主要文件 | 完成判据 |
|---|------|----------|----------|
| **S0** | **绿灯复位**：`harness.rs` 19 个函数/结构加 `pub(crate)`；补回 `compile_legacy_asm_object` 丢失的 `disasm` 绑定与返回；删 `tests/integration/` 下 3 个死骨架，并把 `c_lang.rs` 的 **4 个已丢失测试**搬回活跃主题文件 | `tests/common/harness.rs`、`tests/integration/` | `cargo check --test integration` 0 error；`cargo test --release` 通过数为**基线**（含那 4 个回归），后续 S 步骤不得使该基线下降 |
| **S1** | **harness 完备化**：跨主题复用助手（§6.2 表）从 `integration.rs` 移入 `harness.rs` | `tests/common/harness.rs`、`tests/integration.rs` | `integration.rs` 顶层仅剩壳声明；`harness.rs` 无 `unused import`/`dead_code` 警告 |
| **S2** | **完整拆分**：现存量 906 个 `#[test]`（加 S0 回纳的 4 个、S7 并入的 22 个，共 **932**）按 §5 分配表迁入 15 个 `tests/integration/<theme>.rs`；`integration.rs` 退化为壳（`use` + `#[path] mod harness; use harness::*;` + 15 行 `include!`） | `tests/integration.rs`、`tests/integration/*.rs` | 壳 < 300 行；主题文件 test 数之和 = 932；单文件 ≤ 4000 行 |
| **S3** | **路径策略 P-b**：子文件内 `include_str!("fixtures/…")` → `include_str!("../fixtures/…")`（438 处）、`include_str!("../examples/…")` → `../../examples/…`（2 处）；`env!("CARGO_MANIFEST_DIR")` 系列（30 处）**不改** | `tests/integration/*.rs` | `grep -rn 'include_str!("fixtures/' tests/integration/` = 0；全量 test 绿 |
| **S4** | **asm 引用收口**：67 处 `fixtures/c/asm_*` → `fixtures/asm/asm_*`，含 `docs/roadmap.md`(2)/`docs/support_features.md`(3)/`docs/test_coverage.md`(7)/`tests/fixtures/README.md`(1) | `tests/integration/*.rs`、4 个 docs | `grep -rn 'fixtures/c/asm' .` = 0 |
| **S5** | **零引用 fixture 逐个判定**：24(C)+2(cpp) → 有断言价值则在对应主题文件补 1 个 `#[test]`；确属历史则 `git mv` 进 `_archive/`（§7.4-A） | `tests/fixtures/`、主题文件 | `check_fixtures` 未引用报错数 = 0（无过渡开关） |
| **S6** | **`mNN_` fixture 改名**：16 个 `m27_*` 按 `<domain>_<feature>` 改名并同步全部引用（§7.4-B） | `tests/fixtures/c/`、主题文件 | `ls tests/fixtures/c \| grep -cE '^m[0-9]+_'` = 0；`check_fixtures` 命名项绿 |
| **S7** | **C++ 专项整合**：`test_g12_variadic_templates.rs`(8 test)、`cpp_hir_legacy_correspondence.rs`(6)、`driver_cxx_native_cli_contract.rs`(6)、`legacy_strip_not_in_main_path.rs`(2) 共 **22 个测试**并入 `tests/integration/cpp_{lang,runtime,stdlib}.rs`；`cpp_abi_gcc_cross_check.rs`(8 test) **保留独立 target**（目录发现型 GCC 差分，需独立 CLI 过滤） | `tests/`、`tests/integration/` | 顶层 `tests/*.rs` = 6（`integration`/`errors`/`profile`/`perf_regression`/`reproducibility`/`cpp_abi_gcc_cross_check`）；并入的 22 个 test 计入 L-A 总数 932 |
| **S8** | **重写 `scripts/check_fixtures.sh`**（§8.2 规格，纯 bash） | `scripts/check_fixtures.sh` | 正向全绿 exit 0；§8.3 五类反向注入各 exit 1；不存在"扫不到文件仍打印 ✅"的路径 |
| **S9** | **门禁接线**：`check_fixtures.sh` 进 `e2e.yml` 的 "Unit + integration tests" job（独立 step，非 `continue-on-error`）；`Makefile` `test:` 目标末尾追加；`scripts/test_all.sh` 顶部注释指向 §9 Tier 0 权威清单 | `.github/workflows/e2e.yml`、`Makefile`、`scripts/test_all.sh` | `grep -rn check_fixtures .github/workflows Makefile` 均命中；CI 失败即挡合并 |
| **S10** | **覆盖索引生成**：新增 `scripts/gen_coverage_index.sh`（bash+awk，§10.1），产出 `docs/test_coverage.md` 全量表 | `scripts/gen_coverage_index.sh`、`docs/test_coverage.md` | 索引行数 ≥ 现手工版；含主题归属列；脚本可重复执行且幂等 |
| **S11** | **C++ 5 主题索引**：`docs/test_coverage.md` 固定小节 template/eh/rtti/virtual/overload（+ lifecycle），逐条 fixture → `check_group()` 组名 → 判据 | `docs/test_coverage.md` | 5 小节齐备；`cpp_abi_gcc_cross_check.rs:176/182/188/194/204/213` 的组名与索引一一对应 |
| **S12** | **README 写真**：`tests/README.md` 修正不存在的文件名（`cpp_namespace.cpp`/`cpp_stdlib_native`/`cpp_coroutine.rs`）与"阶段 4 起"措辞；`tests/fixtures/README.md` 去 `fixtures/c/asm_*` 示例；两者只描述终态 | `tests/README.md`、`tests/fixtures/README.md` | 两份 README 的目录树与磁盘 `ls` 完全一致；无阶段措辞 |
| **S13** | **文档一致性**：`docs/ci_guide.md` 新增「测试分层」一节（单向引用本方案 §9）；新建 `.github/PULL_REQUEST_TEMPLATE.md` | `docs/ci_guide.md`、`.github/PULL_REQUEST_TEMPLATE.md` | 分层表与 §9 无差异；模板含 4 项勾选 |

---

## 4. `tests/` 目录终态

```text
tests/
├── README.md                          # 如何跑 / 如何加测试 / 目录约定（终态描述，无阶段措辞）
├── common/
│   └── harness.rs                     # pub(crate) 化的公共 harness（S0/S1 后完整）
├── integration.rs                     # 壳：< 300 行，仅 use + #[path] mod harness + include! 列表
├── integration/                       # 主题子文件（被 include!，非独立 target）
│   ├── c_lang.rs            c_asm.rs            c_kernel.rs
│   ├── c_attr_builtin.rs    c_pp_c23.rs         c_abi_vararg.rs
│   ├── c_link_debug.rs      c_cross.rs          c_sanitizer.rs
│   ├── cpp_lang.rs          cpp_stdlib.rs       cpp_runtime.rs
│   ├── opt.rs               xfail.rs            glue_misc.rs     # 共 15 个
├── errors.rs                          # 保持不变（范本：25 test + 快照）
├── profile.rs                         # 保持独立 target；改为复用 harness（S1 附项）
├── perf_regression.rs                 # 保持
├── reproducibility.rs                 # 保持
├── cpp_abi_gcc_cross_check.rs         # 保持独立 target（目录发现型 GCC 差分）
├── backend_parity.rs                  # ← 由 backend 案引入；本方案只保证不与壳冲突
├── skip/
│   ├── README.md   gcc_dg_skip.txt   torture_skip.txt     # 已在终态
└── fixtures/
    ├── README.md
    ├── c/          # 通用 C 正例（S5 清死肉、S6 清 mNN 前缀）
    ├── asm/        # 64 个 asm_*（已在；S4 把引用接上）
    ├── cpp/        # C++ 正例
    ├── errors/     # eNNNN.c + eNNNN.stderr.snap（保持，不动）
    ├── opt/        # 优化 pass 样例
    ├── gcc_tools/  libiberty 探测      corpus/  真实 .o 二进制样本
    ├── probe/      后端探针（归 backend 案，本方案不主题化）
    ├── c_xfail/    已知预期失败        cpp_abi_{template,eh,rtti,virtual,overload,lifecycle}/
    └── _archive/   # 禁止被 include_str!/join 引用（S5 归档目标落点）
```

**顶层 `tests/*.rs` target 终态 = 6 个**（S7 后）：`integration.rs`、`errors.rs`、`profile.rs`、`perf_regression.rs`、`reproducibility.rs`、`cpp_abi_gcc_cross_check.rs`。

---

## 5. `include!` 约定与主题分配

### 5.1 壳文件形态（终态骨架）

> 下例是**骨架示意**（只固定三件事：导入区、harness 挂载、`include!` 列表）。按 §5.2
> 规则 2，“导入权归壳”，所以壳实际携带的是**全部主题子文件需要的导入集**（含
> `dcc::cpp::*` / `dcc::codegen` / `dcc::link` 等库侧类型与 `std::io::Write`、
> `#[cfg(unix)] std::os::unix::fs::PermissionsExt` 这类 trait），以使主题子文件内部
> **一个 `use` 也不出现**（含函数体内局部 `use`）：实测口径为
> `grep -cE '^[[:space:]]*(use|mod) ' tests/integration/*.rs` 全为 0。

```rust
// tests/integration.rs — 壳：唯一 cargo integration target
use std::fs;
use std::process::{Command, Stdio};

#[path = "common/harness.rs"]
mod harness;
use harness::*;

include!("integration/c_lang.rs");
include!("integration/c_asm.rs");
include!("integration/c_kernel.rs");
include!("integration/c_attr_builtin.rs");
include!("integration/c_pp_c23.rs");
include!("integration/c_abi_vararg.rs");
include!("integration/c_link_debug.rs");
include!("integration/c_cross.rs");
include!("integration/c_sanitizer.rs");
include!("integration/cpp_lang.rs");
include!("integration/cpp_stdlib.rs");
include!("integration/cpp_runtime.rs");
include!("integration/opt.rs");
include!("integration/xfail.rs");
include!("integration/glue_misc.rs");
```

### 5.2 子文件硬性规则

1. 首行统一注释：`// included from tests/integration.rs — not a standalone cargo target`
2. 子文件内**禁止**出现任何 `use` / `mod` / `fn main`（导入权与 harness 挂载权归壳）。理由：`include!` 是文本级粘贴，多个子文件各写一遍 `use harness::*;` 会造成同名项 glob 重名歧义（E0659），且 `crate::common::harness` 这类错误路径正是当前 3 个死文件的成因。
3. `#[test]` / `#[cfg(unix)]` / `#[cfg(feature = "gcc_src")]` 原样保留，**不改断言、不改 label、不新增 `#[ignore]`**。
4. 路径一律 P-b：`include_str!("../fixtures/…")`；运行时路径继续用 `env!("CARGO_MANIFEST_DIR")`（与文件位置无关，禁止改成相对路径）。
5. 单文件 ≤ 4000 行；超限按二级主题再切（`c_asm.rs` → `c_asm_x86.rs` + `c_asm_generic.rs`），仍走 `include!`。
6. `glue_misc.rs` 上限 **60 test / 1200 行**。这是防止"新杂物堆"的硬约束：名称法初分落 glue 的 218 个必须按 fixture 路径与断言主体逐条重投到其余 14 个主题文件。

### 5.3 分配表（名称法初分 → 内容法终稿）

初分依据 `#[test]` 函数名**有序首匹配**（先命中即归属，避免二义）。数值为实测近似，存在重叠（如 `m27_attr_*` 同时命中 attr 与 mNN 规则），**内容法终稿必须让各列 test 数之和恰为 906**。

| 主题文件 | 归属规则（按序） | 初分 test | 初分行 | 校正要求 |
|---|---|---|---|---|
| `c_asm.rs` | `^asm_` / `^s_output` / avx,mmx,x87,bmi,cmov,cmpxchg,goto | 128 | 2527 | 关联 `fixtures/asm/` 64；超 4000 则二级切 |
| `cpp_lang.rs` | `^cpp_`,`^cxx` ∩ template,typeid,coroutine,modules,namespace（非 stdlib/runtime 子类） | 87 | 1486 | 与改名案同 PR（§12） |
| `cpp_runtime.rs` | `^cpp_` ∩ runtime,abi,vtable,dtor,ctor,eh,catch,throw,virtual,link | 25 | 1083 | 吸收 `driver_cxx_native_cli_contract.rs` |
| `cpp_stdlib.rs` | `^cpp_` ∩ stdlib,chrono,string,fstream,filesystem | 4 | 74 | 吸收 `test_g12_variadic_templates.rs`（若已属 lang 则并入 `cpp_lang.rs`） |
| `c_kernel.rs` | kernel,percpu,static_key,objtool,paravirt,vmlinux,kcov,kasan,kcsan,`alt_` | 47 | 1102 | 含 `assert_kernel_corpus_object` 调用者 |
| `c_attr_builtin.rs` | `^attr_`,`^format_attr`,attribute,`^atomic` | 70 | 977 | 16 个 `m27_attr_*` fixture 改名后落此 |
| `c_pp_c23.rs` | `^c23_`,`^c11_`,`^c89_`,`^pp_`,pragma,`_Static`,typeof,trigraph,elifdef,embed | 60 | 809 | |
| `c_abi_vararg.rs` | `^va_`,va_list,va_arg,va_copy,sseup | 33 | 509 | ABI 密集，单列（与 backend ABI 案共享判据） |
| `c_cross.rs` | cross,aarch64,`^arm_`,riscv,mips,qemu,non_x86,windows,darwin,triple | 58 | 2091 | 与 backend parity 探针**不重复**：本方案只做编译期/链接期断言 |
| `c_link_debug.rs` | dwarf,eh_frame,ifunc,vdso,comdat,section,reloc,symbol,`^elf`,dyn,hash,mmd,debug | 54 | 2226 | `elf64_*` 助手全部依赖 harness（§6.2） |
| `c_sanitizer.rs` | san,ubsan,asan,gcov,profile,pgo,offload,pch,incremental,depfile | 29 | 920 | 与独立 `profile.rs` 的边界：CLI 面留 `profile.rs`，codegen 面进主题 |
| `opt.rs` | lto,ssa,fold,inline,`^o[0-3]_`,dce,pass,devirt,vector,optim | 34 | 581 | |
| `xfail.rs` | `^xfail`, `^objtool_xfail` | 2 | ~40 | + `fixtures/c_xfail/` 2 文件 |
| `c_lang.rs` | 通用 C 语义（struct/array/enum/typedef/qual_type/静态局部等） | 59 + mNN 余量 | ~900+ | `^m\d+_` 82 个按语义重投到其余主题文件 |
| `glue_misc.rs` | 兜底 | **218 → ≤ 60** | **4411 → ≤ 1200** | 实测落 glue 的簇：`semantic_layer` 6、`static_local` 4、`qual_type` 4、`pipe_input` 4、`linux_header` 4、`cpu_feature` 4、`nested_block` 3、`multi_file` 3 |

合计校验：初分各列之和因规则重叠不等于 906（名称法近似）；**内容法终稿必须让 15 个文件的 `#[test]` 数之和满足两个量：守恒下界 = 932** = 906（现存量）+ 4（`c_lang.rs` 丢失用例回纳）+ 22（S7 从 4 个顶层 target 并入）——一条都不许丢；**实测总数 = 932 + S5 新增用例数**，因为 932 是「S5 之前」的守恒值，§7.4 给零引用夹具补写的真实用例会继续推高总数（新增来自 §7.4 判定；2026-09-28 磁盘实测 = 970 = 932 + 38）。

---

## 6. harness 契约

### 6.1 可见性（S0 的核心修复）

`tests/common/harness.rs` 中所有被 integration 使用的项必须 `pub(crate)`（在 integration target 这一独立 crate 内，`pub` 与 `pub(crate)` 效果相同，统一用 `pub(crate)`）。终态形式：

```rust
pub(crate) fn run_exe(path: &std::path::Path) -> i32 { … }
pub(crate) struct Elf64Sym { pub(crate) name: String, … }
```

同时：
- 删除 `harness.rs` 内未使用的 `Stdio` 导入（现为 `harness.rs:2:29` 警告）
- 补回 `compile_legacy_asm_object`（`harness.rs:451-468`）丢失的 `disasm` 与返回：函数尾必须是
  `let disasm = String::from_utf8_lossy(&dump.stdout).to_string(); (text, disasm)`
- 壳文件里 `use harness::*;` 不得再触发 `unused import` 警告（作为 S0 完成的旁证）

### 6.2 助手分流表（实测调用点计数）

`tests/integration.rs` 现有顶层非 `#[test]` 项 **25 个**（24 个 `fn` + 1 个 `struct Elf64Sym`），下表 23 行（`elf64_syms`/`Elf64Sym`/`elf64_read_cstr` 三个 ELF 符号助手合为一行）。**被 ≥ 2 个主题复用 → 进 harness；单主题专用 → 随主题留子文件。**

| 助手（当前行号） | 调用点 | 去向 |
|---|---|---|
| `run_cpp_object_test`(148) | 63 | **harness** |
| `cpp_std20_exit_code_cross_check`(7640) | 21 | **harness**（跨 cpp_* 三文件） |
| `dcc_compile_module`(12829) | 20 | **harness** |
| `run_host_link_and_run`(226) | 24 | **harness** |
| `assert_bytes_present`(11) | 15 | **harness** |
| `assert_disasm_present`(18) | 14 | **harness** |
| `elf64_syms`(65) + `struct Elf64Sym`(48) + `elf64_read_cstr`(59) | 11 / 3 | **harness** |
| `elf64_func_symbols`(130) | 8 | **harness** |
| `host_link_cpp_object_and_run`(166) | 10 | **harness** |
| `host_link_objects_and_run`(12850) | 9 | **harness** |
| `elf64_undefined_symbols`(138) | 6 | **harness** |
| `cpp_coroutine_cross_check`(12212) | 7 | **harness** |
| `run_compile_o`(4584) | 5 | **harness** |
| `run_cpp_with_mode`(23876) | 5 | **harness** |
| `assert_kernel_corpus_object`(5420) | 4 | **harness** |
| `path_decision_in`(23908) | 5 | **harness** |
| `run_host_link_and_run_with`(230) | 3 | **harness** |
| `gcc_src_tree_or_skip`(362) | 3 | **harness** |
| `extract_function_body`(2772) | 3 | **harness** |
| `elf64_dynamic_needed`(22) | 2 | **harness** |
| `gxx_native_cpp_exit_code`(198) | 2 | **harness** |
| `object_symbol_size`(23834) | 2 | **harness** |
| `host_cc_available`(23867) | 2 | **harness** |

结论：**25 个全部进 harness**（无单主题专用者）。故 `integration.rs` 壳的终态就是 §5.1 骨架，不含任何 `fn` 定义。harness 体量预估 469 + ~450 ≈ **900–1000 行**；若超过 1200 行，按 `harness/mod.rs` + `harness/{run,elf,cpp}.rs` 二级分文件（仍用 `#[path]` 挂载，不改壳的 `use harness::*;`）。实测落点：阈值规则已触发并落地，但入口与子模块名与预想不同——入口按 `#[path = "common/harness.rs"]` 保持 **`tests/common/harness.rs`**（962 行）而非 `harness/mod.rs`，因子模块按实际职责定为 **`harness/{elf,driver_cli,cpp_frontend}.rs`**（244 / 50 / 307 行，合计 1563 行）；由于是 `#[path]` 挂载而非目录模块，`harness.rs` 内引用子模块时必须显式写 `harness/` 前缀（`#[path = "harness/elf.rs"] mod elf;`），不能省略路径。

`profile.rs` 附项：其自带 `dcc()` + `fixtures_dir()`（`profile.rs:11`、`:17`）与 harness 的 `run_exe`/`CARGO_MANIFEST_DIR` 逻辑重叠，收敛为复用 harness；**但不改动 profile 的断言**。

---

## 7. Fixture 规则（权威）

### 7.1 目录 = 语言 / 角色

| 目录 | 放什么 | 禁止 |
|------|--------|------|
| `c/` | 通用 C 正例（现 316 件，含 `c/{va_arg,va_arg_mix,va_arg_xfail}/` 二级子目录与子目录 README） | `.cpp`、asm 专用件、零引用死件、`m\d+_` 前缀 |
| `asm/` | `asm_*.c/h` 与 asm 专用头（现 64 件） | 非 asm 的语言测试 |
| `cpp/` | `.cpp` C++ 正例（现 89 件） | `.c` |
| `errors/` | `eNNNN.c` + `eNNNN.stderr.snap`（含 W 码，现 92 件） | 正例功能测试 |
| `opt/` | 优化 pass 样例（10 件） | 散落进 `c/` |
| `c_xfail/` | 已知预期失败（终态实测：0 件夹具，仅留 README 作为存放约定——`flag_clobber_frame.c`、`ld_global_read.c` 两个已修件均已按判据转正到 `c/`） | 被当成正例引用 |
| `cpp_abi_{template,eh,rtti,virtual,overload,lifecycle}/` | GCC 差分 ABI 夹具（2/6/2/4/4/11 件），按目录名被 `cpp_abi_gcc_cross_check.rs` 动态发现 | 手工在 `.rs` 里加硬编码清单 |
| `gcc_tools/` | libiberty 探测组合（4 件） | 未登记进白名单 |
| `corpus/` | 真实 `.o` 二进制样本（3 件） | 源码 |
| `probe/` | 后端能力探针（19 件，归 backend 案） | 本方案主题化改动 |
| `_archive/` | 无测试引用的历史件（现 93 件 = `dev/` 88 + `asm/` 1 + `c/` 3 + README） | **任何 `include_str!` / `.join()` 引用** |

> 实测补注（2026-09-28）：件数均为磁盘实测（`find tests/fixtures/<dir> -type f | wc -l`），
> 与本文初稿的“摸底值”出入来自 S5/S6 的判定结果（改名、归档、转正）。`c/` 的二级子目录
> 仍逐件受 UNREFERENCED 约束（caller/callee 差分件必须被 `.rs` 显式字面量引用，不得靠
> 目录整体豁免）；目录与白名单的权威清单以 `tests/fixtures/README.md` 为准。

### 7.2 命名

```text
<domain>_<feature>[_<qualifier>].c
```

- 合法：`asm_avx.c`、`attr_alias_var.c`、`builtin_trap.c`、`generic_ptr.c`
- **禁止**：新增 `m\d+_` / `test_` 前缀；`errors/` 独占 `e\d{4}` / `w\d{4}`
- 取消 v1.0 的"存量白名单 + 二期清"说法：**本次清零**（§7.4-B）

### 7.3 引用完整性（四条满足其一，否则 CI 红）

1. 某 `.rs` 的 `include_str!` / `.join("…")` / `Path` 引用
2. 某 `scripts/*.sh` 引用
3. 外部套件清单（gcc-dg / torture 目录），须登记在 `check_fixtures.sh` 白名单数组
4. `corpus/` 二进制样本（整目录豁免）

扫描面（新定义，`check_fixtures.sh` 与 `gen_coverage_index.sh` 共用）：
`tests/*.rs`、`tests/integration/*.rs`、`tests/common/*.rs`、`scripts/*.sh`、`.github/workflows/*.yml`、`examples/`。

### 7.4 存量债处置（S5 / S6 的具体对象）

**(A) 24 个 `fixtures/c/` 零引用件** —— 逐个判定，不留 TODO：

```text
arm_member_offset.c        attr_hot_pure.c           attr_malloc.c
attr_mode.c                attr_transparent_union.c  btf_qualifiers.c
builtin_misc.c             c11_std_headers.c         cfi_directives.c
double_args.c              field_aligned_attr.c      incbin_test.c
include_level.c            long_double_precision.c   naked_nonx86.c
profile_gcov.c             shf_merge_strings.c       simd_vector_xmm.c
static_local_arrays.c      stmt_auto.c               stmt_typeof.c
sync_atomic.c              tls_model_attr.c          typedef_fn.c
```

判定标准（写进 `tests/fixtures/README.md`）：

| 判据 | 处置 | 落点 |
|------|------|------|
| 可编译且断言点明确（如 `attr_malloc.c`、`builtin_misc.c`、`sync_atomic.c`） | **补 1 个 `#[test]`** 进对应主题文件 | `c_attr_builtin.rs` / `opt.rs` / `c_pp_c23.rs` … |
| 依赖外部工具或目录（`shf_merge_strings.c`、`cfi_directives.c`、`incbin_test.c`） | 补 test 且带 `#[cfg(unix)]`/`tool_exists` 守卫 | `c_link_debug.rs` |
| 与现有件语义重复，或需已废弃特性 | `git mv` → `fixtures/_archive/`（并在 `_archive/README.md` 记一行原因） | `_archive/` |

**(B) 2 个 `fixtures/cpp/` 零引用件**：`enhanced_sfinae.cpp`、`g11_template_templates.cpp` → 归 `cpp_lang.rs` 新增测试（模板相关，语义与 ABI 主题不同源，不进 `cpp_abi_*`）。

**(C) 16 个 `m27_*` fixture 改名映射**（同步 `integration.rs` 内 40 处引用）：

| 现名 | 新名 | 现名 | 新名 |
|---|---|---|---|
| `m27_attr_alias.c` | `attr_alias.c` | `m27_attr_ifunc.c` | `attr_ifunc.c` |
| `m27_attr_alias_invalid.c` | `attr_alias_invalid.c` | `m27_attr_warning.c` | `attr_warning_call.c` |
| `m27_attr_alias_var.c` | `attr_alias_var.c` | `m27_attr_error.c` | `attr_error_call.c` |
| `m27_attr_alias_weak.c` | `attr_alias_weak.c` | `m27_attr_deprecated.c` | `attr_deprecated_call.c` |
| `m27_attr_ctor_priority_invalid.c` | `attr_ctor_priority_invalid.c` | `m27_debugtrap_test.c` | `builtin_debugtrap.c` |
| `m27_fortify_chk.c` | `builtin_fortify_chk.c` | `m27_lowlevel_builtins.c` | `builtin_lowlevel.c` |
| `m27_lowlevel_simple.c` | `builtin_lowlevel_simple.c` | `m27_static_chain_test.c` | `builtin_static_chain.c` |
| `m27_trap_test.c` | `builtin_trap.c` | `m27_unreachable_test.c` | `builtin_unreachable.c` |

> 三个 `*_call.c` 后缀是为避开已存在的 `attr_deprecated.c`（`attr_` 前缀目录下实测已有 20 件）；`attr_alias*.c`、`attr_ifunc.c`、`builtin_{trap,unreachable,debugtrap,lowlevel,static_chain,fortify_chk}.c` 已核对无同名冲突。
> 注：**测试函数名**（`fn m27_attr_alias…`）的改名属 `cpp_test_naming` 案的姊妹范围（C 侧改名），本方案只搬文件与改 fixture 名；若 `fn m27_*` 名称与新 fixture 名不一致，允许，但必须在 `docs/test_coverage.md` 索引里可检索（§12）。

---

## 8. `scripts/check_fixtures.sh` 重写

### 8.1 现脚本致命缺陷（189 行，逐条带行号）

| # | 缺陷 | 行号 | 后果 |
|---|------|------|------|
| 1 | `$REPO_ROOT` **全文从未赋值** | 用于 `:17`、`:50`、`:144-146`、`:148`、`:168`、`:172` | `for test_file in "/tests"/*.rs` 空展开 → **一个测试文件都没扫** |
| 2 | `$errors` 未初始化就自增 | 首次 `:62` | 语义依赖环境；配合 1 永远为 0 |
| 3 | `referenced_fixtures` / `INTENTIONALLY_MISSING` 未 `declare -A` | 写入 `:86`，读取 `:13`、`:111`、`:174` | `[[ -v … ]]` 恒假 → 未引用检查与豁免全部失效 |
| 4 | `$source_ref` 未定义（应为 `$basename`） | `:86`、`:126`、`:135` | 报错信息里的定位为空 |
| 5 | 只扫 `tests/*.rs` 顶层 | `:50` | 拆分后的 `tests/integration/*.rs` 完全不在门禁面内 |
| 6 | Pattern 1 只匹配 `include_str!("fixtures/…")` | `:59` | 匹配不到 P-b 的 `include_str!("../fixtures/…")` → 拆分后存在性检查静默失效 |
| 7 | `find` 失败被丢弃，仍走成功分支 | `:168` + `:186` | **假绿**：`find: '/tests/fixtures'` 照样打印 "✅ 所有 fixture 检查通过" |
| 8 | 未检查目录/扩展名规则 | 全文 | `c/*.cpp`、`_archive` 被引用等违规无门槛 |

**结论**：在此脚本重写前，任何"阶段 4 硬门禁已完成"的声称都无意义。

### 8.2 新脚本规格（纯 bash，禁 python；约 150–200 行）

必备骨架（消除假绿的 5 个硬要求）：

```bash
#!/bin/bash
set -o pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ -d "$REPO_ROOT/tests/fixtures" ]] || { echo "FATAL: repo root 解析失败: $REPO_ROOT"; exit 1; }
errors=0
declare -A referenced_fixtures INTENTIONALLY_MISSING
declare -a WHITELIST_DIRS=(tests/fixtures/corpus tests/fixtures/gcc_tools tests/fixtures/probe)
scanned=0                                  # 扫描到的源文件数，用于反假绿
```

1. **扫描面**：`tests/*.rs`、`tests/integration/*.rs`、`tests/common/*.rs`、`scripts/*.sh`、`.github/workflows/*.yml`、`examples/**`。`scanned == 0` → `FATAL + exit 1`。
2. **引用提取 4 种形式**：`include_str!("fixtures/…")`、`include_str!("../fixtures/…")`、`.join("x.c"/"x.s"/"x.cpp")`、`"tests/fixtures/…"` 字面量；`ErrorCase` 的 `name:`+`fixture_ext:` 动态构造保留（现 `:67-76` 逻辑）。
3. **检查 5 项，全部 exit 1 级**：
   - ① 被引用的 fixture 必须存在（缺失 → `MISSING`）
   - ② 非白名单目录下每个 `.c/.s/.cpp/.h` 必须被引用 ≥ 1 次（未引用 → `UNREFERENCED`）
   - ③ `_archive/` 下任何文件被引用 → `FORBIDDEN-ARCHIVE-REF`
   - ④ 新增 `m\d+_` 前缀 fixture（相对 `git ls-tree HEAD` 的增量）→ `FORBIDDEN-NAME`
   - ⑤ 目录-扩展名一致：`c/` 内不得有 `.cpp`，`cpp/` 与 `cpp_abi_*/` 内不得有 `.c`，`asm/` 内只允许 `asm_*` 前缀 → `DIR-MISMATCH`
4. **过渡开关彻底删除**：不再有 `CHECK_FIXTURES_ALLOW_UNREFERENCED`。存量债由 §7.4 一次性清零。
5. **退出语义**：`errors > 0` → 打印 `共 N 个问题` + `exit 1`；**任何**提前失败路径（root 解析、扫描数为 0、`find` 非零）→ `exit 1` 且**不得**打印成功文案。
6. 输出全 ASCII 前缀标签（`MISSING:`/`UNREFERENCED:`/…），便于 grep 断言；中文仅用于表头说明。

### 8.3 反向自测：`scripts/check_fixtures_selftest.sh`（新增，L-B 的机器判据）

5 类注入必须**逐个 exit 1**，脚本用 `trap` 保证清理、结束时 `git status --porcelain tests/fixtures` 必须为空：

| 注入 | 期望首个标签 |
|------|--------------|
| 在 `tests/integration/c_lang.rs` 追加 `include_str!("../fixtures/c/zz_nonexistent.c")` | `MISSING` |
| `touch tests/fixtures/c/zz_unreferenced.c` | `UNREFERENCED` |
| 某 `.rs` 引用 `tests/fixtures/_archive/dev/builtins.c` | `FORBIDDEN-ARCHIVE-REF` |
| `touch tests/fixtures/c/m99_new_thing.c` | `FORBIDDEN-NAME` |
| `cp tests/fixtures/cpp/auto_deduce.cpp tests/fixtures/c/zz_bad_dir.cpp` | `DIR-MISMATCH` |

```bash
bash scripts/check_fixtures_selftest.sh          # 5/5 PASS，且自身 exit 0
git status --porcelain tests/ | grep -c zz        # 必须为 0（无残留）
```

---

## 9. 门禁与命令（权威 Tier 表）

```text
Tier 0  PR 必绿（本地等价：make test）
        ├─ cargo build --tests
        ├─ bash scripts/cargo_test_summary.sh   # 跑 cargo test --release；除逐 target 行外
        │                                       #   还给测试项总数 / 失败 / 忽略与两份名单
        ├─ bash scripts/check_fixtures.sh        ← 硬失败，无开关
        └─ bash scripts/check_fixtures_selftest.sh   ← 门禁本身有门禁

Tier 1  Linux CI（.github/workflows）
        ├─ e2e.yml: Build dcc / Unit + integration tests / Reproducible output / End-to-end compile and run
        │            └─ 在 "Unit + integration tests"(e2e.yml:44) 追加 check_fixtures step
        ├─ gcc-dg.yml（SKIP_FILE 指向 tests/skip/，已在终态）
        ├─ torture.yml（同上）
        └─ check_feature_matrix.yml（backend 案，与本方案无耦合）

Tier 2  内核 / 性能：kernel_full_check.sh、qemu_boot_matrix、kernel-bench.yml（next_steps P0，优先级高于本方案）
Tier 3  可选深测：libiberty / gcc 源树（`--features gcc_src`）、STRICT_FULL_BUILD
```

入口职责（写进 `tests/README.md`，不再各文档各写一套）：

| 入口 | 现状 | 终态 |
|------|------|------|
| `Makefile:14 test:` | 仅 `cargo test --release` | 追加 `bash scripts/check_fixtures.sh`（失败即红）。**实测落点**：`make test` 逐字跑完 Tier 0 全部四条（`cargo build --tests` → `cargo test --release` → `check_fixtures.sh` → `check_fixtures_selftest.sh`），因为本 §9 开头就写着“Tier 0 PR 必绿（本地等价：make test）” |
| `Makefile:16-18 test-all:` | `test` + `check_fixtures.sh` | 保留，另加 selftest。**实测落点**：`test-all: test` + `bash scripts/test_all.sh`（门禁不在这里重复跑），同时把 `scripts/test_all.sh` 从没有任何调用点的孤儿入口接入 |
| `scripts/test_all.sh` | examples 冒烟，与 cargo 无关 | 顶部注释指向本 §9；只做薄编排，不复制断言逻辑；调用点 = `make test-all` |
| `scripts/cargo_test_summary.sh` | 不存在（原先只能逐 target 数 `test result:` 行） | Tier 0 第 2 条的执行入口：跑 `cargo test --release -j2 -- --test-threads=2`，输出逐 target 表 + 跨 target 总计（测试项总数 / 通过 / 失败 / 忽略）+ 失败列表 + 忽略列表。**它只报告不判绿**：退出码一律透传 cargo；另有「逐行之和 == 总计」自核对，归属异常会点名而不是静默给一张对不上的表 |
| `e2e.yml:54-55` | `cargo test --release` 单条 | 同 job 增加 `check_fixtures.sh` step（**不带** `continue-on-error`）。**实测落点**：该 job 先加 `cargo build --tests` step，再加一个独立 step 依次跑 `check_fixtures.sh` 与 `check_fixtures_selftest.sh`；e2e job 的 5 个 example step 已去掉 `continue-on-error`（实测四个退出码 42/23/100/20 与诊断面均真通过，假绿不再成立） |
| `.github/PULL_REQUEST_TEMPLATE.md` | 不存在 | 新建，4 项勾选：新 fixture 已挂载测试 / 已进主题子文件 / 已跑 `check_fixtures` / 覆盖索引已加行 |

单测过滤习惯（拆分后仍然有效，写进 README）：

```bash
cargo test --test integration asm_          # include! 后 fn 名子串过滤不变
cargo test --test errors
cargo test --lib
```

---

## 10. 覆盖索引（L-C 的核心，从"手维护 10 行"升级为"全量生成"）

### 10.1 `scripts/gen_coverage_index.sh`（新增，bash + awk）

- **输入**：`tests/integration/*.rs`、`tests/*.rs`、`tests/fixtures/`（磁盘真值）
- **抽取**：`#[test]` → 紧随其后的 `^fn` 名（与所属主题文件）；`include_str!`/`.join("…c|s|cpp")` → fixture 路径；`cfg(unix)`/`feature` 标注为列
- **输出**：`docs/test_coverage.md` 的自动生成区（用 `<!-- BEGIN GENERATED -->` / `<!-- END GENERATED -->` 包裹，脚本只重写该区间 → **幂等**，人工校订区不被覆盖）
- **表格列**：`主题文件 | 测试函数 | fixture | 判据(compile/run/cross-check/sanitize) | Tier`
- **交叉校验**：脚本末尾对比 `生成的 test 行数` 与 `grep -c '#\[test\]' tests/integration/*.rs 之和`，不等则 exit 1（索引与代码不会悄悄漂移）

### 10.2 C++ 5 主题固定小节（S11）

`docs/test_coverage.md` 新增人工小节，逐条对齐 `tests/cpp_abi_gcc_cross_check.rs` 的 `check_group()` 调用点：

| 主题 | fixture 目录 | 文件数 | 驱动函数（行号） | 判据 |
|------|--------------|--------|------------------|------|
| template | `tests/fixtures/cpp_abi_template/` | 2 | `cpp_abi_template_group`(`:213`) | g++ 差分 + 退出码/输出比对 |
| exception handling | `tests/fixtures/cpp_abi_eh/` | 6 | `cpp_abi_eh_group`(`:204`) | 同上（含 `tmp_eh/` 交叉件） |
| rtti | `tests/fixtures/cpp_abi_rtti/` | 2 | `cpp_abi_rtti_group`(`:194`) | `typeid` 符号 + 差分 |
| virtual | `tests/fixtures/cpp_abi_virtual/` | 4 | `cpp_abi_virtual_group`(`:176`) | vtable/vtor 符号 + 差分 |
| overload | `tests/fixtures/cpp_abi_overload/` | 4 | `cpp_abi_overload_group`(`:188`) | mangled 符号 + 差分 |
| lifecycle（附带） | `tests/fixtures/cpp_abi_lifecycle/` | 11 | `cpp_abi_lifecycle_group`(`:182`) | ctor/dtor 次序差分 |

并写明**新增主题组的唯一路径**：在 `tests/fixtures/` 下建 `cpp_abi_<主题>/` 目录 → `cpp_abi_groups_are_all_covered`（`:62-68` 的目录发现）自动抓到，无需改清单；`check_fixtures.sh` 的 DIR-MISMATCH 与 UNREFERENCED 规则对该目录族豁免为"整目录被 `check_group` 引用"。

---

## 11. README 与文档一致性（S12/S13 检查点）

| 文档 | 现问题（实测） | 终态要求 |
|------|----------------|----------|
| `tests/README.md` | §2 目录树列了不存在的 `cpp_namespace.cpp`、`cpp_stdlib_native`、`cpp_coroutine.rs`、`c_xfail.rs`；Tier 0 已列 10 个 `--test`（S7 后只剩 6）；措辞含"阶段 4 起" | 目录树与 `ls` 一致；只描述终态；加"如何加一条测试"3 步 + selftest 说明 |
| `tests/fixtures/README.md` | 残留 1 处 `fixtures/c/asm_*` 示例；未记 §7.4 判定标准 | 目录=语言表 + 命名 + 引用完整性 + 零引用处置标准 |
| `docs/test_coverage.md` | 7 处 `fixtures/c/asm_*` 旧路径；表为手维护 10 行 | 生成区全量 + C++ 5 主题小节 + 交叉校验 |
| `docs/ci_guide.md` | 仅 §1–§5 推送操作，无分层 | 新增「测试分层」，单向引用本方案 §9 |
| `docs/roadmap.md` / `docs/support_features.md` | 2 + 3 处 `fixtures/c/asm_*` | 改指 `fixtures/asm/` |
| `.github/PULL_REQUEST_TEMPLATE.md` | 不存在 | 新建 4 项勾选 |

---

## 12. 与两个关联案的错峰接口

| 关联案 | 冲突面 | 接口规则（一次性方案下仍然需要） |
|--------|--------|-----------------------------------|
| `docs/cpp_test_naming_unification_plan.md`（C++ `mNN_*` **测试函数**改名） | 同一个 `integration.rs` cpp 块 + 同一批 `fn` 名 | 本方案的 S2/S7 cpp 搬迁与其改名 **必须同一 PR，或严格互斥一周**；改名只动 `fn` 名/label，不动 `include_str!` 路径 → 与 S3 正交。落点文件名映射：`cpp_lang.rs` / `cpp_runtime.rs` / `cpp_stdlib.rs` |
| `docs/backend_refactor_plan.md`（`tests/backend_parity.rs` + 探针矩阵，**目前该文件不存在**） | 新增顶层 target 与 fixture 目录 | `backend_parity.rs` 由 backend 案引入为独立 target；本方案保证三点：壳 `include!` 列表不被污染、`check_fixtures` 扫描面（`tests/*.rs` glob）天然覆盖新 target、`fixtures/probe/`(19) 与 arch 能力矩阵归 backend 案，本方案不重复索引（语言/行为索引用 §10，arch 能力用 `docs/backend_feature_matrix.md`） |
| freeze 纪律 | 混合 PR | 本方案提交窗口内**不夹带**任何功能/bug 修复；反之，功能 PR 不与本方案窗口重叠 |

优先级：本方案 **P1/P2**，不得压过 `docs/next_steps.md` 内核 P0 红灯排障。

---

## 13. 风险与缓解

| 风险 | 缓解 |
|------|------|
| 一次性大改撞上"分不清谁引入的失败" | S0 先取**基线通过数**（`cargo test --release` 记录 `N passed`），后续每步只与基线比较；`S0` 前树是 744 errors，不存在"历史绿"的假设 |
| `include_str!` 相对路径在拆分后全错（438 + 2 处） | 强制 P-b；`check_fixtures.sh` 新 Pattern 同时认 `fixtures/` 与 `../fixtures/`，MISSING 立即红 |
| `env!("CARGO_MANIFEST_DIR")` 被误改成相对路径（拆分前 30 处） | 规则：运行时路径**一律不改**；grep 断言 `grep -c 'CARGO_MANIFEST_DIR' tests/integration/*.rs` 之和 **≥ 30 且不得因拆分而减少**（§7.4 新增用例自带自己的运行时路径，2026-09-28 实测 = 37） |
| 子文件各写 `use harness::*;` → glob 重名歧义 | §5.2 规则 2 禁止子文件出现 `use`/`mod`；`check_fixtures_selftest` 之外另加 grep 断言 |
| 拆分漏测试（4 个 `c_lang.rs` 死测试即前车之鉴） | L-A 判据用**总数守恒**：各主题文件 test 数之和 = 906（+ S7 并入者），并在 `gen_coverage_index.sh` 内做同样的交叉校验 |
| glue 变成新杂物堆 | 硬上限 60 test / 1200 行，超限即 L-A 不成立 |
| 零引用硬门禁误伤 corpus/gcc_tools/probe | 显式 `WHITELIST_DIRS` 数组（§8.2），且白名单变更须在 `fixtures/README.md` 记原因 |
| fixture 改名丢 git 历史 | 全部用 `git mv`；改名与引用改写在同一提交，`git log --follow` 仍可追 |
| 门禁脚本自身假绿 | 新增 `check_fixtures_selftest.sh` 作为 Tier 0 的**元门禁**（§8.3），并接入 `Makefile` 与 `e2e.yml` |
| 覆盖索引与代码漂移 | `gen_coverage_index.sh` 的 test 数交叉校验 + PR 模板第 4 项勾选 |
| rustfmt / IDE 对 `include!` 子文件支持差 | 子文件保持"纯 `#[test]` 函数序列"，无 `use`；文件头注释标明非独立 target；`cargo build --tests` 是权威，IDE 报错不作为依据 |

---

## 14. 验收清单（可直接执行）

```bash
# L-A 结构
wc -l tests/integration.rs                              # 期望 < 300
ls tests/integration/*.rs | wc -l                       # 期望 = 15
grep -c 'include!("' tests/integration.rs                # 期望 = 15（判据必须带引号：壳文件注释里也出现 "include!" 字样，裸 'include!' 会被污染）
# test 总数：必须同时给出两个量
#   守恒下界 = 932（= 906 现存量 + 4 §1.1(5) 丢失用例回纳 + 22 S7 并入）——一条测试都不许丢，低于即 FAIL
#   实测总数 = 932 + S5 新增用例数（932 是「S5 之前」的守恒值；§7.4 给零引用夹具补的真实用例会推高它，所以 S5 完成后必然 > 932）
grep -c '#\[test\]' tests/integration/*.rs | paste -sd+ | bc   # 守恒下界 = 932（= 906 + 4 + 22）【注：多文件 grep -c 输出带 "文件名:数目" 前缀，此管道实际不可直接喂给 bc；可执行写法见下一行】
grep -h '^#\[test\]' tests/integration/*.rs | wc -l     # 实测总数（2026-09-28 磁盘实测 = 970 = 守恒下界 932 + S5 已新增 38；此值会随后续 §7.4 判定继续变化，作参考量；932 只是下界，不是硬上限）
grep -h '^fn ' tests/integration/*.rs | wc -l         # 期望 = 与上一行「实测总数」同值（每个测试一行；多出来即 helper 未进 harness）；其守恒下界同为 932
cargo check --test integration                          # 期望 0 error（基线为 744）
cargo test --release                                    # 期望与 S0 基线同级通过，无新增 #[ignore]

# L-B 规则
bash scripts/check_fixtures.sh                          # 期望 exit 0，且 scanned > 0
bash scripts/check_fixtures_selftest.sh                 # 期望 5/5 PASS exit 0
grep -rn 'fixtures/c/asm' . --include=*.rs --include=*.sh --include=*.md --exclude=tests_quality_system_plan.md | wc -l   # 0（排除本方案文档：§1.1(3)/§2/§3/§11/§14/§15 里的 `fixtures/c/asm` 是方案自述历史现状，非失效引用）
grep -rn 'crate::common::harness' tests/ | wc -l        # 0
grep -rn 'include_str!("fixtures/' tests/integration/ | wc -l   # 0（必须全是 ../fixtures/）
ls tests/fixtures/ | grep -cx dev                       # 空
ls tests/fixtures/c | grep -cE '^m[0-9]+_'              # 0

# L-C 运营
bash scripts/gen_coverage_index.sh && git diff --stat docs/test_coverage.md   # 生成区非空
grep -c 'cpp_abi_\(template\|eh\|rtti\|virtual\|overload\)' docs/test_coverage.md  # >= 5
grep -rn 'check_fixtures' .github/workflows Makefile | wc -l   # >= 2
test -f .github/PULL_REQUEST_TEMPLATE.md                 # 存在
grep -n '测试分层' docs/ci_guide.md                       # 命中
```

- [ ] **L-A** 全部判据通过（结构可导航 + 绿灯复位）
- [ ] **L-B** 全部判据通过（含 5 类反向注入真的会红）
- [ ] **L-C** 全部判据通过（索引/CI/文档/模板）
- [ ] 单提交、无夹带功能修改、`git revert` 可完整回滚

---

## 15. 提交信息（单次提交）

```text
test: complete one-shot test-system overhaul (L-A/L-B/L-C together)

Split the 24k-line tests/integration.rs (906 tests) into 15 include!-based
theme modules under tests/integration/, leaving a <300-line shell target.
Make tests/common/harness.rs usable again: mark all 19 helpers pub(crate),
restore the dropped `disasm` binding and tuple return in
compile_legacy_asm_object, and move all 25 integration-local helpers
(run_cpp_object_test, elf64_syms, cpp_std20_exit_code_cross_check, ...)
into it. Rewire every include_str! to the ../fixtures/ convention (438 + 2
examples refs) while leaving the 30 CARGO_MANIFEST_DIR runtime paths alone.

Fixtures: point the 67 stale fixtures/c/asm_* references at fixtures/asm/,
rename the 16 m27_* fixtures to <domain>_<feature>, adjudicate the 24 + 2
unreferenced C/C++ fixtures (add tests or archive), fold the four C++
standalone targets into cpp_* theme files, and correct the doc references in
docs/roadmap.md, docs/support_features.md and tests/fixtures/README.md.

Gates: rewrite scripts/check_fixtures.sh from scratch -- it silently passed
because REPO_ROOT was never assigned, so nothing was ever scanned and a
missing find target still printed success. It now hard-fails on missing,
unreferenced, _archive-referenced, m<N>_ named and directory-mismatched
fixtures, with no transitional opt-out env var, and the new
check_fixtures_selftest.sh proves the gate can actually go red. Wire both
into the Makefile and the e2e workflow, add scripts/gen_coverage_index.sh to
generate docs/test_coverage.md (with the five C++ ABI theme sections), add
docs/ci_guide.md test tiering, and add .github/PULL_REQUEST_TEMPLATE.md.
```

---

## 16. 一句话结论

> **v2.0 取消"先整理目录、以后再建门禁和索引"的全部借口**：一次提交内同时做到壳化拆分（906 测试守恒）、harness 真正可用（`pub(crate)` + 修回搬坏的函数）、fixture 引用零悬挂、命名与目录规则存量清零、`check_fixtures` 被反向自测证明真的会红、覆盖索引由脚本全量生成——L-A / L-B / L-C 缺一项，方案就不算完成。

---

*方案版本：v2.0 — 一次性终态定稿。§1 全部为 2026-09-28 实测数据（`cargo check --test integration` = 744 errors），实施时以 §14 的机器判据作为唯一验收依据。*
