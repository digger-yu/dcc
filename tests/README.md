# DCC 测试体系

> 本目录是 DCC 编译器全部集成与回归测试的根目录。
> 本 README 是「**怎么跑、怎么加**」的权威，Tier 分层与主题子文件规则以本文为准；
> fixture 细则见 [`tests/fixtures/README.md`](fixtures/README.md)，
> 「特性 → 夹具 → 用例」的机器生成索引见
> [`docs/test_coverage.md`](../docs/test_coverage.md)（本文不复制那份索引，
> 避免出现第二套真源）。

---

## 1. 分层与入口

```text
Tier 0  PR 必绿（本地等价：make test）——本文 §1.1
Tier 1  Linux CI（.github/workflows：ci.yml / gcc-dg.yml / torture.yml）
Tier 2  内核 / 性能（scripts/kernel_full_check.sh、QEMU 启动矩阵、kernel-bench.yml）
Tier 3  可选深测（libiberty / GCC 源树：设 `GCC_SRC` 环境变量、STRICT_FULL_BUILD=1）
```

Tier 的**权威命令表**就是本文 §1.1；CI 侧的分层导航见 [`docs/ci_guide.md`](../docs/ci_guide.md) §6。

### 1.1 Tier 0 命令清单（提交前逐条跑）

```bash
cargo build --tests
cargo test --release                                  # 全部 8 个顶层 target
bash scripts/check_fixtures.sh                        # fixture 引用完整性：硬失败，无过渡开关
bash scripts/check_fixtures_selftest.sh               # 元门禁：证明上一条真的会红（5 类反向注入逐条 exit 1）
```

Makefile 入口与 Tier 0 的对应关系（终态：`make test` 逐字跑完上面四条；`make test-all` = `make test`
+ `scripts/test_all.sh`，后者只做 smoke 端到端冒烟的薄编排，不复制断言逻辑）以本文 §1.1
与 `Makefile` 里 `test:` 目标上方的注释为准。

上面四条是“该跑什么”的清单，不是“提交前会自动跑什么”：本地装一个钩子可以把
gcc.c-torture 零失败变成硬约束（`make hooks`，装的是
[`scripts/git-hooks/pre-commit`](../scripts/git-hooks/pre-commit)）。钩子比 §1.1 更窄
（只守 torture 一项，不代替上面四条），判据与代价以该脚本头注释为准，本节不重述。
钩子装在 `.git/hooks/` 下，不入库，所以它是个人级便利而非 CI 保证；PR 侧的权威仍是
Tier 1 的 `.github/workflows/torture.yml`。

### 1.2 单测过滤习惯

`tests/integration/` 下的主题子文件被壳 `include!` 进同一个 target，**函数名过滤照常有效**：

```bash
cargo test --test integration asm_          # include! 后 fn 名子串过滤不变
cargo test --test errors
cargo test --lib
```

---

## 2. 目录终态

```text
tests/
├── README.md                      # 本文件
├── common/
│   ├── harness.rs                 # 公共 harness 总入口（条目全部 pub(crate)；以 #[path] 挂载
│   │                              #  下列子模块并 pub(crate) use …::* 重导出，契约见本文 §5）
│   └── harness/
│       ├── elf.rs                 # ELF64 节/符号/动态段解析（elf64_*、readelf_relocations…）
│       ├── driver_cli.rs          # 驱动 CLI 面（dcc_cmd、fixtures_dir、write_src、temp_dir）
│       └── cpp_frontend.rs        # C++ native/legacy 对照助手（callable_key、lower_both…）
├── integration.rs                 # 壳：< 300 行，只有 use + #[path] mod harness + 15 行 include!
├── integration/                   # 主题子文件（被 include!，不是独立 cargo target）
│   ├── c_lang.rs                  # 通用 C 语义
│   ├── c_asm.rs                   # inline asm / mini-asm（关联 fixtures/asm/）
│   ├── c_kernel.rs                # 内核构造（percpu/static_key/objtool/…）
│   ├── c_attr_builtin.rs          # __attribute__ / __builtin__
│   ├── c_pp_c23.rs                # 预处理器与 C89/C11/C23 特性
│   ├── c_abi_vararg.rs            # va_list/va_arg/va_copy/SSE-up ABI
│   ├── c_link_debug.rs            # DWARF/reloc/section/ifunc/vdso/ELF 结构
│   ├── c_cross.rs                 # 交叉目标（aarch64/riscv/mips/qemu/windows/darwin）
│   ├── c_sanitizer.rs             # asan/ubsan/gcov/pgo/incremental/depfile
│   ├── cpp_lang.rs                # C++ 语言面（template/typeid/coroutine/modules/namespace）
│   ├── cpp_stdlib.rs              # C++ 标准库面（chrono/string/fstream/filesystem）
│   ├── cpp_runtime.rs             # C++ 运行期/链接面（vtable/ctor-dtor/eh/throw/catch/abi）
│   ├── opt.rs                     # 优化 pass（lto/ssa/fold/inline/dce/devirt/vector）
│   ├── xfail.rs                   # 已知预期失败（关联 fixtures/c_xfail/）
│   └── glue_misc.rs               # 兜底，硬上限 60 test / 1200 行（防新杂物堆）
├── errors.rs                      # 错误快照测试（独立 target，写法范本）
├── profile.rs                     # PGO/profile CLI 面（独立 target）
├── perf_regression.rs             # 性能回归（独立 target）
├── reproducibility.rs             # 确定性/可复现（独立 target）
├── cpp_abi_gcc_cross_check.rs     # C++ ABI GCC 差分（独立 target：按目录发现夹具）
├── skip/
│   ├── README.md
│   ├── gcc_dg_skip.txt
│   └── torture_skip.txt
└── fixtures/                      # 静态夹具（详见 fixtures/README.md）
    ├── README.md
    ├── c/                         # 通用 C 正例
    ├── asm/                       # asm_*.c/h 与 asm 专用头
    ├── cpp/                       # C++ 正例（*.cpp）
    ├── errors/                    # eNNNN.c + eNNNN.stderr.snap（含 W 码）
    ├── opt/                       # 优化 pass 样例
    ├── c_xfail/                   # 已知预期失败夹具
    ├── cpp_abi_template/          # ┐
    ├── cpp_abi_eh/                # │ C++ ABI 差分夹具，按目录名被动态发现
    ├── cpp_abi_rtti/              # │
    ├── cpp_abi_virtual/           # │
    ├── cpp_abi_overload/          # │
    ├── cpp_abi_lifecycle/         # ┘
    ├── gcc_tools/                 # libiberty 探测组合（白名单目录）
    ├── corpus/                    # 真实 .o 二进制样本（白名单目录）
    ├── probe/                     # 后端能力探针（归 backend 案，白名单目录）
    └── _archive/                  # 无引用的历史件：禁止被任何 include_str!/.join() 引用
```

**顶层 `tests/*.rs` cargo target = 8 个**：`integration`、`errors`、`profile`、
`perf_regression`、`reproducibility`、`cpp_abi_gcc_cross_check`、`disasm_snapshot`、
`g10_snapshot_baseline`。
新增顶层 target 不是默认动作：先把测试挂进合适的主题子文件。

`tests/gcc-dg/`、`tests/gcc-torture/` 是 GCC 官方套件的本地副本（`.gitignore` 排除，
由 `scripts/setup_torture.sh` 与 `scripts/gcc_dg_test.sh` 等驱动），**不是** `fixtures/` 成员。

### 2.1 主题子文件的硬性规则

1. 首行注释固定为 `// included from tests/integration.rs — not a standalone cargo target`。
2. 子文件内**禁止**出现 `use` / `mod` / `fn main`（导入权与 harness 挂载权只属于壳）。
3. 夹具路径一律 `include_str!("../fixtures/…")`（冒烟语料同在 `../fixtures/smoke/…`）。
4. 运行时路径继续用 `env!("CARGO_MANIFEST_DIR")`，**不得**改成相对路径。
5. 单文件 ≤ 4000 行，超限按二级主题再切并同样走 `include!`。
6. 跨主题复用的助手进 `tests/common/harness.rs`（体量自成一体时可落它的 `harness/` 子模块，
   再由 harness.rs 重导出），不得在子文件里重复定义（分流表见本文 §5）。

主题归属规则见本文 §2.1；test 总数守恒与引用面的机器判据见
[`docs/test_coverage.md`](../docs/test_coverage.md)「索引的维护契约」表（`STRUCT-*` / `COUNT-*` / `FIXTURE-MISSING`）。

---

## 3. 如何加一条测试

```text
1. 放夹具：选定 fixtures 子目录（§4 表），按 <domain>_<feature>[_<qualifier>].<ext> 命名
2. 挂测试：在对应主题子文件 tests/integration/<theme>.rs 追加 #[test]，
           include_str!("../fixtures/<dir>/<file>") 引用夹具，复用 harness 入口
3. 验门禁：cargo test --test integration <名字子串>
           bash scripts/check_fixtures.sh
           （再跑 bash scripts/gen_coverage_index.sh 重新生成 docs/test_coverage.md 覆盖索引，使新行进入索引）
```

三步的判据是**同一条链**：夹具存在 → 被引用 → 被跑到 → 被索引到。
缺任何一环，`check_fixtures.sh` 或 PR 模板勾选项就会把它挡下。

### 3.1 加一条错误快照测试

```text
1. tests/fixtures/errors/eNNNN.c（或 wNNNN.c）
2. DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors   # 首次生成 eNNNN.stderr.snap
3. cargo test --test errors                          # 复核
```

### 3.2 加一条 C++ ABI 差分分组

```text
1. mkdir tests/fixtures/cpp_abi_<主题>/，放入 *.cpp
2. 新增 #[test] fn cpp_abi_<主题>_group() { check_group("cpp_abi_<主题>"); }
   并把组名补进 cpp_abi_groups_are_all_covered 的 COVERED 常量
   （目录发现会自动抓新目录：未登记则该元测试直接报红，不会默默漏测）
3. cargo test --test cpp_abi_gcc_cross_check
```

也就是说：该目录族**不需要维护夹具文件清单**（逐个 `.cpp` 由目录列举得到），
但**需要登记主题组本身**。

---

## 4. fixture 目录 = 语言 / 角色（速览）

细则、白名单与零引用处置流程的权威在
[`tests/fixtures/README.md`](fixtures/README.md)；此处只给速览表（与本文 §4 同源）。

| 目录 | 放什么 | 禁止 |
|------|--------|------|
| `c/` | 通用 C 正例 | `.cpp`、asm 专用件、零引用死件、`m\d+_` 前缀 |
| `asm/` | `asm_*.c/h` 与 asm 专用头 | 非 asm 的语言测试 |
| `cpp/` | `.cpp` C++ 正例 | `.c` |
| `errors/` | `eNNNN.c` + `eNNNN.stderr.snap`（含 W 码） | 正例功能测试 |
| `opt/` | 优化 pass 样例 | 散落进 `c/` |
| `c_xfail/` | 已知预期失败 | 被当成正例引用 |
| `cpp_abi_{template,eh,rtti,virtual,overload,lifecycle}/` | GCC 差分 ABI 夹具，按目录名被动态发现 | 手工在 `.rs` 里加**夹具文件**硬编码清单（主题组名仍需登记，见 §3.2） |
| `gcc_tools/` | libiberty 探测组合 | 未登记进白名单 |
| `corpus/` | 真实 `.o` 二进制样本 | 源码 |
| `probe/` | 后端能力探针（归 backend 案） | 语言/行为面主题化改动 |
| `_archive/` | 无测试引用的历史件 | **任何 `include_str!` / `.join()` 引用** |

命名：`<domain>_<feature>[_<qualifier>].c`，例如 `attr_alias_var.c`、`builtin_trap.c`、
`generic_ptr.c`（asm 族的 `asm_avx.c` 落 `asm/`）。**禁止新增 `m\d+_` / `test_` 前缀**；
`e\d{4}` / `w\d{4}` 为 `errors/` 独占。

引用完整性（四条满足其一，否则 CI 红）：① 某 `.rs` 的 `include_str!` / `.join("…")` / `Path`
引用；② 某 `scripts/*.sh` 引用；③ 外部套件清单（gcc-dg / torture）且已登记白名单；
④ `corpus/` 二进制样本（整目录豁免）。

### 4.1 零引用 fixture 的处置判定

发现 `UNREFERENCED:` 时**逐条判定，不留 TODO**：

| 判据 | 处置 | 落点 |
|------|------|------|
| 可编译且断言点明确 | 在对应主题子文件补 1 个 `#[test]` | `c_attr_builtin.rs` / `opt.rs` / `c_pp_c23.rs` … |
| 依赖外部工具或目录 | 补 test 并带 `#[cfg(unix)]` / `tool_exists` 守卫 | `c_link_debug.rs` |
| 与现有件语义重复，或需已废弃特性 | `git mv` 进归档，并在 `_archive/README.md` 记一行原因 | `_archive/` |

---

## 5. harness 用法

`tests/common/harness.rs` 由壳以 `#[path = "common/harness.rs"] mod harness; use harness::*;`
挂载，条目全部 `pub(crate)`；`harness/elf.rs`、`harness/driver_cli.rs`、`harness/cpp_frontend.rs`
三个子模块由 harness.rs 自己以 `#[path]` 挂载并 `pub(crate) use …::*` 重导出，所以主题子文件
看到的仍是 `harness::*` 单一入口（主题子文件内部不得出现 `mod` / `use`，见 §2.1 规则 2）。
按用途分组（完整契约与分流表见本文 §5）：

| 组 | 代表入口 |
|----|----------|
| 编译/运行 | `run_syntax_test`、`run_object_test` / `_flags`、`run_asm_output_test`、`run_compile_and_run` / `_flags`、`run_exe`、`run_compile_o` |
| GCC 差分 | `run_gcc_cross_check`、`run_gcc_two_tu_cross_check`、`cpp_gcc_cross_check`、`gxx_native_cpp_exit_code` |
| 链接/宿主 | `run_host_link_and_run` / `_with`、`host_link_objects_and_run`、`host_link_cpp_object_and_run`、`run_cpp_object_test`、`run_cpp_with_mode` |
| ELF/DWARF | `elf64_sections`、`elf64_section_data`、`elf64_syms`、`elf64_func_symbols`、`elf64_undefined_symbols`、`elf64_dynamic_needed`、`readelf_relocations`、`compile_legacy_asm_object` |
| 断言 | `assert_exit_code`、`assert_bytes_present`、`assert_disasm_present`、`assert_runtime_fails_with`、`assert_kernel_corpus_object` |
| sanitizer/环境 | `run_sanitized_binary`、`tool_exists`、`qemu_runtime_target`、`gcc_src_tree_or_skip`、`host_cc_available`、`path_decision_in`、`object_symbol_size` |

---

## 6. 跳过列表（tests/skip/）

- `tests/skip/gcc_dg_skip.txt`：`scripts/gcc_dg_test.sh`、`scripts/gcc_dg_summary.sh` 读取
- `tests/skip/torture_skip.txt`：`scripts/torture_test.sh`、`scripts/torture_compile_test.sh`、
  `scripts/torture_execute_test.sh`、`scripts/torture_summary.sh` 读取

它们属于外部套件（gcc-dg / torture）的裁剪面，对应本文 §1 的 Tier 1 CI job `gcc-dg.yml` 与
`torture.yml`；**Tier 0 门禁不读这两个文件**。用跳过列表掩盖回归是禁止的
（判据见 `tests/skip/README.md`）。

---

## 7. 门禁本身有门禁

`scripts/check_fixtures.sh` 检查 5 项（缺失引用、未引用夹具、`_archive` 被引用、
`m\d+_` 新增命名、目录-扩展名不一致），任一命中即 `exit 1`。
`scripts/check_fixtures_selftest.sh` 用 5 类**反向注入**证明它真的会红，并在结束时校验
`git status --porcelain tests/fixtures` 为空（注入不留残留）。
两者的关系是元门禁：**门禁脚本自身假绿 = 一次测试体系事故**，改 `check_fixtures.sh` 必须同时让 selftest 保持 5/5 PASS。
注入用例与期望标签表见本文 §7 与 `scripts/check_fixtures_selftest.sh` 头部。
