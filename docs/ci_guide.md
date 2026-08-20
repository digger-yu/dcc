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
- `dcc-e2e`: ~1–2 分钟
  - `cargo test --release`（单元 + integration + errors snapshot）
  - hello/arith/control/call 等 example 编译运行
  - `examples/bad.c` 错误诊断断言（`error[E0006]` + `hint:`）

**仅手动触发**（不自动跑，节省 Actions 额度）:
- `kernel-benchmark`: 约 15–20 分钟, 编 Linux 内核 (Actions → kernel-benchmark → Run workflow)

## 3. 第一性原理: 跑哪一个先看

`dcc-e2e` 是**核心验证**——1 分钟内告诉你 dcc 能不能编出能跑的二进制。
内核 bench 需要时手动跑即可。

## 4. e2e 失败怎么办

CI 会在日志中显示 dcc 的真实报错位置。看:
1. `cargo build --release` 是否过 (语法/类型错误)
2. `cargo test --release` 是否过 (integration + errors snapshot)
3. `Compile hello.c` 是否过 (codegen 错误)
4. `./hello` exit code 是否 42 (运行时错误)

### errors snapshot 不匹配

诊断格式或列号变更后，snapshot 可能失败。详见 [diagnostics.md](./diagnostics.md) 第 9 节。在 Linux 上更新:

```bash
DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors --release
# 或
bash scripts/update_error_snapshots.sh
```

任何一个失败,把日志贴给我,我立刻修。

## 5. Linux 内核回归（可选）

在已经 clone Linux 7.1 源码树 + 已有 `.config` 的开发机上，可依次跑
`scripts/kernel_full_check.sh`（默认 modpost / QEMU / vmlinux，缺产物时 SKIP）。
需要硬门禁时运行 `STRICT_FULL_BUILD=1 bash scripts/kernel_full_check.sh`，
它会强制构建并要求 `vmlinux`、`bzImage`、modpost、pahole、objtool 全部通过。
详见 `docs/kernel_build_guide.md`。
