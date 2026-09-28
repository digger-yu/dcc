# 变更摘要

<!-- 一句话说明这个 PR 做什么；关联 issue / docs/*.md 方案请在此列出 -->

## 变更类型

- 功能 / 缺陷修复
- 测试体系或 fixture 变更（涉及 `tests/`、`tests/fixtures/`、`scripts/check_fixtures.sh`）
- 文档
- 其他

---

## Fixture 与测试挂载检查清单

以下 4 项是**硬要求**（任一项未勾选 = PR 不完整），规则原文见
[`docs/tests_quality_system_plan.md`](../docs/tests_quality_system_plan.md) §7（fixture 规则）与 §9（门禁与命令权威 Tier 表）。

- [ ] 新增 / 改名的 fixture 已挂载测试：它至少被某个 `.rs` 的 `include_str!("../fixtures/…")` / `.join("…")`、
      或某个 `scripts/*.sh`、或外部套件清单（白名单）引用一次，不属于 `_archive/`
- [ ] 新增 / 迁移的 `#[test]` 已进入对应主题子文件 `tests/integration/<theme>.rs`（不是写进 `tests/integration.rs` 壳；
      子文件内不写 `use` / `mod`，路径一律 `../fixtures/`）
- [ ] 已本地跑过 `bash scripts/check_fixtures.sh` 且 exit 0（未把 `UNREFERENCED:` / `MISSING:` /
      `FORBIDDEN-NAME:` / `FORBIDDEN-ARCHIVE-REF:` / `DIR-MISMATCH:` 留给自己）
- [ ] 覆盖索引已加行：`docs/test_coverage.md` 里能看到「主题文件 | 测试函数 | fixture | 判据 | Tier」对应条目
      （由 `bash scripts/gen_coverage_index.sh` 重新生成，不手工编辑生成区）

元门禁（证明上面那条门禁**真的会红**，而非假绿）：

- [ ] 已本地跑过 `bash scripts/check_fixtures_selftest.sh`，5 类反向注入全部按预期 exit 1，脚本自身 exit 0，
      且 `git status --porcelain tests/fixtures` 无残留

---

## Tier 0 自查（本地等价：`make test`）

权威清单以方案 §9 为准，此处仅为提醒，勿在其他文档复制第二套：

```bash
cargo build --tests
cargo test --release                          # 全部 6 个顶层 target
bash scripts/check_fixtures.sh
bash scripts/check_fixtures_selftest.sh
```

- [ ] 以上四条全部通过（若有既有失败，请在描述中指明它是本 PR 之前就存在的，并给出基线证据）

## 结构与纪律

- [ ] 未修改 `tests/integration.rs` 壳的 `include!` 列表以外的顶层 target 数量（终态 6 个）
- [ ] 未新增 `m\d+_` / `test_` 前缀 fixture，未把 `.cpp` 放进 `fixtures/c/`、`.c` 放进 `fixtures/cpp/`
- [ ] 本 PR 不夹带与主题无关的功能 / bug 修复（freeze 纪律，方案 §12）
- [ ] fixture 的移动 / 改名一律 `git mv`，与引用改写在同一提交

## 说明

<!-- 为什么这么做、放弃了什么替代方案、与关联案的接口（方案 §12）；纯机械搬运可留空 -->
