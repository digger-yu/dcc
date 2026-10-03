# G2：`goto` 跳到循环之后第二个标签 ⇒ `-O2/-O3` 静默错值 —— 定位与修复报告

> **路径勘误（2026-10，SSA 目录化）**：本文里的 `src/ssa.rs` 旧路径与行号**不改写**（当时的事实）；
> 该文件已拆为 `src/ssa/` 多个职责文件。对照表见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

## 0. 一句话根因

`src/opt.rs::dce_block`（`-O2` 的 `eliminate_unreachable` pass 的内部实现）把「顶层
`Return` 之后所有非 `Label` 的 block-level item」一律判成不可达清掉。但 `Stmt::Label
{ stmt }` 只把紧随标签的**一条语句**纳入自己，标签之后到下一个 Label/terminator 之间
的顶层 item（典型形态 `out: <body>; return s;` 里的 `return s;`）仍是「跳到 `out`
之后可达」的路径 —— 被误清 ⇒ `goto out` 从 `out:` 顺落到 `late:`，再从函数尾
fall-off，`%eax` 只剩最后一条对 `s` 的写，gcc `-3` 变 dcc `-8`。

**走了真修**：`dce_block` 遇到 `Label` 就把 `stopped` 复位，之后的 item 保留到本
block 末尾或下一个顶层 `Return`。三档实测：夹具四档全部与 gcc 逐字节一致。

---

## 1. 复现与最小形状

### 1.1 原始夹具（`tests/fixtures/opt/loop_exit_ownership_goto_pending.c`）

`a_goto_out2` 的形状：`for(...) { if(i==1) goto out; if(i==3) goto late; s+=i; }
s+=7; return s; out: s-=3; return s; late: s-=5; return s;`

四档读数（本机 `gcc (Ubuntu 13.3.0)`，`-std=gnu2x`；`main` 打印 `goto_out2 <k> <v>`
k=0..6）：

| 档 | gcc | dcc 修复前 | dcc 修复后 |
| --- | --- | --- | --- |
| `-O0` | `7 7 -3 -3 -3 -3 -3` | `7 7 -3 -3 -3 -3 -3` | `7 7 -3 -3 -3 -3 -3` |
| `-O1` | `7 7 -3 -3 -3 -3 -3` | `7 7 -3 -3 -3 -3 -3` | `7 7 -3 -3 -3 -3 -3` |
| `-O2` | `7 7 -3 -3 -3 -3 -3` | **`7 7 -8 -8 -8 -8 -8`** | `7 7 -3 -3 -3 -3 -3` |
| `-O3` | `7 7 -3 -3 -3 -3 -3` | **`7 7 -8 -8 -8 -8 -8`** | `7 7 -3 -3 -3 -3 -3` |

### 1.2 最小形状（新夹具 `dce_label_after_return_minimal.c::F1`）

去掉 for 循环、`s+=7` 分支等无关件；剩下就是「一个顶层 `Return` 之后紧跟两个 Label
+ 各自的 `return s;`」这一条 dce 判据的最小反例：

```c
static int f_goto_two_labels(int n)
{
    int s = n;
    goto out;
    return 999;
out:
    s -= 3;
    return s;
late:
    s -= 5;
    return s;
}
```

四档读数（`main` 打印 `F1 <k> <v>`，k=0..2）：

| 档 | gcc | dcc 修复前 | dcc 修复后 |
| --- | --- | --- | --- |
| `-O0` | `-3 -2 -1` | `-3 -2 -1` | `-3 -2 -1` |
| `-O1` | `-3 -2 -1` | `-3 -2 -1` | `-3 -2 -1` |
| `-O2` | `-3 -2 -1` | **`-8 -7 -6`** | `-3 -2 -1` |
| `-O3` | `-3 -2 -1` | **`-8 -7 -6`** | `-3 -2 -1` |

修复前的 `-8` 就是 `n - 3 - 5`（`out:` 与 `late:` 两条 body 都被串成直落），与原始
夹具 -O2/-O3 的 `-8` 值同源。这条最小形状与循环无关、与 SSA 无关、与多出口循环门
无关，只钉住 `dce_block` 一处。

新夹具里同时保留了两条形为对照：`F2` = 循环 + 双标签（与原始夹具同族的完整形）；
`F3` = `return` 后是**没有标签**的真不可达 —— 用来钉「修 G2 不能把 dce 修成完全不删」。

---

## 2. 定位（减法）

取证开关逐个跑，只跑 `-O2`（`-O3` 结果同）：

| 开关组合 | 读数 | 结论 |
| --- | --- | --- |
| 默认（`-O2`） | `-8 -7 -6` | 病灶可复现 |
| `DCC_SSA_OFF=1 -O2` | `-8 -7 -6` | **不在 SSA 一侧** |
| `DCC_SSA_BAIL=1 -O2` | `-8 -7 -6` | 同上（含 `goto` 本来就被 `is_structured` 挡在 SSA 外） |
| `DCC_OPT_SKIP=run_ssa_passes -O2` | `-8 -7 -6` | 整条 SSA 往返关掉仍错，与前进程留下的归因一致 |
| **`DCC_OPT_SKIP=eliminate_unreachable -O2`** | **`-3 -2 -1`** | **定到具体 pass** |

`eliminate_unreachable`（`src/opt.rs:126`）内部只有一条实现：`dce_block`
（`src/opt.rs:1229`），病灶由此锁定。

---

## 3. 根因站点（文件 + 函数 + 行为）

**文件**：`src/opt.rs`
**函数**：`dce_block(items: &mut Vec<BlockItem>)`（`src/opt.rs:1229`，`eliminate_unreachable` 内部）
**行为**：

```rust
// 修复前（伪码）
let mut stopped = false;
items.retain(|item| {
    if stopped {
        // goto 仍可跳入 return 之后的标签，不能当不可达代码删除
        if matches!(item, BlockItem::Stmt(Stmt::Label { .. })) {
            return true;                 // 只保 Label 本身
        }
        return false;                    // 其他一律清 —— 把 Label 后面的顶层 Return 也清了
    }
    if let BlockItem::Stmt(Stmt::Return { .. }) = item {
        stopped = true;
    }
    true
});
```

`Stmt::Label { stmt }` 在 AST 里只包**一条**紧随标签的语句（`src/ast.rs:316`）；
`out: s -= 3; return s;` 因此 parse 成两个 block-level item：`Label(out){s-=3}`
与 `Return(s)`。旧实现把 stopped 之后所有非 Label 的 item 都清掉 ⇒ 紧随 Label
的 `Return` 被当成不可达丢弃。于是：

- 自然路径：`... return s; /* loop 后的 */`（正常，被保留），后面全是「不可达」。
- `goto out` 路径：跳到 `out:`（Label 保留，body 保留）→ **body 之后顺落**（因为
  `Label` 的 body 只是一条 `s -= 3;`，不是 terminator）→ 落到 `late:`（同样保留
  body）→ 落到函数尾，没有 `return`；`%eax` 只剩最后一次 `s` 的写，即 `s -= 5`
  的结果 `n - 3 - 5 = -8`。

修复前 `dce_block` 之后 items 只剩 `[VarDecl s, Goto, Return 999, Label(out),
Label(late)]`（5 项）—— 两条 `Return s` 都不见了。

---

## 4. 修法：**真修**（不是「不优化但正确」的退路）

`dce_block` 里遇到 `Label` 就把 `stopped` 复位；此后 item 保留到本 block 末尾
或下一个顶层 `Return`（再触发 stopped）。判据是「Label 是跳入入口 ⇒ 其后所有
item 至少有一条从 Label 起的路径可达」。

```rust
// 修复后（`src/opt.rs::dce_block`，见文件当前版本）
if stopped {
    if matches!(item, BlockItem::Stmt(Stmt::Label { .. })) {
        stopped = false;                 // ← 关键：Label 是入口，复位 stopped
        return true;
    }
    return false;
}
```

**没有走 `Builder::unsupported` / 整函数退出优化的退路** —— 因为：

1. `dce_block` 是 AST 层的通用不可达清除，与函数是否含 `goto` 无关；把它整个跳过
   会让其他函数（无 `goto` 只有 `return` 后的 dead code）白白丢掉 `-O2` 的 dce。
2. 复位 `stopped` 是**语义最小**的修法，同时覆盖「Label 后跟 Return」与「Label 后
   跟任何其它 item」两种形态；不需要新增挡点、不需要标记函数「不参与本 pass」。
3. 四档实测与 gcc 逐字节一致（见 §1 表格 + §5 判据），且原有 lib 单测
   `o2_preserves_goto_label_after_return`（`src/opt.rs:2601`）仍然通过。

**判别因子**（证明真修而不是碰巧）：

| 场景 | 修复前 items 数 | 修复后 items 数 | 修复后 stdout vs gcc |
| --- | --- | --- | --- |
| F1（Label+Return+Label+Return） | 5 | 7 | 四档一致 |
| F2（循环+双 Label+Return） | 与 F1 同类，被清两条 Return | 全保 | 四档一致 |
| F3（Return 后无 Label 的真 dead） | 清 2 条 | **仍清 2 条**（`len==1`） | 四档一致（`SHOULD_NEVER_PRINT` 不打印） |

F3 是防矫枉过正的锚：修 G2 后 `dce_block` 仍然能删没有 Label 挡着的 dead 语句。

---

## 5. 判据

### 5.1 lib 单测（`src/opt.rs::tests`）

- `g2_dce_block_retains_items_after_label` —— 直接对 F1 形状 parse 后调用 `dce_block`，
  断言：`Label` 数量 == 2、`Return` 数量 ≥ 3。修复前只 2 个 Return（`999` 与
  第一个 Label 之前那个），失败即命中。
- `g2_dce_block_still_prunes_pure_dead_after_return` —— 对 F3 形状 parse 后调用
  `dce_block`，断言 then-block 长度 == 1。防止「把 dce 修成完全不删」的假修。

实跑：`cargo test --release --lib -- opt::tests::` 17/17 通过（`g2_*` 2 通过）。

### 5.2 集成门禁（`tests/integration/opt_ssa_gate.rs`）

新增两条**按档逐个比**的用例，参照既有 `..._matches_gcc_at_all_levels` 的写法，
失败信息里带档位：

- `dce_label_after_return_minimal_matches_gcc_at_all_levels`（夹具：
  `dce_label_after_return_minimal.c`，F1/F2/F3 三形合一）
- `loop_exit_ownership_goto_matches_gcc_at_all_levels`（夹具：
  `loop_exit_ownership_goto_pending.c`，原始 G2 复现形）

两条都走 `run_gcc_cross_check_opt(name, src, &[flag])` × `["-O0","-O1","-O2","-O3"]`，
逐档 stdout 与 rc 双向对比。失败信息形如：
```
-O2: dce_block 把 Label 后顶层 Return 当不可达清掉 ⇒ goto out 直落进 late 再从函数尾 fall-off（F1/F2 形状）
-O3: a_goto_out2 在 `out:` 与 `late:` 双标签处被 dce_block 误清 → gcc `7 7 -3 -3 -3 -3 -3` 应变为 `7 7 -8 -8 -8 -8 -8`
```

实跑：`cargo test --release --test integration -- --test-threads=1 matches_gcc_at_all_levels`
24/24 通过（新增 2 条 + 既有 22 条，包括 `loop_exit_ownership_boundary_matches_gcc_at_all_levels`
等回归门）。

---

## 6. 夹具归宿

- `tests/fixtures/opt/loop_exit_ownership_goto_pending.c`：**修好 → 留在 `tests/fixtures/opt/`，进门禁**。
  文件头注释从「暂不入全语料门」改为「G2 已修 → 进门禁」，写明新的根因站点与修法，
  保留旧实测读数作为历史锚。
- `tests/fixtures/opt/dce_label_after_return_minimal.c`：**新增最小形状夹具**（F1/F2/F3
  三形），同进门禁。
- 没有需要归档到 `tests/fixtures/_archive/dev/` 的条目。

## 7. 所有权与冲突

本包只改了：`src/opt.rs`（`dce_block` 一处 + 两条 lib 单测）、
`tests/integration/opt_ssa_gate.rs`（新增两条测试）、
`tests/fixtures/opt/loop_exit_ownership_goto_pending.c`（头注释更新，代码未改）、
`tests/fixtures/opt/dce_label_after_return_minimal.c`（新增）、本文件。
未触碰 `src/codegen/**`、`src/ssa*.rs`（未动）、`src/ssa_gvn.rs`、`src/parser.rs`、
`src/preproc.rs`、`src/link/**`、`src/driver.rs`、`Makefile`、`scripts/**`、
`docs/ssa-pass-upgrade-plan.md`、`docs/missing_features.md`、`docs/wp_*.md`（除本份）、
`Cargo.toml`、`Cargo.lock`、`tests/integration.rs`、
`tests/fixtures/c/seg_qualifier.c`、`tests/fixtures/c/seg_object_body.c`、
`tests/integration/c_lang.rs`、`src/codegen/disasm.rs`。

工作树里其他在途改动（`seg_*` 一族）与本包无冲突，实测期间未看到相关编译错误。
