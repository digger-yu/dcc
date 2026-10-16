# DCC 脚本索引

`scripts/` 下 55 个脚本的分工与调用方。判据只有一个——谁在调用它：Makefile 目标、
`.github/workflows/*.yml`、`tests/`、`docs/`，或另一个脚本。

## 共通约定

| 变量 | 含义 |
|------|------|
| `DCC` | 待测编译器路径。默认几乎都是 `target/release/dcc`；唯一例外是 `test_kernel_features.sh`，它默认 `./target/debug/dcc` |
| `DCC_REPO` / `REPO_ROOT` | 仓库根，多数脚本自行推导，可覆盖 |
| `KERNEL_DIR` | 内核树路径（内核那批脚本用；Makefile 默认 `/root/linux-7.1`） |
| `JOBS` | 并发度。Makefile 默认 `nproc`，但跑测试用例脚本时按 `-j2` / `--test-threads=2` 限流 |
| 退出码 | 0 = 通过；非 0 = 已点名问题。缺外部工具时打印显式 `SKIP` 而不是静默通过（如 `check_asm_text_parity.sh` 的 `ASM-PARITY-SKIP`），需要"不许 SKIP"时用各自的 STRICT 开关 |

## Tier 0（`make test`，PR 必绿）

| 脚本 | 用途 |
|------|------|
| `cargo_test_summary.sh` | 跑 `cargo test --release` 并给出跨 target 总计 + 失败/忽略清单；退出码透传 cargo，自身不判绿 |
| `check_fixtures.sh` | fixture 引用完整性 / 命名 / 目录规则，硬失败无过渡开关 |
| `check_fixtures_selftest.sh` | 元测试用例：5 类反向注入逐条 `exit 1`，证明上一条真的会红 |
| `test_all.sh` | `tests/fixtures/smoke` 端到端冒烟的薄编排，不复制断言逻辑；由 `make test-all` 接入 |

## 产物级不变量（`make opt-eq` / `rex-hygiene` / `golden-cross` / `pipe-structure` / `disasm-cov`，及 CI）

| 脚本 | 用途 |
|------|------|
| `check_opt_equivalence.sh` | 优化档位执行等价：全语料 × `-O0/-O2/-O3` 与 gcc 差分 |
| `check_rex_hygiene.sh` | 以 objdump 为权威扫产物：多余 REX 位 = 0、`%ah..%bh` = 0 |
| `arm_mips_golden_baseline.sh` | ARM/MIPS 字节基线；`make golden-cross` 跑 `verify` 模式——基线缺失或产物漂移都置红，禁止 CI 里悄悄重写 |
| `check_pipe_structure.sh` | P16 软件流水的两个独立读数交叉：`DCC_PIPE_DUMP` 的接受/拒因，与 objdump 比较默认产物和 `DCC_SSA_OFF=pipe` 产物 |
| `check_disasm_coverage.sh` | 自家 `-S` 反汇编逐字命中率；`make disasm-cov` 传 `LEVELS="0 2" CEILING=0`，CEILING 必须是显式数字，否则上限=本次实测值、永不红 |
| `check_reproducible_output.sh` | 编译产物可复现性（CI `ci.yml` 的 reproducibility job） |
| `check_asm_text_parity.sh` | `-c` 直产 vs `-S --asm-fallback=gas` 经真 `<triple>-as` 汇编后的指令文本一致（CI `ci.yml` 的 parity job） |
| `probe_backend_features.sh` | 后端能力行为探针，与 `docs/backend_feature_matrix.md` 逐格对账，不一致即 CI 红（`ci.yml` 的 matrix job） |

## GCC torture（`make torture` / CI `torture.yml`）

| 脚本 | 用途 |
|------|------|
| `setup_torture.sh` | 稀疏下载 gcc.c-torture 到 `tests/gcc-torture/` |
| `torture_test.sh` | 总驱动（`compile` / `execute` / `all`） |
| `torture_compile_test.sh` | compile 档 |
| `torture_execute_test.sh` | execute 档 |
| `torture_summary.sh` | 解析两份日志生成 GitHub Actions Summary |
| `torture_gate.sh` | 与 `tests/skip/torture_known_fail_*.txt` 基线对比，只允许失败集合收缩 |

## GCC dg（CI `gcc-dg.yml`）

| 脚本 | 用途 |
|------|------|
| `gcc_dg_test.sh` | `tests/gcc-dg` 语料驱动，跳过清单在 `tests/skip/gcc_dg_skip.txt` |
| `gcc_dg_summary.sh` | 解析日志输出 Actions Summary |

## 内核 / Linux 7.1（详见 `docs/kernel_build_guide.md`）

| 脚本 | 用途 |
|------|------|
| `kernel_full_build.sh` | 全树 `make CC=dcc -k`，收集全部失败而非首错即停 |
| `kernel_full_check.sh` | modpost / prepare0 / bzImage / QEMU / vmlinux 编排；`STRICT_FULL_BUILD=1` 时核心步骤不许 SKIP |
| `kernel_build_smoke.sh` | vmlinux / bzImage 冒烟（`BUILD_BZIMAGE=1` 才建 bzImage） |
| `prepare0_check.sh` / `prepare0_full.sh` | prepare0 窄目标 / 全链路 |
| `modpost_link.sh` | modpost 三 TU 链接 + Rust fixdep |
| `build_fixdep_dcc.sh` | 把 Rust `fixdep` 装进内核树替代 C fixdep |
| `gen_elfconfig_h.sh` | 从 `empty.o` 的 ELF 头生成 `elfconfig.h` |
| `build_initramfs.sh` | busybox(static) + `/init` 的最小 initramfs |
| `qemu_boot_matrix.sh` | QEMU 启动矩阵；`REQUIRE_QEMU_BOOT=1` 时启动失败即红 |
| `objtool_regress.sh` | `.eh_frame` FDE / ORC 回归 |
| `pahole_regress.sh` | BTF 回归（pahole 或 llvm-objcopy） |
| `vmlinux_regress.sh` | vmlinux 级 pahole / objtool 严格回归（需 ET_EXEC vmlinux） |
| `test_kernel_features.sh` | 6 项内核构造夹具：dcc `-c` → gcc 链接 → 运行，外加 readelf 的 WEAK 断言 |
| `arch_x86_compile.sh` | 批量编译 `arch/x86/` 的 C 文件，复用内核命令模板 |
| `dcc-kernel-cc.sh` | 内核编译包装：基础设施文件用 gcc，其余用 dcc |
| `dcc-host-wrapper.sh` | Kbuild `HOSTCC` 包装：dcc 编译、gcc 链接，含参数白名单翻译 |

## 自举 / 静态构建 / GCC 源码树

| 脚本 | 用途 |
|------|------|
| `bootstrap.sh` | `make bootstrap`：dcc0 → dcc1 → dcc2 与字节/语义对比 |
| `build_static.sh` | musl 全静态构建 dcc 自身（见 `docs/static_build.md`） |
| `cc1plus_bootstrap.sh` | 以 dcc 为 `CC`/`CXX` 配置并构建 cc1/cc1plus，产出进度报告，xfail 跟踪收敛 |

## C++ ABI 与差分

| 脚本 | 用途 |
|------|------|
| `cpp_abi_gcc_cross_check.sh` | 逐夹具分别用 g++（真值）与 dcc 编译链接并比对；被 `tests/cpp_abi_gcc_cross_check.rs` 调用 |
| `dcc_gcc_diff.sh` | 同一份 C 源 gcc / dcc 差分执行比对；被 `tests/integration/c_abi_vararg.rs` 与若干夹具 README 引用 |

## 性能与基线

| 脚本 | 用途 |
|------|------|
| `bench_phase_e.sh` | 阶段 E 性能冒烟：mmap + 分块 codegen + `-O2`（`docs/support_features.md` 登记） |
| `optimize_baseline.sh` | 各优化档位产物基线采集（手工） |
| `performance_benchmark.sh` | 快速编译耗时基准（手工，完整基准见 `bench_phase_e.sh`） |

## 生成类

| 脚本 | 用途 |
|------|------|
| `gen_coverage_index.sh` | 重写 `docs/test_coverage.md` 生成区并跑全部交叉校验；改完 `tests/**` 后运行 |
| `update_error_snapshots.sh` | 生成/刷新 `tests/fixtures/errors/*.stderr.snap`（`DCC_UPDATE_SNAPSHOTS=1`） |

## 维护辅助

| 脚本 | 用途 |
|------|------|
| `cleanup_disk.sh` | `make clean-cache`：清调试与内核编译残留、缓存，并盘点 `.work_tmp/` |
| `ci-trigger.sh` | 显示当前 remote/分支/HEAD 并 `git push` 触发 CI（见 `docs/ci_guide.md`） |

## 手工工具（无自动调用方）

不被 Makefile / CI / 测试用例调用，仅供人工复现与对照；改动相关子系统时按需运行。

| 脚本 | 用途 |
|------|------|
| `asm_compare.sh` | 同一夹具下 dcc 与 gcc 的汇编逐条对照 |
| `validate_int128_float_args.sh` | 生成 `tests/diff_test/fixtures/int128_float_args.c`（`__int128` 与 float 混排形参），分别用 gcc 与 dcc 编译运行并比对退出码 |
| `scan_all_fixtures.sh` | 全量扫描 `tests/fixtures/c/*.c`，与 gcc 逐字符差分 |
| `gcc_tree_cpp_probe.sh` | 批量用 dcc 编译 GCC 源码树 C++ TU，暴露前端缺口 |
| `repro_cpp_run.sh` | 手工复现某个 C++ 夹具：dcc 编译 → g++ 链接 → 运行 |
| `dcc_orphan_check.sh` | 检测从未参与编译的孤立源文件 |
