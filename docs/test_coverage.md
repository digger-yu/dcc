# Test Coverage Index

> DCC 的「特性 → 夹具 → 测试函数」索引。
> 权威规格：`docs/tests_quality_system_plan.md` §9（Tier 表）、§10（覆盖索引）、§5（主题划分）。
> 全量索引表由 `scripts/gen_coverage_index.sh` 生成，本文标记区之外为人工校订内容。

## 本文的结构

1. **人工校订区**（本节到下方标记之前）：索引的维护契约、主题划分口径、
   C++ ABI 五主题的逐条映射、判据与 Tier 的定义、实测缺口清单。
2. **自动生成区**（下方成对的 `GENERATED` HTML 注释标记之间）：
   全部 `#[test]` 的逐条索引（主题文件 / 测试函数 / fixture / 判据 / Tier）
   以及统计小结。这一区由脚本从代码与夹具磁盘真值推导，**手工编辑会被覆盖**。

维护动作只有一个：改完 `tests/**` 之后重跑

```bash
bash scripts/gen_coverage_index.sh          # 重写生成区，并跑完全部交叉校验
git diff --stat docs/test_coverage.md       # 只有生成区出现在 diff 里
```

脚本幂等：连跑两次后 `git diff --stat docs/test_coverage.md` 为空。

## 索引的维护契约

覆盖索引不是一张"画完就不再核对"的表。生成器在写文档之前做交叉校验，
任何一条不成立就 `exit 1` 并且**不留下半成品文档**；前置条件失败打印
`FATAL:` 并以 1 退出（不打印成功文案）。校验项与标签：

| 标签 | 断言 | 破了会怎样 |
|------|------|------------|
| `STRUCT-INCLUDE` | `grep -c 'include!("' tests/integration.rs` == `ls tests/integration/*.rs \| wc -l` | 新主题子文件没挂进壳 → 它的用例根本不参与编译，索引却可能照抄旧行数 |
| `STRUCT-USE` | 主题子文件内不出现 `^use ` / `^mod `；壳内 `#[test]` 数为 0 | 子文件各写一遍导入 → glob 重名歧义（§5.2 规则 2） |
| `STRUCT-HELPER` | 主题子文件的 `^fn` 数 == `#[test]` 数 | 助手函数滞留在主题文件里（§6.2 要求进 harness） |
| `STRUCT-DANGLE` | 每个 `#[test]` 之后紧跟的是 `^fn` | 属性还在、函数没了 → 用例被静默吃掉 |
| `COUNT-PER-FILE` / `COUNT-TOTAL` | 逐文件与合计的索引行数 == 磁盘 `grep -c '#\[test\]'` | 索引与代码漂移；漏搬一个测试立刻可见 |
| `FIXTURE-MISSING` | 静态可解析的 `include_str!` / `.join` / 字面量引用在 `tests/fixtures/` 下真的存在 | 夹具改名或删目录后索引继续声称覆盖 |
| `JAC-UNMAPPED` | harness 的每个 `pub(crate) fn`、顶层 target 的每个本地助手都在脚本头部的判据表里登记 | 新助手没人定义它算哪类验收，就会被默认算成"已覆盖" |
| `JAC-UNINFERABLE` | 每个测试函数体至少能推出一个判据（有已登记助手，或有 `Command::new` / `assert*` / `.expect(` 痕迹） | 空壳测试（`#[test] fn x() {}`）永远绿，是典型的假绿 |
| `ABI-CITE-STALE` / `ABI-GROUP-DRIFT` | 本文「C++ ABI 五主题」小节引用的行号与 `tests/fixtures/cpp_abi_*` 目录集合，与 `tests/cpp_abi_gcc_cross_check.rs` 的代码真值逐条一致 | 手工小节的行号/组名过时 |
| `WRITE-MISMATCH` | 待落盘文档的生成区行数 == 本次抽取行数，且标记之外的人工校订内容逐字节不变 | 生成器越界改写人工内容 |

新增一个测试函数不需要改本文任何表格：它会在下一次生成时自动出现在生成区。
需要动手的是**新增夹具或新增 harness 助手**——前者必须被某个测试引用（否则
`scripts/check_fixtures.sh` 报 `UNREFERENCED:`），后者必须在
`scripts/gen_coverage_index.sh` 的判据表里登记一行（否则报 `JAC-UNMAPPED`）。

## 主题划分口径

`tests/integration.rs` 是唯一承载 932 个语言/后端用例的 cargo target，它本身
只有壳声明（`use` + `#[path] mod harness;` + 15 行 `include!`），用例按主题
位于 `tests/integration/*.rs`。主题文件**不是**独立 target，它们是文本级粘贴，
所以同一个 crate 里共享 `harness::*`。文件顺序与职责以壳里的 `include!` 列表为准：

| 主题文件 | 覆盖面 |
|----------|--------|
| `c_lang.rs` | 通用 C 语义：声明/类型/结构体/联合体/枚举/数组/函数指针/限定符/静态局部 |
| `c_asm.rs` | 内联汇编：约束与 clobber、`asm goto` 及输出回写、`-S` 反汇编、寄存器分配、段与内核模式 |
| `c_kernel.rs` | 内核路径：per-cpu、`alt_*`、objtool 语料、`vmlinux`/kcov/kasan/kcsan 相关件、GCC 源树门控 |
| `c_attr_builtin.rs` | `__attribute__` 与 `__builtin_*`、`_Atomic`/原子内建 |
| `c_pp_c23.rs` | 预处理器与 C23/C11/C89 方言：`#pragma`、`_Static_assert`、`typeof`、trigraph、`-dM` |
| `c_abi_vararg.rs` | `va_list` / `va_arg` / `va_copy`、SSEUP 槽位、可变参数 ABI |
| `c_link_debug.rs` | 链接与调试信息：DWARF、`.eh_frame`、重定位、符号表、ifunc、comdat、vdso、TLS |
| `c_cross.rs` | 交叉编译：aarch64 / arm / riscv / mips / 非 x86 目标、qemu 运行时、Windows/Darwin 三元组 |
| `c_sanitizer.rs` | sanitizer / 覆盖率 / PGO / offload / PCH / 增量编译 |
| `cpp_lang.rs` | C++ 语言面：模板、概念、`auto`、lambda、命名空间、modules、`--cxx-native` 语言侧决策 |
| `cpp_stdlib.rs` | C++ 标准库互操作面（`<print>`、`<format>` 一类由宿主库承接的用例） |
| `cpp_runtime.rs` | C++ 运行期与后端衔接：ctor/dtor、vtable、EH 落地、HIR 与 legacy lowering 对应关系、CLI 契约 |
| `opt.rs` | 优化 pass：LTO、SSA、常量折叠、内联、DCE、去虚调用、向量化 |
| `xfail.rs` | 预期失败目录（`fixtures/c_xfail/`）与 objtool xfail 语料 |
| `glue_misc.rs` | 兜底主题，硬上限 60 test / 1200 行；落点含糊的用例在这里，改名或补夹具后应重投到其余 14 个文件 |

顶层 target 另有 6 个（`tests/*.rs`）：`integration.rs`（壳，0 个测试）、
`errors.rs`（诊断快照，夹具在 `fixtures/errors/`，含 `eNNNN.c` 与 `*.stderr.snap`）、
`profile.rs`（PGO / AutoFDO / LBR / MemProf / offload 的 CLI 与模块行为）、
`perf_regression.rs`（复杂度与挂起护栏）、`reproducibility.rs`（逐字节可重现）、
`cpp_abi_gcc_cross_check.rs`（目录发现型 GCC 差分，见下一节，需要独立 CLI 过滤所以保持独立 target）。
生成区把它们和主题文件放在同一张表里，第一列写全路径以便区分。

`tests/common/harness.rs` 与其子模块 `harness/{elf,cpp_frontend,driver_cli}.rs`
是判据的唯一真值来源：生成器从这些文件里读出助手名单，再按脚本头部的
映射表把每个测试函数体里出现的助手折算成判据。

## C++ ABI 五主题（GCC 差分夹具族）

这一族不由 `tests/integration/*.rs` 承载，而是独立 target
`tests/cpp_abi_gcc_cross_check.rs`：夹具自己把观察值 `printf` 出来，
g++ 原生构建运行得到的**退出码 + stdout** 与 dcc 产物（`dcc -c` → 宿主 `g++` 链接）
逐行相等才算通过；只看退出码会放过"算错了但没触发断言"的情况。
主机没有 g++ 时整组打印跳过说明后 `return`（不误报失败）。

下表全部行号为 `tests/cpp_abi_gcc_cross_check.rs` 的实测位置（生成器会用
`ABI-CITE-STALE` 校验这些引用是否仍然指到对应的 `fn` 定义行与 `check_group()` 调用行）：

| 主题 | fixture 目录 | 驱动函数（定义行）→ `check_group()` 调用点 | 夹具数 | 判据 |
|------|--------------|--------------------------------------------|--------|------|
| template | `tests/fixtures/cpp_abi_template/` | `cpp_abi_template_group` (`:212`) → `check_group("cpp_abi_template")` (`:213`) | 2 | cross-check：类型特征折成编译期常量、依赖名在非类型实参里的折叠，`traits 永远为 0` 会以 stdout 差异暴露 |
| exception handling | `tests/fixtures/cpp_abi_eh/` | `cpp_abi_eh_group` (`:203`) → `check_group("cpp_abi_eh")` (`:204`) | 6 | cross-check：zero-cost EH 的 invoke/landing-pad/LSDA 闭环；LSDA 字段错位或缺 call-site 区间表现为 `std::terminate`（退出码 134）或析构/捕获顺序错乱 |
| rtti | `tests/fixtures/cpp_abi_rtti/` | `cpp_abi_rtti_group` (`:193`) → `check_group("cpp_abi_rtti")` (`:194`) | 2 | cross-check：`typeid` 名字与 `dynamic_cast` 的端到端派发 |
| virtual | `tests/fixtures/cpp_abi_virtual/` | `cpp_abi_virtual_group` (`:175`) → `check_group("cpp_abi_virtual")` (`:176`) | 4 | cross-check：多继承 secondary vtable 槽指向 this-adjust thunk、虚基子对象有自己的 vptr、类对象尺寸逐字节一致 |
| overload | `tests/fixtures/cpp_abi_overload/` | `cpp_abi_overload_group` (`:187`) → `check_group("cpp_abi_overload")` (`:188`) | 4 | cross-check：链接名与候选选定（参数类型、const 限定、构造重载、成员函数重载） |
| lifecycle | `tests/fixtures/cpp_abi_lifecycle/` | `cpp_abi_lifecycle_group` (`:181`) → `check_group("cpp_abi_lifecycle")` (`:182`) | 11 | cross-check：构造/析构次序、委托构造、copy/move 构造、隐式成员初始化次序与实参个数 |

夹具总数 29，逐条由 `check_group()` → `check_one()`（`:134`）驱动；
`gxx_truth()`（`:75`）取 g++ 真值，`dcc_result()`（`:99`）取 dcc 产物结果。
继承（单/多/虚基）不是独立目录，它落在 `cpp_abi_virtual/` 的
`multi_inheritance_secondary_thunk.cpp`、`virtual_base_override_dispatch.cpp`、
`virtual_base_shared_subobject.cpp` 与 `cpp_abi_lifecycle/` 的构造链夹具里。

两个元测试：

* `cpp_abi_groups_are_all_covered`（`:218`）——把 `all_groups()`（`:63`，
  按前缀 `cpp_abi_` 枚举 `tests/fixtures/` 下的目录）得到的目录名与函数体内
  **硬编码的 `COVERED` 常量**（`:219`-`:226`）比对，出现未登记的组就 assert 失败。
* `cpp_abi_fixtures_are_standard_cxx`（`:239`）——遍历所有组的所有夹具，
  要求 g++ 本身接受（否则它只是 dcc 方言，交叉验证失去意义）。

### 新增一个 ABI 主题组的真实路径

**只往 `tests/fixtures/cpp_abi_<主题>/` 放 `.cpp` 是不够的**：目录会被
`all_groups()` 自动发现，但 `cpp_abi_groups_are_all_covered` 比对的 `COVERED`
是硬编码常量，发现未登记的新组会直接把该元测试跑红。真实路径是三步：

1. 建目录 `tests/fixtures/cpp_abi_<主题>/` 并放入至少一条自报观察值的 `.cpp` 夹具
   （`check_group()` 对空目录断言失败，见 `:159`）。
2. 在 `tests/cpp_abi_gcc_cross_check.rs` 里写 `#[test] fn cpp_abi_<主题>_group()`
   并让它调用 `check_group("cpp_abi_<主题>")`，同时把同一个组名加进
   `cpp_abi_groups_are_all_covered` 的 `COVERED` 常量。两步缺一不可：
   少了 ① 组不会被交叉验证，少了 ② 元测试红。
3. 重跑 `bash scripts/gen_coverage_index.sh`。生成器会把本文表格引用的行号与
   目录集合和代码真值对账（`ABI-CITE-STALE` / `ABI-GROUP-DRIFT`），
   所以这一小节必须同步改，不能停留在文档里。

`scripts/check_fixtures.sh` 对 `cpp_abi_*` 目录族按"整目录被 `check_group()` 引用"
处理（该族内新增 `.cpp` 不会被判 `UNREFERENCED`），但目录内的 `.c` 仍会被判
`DIR-MISMATCH`——这一族只放 C++。

## 判据与 Tier 口径

生成区第一列之后的四个信息列含义如下。判据来自函数体实际调用的 harness 助手，
一个函数体命中多类时按 `report-only > cross-check > sanitize > run > compile`
取最强的一条；推不出任何判据（既没有已登记助手也没有任何执行/断言痕迹）不是
"默认 compile"，而是让生成器报错。

| 判据 | 含义 |
|------|------|
| `compile` | 只断言编译产物：`.o` 字节/段/符号、`-S` 汇编文本、stderr 诊断、IR 结构 |
| `run` | 产物真的执行（主机或 qemu），断言退出码或输出 |
| `cross-check` | 与外部真值或另一条路径对照：g++/gcc、宿主链接器，或 HIR vs legacy lowering |
| `sanitize` | 带 sanitizer 运行，或断言"运行时必须以特定方式失败" |
| `report-only` | `#[ignore]`：默认 `cargo test` 不执行，只作为报告/对照明细存在 |

| Tier | 含义（§9 权威表的用例侧投影） |
|------|--------------------------------|
| `T0` | `cargo test --release` 默认执行 —— PR 必绿面（Tier 0） |
| `T3` | 需要非默认 feature、GCC 源树，或被 `#[ignore]`/`#[cfg(any())]` 挡住的深测面（Tier 3） |

Tier 1（Linux CI job 划分）与 Tier 2（内核 / 性能，`kernel_full_check.sh`、
`kernel-bench.yml`）是**运行环境**的属性，不是单个用例的属性，因此不出现在
生成区；`docs/ci_guide.md` 的「测试分层」一节单向引用 §9。

fixture 列的记录约定：`(inline)` = 源码在函数体内联构造、没有外部夹具文件；
`+N more` = 同一用例还引用了更多夹具（完整清单由 `scripts/check_fixtures.sh`
逐个把关，本文不重复展开）；路径一律写成相对 `tests/` 的形式，例如
`fixtures/asm/asm_avx.c` 实际是 `tests/fixtures/asm/asm_avx.c`。asm 夹具只有
`fixtures/asm/` 一个落点，`fixtures/c/` 下不存在 asm 专用件。

## 已知缺口（实测）

以下是"文档/计划声称，但磁盘上不是那么回事"的实测清单。只登记事实与影响面，
修复动作落在 `tests/**` 与 `src/**`，不属于本文的职责。

| 声称 | 实测 | 影响 |
|------|------|------|
| `fixtures/cpp/overload.cpp` 驱动重载解析正例 | `tests/fixtures/cpp/` 下没有任何 `overload*.cpp`；重载面只有 `cpp_abi_overload/` 的 4 条差分夹具 | 重载解析的**语言面**（非 ABI 差分）缺一个 `fixtures/cpp/` 正例 |
| `fixtures/cpp/global_object_dynamic_init.cpp` | 该件实际位于 `tests/fixtures/cpp_abi_lifecycle/`，`fixtures/cpp/` 下不存在 | 全局动态初始化只有 GCC 差路面，没有 dcc 单侧正例 |
| `fixtures/cpp/variadic_*.cpp` | `fixtures/cpp/` 下无 `variadic*` 文件；G12 可变参数模板的 8 个用例（`cpp_lang.rs:1862`-`2037`）走内联 AST 构造与 `--cxx-native` 源码串 | 可变参数模板没有可复用的 `.cpp` 夹具，判据只能停在语言面 |
| `fixtures/c/riscv64_*.c` | `fixtures/c/` 下只有 `riscv_d_extension.c`、`riscv_rvv_ops.c`，没有 `riscv64_` 前缀件 | 16 个 `riscv64_*` 用例里大部分靠内联源码；跨目标面缺成组夹具 |
| `fixtures/c/attr_*.c`（约 30 件）驱动属性主题 | `fixtures/c/` 里 `attr_*` 前缀 20 件，另有 16 件仍是 `m27_*` 前缀（含 `m27_attr_alias.c`、`m27_trap_test.c` 等） | §7.2 命名规则与 §7.4-B 改名映射尚未落地，索引里按真值显示 `m27_*` |
| `asm_register_planner_reassigns_early_clobbers` | 真名是 `asm_register_planner_reassigns_early_clobber_outputs` | 早期 clobber 重分配用例存在，只是旧文档里的名字查不到 |
| `non_x86_backend_c_mode_no_panic` / `non_x86_backend_s_mode_compiles` | 实际用例为 `non_x86_c_mode_produces_valid_elf` 与 `non_x86_asm_minus_s_preserves_global_asm_text` | 非 x86 后端的 c/s 模式有覆盖，旧名字是编造的 |
| `fixtures/cpp/g11_template_templates.cpp` 被模板主题驱动 | 文件在磁盘上，但 `tests/` 全树没有任何引用 | 未引用夹具；`check_fixtures.sh` 的 `UNREFERENCED` 会红，需按 §7.4-A 判定（补 test 或归档） |
| 同上，`fixtures/cpp/enhanced_sfinae.cpp` | 同样零引用 | 同上 |
| `fixtures/c_xfail/` 有 2 件 | 只有 `ld_global_read.c` 一条夹具（外加 `README.md`）；`flag_clobber_frame.c` 只在 `c_asm.rs:1534` 的注释里被当作历史提及 | xfail 面比声称的小一半 |
| 10 个 C++ 专项顶层 target 文件（`cpp_hir_legacy_correspondence.rs`、`driver_cxx_native_cli_contract.rs`、`legacy_strip_not_in_main_path.rs`、`test_g12_variadic_templates.rs`） | 顶层 `tests/*.rs` 只有 6 个；那 4 个文件的 22 个用例已并入主题文件：8 个 G12 + 4 个 `cpp_cxx_native_*` + 1 个 `cpp_strip_keywords_all_removed` → `cpp_lang.rs`，6 个 HIR/legacy 对应关系 + `help_documents_the_cxx_native_switch` + `off_forces_legacy_with_visible_decision_and_summary` + `strip_cpp_keywords_is_not_exported_in_default_build` → `cpp_runtime.rs` | 索引按真值给出落点；按主题文件过滤即可定位 |
| `tests/integration/{asm_lang,c_lang,cpp_lang}.rs` 里的骨架文件曾长期不被 `include!` | 15 个主题文件全部挂在壳上，`^fn` 数与 `#[test]` 数逐文件相等（生成区"逐文件用例数"表就是证据） | 结构守恒由 `STRUCT-*` / `COUNT-*` 校验持续把关 |

<!-- BEGIN GENERATED -->
> 本区域由 `scripts/gen_coverage_index.sh` 从 tests/ 代码与 tests/fixtures/ 磁盘真值
> 全量生成；手工编辑会被下一次生成覆盖。人工校订小节都在本标记之外。

## 生成区统计

| 指标 | 实测值 |
|------|--------|
| 主题子文件（`tests/integration/*.rs`） | 15 |
| 主题子文件里的 `#[test]` | 970 |
| 顶层 target（`tests/*.rs`，含壳） | 6 |
| 顶层 target 里的 `#[test]`（不含主题） | 98 |
| 索引数据行合计 | 1068 |

### 判据分布

| 判据 | 用例数 |
|------|--------|
| compile | 687 |
| cross-check | 114 |
| report-only | 1 |
| run | 257 |
| sanitize | 9 |

### Tier 分布（§9 权威表；T1/T2 是 CI job 划分，不是用例属性）

| Tier | 用例数 |
|------|--------|
| T0 | 1063 |
| T3 | 5 |

### C++ ABI 夹具族（磁盘真值 + 代码真值，与人工小节逐条对账）

| 组 | 驱动 fn 行 | `check_group()` 行 | .cpp 夹具数 | 已登记 COVERED |
|----|------------|----------------------|-------------|----------------|
| `cpp_abi_eh` | `:203` | `:204` | 6 | yes |
| `cpp_abi_lifecycle` | `:181` | `:182` | 11 | yes |
| `cpp_abi_overload` | `:187` | `:188` | 4 | yes |
| `cpp_abi_rtti` | `:193` | `:194` | 2 | yes |
| `cpp_abi_template` | `:212` | `:213` | 2 | yes |
| `cpp_abi_virtual` | `:175` | `:176` | 4 | yes |

### 报告项：引用了磁盘上不存在夹具的用例

这些行不阻断生成（修复动作在 `tests/**` 与夹具侧），但索引不掩盖它们：
对应 fixture 单元格在生成区里带 ⚠。

无。全部静态可解析的夹具引用都能在磁盘上找到。

### 逐文件用例数（与磁盘 `grep -c '#[test]'` 逐文件核对）

| 文件（相对 tests/） | 索引行数 | 一致 |
|---------------------|----------|------|
| integration/c_lang.rs | 136 | yes |
| integration/c_asm.rs | 105 | yes |
| integration/c_kernel.rs | 43 | yes |
| integration/c_attr_builtin.rs | 103 | yes |
| integration/c_pp_c23.rs | 106 | yes |
| integration/c_abi_vararg.rs | 38 | yes |
| integration/c_link_debug.rs | 72 | yes |
| integration/c_cross.rs | 82 | yes |
| integration/c_sanitizer.rs | 54 | yes |
| integration/cpp_lang.rs | 84 | yes |
| integration/cpp_stdlib.rs | 8 | yes |
| integration/cpp_runtime.rs | 47 | yes |
| integration/opt.rs | 62 | yes |
| integration/xfail.rs | 6 | yes |
| integration/glue_misc.rs | 24 | yes |
| cpp_abi_gcc_cross_check.rs | 8 | yes |
| errors.rs | 26 | yes |
| perf_regression.rs | 4 | yes |
| profile.rs | 53 | yes |
| reproducibility.rs | 7 | yes |

## 全量索引

列义：**判据** = 该用例实际验收的东西（`compile` 只看编译产物 / `run` 跑起来看退出码
或输出 / `cross-check` 与 g++、宿主链接器或另一条 lowering 路径对照 / `sanitize` 带
sanitizer 运行或断言运行时失败 / `report-only` = `#[ignore]`，默认不执行；`disabled` =
被 `#[cfg(any())]` 永久排除在构建之外）；**Tier** = §9 档位（`T0` = cargo test --release
默认执行，`T3` = 特征门控 / 永不构建 / 需 GCC 源树）。Tier 列 `·` 之后是该用例携带的
门控标注（含文件级 `#![cfg(unix)]`）。fixture 列的 `(inline)` 表示源码在函数体内联
构造、没有外部夹具；`+N more` 表示同一用例还引用了更多夹具（完整清单由
`scripts/check_fixtures.sh` 逐个把关）；⚠ 前缀表示该引用在磁盘上找不到夹具。

| 主题文件 | 测试函数 | fixture | 判据(compile/run/cross-check/sanitize) | Tier |
|----------|----------|---------|--------------------------------------|------|
| `tests/integration/c_lang.rs` | `unknown_typedef_is_not_coerced_to_int` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `s128_typedef_with_attribute_parses` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `struct_comma_separated_field_declarators_parse` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `array_decay_and_nested_block_redecl_compile` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `enum_trailing_comma_parses` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `struct_function_pointer_field_parses` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `extern_comma_array_declarators_parse` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `gnu_case_range_switch_compiles` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `switch_range_comparison_correctness` | \`fixtures/c/switch_range_correctness.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `indirect_call_assignment_preserves_lvalue_address` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `function_typedef_parses` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `restrict_parameter_parses` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `comma_pointer_declarators_parse` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `devtable_static_init_with_function_pointer` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `stdbool_before_linux_stddef_enum_parses` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `generic_with_ternary_controlling_expr_compiles` | \`fixtures/c/generic_tern.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `generic_pointer_precise_match_compiles` | \`fixtures/c/generic_ptr.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `stmt_expr_trailing_call_compiles` | \`fixtures/c/stmt_call.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `bitfield_comma_separated_compiles` | \`fixtures/c/bitfield.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `struct_empty_field_compiles` | \`fixtures/c/struct_empty.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `multidim_array_field_compiles` | \`fixtures/c/multidim.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `noreturn_trap_emits_ud2_compiles` | \`fixtures/c/noreturn_trap.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `vfs_pressure_stmt_expr_compiles` | \`fixtures/c/stmt_vfs.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `ternary_ptr_deref_compiles` | \`fixtures/c/ternary_ptr.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `ternary_side_effects_local_shadows_global` | \`fixtures/c/ternary_side_effects.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `chained_assignment_value_truncated_to_lvalue_type` | \`fixtures/c/chained_assign_truncation.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `may_alias_struct_compiles` | \`fixtures/c/may_alias.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `seg_qualifier_compiles_and_runs` | \`fixtures/c/seg_qualifier.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `compat_typedef_redefinition_compiles` | \`fixtures/c/typedef.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `local_shadows_libc_function_name` | \`fixtures/c/shadow.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `complex_div_run_returns_zero` | \`fixtures/c/complex_div.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `stdc_cx_limited_range_controls_complex_arithmetic_codegen` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `double_complex_abi_run_returns_zero` | \`fixtures/c/complex_abi.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `function_return_keep_preserves_plain_ret` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `int128_call_abi_run_returns_zero` | \`fixtures/c/int128_abi.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `int128_mul_div_mod_run_returns_zero` | \`fixtures/c/int128_arith.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `float_mod_run_returns_zero` | \`fixtures/c/float_mod.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `int128_logical_run_returns_zero` | \`fixtures/c/int128_log.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `global_indexed_run_returns_zero` | \`fixtures/c/global_idx.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `ms_extensions_compile_and_run` | \`fixtures/c/ms_ext.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `chained_designator_init` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `trapv_negation_overflow` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `thread_local_exec_returns_15` | \`fixtures/c/tls_simple.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `kr_old_style_function` | \`fixtures/c/kr_func.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `implicit_function_decl_is_error_by_default` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `implicit_function_decl_error_ignores_wno_in_modern_std` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `gnu89_implicit_function_decl_warns_and_continues` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `m15_float_const_eval` | \`fixtures/c/float_const.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m15_string_literal_and_concat` | \`fixtures/c/string_const.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m15_address_const` | \`fixtures/c/address_const.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `const_eval_comprehensive_bitfield_sizeof_ternary` | \`fixtures/c/const_eval_comprehensive.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `struct_abi_comprehensive` | \`fixtures/c/struct_abi_comprehensive.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `struct_abi_extended` | \`fixtures/c/struct_abi_extended.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `ld80_arith_cmp` | \`fixtures/c/ld80_arith_cmp.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `ld80_asm_t_channel` | \`fixtures/c/ld80_asm_t_channel.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `ld80_asm_x_channel` | \`fixtures/c/ld80_asm_x_channel.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `ld80_asm_x_rejects_long_double` | \`fixtures/c/ld80_asm_x_rejects_long_double.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `m15_float_store_paths` | \`fixtures/c/abi16_struct_extra.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m15_constexpr_function` | \`fixtures/c/constexpr.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m19_counter_monotonic` | \`fixtures/c/counter.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `double_complex_decl_run_returns_zero` | \`fixtures/c/complex_decl.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `complex_literal_run_returns_zero` | \`fixtures/c/complex_literal.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `imaginary_basic_run_returns_zero` | \`fixtures/c/imaginary_basic.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `wide_char_literal_run_returns_zero` | \`fixtures/c/wide_lit.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `fenv_runtime_calls_compile_and_run` | \`fixtures/c/fenv.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `fenv_access_rounding_mode_affects_runtime_arith` | \`fixtures/c/fenv_round.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `complex_libm_fixture_runs` | \`fixtures/c/complex_libm.c\` | cross-check | T0 |
| `tests/integration/c_lang.rs` | `unsigned_float_conversion_runtime` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `assign_generic_lvalue_fallback_runtime` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `incdec_memory_lvalue_runtime` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `array_decay_value_contexts` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `m1_1_hello_world` | \`fixtures/c/hello.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m1_1_hello_stdout` | \`fixtures/c/hello.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m1_2_multi_file` | \`fixtures/c/helper.c\` \`fixtures/c/main.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m2_1_type_system` | \`fixtures/c/types.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m2_2_struct_layout` | \`fixtures/c/struct.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `bitfield_runtime` | \`fixtures/c/bitfield_runtime.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m2_3_integer_promotion` | \`fixtures/c/promotion.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m2_4_pointers` | \`fixtures/c/pointer.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `struct_field_aligned_codegen` | \`fixtures/c/struct_field_aligned.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m3_3_statement_expressions` | \`fixtures/c/stmt_expr.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `m3_5_vla_zero_array` | \`fixtures/c/vla.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `c_arrow_member_store_offset_and_width` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `func_type_param_compiles` | \`fixtures/c/func_type_param.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `static_addr_const_init_compiles` | \`fixtures/c/static_addr_const.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `multi_dim_array_compiles_and_runs` | \`fixtures/c/multi_dim_array.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `nested_block_redecl_different_type_no_error` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `nested_block_redecl_shadows_global` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `nested_block_redecl_multi_level` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_enum_type_traits_runtime` | \`fixtures/cpp/semantic_run.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `strict_aliasing_overflow_flags` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `qual_type_compiles_and_runs` | \`fixtures/c/qual_type.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `qual_type_object_code` | \`fixtures/c/qual_type.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `qual_type_volatile_survives_o2` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `qual_type_multi_level_ptr_syntax` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `multichar_const_syntax_ok` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `multichar_const_run_returns_zero` | \`fixtures/c/multichar_const.c\` | run | T0 |
| `tests/integration/c_lang.rs` | `void_ptr_implicit_conversion_syntax_ok` | \`fixtures/c/void_ptr_implicit.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `const_assign_error` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `const_correct_syntax_ok` | \`fixtures/c/const_correct.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `volatile_semantics_syntax_ok` | \`fixtures/c/volatile_semantics.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `volatile_qualifier_parsed` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `fenv_exception_functions_compile_and_run` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `fenv_save_restore_env` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `nested_func_trampoline_compile` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `nested_func_direct_call` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `static_local_scalar_persistence` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `static_local_uninit_array` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `static_local_init_list_array` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `static_local_string_init_array` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `static_local_in_nested_block` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `static_local_address_of` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `static_local_partial_init_array` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_layer_static_local_name_conflict` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_layer_multidim_array` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_layer_vla_runtime` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_layer_addr_vs_load` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_layer_aggregate_array` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `semantic_layer_static_local_addr_and_index` | (inline) | run | T0 |
| `tests/integration/c_lang.rs` | `m2_5_float` | \`fixtures/c/float.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `tag_compat_redecl_compiles` | \`fixtures/c/tag_compat_redecl.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `two_double_params_use_separate_xmm_regs` | \`fixtures/c/double_args.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `enum_decl_with_parenthesized_declarator_matches_gcc` | \`fixtures/c/enum.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `field_level_aligned_attrs_take_effect_at_runtime` | \`fixtures/c/field_aligned_attr.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `long_double_size_and_x87_arithmetic_match_gcc` | \`fixtures/c/long_double_precision.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `simd_vector_ops_on_128bit_match_gcc` | \`fixtures/c/simd_vector_xmm.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `static_local_arrays_shadow_file_scope_global` | \`fixtures/c/static_local_arrays.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `typedef_function_type_param_adjusts_across_tu` | \`fixtures/c/typedef_fn.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `fwd_decl_same_tu_dedup` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `gdt_designated_init_and_asm_inline_compiles` | \`fixtures/c/designated.c\` | compile | T0 |
| `tests/integration/c_lang.rs` | `m15_abi16_struct_passing` | \`fixtures/c/abi16_struct.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `ld_global_read_long_double_uses_80bit_load_and_exact_data` | \`fixtures/c/ld_global_read.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_lang.rs` | `permissive_implicit_function_decl_warns_and_continues` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `permissive_unknown_typedef_warns` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `unknown_variable_read_is_not_implicit_function_warning` | (inline) | compile | T0 |
| `tests/integration/c_lang.rs` | `user_warning_does_not_break_compile` | \`fixtures/c/user_diag.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `s_output_disassembles_function_body` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `s_output_disassembles_return_thunk_symbol` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `s_output_emits_custom_section_functions` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `s_output_disassembles_lock_cmpxchg` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `kbuild_define_emits_ascii_in_asm_output` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_register_planner_avoids_gp_clobbers` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_register_planner_avoids_xmm_clobbers` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_register_planner_reassigns_early_clobber_outputs` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_register_planner_respects_fixed_gp_constraints` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_float_gp_input` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_float_gp_input_syntax_only` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_float_gp_output` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_float_rm_input` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_validation_allows_matching_operands` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_integer_xmm_input` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_explicit_gp_float_input` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_type_mismatch_warns_matching_float_to_gp_output` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `mini_asm_unknown_mnemonic_is_hard_error` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `mini_asm_invalid_operands_is_hard_error` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `o2_dce_preserves_outputless_inline_asm` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraints_compile` | \`examples/assembly/constraints.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `o2_goto_label_after_return` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_inline_qualifier_compiles` | \`fixtures/asm/asm_inline.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_local_label_compiles` | \`fixtures/asm/asm_local.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_label_insn_same_line_compiles` | \`fixtures/asm/asm_label.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_goto_hash_comment_compiles` | \`fixtures/asm/asm_goto.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_goto_output_writeback` | \`fixtures/asm/asm_goto_outputs.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `s_output_disassembles_movd_without_desync` | \`fixtures/c/vec_small_return.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `s_output_roundtrips_through_gas_for_small_vector` | \`fixtures/c/vec_small_return.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_cmpxchg_mem_compiles` | \`fixtures/asm/asm_cmpxchg.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_cmpxchg8b_has_opcode_and_disassembly` | \`fixtures/asm/asm_cmpxchg8b.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_pushfl_iret_has_opcode_and_disassembly` | \`fixtures/asm/asm_pushfl_iret.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_cmov_full_compiles_without_nop_fallback` | \`fixtures/asm/asm_cmov_full.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_bare_in_out_has_opcode_and_disassembly` | \`fixtures/asm/asm_bare_in_out.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_shldw_shrdw_has_opcode_and_disassembly` | \`fixtures/asm/asm_shldw_shrdw.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_bare_seg_prefix_has_opcode_and_disassembly` | \`fixtures/asm/asm_bare_seg_prefix.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_pushsection_data_compiles_with_sections` | \`fixtures/asm/asm_pushsection_data.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_pushsection_exec_code_lands_in_custom_section` | \`fixtures/asm/asm_pushsection_exec.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_q_constraint_compiles_and_runs` | \`fixtures/asm/asm_q_constraint.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_x86_reg_constraints_compile_and_run` | \`fixtures/asm/asm_x86_reg_constraints.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_nested_pushsection_compiles_with_sections` | \`fixtures/asm/asm_nested_pushsection.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_numeric_labels_are_scoped_per_block` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_empty_function_symbol_gets_starting_instruction` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `executable_pushsection_alignment_padding_is_nop` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_section_previous_vdso_pattern` | \`fixtures/asm/asm_section_previous_vdso.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `function_return_thunk_avoids_naked_ret_in_c_and_asm` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_extended_compiles_without_nop_fallback` | \`fixtures/asm/asm_ext.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_constraints_ext_compiles_and_runs` | \`fixtures/asm/asm_constr.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_constraint_ext2_compiles_and_runs` | \`fixtures/asm/asm_constraint_ext2.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_kernel_extra_compiles_without_nop_fallback` | \`fixtures/asm/asm_kernel.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_kernel_ext_compiles_without_nop_fallback` | \`fixtures/asm/asm_kernel_ext.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_kernel_features_byte_pushsection_popcnt` | \`fixtures/asm/asm_kernel_features.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_paravirt_boot_compiles_without_nop_fallback` | \`fixtures/asm/asm_paravirt.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_avx_aes_fma_tsx_compiles_without_nop_fallback` | \`fixtures/asm/asm_avx.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_x87_fpu_compiles_without_nop_fallback` | \`fixtures/asm/asm_x87_fpu.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_mmx_compiles_without_nop_fallback` | \`fixtures/asm/asm_mmx.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_string_loop_compiles_without_nop_fallback` | \`fixtures/asm/asm_string_loop.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_bmi_compiles_without_nop_fallback` | \`fixtures/asm/asm_bmi.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_operand_patterns_compiles_without_nop_fallback` | \`fixtures/asm/asm_operand_patterns.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_far_jumps_compiles_without_nop_fallback` | \`fixtures/asm/asm_far_jumps.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_global_asm_compiles_without_nop_fallback` | \`fixtures/asm/asm_global_asm.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_extended_forms_compiles_without_nop_fallback` | \`fixtures/asm/asm_extended_forms.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_sym_operand_forms_compiles_without_nop_fallback` | \`fixtures/asm/asm_sym_operand_forms.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_template_modifiers_compile_and_run` | \`fixtures/asm/asm_template_modifiers.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_raw_modifiers_p_a_text_output` | \`fixtures/asm/asm_raw_modifiers.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_warning_location_maps_to_real_source_line` | \`fixtures/asm/asm_warn_loc.h\` \`fixtures/asm/asm_warn_loc.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_warning_location_stable_across_header_cache_replay` | \`fixtures/asm/asm_warn_loc.h\` \`fixtures/asm/asm_warn_loc.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_unsupported_instructions_error_not_nop` | \`fixtures/asm/asm_unsupported_error.h\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_seg_prefix_and_modifiers_runtime` | \`fixtures/asm/asm_seg_prefix_percpu.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `byte_level_asm_nop_encoding` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `byte_level_asm_mov_eax_encoding` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `byte_level_asm_ret_encoding` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `byte_level_asm_frame_header_encoding` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `flag_clobber_frame` | \`fixtures/c/flag_clobber_frame.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_clobber_full_compiles_and_runs` | \`fixtures/asm/asm_clobber.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_advanced_features_compiles_and_runs` | \`fixtures/asm/asm_advanced_features.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_diag_validation_compiles_with_warnings` | \`fixtures/asm/asm_diag_validation.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_optimizer_preservation_compiles_and_runs` | \`fixtures/asm/asm_optimizer_preservation.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_debug_info_compiles_with_g` | \`fixtures/asm/asm_debug_info.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `asm_per_insn_debug_insn_entries` | \`fixtures/asm/asm_debug_insn.c\` | run | T0 |
| `tests/integration/c_asm.rs` | `m3_1_inline_asm` | \`fixtures/asm/asm_constraints_io_clobbers.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_volatile_semantics` | \`fixtures/asm/asm_volatile_semantics.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_label_symbol_naming` | \`fixtures/asm/asm_label_symbol.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_extended_constraint_coverage` | \`fixtures/asm/asm_extended_constraints.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_goto_label_expressions` | \`fixtures/asm/asm_goto_label_expr.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_dead_code_elimination` | \`fixtures/asm/asm_dce.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_goto_output_paths_compiles_and_runs` | \`fixtures/asm/asm_goto_output_paths.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_goto_label_addr_mix_compiles_and_runs` | \`fixtures/asm/asm_goto_label_addr_mix.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_nested_macro_rept_compiles_and_runs` | \`fixtures/asm/asm_nested_macro_rept.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_memory_clobber_optimizer_compiles_and_runs` | \`fixtures/asm/asm_memory_clobber_optimizer.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_avx512_runtime_compiles_and_runs` | \`fixtures/asm/asm_avx512_runtime.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_goto_multi_tu_compiles_and_runs` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_non_x86_backend_s_mode_compiles` | \`fixtures/asm/asm_non_x86_targets.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `asm_non_x86_backend_c_mode_no_panic` | \`fixtures/asm/asm_non_x86_targets.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `non_x86_asm_minus_s_preserves_global_asm_text` | \`fixtures/asm/asm_non_x86_preserve.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `non_x86_asm_minus_c_warns_on_asm_goto` | \`fixtures/asm/asm_non_x86_preserve.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `non_x86_asm_arm_mips_warn_on_inline_asm_expr` | \`fixtures/asm/asm_non_x86_preserve.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `non_x86_asm_constraint_resolution_minus_s` | \`fixtures/asm/asm_non_x86_constraints.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `non_x86_asm_memory_clobber_barrier` | \`fixtures/asm/asm_non_x86_constraints.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `non_x86_asm_operand_resolution_produces_register_names` | \`fixtures/asm/asm_non_x86_constraints.c\` | compile | T0 |
| `tests/integration/c_asm.rs` | `kernel_opt_asm_memory_clobber_preserved_at_o3` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `aarch64_neon_asm_basic` | (inline) | compile | T0 |
| `tests/integration/c_asm.rs` | `parallel_codegen_keeps_file_scope_register_asm_binding` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_label_test_renames_elf_symbols_exactly` | \`fixtures/asm/asm_label_test.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_asm.rs` | `asm_empty_template_minimal_stays_transparent_across_tu` | \`fixtures/asm/asm_minimal.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `arch_x86_wrapper_preserves_supported_kernel_flags` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `libdecnumber_combined_tu_compiles` | \`fixtures/gcc_tools/libdecnumber_combined.c\` | compile | T3 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `gcc_cc1plus_bootstrap_gate` | (inline) | compile | T3 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `strict_full_build_exports_required_kernel_gates` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `strict_kernel_scripts_reject_missing_artifacts` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `strict_vmlinux_regress_requires_debug_tools` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kasan_stack_redzone_poison_unpoison` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `kasan_global_redzone_padding` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `percpu_decl_compiles` | \`fixtures/c/percpu.c\` | compile | T0 |
| `tests/integration/c_kernel.rs` | `percpu_access_runtime` | \`fixtures/c/percpu_access.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `percpu_full_support_compiles` | \`fixtures/c/percpu_full.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `percpu_offset_compiles` | \`fixtures/c/percpu_offset.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kernel_builtin_coverage_runtime` | \`fixtures/c/builtin_kernel_coverage.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kernel_asm_instr_coverage_runtime` | \`fixtures/asm/asm_kernel_instr_coverage.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `static_key_maybe_compiles` | \`fixtures/c/skey_maybe.c\` | compile | T0 |
| `tests/integration/c_kernel.rs` | `static_key_chain_compiles` | \`fixtures/c/skey_chain.c\` | compile | T0 |
| `tests/integration/c_kernel.rs` | `static_key_decl_compiles` | \`fixtures/c/skey_decl.c\` | compile | T0 |
| `tests/integration/c_kernel.rs` | `kernel_config_has_serial_console_for_qemu_boot` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `qemu_boot_real_kernel_produces_boot_log` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kernel_object_corpus_arch_x86_kernel` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `kernel_object_corpus_kernel_sched` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `kernel_object_corpus_lib` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `orc_consumer_objtool_strict_single_object` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `orc_consumer_function_boundaries_objtool` | \`fixtures/c/func_symbol_size.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kasan_runtime_out_of_bounds` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kcsan_runtime_race_instrumentation` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kcov_fuzzer_trace_cmp` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kcov_fuzzer_trace_switch` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kcov_fuzzer_8bit_counters` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kcov_fuzzer_pc_table` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kcov_fuzzer_no_sanitize_interaction` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kernel_kasan_inline_shadow_report_emitted` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `tsan_with_kasan_emits_both_checks` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `kasan_store_check_on_postinc_deref_emitted` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `kasan_aggregate_load_store_above_16_bytes` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `linux_header_compiler_types_preprocess_e2e` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `linux_header_rwonce_preprocess_e2e` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `linux_header_autoconf_preprocess_e2e` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `linux_header_graph_dependency_closure` | (inline) | compile | T0 |
| `tests/integration/c_kernel.rs` | `kernel_opt_volatile_not_eliminated_at_o2` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kernel_opt_barrier_prevents_reordering_at_o2` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `ftrace_modes_compile` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_kernel.rs` | `kernel_code_model_data_symbols_bypass_got` | \`fixtures/c/kernel_code_model_data.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_ptr_func_decl_compiles` | \`fixtures/c/attr_ptr.c\` | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `attr_propagation_compiles_and_runs` | \`fixtures/c/attr_propagation.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `object_size_builtin_run_returns_zero` | \`fixtures/c/objsize2.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_object_size_fortify_run_returns_zero` | \`fixtures/c/obj_size.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_apply_return_run_returns_zero` | \`fixtures/c/apply_ret.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_prefetch_compiles` | \`fixtures/c/prefetch.c\` | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_constant_p_full_run_returns_zero` | \`fixtures/c/constp.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_clear_cache_compiles` | \`fixtures/c/builtin_clear_cache.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_shift_overflow_run_returns_zero` | \`fixtures/c/builtin_shift_overflow.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_cpu_compiles` | \`fixtures/c/builtin_cpu.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_m12_extra_run_returns_zero` | \`fixtures/c/builtins_misc.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_expect_nonzero_p_run_returns_zero` | \`fixtures/c/builtin_expect_nonzero.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_rotate_clrsb_run_returns_zero` | \`fixtures/c/rotate.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_strcasecmp_run_returns_zero` | \`fixtures/c/strcasecmp.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_file_line_func_run_returns_zero` | \`fixtures/c/file_line.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_classify_compat_run_returns_zero` | \`fixtures/c/classify.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_choose_full_run_returns_zero` | \`fixtures/c/choose.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `unknown_builtin_is_hard_error_e0018` | \`fixtures/c/unk_builtin.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_cleanup_compile_and_run` | \`fixtures/c/attr_cleanup.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_target_clones_compile_and_run` | \`fixtures/c/attr_clones.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_no_sanitize_compile_and_run` | \`fixtures/c/attr_nosan.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_no_trailing_underscore_alias` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_copy_returns_nonnull_compile_and_run` | \`fixtures/c/attr_copy.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_copy_full_symbol_alias` | \`fixtures/c/attr_copy_full.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_returns_nonnull_runtime_check` | \`fixtures/c/attr_returns_nonnull.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `format_attr_warns_on_missing_arg` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `format_attr_comprehensive_no_warn` | \`fixtures/c/format_attr_full.c\` | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `format_attr_type_mismatch_warns` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `format_attr_gnu_printf_spelling` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `format_attr_scanf_non_pointer_warns` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `compiler_builtin_atomic_call_is_not_implicit_function_error` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `m15_builtin_fold` | \`fixtures/c/builtin_fold.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `math_builtin_copysign_libc` | \`fixtures/c/math_builtin_copysign.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `math_builtin_libm_forwarding` | \`fixtures/c/math_builtin_libm.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_attr_retain_compiles_and_runs` | \`fixtures/c/attr_retain.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_attr_used_compiles_and_runs` | \`fixtures/c/attr_used.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_attr_flatten_compiles_and_runs` | \`fixtures/c/attr_flatten.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_flatten_comprehensive_at_all_opt_levels` | \`fixtures/c/attr_flatten_full.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_flatten_asm_no_call_to_inlined` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `attr_target_clones_ifunc_symbols` | \`fixtures/c/attr_target_clones.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_attr_deprecated_compiles_and_runs` | \`fixtures/c/attr_deprecated.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_attr_ctor_priority_orders_correctly` | \`fixtures/c/attr_ctor_priority.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_builtin_dbg_compiles_and_runs` | \`fixtures/c/builtin_dbg.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m23_attr_retain_used_compiles_and_runs` | \`fixtures/c/attr_retain_used.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_builtin_trap_emits_ud2` | \`fixtures/c/builtin_trap.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_builtin_debugtrap_emits_int3` | \`fixtures/c/builtin_debugtrap.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_builtin_unreachable_emits_ud2` | \`fixtures/c/builtin_unreachable.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_lowlevel_builtins_runtime` | \`fixtures/c/builtin_lowlevel.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_alias_runtime` | \`fixtures/c/attr_alias.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_alias_symbol_exists` | \`fixtures/c/attr_alias.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_warning_emits_warning` | \`fixtures/c/attr_warning_call.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_error_emits_error` | \`fixtures/c/attr_error_call.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_ifunc_with_builtin_linker` | \`fixtures/c/attr_ifunc.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_ifunc_symbol_type_is_ifunc` | \`fixtures/c/attr_ifunc.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_ctor_priority_compiles` | \`fixtures/c/attr_ctor_priority.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_alias_target_missing_compiles` | \`fixtures/c/attr_alias_invalid.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_alias_var_runtime` | \`fixtures/c/attr_alias_var.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_alias_var_symbol_exists` | \`fixtures/c/attr_alias_var.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_alias_weak_runtime` | \`fixtures/c/attr_alias_weak.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_retain_symbol_exists` | \`fixtures/c/attr_retain.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_used_symbol_exists` | \`fixtures/c/attr_used.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_deprecated_emits_warning` | \`fixtures/c/attr_deprecated_call.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m27_attr_ctor_priority_invalid_compiles` | \`fixtures/c/attr_ctor_priority_invalid.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `wcs_builtins_forward_to_libc` | \`fixtures/c/wcs.c\` | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `m3_2_attributes` | \`fixtures/c/attrs.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `builtin_typedefs_size_ptrdiff` | \`fixtures/c/builtin_typedefs.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `c11_atomic_macros_hosted_compile_and_run` | \`fixtures/c/c11_atomic_macros.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `c11_atomic_freestanding_no_atomics` | \`fixtures/c/c11_atomic_freestanding.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `c11_atomic_lock_free_macros` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `atomic_fence_builtins_run` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `atomic_fetch_nand_run` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `atomic_test_and_set_clear_run` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `aarch64_atomic_tas_clear_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `riscv64_atomic_tas_clear_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `atomic_is_lock_free_run` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `aarch64_atomic_is_lock_free_cross` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `riscv64_atomic_is_lock_free_cross` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `sync_legacy_builtins_run` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `aarch64_atomic_fetch_nand_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `riscv64_atomic_fetch_nand_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `aarch64_atomic_fence_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `riscv64_atomic_fence_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `aarch64_atomic_builtins_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `riscv64_atomic_builtins_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_prefetch_emits_instructions` | \`fixtures/c/prefetch_test.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `builtin_nonzero_p_compiles` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `builtin_expect_with_probability_compiles` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `c11_atomic_struct_compile_and_run` | \`fixtures/c/c11_atomic_struct.c\` | run | T0 |
| `tests/integration/c_attr_builtin.rs` | `math_builtin_sqrt_inline_sse` | \`fixtures/c/math_builtin_sqrt.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `math_builtin_rounding_inline_sse` | \`fixtures/c/math_builtin_rounding.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `math_builtin_fmin_fmax_inline_sse` | \`fixtures/c/math_builtin_fmin_fmax.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `aarch64_atomic_builtins_asm_output` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `riscv64_atomic_builtins_asm_output` | (inline) | compile | T0 |
| `tests/integration/c_attr_builtin.rs` | `m27_static_chain_compiles` | \`fixtures/c/builtin_static_chain.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `m3_6_builtins` | \`fixtures/c/builtins.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_hot_pure_optimization_hints_match_gcc` | \`fixtures/c/attr_hot_pure.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_mode_integer_and_fp_modes_match_gcc` | \`fixtures/c/attr_mode.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_transparent_union_and_visibility_opts_match_gcc` | \`fixtures/c/attr_transparent_union.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `attr_malloc_family_decls_keep_external_linkage` | \`fixtures/c/attr_malloc.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `format_attr_decl_keeps_call_site_and_linkage` | \`fixtures/c/format_attr.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `builtin_misc_expand_fully_without_dangling_refs` | \`fixtures/c/builtin_misc.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `builtin_string_lowers_to_libc_entries_not_builtin_names` | \`fixtures/c/builtin_string.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_attr_builtin.rs` | `sync_atomic_builtins_full_surface_match_gcc` | \`fixtures/c/sync_atomic.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `failed_static_assert_is_a_hard_error` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `typeof_cast_in_statement_expression_parses` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `hosted_stdio_header_parses` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `typeof_struct_member_and_def_field_pattern` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `typeof_struct_compiles` | \`fixtures/c/typeof_struct.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `auto_type_decl_compiles` | \`fixtures/c/auto_type.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `static_assert_offsetof_compiles` | \`fixtures/c/sassert_off.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `div_round_up_macro_expands` | \`fixtures/c/div_round_up.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `gnu_elvis_operator_compiles` | \`fixtures/c/elvis.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `current_thread_info_macro_compiles` | \`fixtures/c/thread_info.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `nofail_stringify_compiles` | \`fixtures/c/stringify.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `typeof_generic_read_once_compiles` | \`fixtures/c/typeof_generic.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_constexpr_static_compile_and_run` | \`fixtures/c/c23_constexpr_static.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_constexpr_function_compile_and_run` | \`fixtures/c/c23_constexpr.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `dM_flag_emits_all_macros` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `include_macro_expansion` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `stringify_after_paste` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `pragma_family_compile_and_run` | \`fixtures/c/pragma_fam.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `pragma_operator_compile_and_run` | \`fixtures/c/pragma_op.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `gcc_system_header_compile_and_run` | \`fixtures/c/system_header.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `static_assert_stringification_succeeds` | \`fixtures/c/sassert_msg.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `stdatomic_macros_hosted` | \`fixtures/c/atomic.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m19_counter_in_function_macro` | \`fixtures/c/counter_in_macro.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m19_va_opt_empty_and_nonempty` | \`fixtures/c/va_opt.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m19_predefined_macros_available` | \`fixtures/c/predefined_macros.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `base_file_macro_available` | \`fixtures/c/base_file.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `include_level_macro_available` | \`fixtures/c/include_level_inner.h\` \`fixtures/c/include_level.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `timestamp_macro_available` | \`fixtures/c/timestamp.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m19_pragma_locale_accepted` | \`fixtures/c/pragma_locale.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_elifdef_elifndef_runs` | \`fixtures/c/elifdef.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_has_macros_c23_runs` | \`fixtures/c/has_macros.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_embed_limit_prefix_suffix_runs` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_embed_if_empty_runs` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_has_embed_selects_branch` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_pragma_once_dedupes_include` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_include_next_chain` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `m26_char16_char32_compiles_and_runs` | \`fixtures/c/char16_char32.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `fenv_pragma_parses_without_error` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `m3_4_typeof` | \`fixtures/c/typeof.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_alignas` | \`fixtures/c/c23_alignas.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `has_extension_macro` | \`fixtures/c/has_extension.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_constexpr_var` | \`fixtures/c/c23_constexpr_var.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_constexpr_var_error` | \`fixtures/c/c23_constexpr_var_error.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_fallthrough_nodiscard_compile_and_run` | \`fixtures/c/c23_fallthrough_nodiscard.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_nodiscard_warn_emits_w0401` | \`fixtures/c/c23_nodiscard_warn.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_bool` | \`fixtures/c/c23_bool.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_brace_init` | \`fixtures/c/c23_brace_init.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_enum_under` | \`fixtures/c/c23_enum_under.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_has_c_attribute` | \`fixtures/c/c23_has_c_attribute.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_nullptr` | \`fixtures/c/c23_nullptr.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_printf_b` | \`fixtures/c/c23_printf_b.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_static_assert` | \`fixtures/c/c23_static_assert.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_thread_local` | \`fixtures/c/c23_thread_local.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c23_typeof_unqual` | \`fixtures/c/c23_typeof_unqual.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `preprocess_only_with_output_file` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `preprocess_only_stdout` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `ms_pragma_accepted_noop` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `error_message_after_macro_expansion` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `error_in_header_carries_header_source_line_regression` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `macro_backtrace_limit_configurable` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `hex_float_syntax_ok` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `hex_float_run_returns_zero` | \`fixtures/c/hex_float.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `line_directive_syntax_ok` | \`fixtures/c/line_directive.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `gnu_range_init_syntax_ok` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `gnu_range_init_run_returns_zero` | \`fixtures/c/gnu_range_init.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c99_std_macros_syntax_ok` | \`fixtures/c/c99_std_macros.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `inttypes_format_macros_defined` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `errno_constants_defined` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c99_unicode_ident_syntax_ok` | \`fixtures/c/c99_unicode_ident.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c99_unicode_ident_run_returns_zero` | \`fixtures/c/c99_unicode_ident.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `signal_constants_defined` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `tgmath_macros_defined` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_stdc_version_value` | \`fixtures/c/c23_stdc_version.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c11_stdnoreturn_stdalign_macros` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c11_stdalign_macros_c11_mode` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c11_std_compliance_macros` | \`fixtures/c/c11_std_compliance_macros.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `hosted_iec559_iso10646_macros` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `offsetof_macro_via_builtin` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `has_feature_known_features` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_variadic_macro_no_leading_param` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c11_anonymous_struct_union_members` | \`fixtures/c/c11_anonymous_member.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_char8_t_type` | \`fixtures/c/c23_char8_t.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_constexpr_variables` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_std_attributes_effect` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_alignas_in_struct` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_enum_underlying_type` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_stdbit_macros` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c89_digraphs_compile_and_run` | \`fixtures/c/c89_digraphs.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c89_digraphs_preprocess` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c89_trigraphs_compile_and_run` | \`fixtures/c/c89_trigraphs.c\` | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c89_trigraphs_preprocess` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c89_trigraphs_not_in_strings` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c89_trigraphs_disabled_by_default` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `c99_restrict_compile_and_run` | \`fixtures/c/c99_restrict.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c11_noreturn_compile_and_run` | \`fixtures/c/c11_noreturn.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `c23_attributes_compile_and_run` | \`fixtures/c/c23_attributes.c\` | run | T0 |
| `tests/integration/c_pp_c23.rs` | `openmp_parallel_for_pragma` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `pragma_simd` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `openmp_simd_pragma` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `constant_builtin_and_static_assert_pass_through_typed_ir` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `float_builtin_macros_defined` | (inline) | compile | T0 |
| `tests/integration/c_pp_c23.rs` | `m1_3_preprocessor` | \`fixtures/c/preproc.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `c11_std_headers_compat_macros_compile` | \`fixtures/c/c11_std_headers.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `stmt_expr_auto_type_matches_gcc` | \`fixtures/c/stmt_auto.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `stmt_expr_typeof_matches_gcc` | \`fixtures/c/stmt_typeof.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_pp_c23.rs` | `forced_include_comment_header_matches_gcc` | \`fixtures/c/comment.h\` | compile | T0 · cfg(unix) |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_gp_path` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_overflow_path` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_double_path` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_double_overflow` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_no_external_symbols` | (inline) | compile | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_fixture_compiles` | \`fixtures/c/va_arg.c\` | compile | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_x86_64_long_double` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_list_as_function_param` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_list_pointer_deref` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_list_struct_member` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_va_copy_independence` | \`fixtures/c/va_arg/va_copy.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_struct_mixed` | \`fixtures/c/va_arg/struct_mixed.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_long_double_struct` | \`fixtures/c/va_arg/long_double.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_list_param_fixture` | \`fixtures/c/va_arg/va_list_param.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_aggregate_direct` | \`fixtures/c/va_arg/aggregate_va_arg.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_xmm_reg_count_not_eightbyte_count` | \`fixtures/c/va_arg/xmm_reg_count_not_eightbyte_count.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_align32_vector_memory` | \`fixtures/c/va_arg/align32_vector_mem.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_agg_via_va_list_param` | \`fixtures/c/va_arg/agg_via_va_list_param.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_int128_gp_boundary_no_split` | \`fixtures/c/va_arg/int128_gp_boundary_no_split.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_int128_via_va_list_param` | \`fixtures/c/va_arg/int128_via_va_list_param.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_int128_global_valist` | \`fixtures/c/va_arg/int128_global_valist.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_int128_loop_local_full_width` | \`fixtures/c/va_arg/int128_loop_local_full_width.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_copy_advanced_forms` | \`fixtures/c/va_arg/va_copy_advanced.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_double_precision` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `non_x86_qemu_runtime_va_arg` | (inline) | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_list_sse_compiles` | \`examples/advanced/va_list.c\` | compile | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_sseup_shares_one_slot` | \`fixtures/c/va_arg/sseup_shares_one_slot.c\` | run | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_sse_vs_sseup_slot_layout` | \`fixtures/c/va_arg/sse_vs_sseup_slot_layout.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_mixed_sse_pipeline_named` | \`fixtures/c/va_arg/mixed_sse_pipeline_named.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_mixed_sse_via_valist_param` | \`fixtures/c/va_arg/mixed_sse_via_valist_param.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_global_valist_sse_agg` | \`fixtures/c/va_arg/global_valist_sse_agg.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_vacopy_sse_agg` | \`fixtures/c/va_arg/vacopy_sse_agg.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_int128_sse_interleave` | \`fixtures/c/va_arg/int128_sse_interleave.c\` | cross-check | T0 |
| `tests/integration/c_abi_vararg.rs` | `va_arg_mix_m128_shared_slot_cross_tu_gcc_interop` | \`fixtures/c/va_arg_mix/m128_shared_slot_callee.c\` \`fixtures/c/va_arg_mix/m128_shared_slot_caller.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_abi_vararg.rs` | `va_arg_mix_int128_11named_cross_tu_gcc_interop` | \`fixtures/c/va_arg_mix/int128_11named_callee.c\` \`fixtures/c/va_arg_mix/int128_11named_caller.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_abi_vararg.rs` | `va_arg_mix_ld80_vararg_cross_tu_gcc_interop` | \`fixtures/c/va_arg_mix/ld80_vararg_callee.c\` \`fixtures/c/va_arg_mix/ld80_vararg_caller.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_abi_vararg.rs` | `va_arg_mix_ld80_namedparam_cross_tu_gcc_interop` | \`fixtures/c/va_arg_mix/ld80_namedparam_callee.c\` \`fixtures/c/va_arg_mix/ld80_namedparam_caller.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_abi_vararg.rs` | `va_arg_xfail_vla_agg_via_va_arg_still_diverges_from_gcc` | \`fixtures/c/va_arg_xfail/vla_agg_via_va_arg.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `debug_sections_emitted_with_g_flag` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `sanitizer_metadata_section_emitted` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `eh_frame_with_frame_pointer` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `global_asm_exports_symbol` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `btf_section_contains_struct_kind` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `eh_frame_larger_than_cie_only` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `patchable_entry_function_sections_keep_prefix` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `section_concat_attribute_compiles` | \`fixtures/c/section.c\` | compile | T0 |
| `tests/integration/c_link_debug.rs` | `orc_unwind_sections_not_emitted_by_dcc` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `orc_sections_not_emitted_when_unwind_tables_disabled` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `func_symbol_size_non_zero_for_defined_functions` | \`fixtures/c/func_symbol_size.c\` | compile | T0 |
| `tests/integration/c_link_debug.rs` | `exec_elf_has_symtab` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `sanitizer_large_asan_uses_libasan_generic_symbols` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `cmath_pass_through_emits_external_symbols` | \`fixtures/c/cmath.c\` | compile | T0 |
| `tests/integration/c_link_debug.rs` | `setjmp_x86_64_fixture_compiles` | \`fixtures/c/setjmp.c\` | compile | T0 |
| `tests/integration/c_link_debug.rs` | `setjmp_x86_64_eh_frame_has_cfi_ops` | \`fixtures/c/setjmp.c\` | compile | T0 · cfg(target_arch = "x86_64") |
| `tests/integration/c_link_debug.rs` | `wchar_libc_fixture_runs` | \`fixtures/c/wchar.c\` | cross-check | T0 |
| `tests/integration/c_link_debug.rs` | `signal_libc_fixture_runs` | \`fixtures/c/signal.c\` | cross-check | T0 |
| `tests/integration/c_link_debug.rs` | `sigaction_sigaltstack_fixture_compiles` | (inline) | cross-check | T0 · cfg(target_arch = "x86_64") |
| `tests/integration/c_link_debug.rs` | `et_exec_external_symbol_no_panic` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `m1_4_elf_format` | \`fixtures/c/elf.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `tls_model_codegen` | \`fixtures/c/tls_model_codegen.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `m3_7_weak_symbols` | \`fixtures/c/weak.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `symbol_resolve_nm_verification` | \`fixtures/c/symbol_resolve.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `m3_8_visibility` | \`fixtures/c/visibility.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `m3_9_tls` | \`fixtures/c/tls.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `m3_11_got_func_addr` | \`fixtures/c/got_func.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `comdat_weak_functions` | \`fixtures/c/comdat_weak.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `cet_ibt_plt_sec_and_gnu_property_sections` | (inline) | run | T0 |
| `tests/integration/c_link_debug.rs` | `cet_no_ibt_plt_sec_empty` | (inline) | run | T0 |
| `tests/integration/c_link_debug.rs` | `bind_now_plt_got_section` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `ms_declspec_dllimport_dllexport` | (inline) | run | T0 |
| `tests/integration/c_link_debug.rs` | `elf64_symtab_matches_gabi_layout` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `gnu_hash_section_content_valid` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `eh_frame_hdr_content_has_fde` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `ifunc_resolver_runtime` | \`fixtures/c/attr_ifunc.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `relro_pie_has_gnu_relro_segment` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `bind_now_dynamic_has_bind_now_flag` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `symver_directive_compiles` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `build_id_sha1_creates_note_section` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `dwarf_addr2line_resolves_function_name` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `dwarf_debug_line_section_has_content` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `btf_full_contains_source_types` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `dwarf_debug_str_contains_function_names` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `compress_debug_zlib_cli_accepted` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `compress_debug_zstd_linker_cli_accepted` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `compress_debug_none_preserves_uncompressed` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `stack_clash_protection` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `short_wchar_mode` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `dwarf_debug_info_generation` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `elf_section_types_init_fini_array` | \`fixtures/c/init_fini_array.s\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `comdat_group_elf_output` | \`fixtures/cpp/comdat_test.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `file_prefix_map_remaps_paths` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `symver_end_to_end` | \`fixtures/c/symver_e2e.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `comdat_group_gcc_inline_asm_section` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `comdat_aarch64_object_compiles` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `comdat_riscv64_object_compiles` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `elf32_symtab_matches_gabi_layout` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `visibility_attr_elf_symbols` | \`fixtures/c/visibility_attr.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `pie_builtin_linker_emits_interp_segment` | (inline) | run | T0 |
| `tests/integration/c_link_debug.rs` | `pie_builtin_linker_return_42` | (inline) | run | T0 |
| `tests/integration/c_link_debug.rs` | `builtin_linker_cleans_tmp_objs_on_failure` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `dwarf_readelf_debug_info_parseable_cu_header` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `btf_section_has_valid_header_magic` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `comdat_group_constants_defined` | (inline) | compile | T0 |
| `tests/integration/c_link_debug.rs` | `vdso_auto_detect_and_embed` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `vdso_embed_note_section_verification` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `btf_qualifier_records_and_strtab_track_qualified_declarations` | \`fixtures/c/btf_qualifiers.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `cfi_directives_survive_asm_text_and_eh_frame_chain` | \`fixtures/c/cfi_directives.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `shf_merge_strings_flags_and_payload_are_read_from_section_headers` | \`fixtures/c/shf_merge_strings.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `tls_model_attr_fixture_lays_out_tbss_and_emits_tlsdesc_on_a64` | \`fixtures/c/tls_model_attr.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_link_debug.rs` | `wcs_libc_call_passes_through` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `int128_mul_div_mod_reach_machine_codegen` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_hello_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_ir_add_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_if_while_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_nested_sret_cross_compile` | \`fixtures/c/aarch64_nested_sret.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_tls_cross_compile` | \`fixtures/c/aarch64_tls.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_float_call_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_hello_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_float_call_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_ir_global_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `mips_hello_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `mips_if_while_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `arm_hello_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `arm_if_while_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `mips64_hello_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cargo_config_has_musl_static_linking` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `non_x86_c_mode_produces_valid_elf` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_cross_call_runs_under_qemu` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `aarch64_vector_expression_codegen_covers_non_binop_paths` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `non_x86_qemu_runtime_matrix_runs_when_available` | (inline) | run | T0 |
| `tests/integration/c_cross.rs` | `non_x86_qemu_runtime_hello` | (inline) | run | T0 |
| `tests/integration/c_cross.rs` | `non_x86_qemu_runtime_control_flow` | (inline) | run | T0 |
| `tests/integration/c_cross.rs` | `non_x86_qemu_runtime_aggregate_abi` | (inline) | run | T0 |
| `tests/integration/c_cross.rs` | `aarch64_hfa_abi_compile_and_elf_structure` | \`fixtures/c/hfa_aarch64.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_hfa_abi_compile_and_elf_structure` | \`fixtures/c/hfa_riscv64.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `non_x86_asm_no_result_zero_warning_in_s_mode` | \`fixtures/asm/asm_non_x86_preserve.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `unsupported_target_arch_rejected` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `windows_darwin_triple_object_format` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `windows_msvc_output_is_coff` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `mips_arm_elf32_object_reader_accepts` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `windows_msvc_coff_object_reader_accepts` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `windows_msvc_output_is_pe_executable` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `stack_clash_protection_non_x86` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mavx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_cet` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_apx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_default_none` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_verbose_output_shows_features` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_sync_legacy_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_sync_legacy_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_tls_cross_compile` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `fno_flags_accepted` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `target_clones_resolver_code_generation` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `target_clones_resolver_vaddr_patching` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_tls_local_exec_compiles` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `aarch64_sve_asm_basic` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `static_local_cross_function_independence` | (inline) | run | T0 |
| `tests/integration/c_cross.rs` | `riscv64_cross_call_preserves_ra` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `c_ld_implicit_truncate_gcc_cross_check` | \`fixtures/c/ld_implicit_truncate.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `c_static_agg_by_value_gcc_cross_check` | \`fixtures/c/static_agg_by_value.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `c_agg_param_reg_stack_split_gcc_cross_check` | \`fixtures/c/agg_param_reg_stack_split.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `c_array2d_global_stride_gcc_cross_check` | \`fixtures/c/array2d_global_stride.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `c_agg_wide_value_load_gcc_cross_check` | \`fixtures/c/agg_wide_value_load.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `c_complex_component_conv_cross_tu_gcc_interop` | \`fixtures/c/complex_component_conv_provider.c\` \`fixtures/c/complex_component_conv_consumer.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `c_nested_loops_run_gcc_cross_check` | \`fixtures/c/nested_loops_run.c\` | cross-check | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `aarch64_hfa_asm_output_has_all_functions` | \`fixtures/c/hfa_aarch64.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_hfa_asm_output_has_all_functions` | \`fixtures/c/hfa_riscv64.c\` | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mno_avx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mavx2_implies_avx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mavx512f_implies_avx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mfma_implies_avx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mavxvnni_implies_avx` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_mno_avxvnni` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_avx10_256` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cpu_feature_cli_avx10_512` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `apx_egpr_reg_parse` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `march_x86_64_v3_enables_avx2` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `riscv64_tls_asm_output` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cross_compile_aarch64_predefined_macros` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cross_compile_arm_predefined_macros` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cross_compile_mips_predefined_macros` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cross_compile_riscv64_predefined_macros` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cross_compile_no_x86_macros_on_arm` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `cross_compile_elf_machine_headers` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `march_cli_parsing` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `mtune_cli_parsing` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `mcpu_cli_parsing` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `march_native_detects_features` | (inline) | compile | T0 |
| `tests/integration/c_cross.rs` | `arm_member_offset_aarch64_addresses_members_at_exact_offsets` | \`fixtures/c/arm_member_offset.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `riscv_d_extension_emits_double_precision_ops_per_function` | \`fixtures/c/riscv_d_extension.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `naked_nonx86_bodies_skip_prologue_and_auto_return` | \`fixtures/c/naked_nonx86.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_cross.rs` | `arm32_member_offset_reads_use_exact_fp_immediates` | \`fixtures/c/arm_member_offset.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `md_dependency_file_lists_includes` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `dep_file_uses_backslash_continuations` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `dep_file_mp_emits_phony_targets` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `sanitizer_prologue_in_all_functions` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `host_wrapper_fixdep_failure_does_not_write_stub_depfile` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `gcc_poison_compile_and_run` | \`fixtures/c/gcc_poison.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `ubsan_runtime_division_by_zero` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `ubsan_runtime_shift_overflow` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `ubsan_runtime_vla` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `ubsan_runtime_null_dereference` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `ubsan_runtime_alignment_violation` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `ubsan_runtime_float_cast` | (inline) | sanitize | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `hwasan_software_tag_path` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `unknown_sanitizer_negative_diagnostic` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `m15_bounds_check_inrange` | \`fixtures/c/bounds_check.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `m15_bounds_check_oob_diag` | \`fixtures/c/bounds_check_oob.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `sanitizer_host_link_adds_runtime_flags` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `m27_fortify_memcpy_chk_links_correctly` | \`fixtures/c/builtin_fortify_chk.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `pch_create_then_include_compiles_and_runs` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `pch_missing_include_warns_and_compiles` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `pch_create_write_failure_warns_and_compiles` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `pch_create_preproc_error_warns_without_pch` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `ubsan_vla_bound_check_emitted` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `ubsan_null_deref_check_emitted` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `ubsan_float_cast_check_emitted` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `tsan_only_emits_tsan_access` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `m3_10_stack_canary` | \`fixtures/c/stack_canary.c\` | run | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `pch_negative_corrupted_content` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `pch_negative_write_failure_snapshot` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `fixdep_unescape_paths_with_spaces` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `fixdep_skips_mp_phony_targets` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `depfile_backslash_continuation_with_escaped_paths` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `stack_guard_reg_gs` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `incremental_first_compile_produces_object` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `incremental_second_compile_skipped` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `incremental_source_change_triggers_recompile` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `incremental_flag_change_triggers_recompile` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `incremental_without_flag_always_compiles` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `incremental_cli_flag_parsing` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `hwasan_mte_aarch64_asm_instructions` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `hwasan_mte_aarch64_addg_asm` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `hwasan_mte_aarch64_stg_asm` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `sanitizer_builtin_pie_records_runtime_needed` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `pch_negative_hash_mismatch_with_define` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `pch_negative_version_incompatibility` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `fixdep_skips_hash_comments_in_depfile` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `m_flag_outputs_deps_to_stdout_includes_system_headers` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `mm_flag_outputs_deps_excludes_system_headers` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `mmd_excludes_isystem_headers_from_depfile` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `fixdep_file_open_failure_exits_nonzero` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `mq_flag_quotes_dollar_in_target` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `m_flag_with_mf_writes_to_file` | (inline) | compile | T0 |
| `tests/integration/c_sanitizer.rs` | `gcov_profile_fixture_emits_gcno_and_counts_blocks` | \`fixtures/c/profile_gcov.c\` | compile | T0 · cfg(unix) |
| `tests/integration/c_sanitizer.rs` | `mt_flag_uses_literal_target` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `cpp_auto_deduce` | \`fixtures/cpp/auto_deduce.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_decltype` | \`fixtures/cpp/decltype.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_decltype_auto` | \`fixtures/cpp/decltype_auto.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_final_override` | \`fixtures/cpp/final_override.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_friend` | \`fixtures/cpp/friend.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_if_constexpr` | \`fixtures/cpp/if_constexpr.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_member_ptr` | \`fixtures/cpp/member_ptr.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_mutable` | \`fixtures/cpp/mutable.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_mutable_const_method` | \`fixtures/cpp/mutable.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_member_ptr_deref` | \`fixtures/cpp/member_ptr_deref.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_typeid_dynamic_rtti` | \`fixtures/cpp/typeid_rtti.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_specialization` | \`fixtures/cpp/template_specialization.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_modules_odr` | \`fixtures/cpp/modules_odr.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_new_array` | \`fixtures/cpp/new_array.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_smoke` | \`fixtures/cpp/smoke.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_typeid` | \`fixtures/cpp/typeid.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_volatile_member` | \`fixtures/cpp/volatile_member.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_ssa_control_flow` | \`fixtures/cpp/ssa_control_flow.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_ssa_volatile_semantics` | \`fixtures/cpp/ssa_volatile_semantics.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_nttp_member_use` | \`fixtures/cpp/template_nttp_member_use.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_alias_template_expansion` | \`fixtures/cpp/alias_template_expansion.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_braced_init_var` | \`fixtures/cpp/template_braced_init_var.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_nested_call_arg` | \`fixtures/cpp/template_nested_call_arg.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_coroutine_std_qualified_awaiter_return` | \`fixtures/cpp/coroutine_std_qualified_awaiter_return.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_index` | \`fixtures/cpp/multi_index.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_concept` | \`fixtures/cpp/concept.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_modules` | \`fixtures/cpp/modules.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_member_ptr_dotstar` | \`fixtures/cpp/member_ptr_deref.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_typeid_rtti` | \`fixtures/cpp/typeid_rtti.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_derived_member_unqualified_lookup` | \`fixtures/cpp/derived_member_unqualified_lookup.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_typeid_rtti_name_before` | \`fixtures/cpp/typeid_rtti_name_before.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_arg_syntax` | \`fixtures/cpp/template_arg.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_depth_ok` | \`fixtures/cpp/template_depth.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_template_depth_limit_rejected` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_sfinae_smoke` | \`fixtures/cpp/sfinae.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_enhanced_sfinae_void_t_layer` | \`fixtures/cpp/enhanced_sfinae.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_g11_template_templates_recognized_not_lowered` | \`fixtures/cpp/g11_template_templates.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_new_arr_mangler` | \`fixtures/cpp/new_arr_mangler.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_new_array_alloc` | \`fixtures/cpp/new_array_alloc.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_coroutine_state_machine` | \`fixtures/cpp/coroutine_state.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_modules_bmf` | \`fixtures/cpp/modules_bmf.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_module_cross_tu_import` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_module_import_not_found_diagnostic` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_module_cross_tu_private_access_rejected` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_odr_conflict_rejected` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_odr_overload_not_conflict` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_odr_identical_imports_ok` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_gcc_compat` | \`fixtures/cpp/gcc_compat.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_comdat_template_instantiation` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `cpp98_iostream_clog_ws_compile` | \`fixtures/cpp/iostream_basic.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp98_dtor_implicit_noexcept_compile` | \`fixtures/cpp/dtor_implicit_noexcept.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp98_iterator_traits_compile` | \`fixtures/cpp/iterator_traits.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp11_using_type_alias_compile` | \`fixtures/cpp/using_type_alias.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp20_features_compile` | \`fixtures/cpp/cpp20_features.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp23_features_compile` | \`fixtures/cpp/cpp23_features.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_feature_keywords_all_detected` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `cpp_namespace_typedef_qualify_name_consistent` | (inline) | compile | T3 · cfg(feature = "legacy-strip-debug") |
| `tests/integration/cpp_lang.rs` | `cpp_switch_case_prefix_exact_gcc_cross_check` | \`fixtures/cpp/switch_case_prefix_exact.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_long_mul_wide_const_gcc_cross_check` | \`fixtures/cpp/long_mul_wide_const.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_coroutine_fat_awaiter_slot_run` | \`fixtures/cpp/coroutine_fat_awaiter_slot.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_declarator_local` | \`fixtures/cpp/multi_declarator_local.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_declarator_for_init` | \`fixtures/cpp/multi_declarator_for_init.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_declarator_global` | \`fixtures/cpp/multi_declarator_global.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_declarator_static_local` | \`fixtures/cpp/multi_declarator_static_local.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_declarator_member` | \`fixtures/cpp/multi_declarator_member.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_multi_declarator_typedef` | \`fixtures/cpp/multi_declarator_typedef.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_nested_for_two_level_run` | \`fixtures/cpp/nested_for_two_level.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_nested_for_three_level_break_continue_run` | \`fixtures/cpp/nested_for_three_level_break_continue.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_nested_for_while_and_while_dowhile_run` | \`fixtures/cpp/nested_for_while_and_while_dowhile.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_nested_loop_member_call_run` | \`fixtures/cpp/nested_loop_member_call.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_nested_loop_deep_stress_run` | \`fixtures/cpp/nested_loop_deep_stress.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_global_array_dims_and_sizeof_run` | \`fixtures/cpp/global_array_dims_and_sizeof.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_global_array_init_and_access_run` | \`fixtures/cpp/global_array_init_and_access.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_constexpr_call_folds_in_constant_context_run` | \`fixtures/cpp/constexpr_call_folds_in_constant_context.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_namespace_array_and_extern_pairing_run` | \`fixtures/cpp/global_namespace_array_and_extern.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `cpp_global_object_symbol_sizes_in_object_file` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_lang.rs` | `test_variadic_template_declaration` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_instantiate_variadic_tuple` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_type_key_with_pack` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_merge_expanded_args` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_regular_template_still_works` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_parse_variadic_fn_template_param_is_pack` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_parse_pack_expansion_and_sizeof_pack_ast_shape` | (inline) | compile | T0 |
| `tests/integration/cpp_lang.rs` | `test_parse_class_template_records_member_access` | (inline) | compile | T0 |
| `tests/integration/cpp_stdlib.rs` | `cpp_chrono` | \`fixtures/cpp/chrono.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_filesystem` | \`fixtures/cpp/filesystem.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_thread_mutex` | \`fixtures/cpp/thread_mutex.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_condition_variable` | \`fixtures/cpp/condition_variable.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_expected` | \`fixtures/cpp/expected.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_flat_map` | \`fixtures/cpp/flat_map.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_regex` | \`fixtures/cpp/regex.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_stdlib.rs` | `cpp_ranges` | \`fixtures/cpp/ranges.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_class_with_ctor_lowers_and_compiles` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_explicit_ctor` | \`fixtures/cpp/explicit_ctor.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_inheriting_ctor` | \`fixtures/cpp/inheriting_ctor.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_noexcept` | \`fixtures/cpp/noexcept.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_placement_new` | \`fixtures/cpp/placement_new.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_weak_vtable_multifile_link` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_ctor_dtor_virtual_dispatch` | \`fixtures/cpp/ctor_dtor_virtual_dispatch.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_delete_runs_virtual_dtor` | \`fixtures/cpp/delete_virtual_dtor.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_dynamic_cast_runtime_interop` | \`fixtures/cpp/dynamic_cast_basic.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_placement_new_ctor` | \`fixtures/cpp/placement_new_ctor.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_enum_semantics_runtime` | \`fixtures/cpp/enum_semantics.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_member_parser_runtime` | \`fixtures/cpp/member_parser.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_auto_deduction_runtime` | \`fixtures/cpp/auto_deduction.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_noexcept_expr_runtime` | \`fixtures/cpp/noexcept_expr.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_coroutine_runtime` | \`fixtures/cpp/coroutine_runtime.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_virtual_inheritance_runtime` | \`fixtures/cpp/virtual_inheritance.cpp\` | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_partition_import_runtime` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_export_single_declaration_private_fragment_runtime` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_private_fragment_internal_linkage` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_cross_tu_template_runtime` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_cross_tu_virtual_runtime` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_imported_polymorphic_ctor_not_duplicated` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_module_mangled_linkage_no_name_collision` | (inline) | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_template_vector_int_instantiation` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `cpp_vector_header_is_template_class` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `cpp_coroutine_co_await_runtime` | \`fixtures/cpp/coroutine_co_await.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_coroutine_shared_promise_runtime` | \`fixtures/cpp/coroutine_shared_promise.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_global_static_initializers_run` | \`fixtures/cpp/global_static_initializers.cpp\` | cross-check | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `dump_correspondence_report` | (inline) | report-only | T3 · ignored |
| `tests/integration/cpp_runtime.rs` | `every_defined_entity_is_registered_in_hir` | (inline) | cross-check | T0 |
| `tests/integration/cpp_runtime.rs` | `every_defined_entity_is_emitted_by_legacy` | (inline) | cross-check | T0 |
| `tests/integration/cpp_runtime.rs` | `legacy_emits_no_plain_named_entity_native_lacks` | (inline) | cross-check | T0 |
| `tests/integration/cpp_runtime.rs` | `every_native_class_has_a_legacy_struct_definition` | (inline) | cross-check | T0 |
| `tests/integration/cpp_runtime.rs` | `hir_dump_exposes_functions_and_stmts` | (inline) | cross-check | T0 |
| `tests/integration/cpp_runtime.rs` | `help_documents_the_cxx_native_switch` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `invalid_mode_is_a_hard_error` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `off_forces_legacy_with_visible_decision_and_summary` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `default_and_on_use_the_native_path` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `on_never_falls_back_silently` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `auto_fallback_is_visible_in_verbose` | (inline) | compile | T0 |
| `tests/integration/cpp_runtime.rs` | `strip_cpp_keywords_is_not_exported_in_default_build` | (inline) | cross-check | T0 · cfg(not(feature = "legacy-strip-debug")) |
| `tests/integration/cpp_runtime.rs` | `lower_module_remains_accessible_in_default_build` | (inline) | compile | T0 · cfg(not(feature = "legacy-strip-debug")) |
| `tests/integration/cpp_runtime.rs` | `cpp_strip_keywords_all_removed` | (inline) | compile | T3 · cfg(feature = "legacy-strip-debug") |
| `tests/integration/cpp_runtime.rs` | `cpp_cxx_native_off_forces_legacy` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_cxx_native_on_forces_native` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_cxx_native_auto_uses_native_for_plain_class` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/cpp_runtime.rs` | `cpp_cxx_native_auto_falls_back_for_unsupported` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `flto_summary_includes_function_size` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `o1_constant_folding_emits_immediate` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `o2_constant_branch_fold` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `parallel_codegen_o2_multi_function` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `vector_binop_non_ident_run_returns_zero` | \`fixtures/c/vec_binop.c\` | run | T0 |
| `tests/integration/opt.rs` | `vector_global_copy_compiles` | \`fixtures/c/vec_global.c\` | compile | T0 |
| `tests/integration/opt.rs` | `vector_global_run_returns_zero` | \`fixtures/c/vec_run.c\` | run | T0 |
| `tests/integration/opt.rs` | `vector_init_rvalue_run_returns_zero` | \`fixtures/c/vec_init.c\` | run | T0 |
| `tests/integration/opt.rs` | `vector_small_return_uses_width_aware_xmm_load` | \`fixtures/c/vec_small_return.c\` | compile | T0 |
| `tests/integration/opt.rs` | `stdc_fp_contract_controls_fma_codegen` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `vector_size_cast_in_expression_run_returns_zero` | \`fixtures/c/vec_cast.c\` | run | T0 |
| `tests/integration/opt.rs` | `vector_float_scalar_run_returns_zero` | \`fixtures/c/vec_fscalar.c\` | run | T0 |
| `tests/integration/opt.rs` | `inline_attrs_o1_run_returns_zero` | \`fixtures/c/inline.c\` | run | T0 |
| `tests/integration/opt.rs` | `always_inline_o0_run_returns_zero` | \`fixtures/c/always_inline.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `lto_link_strips_unreferenced_static` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `m16_ssa_ir` | \`fixtures/opt/ssa_ir.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `m16_strength_reduction` | \`fixtures/opt/strength_reduction.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `m16_sccp` | \`fixtures/opt/sccp.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `m16_loop_opt` | \`fixtures/opt/loop_opt.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `m16_tco` | \`fixtures/opt/tco.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `m16_function_attr` | \`fixtures/opt/function_attr.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `fenv_runtime_calls_are_inlined` | \`fixtures/c/fenv.c\` | compile | T0 |
| `tests/integration/opt.rs` | `ssa_volatile_semantics_compile_and_run` | \`fixtures/c/ssa_volatile_semantics.c\` | run | T0 |
| `tests/integration/opt.rs` | `ssa_qualifier_fallback_switch_label` | \`fixtures/c/ssa_qualifier_fallback.c\` | run | T0 |
| `tests/integration/opt.rs` | `sched_barrier_patterns_compiles` | \`fixtures/c/sched_barrier_patterns.c\` | compile | T0 |
| `tests/integration/opt.rs` | `sched_rq_lock_patterns_compiles` | \`fixtures/c/sched_rq_lock_patterns.c\` | compile | T0 |
| `tests/integration/opt.rs` | `array_dim_const_fold_compiles` | \`fixtures/c/array_dim_const_fold.c\` | compile | T0 |
| `tests/integration/opt.rs` | `ssa_alias_analysis_non_aliasing` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `ssa_sccp_non_constant_not_folded` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `ssa_tco_deep_recursion_no_overflow` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `ssa_dae_reachable_function_preserved` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `ssa_ipa_sra_scalar_propagation` | (inline) | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `qualifier_aware_optimization` | \`fixtures/c/qualifier_opt.c\` | compile | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `ssa_preserves_volatile_qualifier` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `ssa_volatile_not_eliminated` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `lto_inline_config_default` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `lto_inline_small_function_compiles` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `ofast_implies_o3_fast_math` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `fno_fast_math_cli` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `fp_contract_fast_cli` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `fp_contract_off_cli` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `auto_vectorize_vnni_int16_pattern` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `dir_vector_always` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `dir_ivdep` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `dir_unroll` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `software_pipelining_simple_loop` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `regalloc_many_locals_o3_compile_and_run` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `regalloc_nested_loops_o2` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `m16_cross_function_dce` | \`fixtures/opt/cross_function_dce.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `cross_tu_lto_bitcode_serialize_deserialize_roundtrip` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `cross_tu_lto_multiple_tus_compile_and_link` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `cross_tu_lto_const_global_propagation` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `cross_tu_lto_inline_small_function` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `lto_bitcode_section_present_in_object` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `lto_bitcode_verbose_output_shows_section_sizes` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `lto_single_object_relocatable_link` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `flto_relocatable_link_merges_section` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `flto_section_emitted` | (inline) | compile | T0 |
| `tests/integration/opt.rs` | `opt_gvn_o2_run_returns_zero` | \`fixtures/opt/gvn.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `opt_loop_unroll_o3_run_returns_zero` | \`fixtures/opt/unroll.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `opt_loop_vectorize_o3_run_returns_zero` | \`fixtures/opt/vecopt.c\` | run | T0 · cfg(unix) |
| `tests/integration/opt.rs` | `ffast_math_cli` | (inline) | compile | T0 |
| `tests/integration/xfail.rs` | `objtool_xfail_cant_decode_instruction` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/xfail.rs` | `objtool_xfail_cant_find_starting_instruction` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/xfail.rs` | `objtool_xfail_cant_find_jump_dest` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/xfail.rs` | `objtool_xfail_rethunk_naked_return` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/xfail.rs` | `xfail_unknown_offload_triple_diagnostic` | (inline) | compile | T0 |
| `tests/integration/xfail.rs` | `xfail_vector_scalar_size_mismatch_rejected` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `static_build_script_exists` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/glue_misc.rs` | `host_wrapper_rejects_unknown_wp_option` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/glue_misc.rs` | `host_wrapper_accepts_whitelisted_wp_options` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/glue_misc.rs` | `m15_strict_warning_compile` | \`fixtures/c/strict_warning.c\` | compile | T0 |
| `tests/integration/glue_misc.rs` | `m2_6_batch_compile` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/glue_misc.rs` | `error_diag_github_format_caret_and_source` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `error_diag_msvc_format` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `error_diag_clang_format` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `warning_does_not_block_compilation` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `unsupported_z_options_are_errors` | (inline) | run | T0 |
| `tests/integration/glue_misc.rs` | `no_common_mode` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/glue_misc.rs` | `unknown_flags_emit_warnings` | (inline) | compile | T0 · cfg(unix) |
| `tests/integration/glue_misc.rs` | `jobs_flag_parsing` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `response_file_with_comments_and_quotes` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `m5_bootstrap_selftest` | \`fixtures/c/bootstrap_selftest.c\` | compile | T0 |
| `tests/integration/glue_misc.rs` | `m5_bootstrap_determinism` | \`fixtures/c/bootstrap_selftest.c\` | compile | T0 |
| `tests/integration/glue_misc.rs` | `response_file_at_file_expands_arguments` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `pipe_input_c_mode_stdin_to_stdout` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `pipe_input_e_mode_stdin_to_stdout` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `pipe_input_s_mode_stdin_to_stdout` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `pipe_input_default_output_name_is_a_o` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `multi_file_parallel_compile` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `multi_file_parallel_byte_identical` | (inline) | compile | T0 |
| `tests/integration/glue_misc.rs` | `multi_file_rejects_output_flag` | (inline) | compile | T0 |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_virtual_group` | \`fixtures/cpp_abi_virtual\` | cross-check | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_lifecycle_group` | \`fixtures/cpp_abi_lifecycle\` | cross-check | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_overload_group` | \`fixtures/cpp_abi_overload\` | cross-check | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_rtti_group` | \`fixtures/cpp_abi_rtti\` | cross-check | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_eh_group` | \`fixtures/cpp_abi_eh\` | cross-check | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_template_group` | \`fixtures/cpp_abi_template\` | cross-check | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_groups_are_all_covered` | \`fixtures/cpp_abi_virtual\` \`fixtures/cpp_abi_lifecycle\` \`fixtures/cpp_abi_overload\` `+3 more` | compile | T0 · file-cfg(unix) |
| `tests/cpp_abi_gcc_cross_check.rs` | `cpp_abi_fixtures_are_standard_cxx` | (inline) | cross-check | T0 · file-cfg(unix) |
| `tests/errors.rs` | `diagnostic_error_snapshots` | (inline) | compile | T0 |
| `tests/errors.rs` | `e0100_no_input_file_cli` | (inline) | compile | T0 |
| `tests/errors.rs` | `e0100_cannot_read_file` | (inline) | compile | T0 |
| `tests/errors.rs` | `w0001_implicit_int_warning_snapshot` | \`fixtures/errors/w0001.c\` | compile | T0 |
| `tests/errors.rs` | `w0004_asm_goto_missing_label_warning_snapshot` | \`fixtures/errors/w0004.c\` | compile | T0 |
| `tests/errors.rs` | `w0501_implicit_fallthrough_warning_snapshot` | \`fixtures/errors/w0501.c\` | compile | T0 |
| `tests/errors.rs` | `w0501_cpp_implicit_fallthrough_warning_snapshot` | \`fixtures/errors/w0501_cpp.cpp\` | compile | T0 |
| `tests/errors.rs` | `e0205_unreadable_include_snapshot` | \`fixtures/errors/e0205.c\` | compile | T0 · cfg(unix) |
| `tests/errors.rs` | `e0209_force_include_unreadable_snapshot` | \`fixtures/errors/e0209.c\` | compile | T0 · cfg(unix) |
| `tests/errors.rs` | `e0208_invalid_utf8_snapshot` | (inline) | compile | T0 |
| `tests/errors.rs` | `diagnostic_format_locked_snapshot` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diagnostic_format_version_is_current` | (inline) | compile | T0 |
| `tests/errors.rs` | `all_snapshots_match_locked_schema` | (inline) | compile | T0 |
| `tests/errors.rs` | `cases_have_matching_fixtures_and_snapshots` | (inline) | compile | T0 |
| `tests/errors.rs` | `cli_multiple_sources_error` | (inline) | compile | T0 |
| `tests/errors.rs` | `cli_relocatable_requires_object_inputs` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diagnostic_format_has_location_and_caret` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_color_never_strips_ansi` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_color_always_emits_ansi` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_color_rejects_unknown_value` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_format_clang_strips_brackets` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_format_msvc_uses_file_line_col` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_format_vi_is_single_line` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_format_sarif_emits_json` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_format_rejects_unknown_value` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/errors.rs` | `diag_template_tree_runs_without_panic` | \`fixtures/errors/e0004.c\` | compile | T0 |
| `tests/perf_regression.rs` | `perf_extern_decl_scaling` | (inline) | compile | T0 |
| `tests/perf_regression.rs` | `perf_function_param_scaling` | (inline) | compile | T0 |
| `tests/perf_regression.rs` | `perf_large_frame_compiles` | (inline) | compile | T0 |
| `tests/perf_regression.rs` | `perf_deep_declaration_no_stack_overflow` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_profile_generate_is_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_profile_use_missing_path_rejects` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_emit_asm_still_works_with_profile` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_test_coverage_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `gcov_end_to_end_counter_gate` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_autofdo_text_profile_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_autofdo_binary_profile_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_autofdo_missing_path_rejects` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_lbr_profile_text_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_lbr_profile_missing_path_rejects` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_memprof_use_binary_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_memprof_use_missing_path_rejects` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_topdown_with_pgo_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `autofdo_weight_influences_section_layout` | (inline) | compile | T0 |
| `tests/profile.rs` | `lbr_weight_influences_section_layout` | (inline) | compile | T0 |
| `tests/profile.rs` | `memprof_weight_influences_section_layout` | (inline) | compile | T0 |
| `tests/profile.rs` | `topdown_metrics_with_optimization` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_driver_link_nvcc_missing_generates_host_flags` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_driver_link_rocm_search_dirs` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_driver_link_host_flags_include_rpath` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_driver_link_device_link_commands` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_driver_link_diagnostics_on_missing_runtime` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_driver_link_render_script` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_compile_without_nvcc_succeeds` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_runtime_kernel_launch_returns_not_available_without_driver` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_runtime_rocm_probe_returns_not_available_without_driver` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_cuda_probe_returns_not_available_without_driver` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_runtime_launch_without_device_code_fails_fast` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_runtime_kernel_not_found_returns_correct_error` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_record_profile_rejects_missing_path` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fopenmp_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fopenmp_targets_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fcuda_and_fhip_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fmemprof_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `memprof_runtime_end_to_end_gate` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_topdownmetrics_accepted` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_lbr_profile_rejects_missing` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fopenmp_eq_variants_parsed` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fopenmp_targets_forms_parsed` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_fopenmp_targets_bad_triple_rejects_with_value` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_offload_arch_forms_parsed` | (inline) | compile | T0 |
| `tests/profile.rs` | `cli_offload_arch_bad_value_rejects_with_value` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_openmp_emits_section_with_pragma_locations` | \`fixtures/c/offload_omp.c\` | compile | T0 |
| `tests/profile.rs` | `offload_cuda_emits_section_with_func_locations` | \`fixtures/c/offload_cuda.c\` | compile | T0 |
| `tests/profile.rs` | `offload_arch_recorded_in_entry` | \`fixtures/c/offload_cuda.c\` | compile | T0 |
| `tests/profile.rs` | `offload_cuda_requires_flag_with_precise_location` | \`fixtures/c/offload_cuda.c\` | compile | T0 |
| `tests/profile.rs` | `offload_global_host_mixed_rejects_with_location` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_hip_emits_section_with_amd_triple` | \`fixtures/c/offload_cuda.c\` | compile | T0 |
| `tests/profile.rs` | `pgo_profile_values_generates_runtime_histogram_gate` | (inline) | compile | T0 |
| `tests/profile.rs` | `pgo_end_to_end_profraw_merge_profdata_use_gate` | (inline) | compile | T0 |
| `tests/profile.rs` | `offload_device_codegen_pipeline_verbose_output` | \`fixtures/c/offload_cuda.c\` | compile | T0 |
| `tests/profile.rs` | `offload_device_codegen_unsupported_triple_error` | (inline) | compile | T0 |
| `tests/profile.rs` | `profile_values_standalone_compilation` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `o2_object_is_byte_reproducible` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `o3_object_is_byte_reproducible` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `o2_assembly_is_byte_reproducible` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `o2_with_debuginfo_is_byte_reproducible` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `o0_and_o1_stay_byte_reproducible` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `linked_executable_is_byte_reproducible` | (inline) | compile | T0 |
| `tests/reproducibility.rs` | `rebuild_block_uses_source_declaration_order` | (inline) | compile | T0 |
<!-- END GENERATED -->
