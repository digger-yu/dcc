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

## GCC libiberty

GCC libiberty 探测由本地 `cargo test --test integration` 驱动，详见
`docs/gcc_build_guide.md`。

| 脚本 | 用途 |
|------|------|
| `update_error_snapshots.sh` | 刷新 E1xxx snapshot |

## GCC 构建验证

| 脚本 | 用途 |
|------|------|
| `test_tr2.sh` + `t_tr2.c` | TRANSLATE 宏展开回归测试 |
| `test_v3_cache.sh` | V3 缓存键正确性验证 |
| `rootcause_sim.py` | TRANSLATE(*d) 根因模拟 |

## 内核构建验证

| 脚本 | 用途 |
|------|------|
| `test_asm_offsets.py` | 用 dcc 编译 asm-offsets.c 验证 parser |
| `summarize_err.py` | 解析/语义错误分类汇总 |
