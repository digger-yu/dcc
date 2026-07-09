# DCC 性能模型 (第一性原理)

## 目标

在 ubuntu-latest 免费 runner 上 (2 cores, 7GB), 用 `defconfig -O2` 编 linux 6.6 内核:
- GCC: 30-60 分钟 (cold), 15-30 分钟 (warm)
- DCC: 目标 < 30 分钟 cold, < 15 分钟 warm

## 加速来源 (理论)

```
T_dcc = T_gcc / (S_macro * S_header * S_type * S_opt * S_dbg * S_codegen * S_io)
```

各因子:

| 因子 | 含义 | DCC 加速 | 理由 |
|------|------|----------|------|
| S_macro | 宏不重解析 | 2-5x | 宏 = AST 节点插入,不再走 lexer/parser |
| S_header | Header 缓存 | 3-10x | mmap 共享的全局类型表 |
| S_type | 跨 TU 类型共享 | 2-4x | typedef/struct 全局一致,无需重复定义 |
| S_opt | 按需优化 | 2-3x | 只跑已知收益高的 pass,跳过 LLVM 风格的多 pass |
| S_dbg | 调试信息直出 | 1.5-2x | AST 留类型,直接 emit DWARF |
| S_codegen | 并行代码生成 | 2x | 函数级并行,rayon 调度 |
| S_io | mmap 输入 | 1.2-1.5x | 不复制文件,零拷贝 |

**理论上限**: 36-180x 加速。但实际受限于:
- 磁盘 IO (kernel 编译大头是 -j 时的 IO 抖动)
- 链接阶段 (DCC 暂不自做链接,靠 ld)
- make 的 fork/exec 开销

**现实预期**: 3-10x 加速, 即 DCC 编内核 < 6-10 分钟 warm。

## 测量方法

每个 PR 触发 `bench-free` job:
1. 浅克隆 linux-6.6
2. defconfig
3. `make HOSTCC=... vmlinux` 三次: GCC baseline / DCC cold / DCC warm
4. 写 summary 到 GitHub Step Summary
5. 与历史 PR 对比, 退化 > 20% 自动开 issue

## 关键指标 (KPI)

| 指标 | 目标 | 监控 |
|------|------|------|
| 内核编译时间 (warm) | < GCC/2 | GitHub Actions 趋势 |
| 二进制大小 (a.out) | 跟 GCC 同档 | CI artifact |
| 错误诊断 LOC | 每条 < 200 字符 | snapshot 测试 |
| 自举差 (dcc1 vs dcc2 字节差) | = 0 | CI diff |
