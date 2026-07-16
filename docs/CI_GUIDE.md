# 推送并触发 CI 的精确操作

## 1. 推送代码 (2 选 1)

**A) 用脚本** (推荐):
```bash
cd D:\git\dcc
bash scripts/ci-trigger.sh
```

**B) 手动**:
```bash
cd D:\git\dcc
git push origin main
```

## 2. 查看 CI 结果

打开: https://github.com/digger-yu/dcc/actions

**push/PR 自动跑**:
- `dcc-e2e`: ~0.5–1 分钟, hello/arith/control/call 等 e2e + 错误诊断

**仅手动触发**（不自动跑，节省 Actions 额度）:
- `kernel-benchmark`: 约 15–20 分钟, 编 Linux 内核 (Actions → kernel-benchmark → Run workflow)

## 3. 第一性原理: 跑哪一个先看

`dcc-e2e` 是**核心验证**——1 分钟内告诉你 dcc 能不能编出能跑的二进制。
内核 bench 需要时手动跑即可。

## 4. e2e 失败怎么办

CI 会在日志中显示 dcc 的真实报错位置。看:
1. `cargo build --release` 是否过 (语法/类型错误)
2. `Compile hello.c` 是否过 (codegen 错误)
3. `./hello` exit code 是否 42 (运行时错误)

任何一个失败,把日志贴给我,我立刻修。
