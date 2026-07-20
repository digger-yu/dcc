# DCC 测试与验收脚本

## 日常 / CI

| 脚本 | 用途 |
|------|------|
| `sync_remote_build.sh` | rsync 远程 + `cargo test` + feature + 内核窄目标 |
| `remote_feature_check.sh` | 功能矩阵（含 aarch64 hello、LTO、sanitizer） |
| `remote-m18.sh` | 远程 M18 集成测试（pull + release build + m18_*） |
| `remote-build.sh` | 远程 release 编译 |
| `remote-test-all.sh` | 远程 m15 + m16 集成测试 |
| `remote-test-m15.sh` / `remote-test-m16.sh` | 远程单里程碑集成测试 |
| `remote-test-ssa.sh` | 远程 SSA 单元测试 |
| `remote-test-attr.sh` | 远程 function attribute 示例编译运行 |
| `test_all.sh` | 本地全量测试 |
| `ci-trigger.sh` | CI 触发 |
| `bench.sh` / `bench_phase_e.sh` | 性能与阶段 E 基准 |

## 内核 / Linux 6.6

| 脚本 | 用途 |
|------|------|
| `kernel_full_check.sh` | modpost / QEMU / vmlinux 编排 |
| `kernel_full_build.sh` | 全树构建 |
| `kernel_build_smoke.sh` / `kernel_phase_b_check.sh` | smoke / phase B |
| `prepare0_check.sh` / `prepare0_full.sh` | prepare0 窄/全链路 |
| `modpost_link.sh` | modpost 链接 + fixdep |
| `pahole_regress.sh` / `objtool_regress.sh` / `vmlinux_regress.sh` | 回归 |
| `qemu_boot_matrix.sh` | QEMU 启动矩阵 |
| `test_kernel_features.sh` | 内核特性示例 |
| `build_fixdep_dcc.sh` / `gen_elfconfig_h.sh` | 工具链辅助 |

## GCC libiberty

| 脚本 | 用途 |
|------|------|
| `remote_gcc_c_probe.sh` | libiberty / fixincludes 探测 |
| `update_error_snapshots.sh` | 刷新 E1xxx snapshot |

## 归档

历史调试与一次性探针脚本见 [`archive/README.md`](archive/README.md)。
