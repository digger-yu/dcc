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
  - `examples/advanced/bad_syntax.c` 错误诊断断言（`error[E0006]` + `hint:`）

**仅手动触发**（不自动跑，节省 Actions 额度）:
- `kernel-benchmark`: 约 15–20 分钟, 编 Linux 内核 (Actions → kernel-benchmark → Run workflow)

上面的列表只是 workflow 视角。按**门禁强度**划分的测试分层见 §6。

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
`scripts/kernel_full_check.sh`（默认 modpost / QEMU / vmlinux，缺产物时 SKIP；已有 bzImage 后 QEMU 为硬门禁）。
需要硬门禁时运行 `STRICT_FULL_BUILD=1 bash scripts/kernel_full_check.sh`，
它会强制构建并要求 `vmlinux`、`bzImage`、modpost、pahole、objtool 全部通过。
详见 `docs/kernel_build_guide.md`。

## 6. 测试分层（Tier 0–3）

DCC 的测试按**门禁强度**分四层。本节只做导航与概览；
**每个 Tier 的权威命令清单只有一处**：
[`docs/tests_quality_system_plan.md`](./tests_quality_system_plan.md) §9「门禁与命令（权威 Tier 表）」。
本节**有意不复制完整清单**，避免出现第二套真源后与方案漂移。

```text
Tier 0  PR 必绿（本地等价：make test）
        cargo build --tests / cargo test --release（全部 6 个顶层 target）
        + scripts/check_fixtures.sh（fixture 引用完整性，硬失败无开关）
        + scripts/check_fixtures_selftest.sh（对上述门禁本身做反向注入的元门禁）

Tier 1  Linux CI（.github/workflows）
        e2e.yml / gcc-dg.yml / torture.yml / check_feature_matrix.yml
        └─ check_fixtures 落在 e2e.yml 的 "Unit + integration tests" job（独立 step，不带 continue-on-error）

Tier 2  内核 / 性能
        scripts/kernel_full_check.sh、QEMU 启动矩阵、kernel-bench.yml（手动触发）

Tier 3  可选深测
        libiberty / GCC 源树（`--features gcc_src`）、STRICT_FULL_BUILD=1
```

| Tier | 何时必须跑 | 是否挡合并 | 触发入口 |
|------|------------|------------|----------|
| Tier 0 | 每个 PR / 提交前 | **挡**（任一条红即不可合并） | `make test`（= §9 四条逐字对应：`cargo build --tests` → `cargo test --release` → `scripts/check_fixtures.sh` → 元门禁 `scripts/check_fixtures_selftest.sh`）；`make test-all` = Tier 0 全量 + `scripts/test_all.sh`（examples 端到端冒烟） |
| Tier 1 | push / PR 自动跑（外部套件按需手动） | `e2e.yml`、`check_feature_matrix.yml` 挡；`gcc-dg.yml`、`torture.yml` 是 `workflow_dispatch` 手动作业（不挡单个 PR） | `.github/workflows/e2e.yml`、`check_feature_matrix.yml`（push/PR）＋ `gcc-dg.yml`、`torture.yml`（手动） |
| Tier 2 | 需要内核级回归时（手动/定时） | 不挡单个 PR | `scripts/kernel_full_check.sh`、`kernel-bench.yml` |
| Tier 3 | 深测/取证 | 不挡 | `--features gcc_src`、`STRICT_FULL_BUILD=1` |

两条补充规则：

- **优先级**：Tier 2 的内核红灯（`docs/next_steps.md` 记为 P0）优先于测试体系自身的整改，不得被 Tier 0 的改造挤占。
- **元门禁**：Tier 0 里 `check_fixtures_selftest.sh` 的作用是证明 `check_fixtures.sh` **真的会红**（反向注入 5 类违规，逐条 exit 1）；
  它自身失败同样视为主门禁失败。具体注入用例与期望标签见方案 §8.3。
