# 归档脚本

本目录存放仍有文档引用的一次性探测脚本，**不再纳入日常 CI**。
活跃脚本见 `scripts/README.md`。

## 保留脚本

| 脚本 | 用途 | 引用位置 |
|------|------|----------|
| `test_asm_offsets.py` | 用 dcc 编译 asm-offsets.c 验证 parser | `docs/kernel_build_guide.md` |
| `summarize_err.py` | 解析/语义错误分类汇总 | `docs/kernel_build_guide.md` |
| `test_tr2.sh` + `t_tr2.c` | TRANSLATE 宏展开回归测试 | `docs/gcc_build_guide.md` |
| `test_v3_cache.sh` | V3 缓存键正确性验证 | `docs/gcc_build_guide.md` |
| `rootcause_sim.py` | TRANSLATE(*d) 根因模拟 | `docs/gcc_build_guide.md` |

## 历史

2026-08-28 清理：删除 23 个无引用的一次性调试脚本（regex 探针、预处理 diff、
旧版翻译器回归等），保留上述 6 个仍有文档引用的脚本。
