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

**两个 workflow 会自动跑**:
- `dcc-e2e` (本 commit 新增): 5-8 分钟, hello/arith/control/call 4 个 e2e + 错误诊断
- `DCC Kernel Compilation Benchmark` (M0 已写): 60+ 分钟, 编 Linux 内核 (M5 之前 GCC baseline 有意义, DCC 还在跑不动)

## 3. 第一性原理: 跑哪一个先看

`dcc-e2e` 是**核心验证**——5 分钟告诉你 dcc 能不能编出能跑的二进制。
内核 bench 在 DCC 还不能编真实代码时**没有意义**——会被 DCC 自身的 TODO panic 阻塞。

## 4. e2e 失败怎么办

CI 会在日志中显示 dcc 的真实报错位置。看:
1. `cargo build --release` 是否过 (语法/类型错误)
2. `Compile hello.c` 是否过 (codegen 错误)
3. `./hello` exit code 是否 42 (运行时错误)

任何一个失败,把日志贴给我,我立刻修。
