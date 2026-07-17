# 归档脚本

本目录存放一次性调试、探测与历史回归脚本，**不再纳入日常 CI**。
活跃脚本仍保留在 `scripts/` 根目录。

## 分类

| 前缀/类型 | 说明 |
|-----------|------|
| `_*.sh` | 远程调试、libiberty 失败复现、regex 探针 |
| `debug_*.sh` / `analyze_*.sh` | 预处理/regex 专项调试 |
| `test_tr*.sh` / `test_v3_*.sh` | 旧版翻译器/缓存回归 |
| `*.py` | 预处理 diff、错误汇总等辅助工具 |

## 活跃脚本（仍在 `../`）

- `sync_remote_build.sh` — 远程同步 + 全量测试
- `remote_feature_check.sh` — 功能矩阵
- `kernel_full_check.sh` / `prepare0_*.sh` — 内核验收
- `bench_phase_e.sh` / `test_all.sh` — 基准与全量

如需复现历史问题，可在此目录查找对应探针脚本。
