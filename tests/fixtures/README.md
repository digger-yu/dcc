# DCC 测试夹具（fixtures）目录约定

> 本文是 `tests/fixtures/` 下**目录 = 语言/角色**、命名与引用完整性规则的就近细则。
> 上位契约：[`docs/tests_quality_system_plan.md`](../../docs/tests_quality_system_plan.md) §7（下称**方案 §7**）。
> 如何跑、如何加测试见 [`tests/README.md`](../README.md)。本文与方案**单向从属**：
> 规则语义以方案 §7 为准，本文负责落到"该放哪个目录、叫什么名、怎么算被引用"。

---

## 1. 目录 = 语言 / 角色

| 目录 | 放什么 | 禁止 |
|------|--------|------|
| `c/` | 通用 C 正例 | `.cpp`、asm 专用件、零引用死件、`m\d+_` 前缀 |
| `asm/` | `asm_*.c/h` 与 asm 专用头（asm 夹具**只**住这里） | 非 asm 的语言测试 |
| `cpp/` | `.cpp` C++ 正例 | `.c` |
| `errors/` | `eNNNN.c` + `eNNNN.stderr.snap`（含 `wNNNN` 警告码）；实测还存在变体形态：带限定后缀的 `eNNNN_<feature>.c`、汇编件 `e0102.s`、C++ 件 `w0501_cpp.cpp`，以及按用例名配对的无编号件 `bad.c` / `diagnostic_format.stderr.snap`（快照与源文件同名配对） | 正例功能测试 |
| `opt/` | 优化 pass 样例 | 散落进 `c/` |
| `c_xfail/` | 已知预期失败 | 被当成正例引用 |
| `cpp_abi_{template,eh,rtti,virtual,overload,lifecycle}/` | C++ Itanium ABI 的 GCC 差分夹具，按**目录名**被 `tests/cpp_abi_gcc_cross_check.rs` 动态发现 | 在 `.rs` 里逐个硬编码**夹具文件**清单（主题组名仍需登记，见 §3） |
| `gcc_tools/` | libiberty 探测组合（`config.h`/`gstdint.h`/`libdecnumber_combined.c`/`test_decbasic.c`） | 未登记进白名单 |
| `corpus/` | 真实 `.o` 二进制样本（`io_delay.o`、`fair.o`、`clz_ctz.o`） | 源码 |
| `probe/` | 后端能力探针（归 [`docs/backend_refactor_plan.md`](../../docs/backend_refactor_plan.md)） | 本体系的语言/行为面主题化改动 |
| `_archive/` | 无测试引用的历史件 | **任何 `include_str!` / `.join()` / `Path` 引用** |

三条边界说明：

- **asm 夹具的位置**：`asm_*.c/h` 一律 `fixtures/asm/asm_*`，`fixtures/c/` 内不存在也不得再出现 asm 专用件。
- **`c/` 下的同族二级子目录**：仅用于"一族多 TU / 批量同题"夹具（实测：`c/va_arg/`、`c/va_arg_mix/`、
  `c/va_arg_xfail/`）。它不是新的角色目录，不享受白名单豁免，同样受 §3 引用完整性约束；
  预期失败角色的夹具终态应落 `c_xfail/`。
- **新增角色目录 = 改契约**：先在方案 §7.1 增行，再建目录；未登记的目录既过不了 §2 的
  目录-扩展名一致检查，也过不了 §4 的白名单规则。

---

## 2. 命名

```text
<domain>_<feature>[_<qualifier>].c
```

- 合法示例：`asm_avx.c`（在 `asm/`）、`attr_alias_var.c`、`builtin_trap.c`、`generic_ptr.c`
- **禁止新增** `m\d+_` 前缀（存量改名记录见 §6）、禁止 `test_` 前缀（存量已清零，`c/`+`cpp/` 实测 0 件）
- `e\d{4}` / `w\d{4}` 由 `errors/` **独占**，其它目录不得使用该形态
- `cpp_abi_` 前缀仅出现在 `cpp_abi_<主题>/` 目录族

扩展名与目录必须一致（`check_fixtures.sh` 的 `DIR-MISMATCH` 项）：`c/` 内不得有 `.cpp`，
`cpp/` 与 `cpp_abi_*/` 内不得有 `.c`，`asm/` 内只允许 `asm_*` 前缀。

---

## 3. 引用完整性：四条满足其一

每个 `fixtures/**`（非 `_archive/`）文件**必须至少满足一条**，否则 CI 红：

1. 某 `.rs` 的 `include_str!` / `.join("…")` / `Path` 引用
2. 某 `scripts/*.sh` 引用
3. 外部套件清单（gcc-dg / torture 目录），且已登记在 `scripts/check_fixtures.sh` 的白名单数组
4. `corpus/` 二进制样本（整目录豁免）

扫描面（`check_fixtures.sh` 与 `gen_coverage_index.sh` 共用同一定义）：
`tests/*.rs`、`tests/integration/*.rs`、`tests/common/*.rs`、`scripts/*.sh`、
`.github/workflows/*.yml`。

`cpp_abi_*/` 目录族的语义是**整目录被 `check_group()` 引用**：夹具文件清单不在 `.rs` 里维护
（`fixtures_in(group)` 仍按目录列举 `*.cpp`）。但**主题组本身必须登记**：新增 `cpp_abi_<主题>/`
后，`cpp_abi_groups_are_all_covered` 的目录发现会抓到新目录，若未同步添加
`#[test] fn cpp_abi_<主题>_group()` 与其 `COVERED` 常量条目，该元测试即报红（不会默默漏测）。

---

## 4. 白名单目录

零引用检查（`UNREFERENCED`）对以下目录**整目录豁免**，豁免项在 `scripts/check_fixtures.sh`
的 `WHITELIST_DIRS` 数组中逐条登记（`cpp_abi_*/` 族例外：它由同脚本 `is_whitelisted()` 的
**前缀匹配**豁免，不进数组）：

| 目录 | 豁免理由 |
|------|----------|
| `corpus/` | 二进制样本，按 §3 规则 4 整目录豁免 |
| `gcc_tools/` | libiberty 探测组合，由 `scripts/*.sh` 与 GCC 源树用例消费 |
| `probe/` | 后端探针，归 backend 案，不由本体系索引 |
| `errors/` | `eNNNN.c`/`wNNNN.c` + `*.stderr.snap` 的路径由 `tests/errors.rs` 的 `ErrorCase{name, fixture_ext}` **动态拼接**，逐个文件不出现在字面量中；动态名匹配（Pattern-D）已把它们标进 REFERENCED，白名单只是双保险，避免快照件与负向件被误报 |
| `cpp_abi_{template,eh,rtti,virtual,overload,lifecycle}/` | GCC 差分 ABI 夹具族，由 `tests/cpp_abi_gcc_cross_check.rs` 按**目录名**动态发现（`cpp_abi_groups_are_all_covered`），等价于「整目录被 `check_group()` 引用」（见 §3）；豁免途径是 `is_whitelisted()` 的 `cpp_abi_*` 前缀匹配 |

**白名单变更必须在本文留原因**（一行即可：目录 + 谁消费 + 为什么），无记录的白名单视为违规。

`_archive/` 与白名单是**相反**的语义：它是"已判定无价值但保留可追溯性"的落点，
其中任何文件被引用即 `FORBIDDEN-ARCHIVE-REF` 硬失败；从 `_archive/` 取回夹具必须 `git mv`
回对应角色目录并补上引用（见 [`_archive/README.md`](_archive/README.md)）。

---

## 5. 零引用件的处置判定标准

`check_fixtures.sh` 报 `UNREFERENCED: <path>` 时，**逐条判定，不留 TODO**：

| 判据 | 处置 | 落点 |
|------|------|------|
| 可编译且断言点明确（如 `attr_malloc.c`、`builtin_misc.c`、`sync_atomic.c`） | 在对应主题子文件补 1 个 `#[test]` | `c_attr_builtin.rs` / `opt.rs` / `c_pp_c23.rs` … |
| 依赖外部工具或目录（如 `shf_merge_strings.c`、`cfi_directives.c`、`incbin_test.c`） | 补 test 且带 `#[cfg(unix)]` / `tool_exists` 守卫 | `c_link_debug.rs` |
| 与现有件语义重复，或需已废弃特性 | `git mv` 进 `_archive/`，并在 `_archive/README.md` 记一行原因 | `_archive/` |

C++ 侧同理：语义属模板等**语言面**的补测进 `cpp_lang.rs`，不进 `cpp_abi_*`（ABI 与语言主题不同源）。

---

## 6. `mNN_` 改名映射（存量清零记录）

`fixtures/c/` 的 16 个 `m27_*` 已按下表改名为 `<domain>_<feature>`，**引用同步在同一提交内完成**，
`git mv` 保证 `git log --follow` 可追。本节作为映射记录保留，便于按旧名检索：

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

- 三个 `*_call.c` 后缀用于避开已存在的 `attr_deprecated.c`；其余新名均已核对无同名冲突。
- 改名后按 §5 的再判定：16 件新名中 **15 件现住 `c/`**，`builtin_lowlevel_simple.c` 已作为「与
  `builtin_lowlevel.c` 语义重复」进 `_archive/c/`，所以在 `c/` 里按新名 `builtin_lowlevel_simple.c`
  找不到它是正常的（历史件仍在 `_archive/c/`，禁止引用）。
- 规则从此生效：**新增 `m\d+_` 前缀 = `FORBIDDEN-NAME` 硬失败**（相对 `git ls-tree HEAD` 的增量判定）。
- **测试函数名**（`fn m27_attr_alias…`）的改名归
  [`docs/cpp_test_naming_unification_plan.md`](../../docs/cpp_test_naming_unification_plan.md)；
  若 `fn m27_*` 与新夹具名不一致，允许，但必须能在 `docs/test_coverage.md` 索引里检索到。

---

## 7. 路径策略（强制）

- 主题子文件 `tests/integration/*.rs` 写 `include_str!("../fixtures/<dir>/<file>")`。
- 壳 `tests/integration.rs` 不含任何夹具引用（它只有 `use` + `#[path] mod harness` + `include!` 列表）。
- **禁止**子文件写裸 `fixtures/...`（`include_str!` 按当前文件位置解析，会指向 `tests/integration/fixtures/`）。
- 运行时拼出来的路径继续用 `env!("CARGO_MANIFEST_DIR")`，**不得**改成相对路径（它与文件位置无关）。
- 冒烟语料在 `tests/fixtures/smoke/`，从 `tests/integration/*.rs` 用 `include_str!` 引用时写作 `../../fixtures/smoke/...`。

---

## 8. 新增一条夹具的最小清单

1. 按 §1 选定**角色目录**，按 §2 命名（`<domain>_<feature>[_<qualifier>]`，无 `m\d+_` / `test_` 前缀）
2. 在对应主题子文件里用 `include_str!("../fixtures/…")`（或 `.join()`）挂上引用，并写 `#[test]`
3. `bash scripts/check_fixtures.sh` 绿（五类标签：`MISSING` / `UNREFERENCED` /
   `FORBIDDEN-ARCHIVE-REF` / `FORBIDDEN-NAME` / `DIR-MISMATCH` 全部为空）
4. `bash scripts/check_fixtures_selftest.sh` 绿（门禁本身仍会红）
5. 重新生成覆盖索引：`bash scripts/gen_coverage_index.sh`，确认新行进入 `docs/test_coverage.md`
6. 跑主题测试绿（如 `cargo test --test integration asm_`）
