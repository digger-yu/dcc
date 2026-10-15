# DCC 测试与验收脚本

## 日常 / CI

| 脚本 | 用途 |
|------|------|
| `test_all.sh` | 本地全量测试 |
| `ci-trigger.sh` | CI 触发 |
| `bench_phase_e.sh` | 性能基准（串行/并行、O2 折叠、aarch64） |

## 内核 / Linux 7.1

| 脚本 | 用途 |
|------|------|
| `kernel_full_check.sh` | modpost / QEMU / vmlinux 编排；`STRICT_FULL_BUILD=1` 禁止核心门禁 SKIP |
| `kernel_full_build.sh` | 全树构建 |
| `kernel_build_smoke.sh` | vmlinux / bzImage smoke 测试 |
| `prepare0_check.sh` / `prepare0_full.sh` | prepare0 窄/全链路 |
| `modpost_link.sh` | modpost 链接 + fixdep |
| `pahole_regress.sh` / `objtool_regress.sh` / `vmlinux_regress.sh` | 回归 |
| `qemu_boot_matrix.sh` | QEMU 启动矩阵 |
| `test_kernel_features.sh` | 内核特性示例 |
| `build_fixdep_dcc.sh` / `gen_elfconfig_h.sh` | 工具链辅助 |

## GCC Torture Suite

| 脚本 | 用途 |
|------|------|
| `setup_torture.sh` | 下载 GCC C Torture Suite 到 tests/gcc-torture/ |
| `torture_test.sh` | 运行 torture 测试 (compile/execute/all) |
| `torture_compile_test.sh` / `torture_execute_test.sh` | 分别跑 compile / execute 两档 |
| `torture_summary.sh` | 解析两份日志输出 GitHub Actions Summary |
| `torture_gate.sh` | 与 `tests/skip/torture_known_fail_*.txt` 基线对比，只允许失败集合收缩 |

## GCC libiberty

GCC libiberty 探测由本地 `cargo test --test integration` 驱动，详见
`docs/gcc_build_guide.md`。

| 脚本 | 用途 |
|------|------|
| `update_error_snapshots.sh` | 刷新 E1xxx snapshot |

## 手工工具（无自动调用方）

不被 Makefile / CI / 测试用例调用，仅供人工复现与对照；改动相关子系统时按需运行。

| 脚本 | 用途 |
|------|------|
| `asm_compare.sh` | 同一夹具下 dcc 与 gcc 的汇编逐条对照 |
| `optimize_baseline.sh` | 各优化档位产物基线采集 |
| `performance_benchmark.sh` | 快速编译耗时基准（完整基准见 `bench_phase_e.sh`） |
| `validate_int128_float_args.sh` | `__int128` / 浮点形参 ABI 分类验证 |
| `scan_all_fixtures.sh` | 全量扫描 `tests/fixtures/c/*.c` 与 gcc 逐字符差分 |
| `gcc_tree_cpp_probe.sh` | 批量用 dcc 编译 GCC 源码树 C++ TU 的探针 |
| `repro_cpp_run.sh` | 手工复现某个 C++ 夹具的编译 + g++ 链接 + 运行 |
| `dcc_orphan_check.sh` | 检测从未参与编译的孤立源文件 |

## 已删除的一次性脚本

Path A / ARM-MIPS 模块化 / PR-A2 / phase1–4 那批迁移与阶段脚本已随工作完成而删除
（`git log --diff-filter=D -- scripts/` 可取回）。删除判据是两条硬证据：在 Makefile /
CI / docs / tests / 本 README 中零调用方，且脚本内部仍引用已不存在的源码路径或
已不存在的 cargo feature（如 `arm-modular`）。
