# SSA 子系统目录化方案（src/ssa* 五文件的布局重构）

> **状态：已完工（2026-10，Phase 0-5 全部做完，§6 验收清单全绿）。**
> 本文保留原设计形态作为依据；§10 是执行记录，含**复测数据**、**与设计的偏差清单**
> 与**验收实测值**。开工触发条件（§8 条件 1）在本轮成立：`src/ssa.rs` 已从取数时点的
> 7753 行涨到 **9089 行**。
>
> 取数时点：HEAD `439bbf0` + 工作区未提交改动，2026-10。§2 的每个数字
> 都附了复取命令，隔段时间再读必须先复核（文件行数会变）。

## 1. 目标与非目标

**目标**

1. 「SSA 优化器子系统」在文件系统上成为一个目录，且**位置与模块路径一致**。
2. `src/ssa.rs`（7753 行）降为若干 <2000 行的职责文件，`mod.rs` 符合
   「精简：只放声明与再导出」的既有规范。
3. 消除 `ssa_gvn` 的命名误导 —— 它在 AST 上工作，与 SSA IR 无关。
4. 清掉 `src/opt/` 空目录（历史上 opt 目录化回退留下的残骸，git 里 0 个文件）。

**非目标（本方案绝不做）**

- 不改任何优化语义、pass 顺序、IR 数据结构、属性 helper 的判据。
- 不碰 `src/opt.rs`（3373 行 / 64 个函数）的目录化 —— 两个大子系统不同时动。
- 不做 **P12 的字面形态**（`ast::Ident` 携带寄存器号 + codegen 不再分配 + 真 SPILL/RELOAD，
  需重写单遍发射模型；登记在 `ssa-pass-upgrade-plan.md` §7.15，约束见 §7.3。
  注：§7.20 的「P12-B 已落地」说的是另一件事 —— SSA 层活性分配已经进了产物）。
- **硬判据：全程产物零字节变化**。搬迁过程中产物出现任何差异，即说明语义被
  一起改动了，该步立即回退，不得以"这个差异看起来无害"放行。

## 2. 现状盘点（实测）

### 2.1 五个文件

| 文件 | 行数 | 其中测试 | 测试块起点 | 依赖的同类模块 |
|---|---|---|---|---|
| `src/ssa.rs` | 7753 | 735（尾部另有一个 `cfg(test)` 块） | 7018 | 无（基座） |
| `src/ssa_passes.rs` | 4907 | 1594 | 3313 | ssa、ssa_pipelining |
| `src/ssa_pipelining.rs` | 3168 | **2640（占全文 83%）** | 528 | ssa |
| `src/ssa_regalloc.rs` | 973 | 288 | 685 | ssa、ssa_passes、ssa_gvn |
| `src/ssa_gvn.rs` | 1388 | 235 | 1153 | 无（在 AST 上） |

```bash
wc -l src/ssa*.rs
for f in src/ssa*.rs; do echo "$f $(grep -n '^mod tests' $f | head -1 | cut -d: -f1)"; done
```

结论：按职责是「4 + 1」—— 前四个是一个严格单向 DAG 的子系统，
`ssa_gvn.rs` 是挂错名的外来户。

### 2.2 外部耦合面（决定成本）

- **代码引用只有 5 个文件 / 19 处**：`opt.rs` 13、`cpp/lower_ir.rs` 2、
  `codegen/x86_64/emit/functions.rs` 2、`cpp/native.rs` 1、`codegen/x86_64/codegen.rs` 1。
- `ssa_gvn` 的消费仅 **3 处代码 + 1 处注释**：`opt.rs:129`（`run`）、
  `ssa_regalloc.rs:157/158`（`addr_escaped_names`）、`ssa_regalloc.rs:29`（注释）。
- `ssa.rs` 的 `pub` 项 26 个，其中 **25 个被外部引用**（`Function` 出现在 28 个文件）
  ⇒ 这就是 `mod.rs` 再导出清单的规模。
- **Makefile 与 scripts/ 里 0 处**路径依赖；五个文件里**没有** `file!()` /
  `module_path!()` / `#[path]` ⇒ 不存在"路径进产物或进测试名"的隐式依赖。
- `docs/` 里路径引用 51 处（`ssa.rs` 22、`ssa_passes.rs` 11、`ssa_gvn.rs` 9、
  `ssa_pipelining.rs` 8、`ssa_regalloc.rs` 1）。

```bash
grep -rn "ssa_gvn::\|ssa_passes::\|ssa_pipelining::\|ssa_regalloc::\|crate::ssa::" src/ \
  --include=*.rs | grep -v "^src/ssa"
grep -oE "^pub (fn|struct|enum) [a-zA-Z_]+" src/ssa.rs | awk '{print $3}' | sort -u
grep -rho "ssa_gvn\.rs\|ssa_passes\.rs\|ssa_pipelining\.rs\|ssa_regalloc\.rs\|ssa\.rs" docs/ | wc -l
```

### 2.3 硬约束（决定方案形状）

| 约束 | 后果 |
|---|---|
| **E0761**：`src/ssa.rs` 与 `src/ssa/` 不能并存 | 目录化必然要求 `git mv ssa.rs → ssa/mod.rs` |
| 规范：`mod.rs` 保持精简、只放声明与再导出 | 光移动不够，**必须同时**把 7753 行拆成多文件 |
| 规范：修错/重构/迁移一律逐条定向编辑，禁止批量脚本改代码 | 搬迁不可用 sed 批量改写；`git mv`（单目标确定性）与只读 grep 计数允许 |

这两条约束把"建目录"和"拆内容"锁成一步 —— **这是本方案全部成本的来源**，
也是默认建议「不顺手做」的原因（§8 给出何时才该做）。

### 2.4 历史教训：`src/opt/`

`src/opt/mod.rs` 由 `85045d1` 加入，后续退回单文件 `src/opt.rs`，git 中
`git ls-files src/opt | wc -l` = 0，但磁盘上仍留有一个**空目录**。
那次回退的失败模式是：子模块彼此用 `super` 互访 + 大量 `pub(use)` 转发，
外部调用方全断。本方案 §4 的设计（**外部模块路径保持不变**）正是为了
绕开同一个坑。

## 3. 目标布局

```
src/ssa/
  mod.rs        <120 行：pub mod 声明 + 25 条 pub use 再导出 + 一段「这里有什么」的地图注释
  ir.rs         ~1100 行：SsaVar/Operand/InstKind/Inst/Terminator/BasicBlock/Phi/
                        OrderedNameSet/Function 及其 impl + 属性 helper
                        （kind_operands / inst_reads_memory / inst_unremovable /
                          inst_clobbers_memory / inst_written_bases / collect_uses_*）
  cfg.rs        ~700 行：term_succs / collect_all_preds / compute_dominators /
                        reverse_postorder / find_loop_body{,_multi} / is_back_edge /
                        compute_ipdoms / refresh_cfg / ordered_block_names /
                        has_multi_exit_cycle / dangling_refs / duplicate_ssa_defs
  structure.rs  ~600 行：is_structured* + lower_switches*（进 SSA 前的 AST 侧降级）
  build.rs      ~2200 行：Builder + lower_lvalue + collect_local_decls* +
                        insert_loop_phis / insert_label_phis / bump_version +
                        build_from_func / build_ssa / expr_shape
  rebuild.rs    ~2600 行：rebuild_block / rebuild_bb / rebuild_cycle / RebuildCtx /
                        walk_to_join / branch_join / emit_rebuild_inst /
                        inst_to_expr{,_resolved} / operand_to_expr /
                        类型推导 helper（int_const_ty / infer_binop_type /
                        unary_result_ty / store_pointee_ty / addr_of_ty / …）
  inline_temps.rs ~500 行：ExpandedTemp / InlinePlan / inline_window_safe /
                        plan_inlined_temps / is_synthetic_temp_base / find_use_site
  dump.rs       ~60 行：dump_function
  passes.rs     原 ssa_passes.rs（内容不动）
  pipelining.rs 原 ssa_pipelining.rs（内容不动）
  regalloc.rs   原 ssa_regalloc.rs（内容不动）
src/ast_gvn.rs  原 ssa_gvn.rs（改名，内容不动）
```

**测试的放置原则**：跟着被测主体走；跨切测试（一条用例同时构造 → 跑 pass →
重建检查产物形态的）**不硬拆**，集中放 `src/ssa/tests.rs`，由 `mod.rs` 以
`#[cfg(test)] mod tests;` 挂上。为了「文件对齐」而把一个回归场景切成两半，
会同时毁掉它的可读性与判据价值。

## 4. 设计决策（含被否方案）

| 决策点 | 选择 | 理由 / 被否掉的替代 |
|---|---|---|
| 外部模块路径是否变 | **不变**：外部继续 `crate::ssa::{Function, build_from_func, rebuild_block, dump_function, …}`，由 `mod.rs` 的 `pub use` 维持 | 使 Phase 2 的 19 处外部引用一处都不改，风险被压缩在目录内部；直接针对 §2.4 那次回退的失败模式 |
| 子模块命名 | `ssa::passes` / `ssa::pipelining` / `ssa::regalloc`（去 `ssa_` 前缀） | 前缀在目录里是冗余；代价是 6 处路径修改（`opt.rs` 3、`cpp/*` 2、`codegen/*` 1），逐条定向改 |
| 用 `#[path]` 保持旧模块名 | **否** | `#[path = "ssa/pipelining.rs"] pub mod ssa_pipelining;` 能让文件进目录而路径不变，但「位置与路径解耦」正是本方案要消除的性质，等于把债换个形态 |
| 拆内容与建目录分两步 | **否（做不到）** | E0761 与 `mod.rs` 精简规则把两者锁在一起；补偿性约束是「拆文件时禁止顺手改逻辑」，见 §7 R2 |
| `ssa_gvn` 进 `src/ssa/` | **否** | 它不在 IR 上工作，放进目录会强化而非消除错误认知；改为 `src/ast_gvn.rs` |
| 留兼容别名 `pub use crate::ssa::passes as ssa_passes;` | **否** | 同一模块两套名字，读代码的人要先弄清哪个是真的 |
| 同时目录化 `opt.rs` | **否** | 3373 行 / 64 函数，与 SSA 是两个子系统；同批动会让「产物零字节」判据失去归因能力 |

## 5. 实施步骤

每步的通用形状：**做完 → 编译（0 warning）→ 跑测试用例 → 与基线做产物字节对照 → 不一致就回退本步**。
构建/测试一律限 CPU（`-j2`、`--test-threads=2`）。

### Phase 0 —— 基线与两件零风险小事

0.1 取基线，存进 `.work_tmp/layout_base/`：

```bash
mkdir -p .work_tmp/layout_base
cargo build --release -j2
cargo test --release -j2 -- --test-threads=2 2>&1 | tail -5 > .work_tmp/layout_base/test.txt
bash scripts/check_disasm_coverage.sh LEVELS="0 2" > .work_tmp/layout_base/disasm.txt 2>&1
bash scripts/check_reproducible_output.sh > .work_tmp/layout_base/repro.txt 2>&1
```

0.2 建「三档产物哈希」基线（一个只读、单目标的 bash，遍历
`tests/fixtures/c/*.c` 以 `-O0/-O2/-O3` 编译取 `.s` 的 sha256 汇总）存为
`.work_tmp/layout_base/hash.txt`。它是后续每个 Phase 的主判据。

0.3 删除空目录 `src/opt/`；建备份分支 `git branch backup/pre-ssa-layout-rewrite`。

**验证**：`git status --short` 符合预期、`cargo build --release -j2` 通过。

### Phase 1 —— `ssa_gvn.rs` → `ast_gvn.rs`（独立先做，改动面 7 处）

1.1 `git mv src/ssa_gvn.rs src/ast_gvn.rs`。
1.2 `src/lib.rs`：`pub mod ssa_gvn;` → `pub mod ast_gvn;`（保持字母序位置）。
1.3 逐条改 3 处代码引用（`opt.rs:129`、`ssa_regalloc.rs:157`、`:158`）与
    1 处注释（`ssa_regalloc.rs:29`）。
1.4 改 `src/ast_gvn.rs` 文件头，明确写：「本模块在 **AST** 上工作，与
    `crate::ssa::passes` 里的 GVN 是两套实现；名字里曾有 `ssa_` 前缀，属误导」。

**验证**：`grep -rn ssa_gvn src/` 为空 → 编译 0 warning → 测试用例全绿 → 产物哈希与基线一致。

### Phase 2 —— 建 `src/ssa/`，把 `ssa.rs` 拆成 8 个文件（主步）

2.1 `git mv src/ssa.rs src/ssa/mod.rs`（先移动再改内容，避免出现 E0761 中间态）。
2.2 按 §3 边界**逐块手工搬**：`Read` 取该段 → 目标文件写入 → 从 `mod.rs` 删除该段 →
    编译通过 → 再搬下一块。**顺序按依赖**（被依赖者先落地）：
    `ir.rs → cfg.rs → structure.rs → build.rs → rebuild.rs → inline_temps.rs → dump.rs`。
2.3 每搬一块即补可见性：兄弟模块用到的项标 `pub(super)`/`pub(crate)`；
    只有 §2.2 那 25 个真被外部引用的名字保留 `pub`，并在 `mod.rs` 收进 `pub use`。
2.4 测试搬移：`mod tests`（735 行）与尾部第二个 `cfg(test)` 块整体移入
    `src/ssa/tests.rs`，`mod.rs` 挂 `#[cfg(test)] mod tests;`。
2.5 `mod.rs` 顶部写「地图注释」：每个子文件一行，说明放什么、以及**不放什么**
    （例：降级政策只在 `build.rs` 一处，不要在 `rebuild.rs` 复制判据）。

**验证**：每个子步 `cargo build --release -j2` 且 0 warning、`wc -l src/ssa/mod.rs` < 500；
整 Phase 完成后跑测试用例（lib + integration）、`make golden`、`make test`、产物哈希对照。

### Phase 3 —— 三个 `ssa_*` 收编为子模块

3.1 `git mv src/ssa_passes.rs src/ssa/passes.rs`、
    `git mv src/ssa_pipelining.rs src/ssa/pipelining.rs`、
    `git mv src/ssa_regalloc.rs src/ssa/regalloc.rs`。
3.2 `src/lib.rs` 删掉这 3 个 `pub mod`；`src/ssa/mod.rs` 加 3 个 `pub mod`（+ 需要对外保留的 `pub use`）。
3.3 逐条改 6 处模块路径：`opt.rs` 的 `crate::ssa_passes::{run_with_opts, cross_function_dce_with_roots}`、
    `cpp/lower_ir.rs` 与 `cpp/native.rs` 的 `ssa_passes::`、
    `codegen/x86_64/codegen.rs` 与 `codegen/x86_64/emit/functions.rs` 的 `ssa_regalloc::`；
    目录内部引用（`mod.rs` → `pipelining`、`regalloc` → `passes`）改为 `super::`/`crate::ssa::`。
3.4 同步改各文件头注释里的 `// src/ssa_passes.rs - ...` —— 这些注释本身就是文档，错了比没有更坏。

**验证**：`grep -rn "crate::ssa_passes\|crate::ssa_pipelining\|crate::ssa_regalloc" src/` **必须为空**；
其余同 Phase 2 全套。

### Phase 4 —— 反证「没有改到语义」

4.1 逐条比诊断通道的行为与输出：`DCC_SSA_OFF=*`、`DCC_OPT_SKIP=*`、
    `DCC_REGALLOC=0`、`DCC_PIPE_DUMP=1`、`DCC_SSA_DUMP=1`、`DCC_SSA_ERR_DUMP=1`。
    诊断字符串也在对照范围内（搬迁不该改文案）。
4.2 与 `backup/pre-ssa-layout-rewrite` 做**全语料产物对拍**：同一份哈希生成脚本
    在两个分支上各跑一次，`cmp` 必须完全一致。

### Phase 5 —— 文档与规范收口

5.1 更新 `docs/` 里 51 处路径引用。**注意区分**：§7.x 这类执行记录里的旧名是
    **当时的事实**，不改写；只在末尾新开一条记录给出「旧名 → 新名」对照表，
    让历史条目仍可查。
5.2 `docs/architecture.md` 补一张优化器侧的模块图（目前只有 codegen 侧有双层结构说明）。
5.3 更新长期记忆：把「codegen 各后端已统一为目录 + mod.rs」扩为覆盖优化器侧的
    布局规范；并把「`mod.rs` 精简规则会把目录化与内容拆分锁成一步」这条成本来源记下。

## 6. 验收清单（全绿才算完成）

- [ ] `ls -d src/ssa/`；`ls src/ssa.rs src/ssa_passes.rs src/ssa_pipelining.rs src/ssa_regalloc.rs src/ssa_gvn.rs` 全部不存在（无 E0761、无残留）。
- [ ] `wc -l src/ssa/mod.rs` < 500（目标 <120），且其中除 `pub mod` / `pub use` / 注释外无实现代码。
- [ ] `grep -rn "crate::ssa_passes\|crate::ssa_pipelining\|crate::ssa_regalloc\|ssa_gvn" src/` 为空。
- [ ] `cargo check --release --all-targets -j2` **0 error / 0 warning**（搬漏 `mut`、重复 `use`、
      越权可见性都会在这里暴露）。**不得只跑 `cargo build --release`**：它不编译
      `#[cfg(test)]` 目标，本轮就因此漏过一次 test-target 编译失败（见 §10.8.1）。
      `cargo build --release -j2` 仅用于产出做产物对拍的二进制，同样要求 0 warning。
- [ ] `cargo test --release -j2 -- --test-threads=2` 全绿，且 **lib 用例条数与基线相同** —— 条数变了就是有测试块被漏搬，这是目录化最阴的失败模式。
- [ ] `make test` 全绿（含三架构交叉汇编、pipeline-parity 等测试用例脚本）。
- [ ] 产物三档哈希与 Phase 0 基线逐字节一致；`scripts/check_reproducible_output.sh` 全绿；`scripts/check_disasm_coverage.sh` 残留数 = 基线值。
- [ ] `git diff --stat backup/pre-ssa-layout-rewrite HEAD` 以 rename 为主，内容行数变化接近 0。

## 7. 风险登记

| 编号 | 风险 | 处置 |
|---|---|---|
| R1 | 搬动时"顺手优化"了某判据，产物差异被当成搬迁噪声放行 | 每个子步都跑产物哈希对照；任何差异一律回退，不允许主观判断"无害" |
| R2 | 拆文件与改逻辑混在同一次改动里，出问题无法二分 | Phase 2/3 期间禁止任何语义提交；若必须修 bug，先回退搬迁、单独修完再重做搬迁 |
| R3 | 可见性收紧误伤外部引用 | 以 §2.2 那 25 个再导出名单为白名单逐条核对 `pub use`；编译期是强判据 |
| R4 | 测试块拆分导致用例静默丢失 | 用「用例条数不变」判据（§6）；跨切用例集中在 `ssa/tests.rs` 不拆散 |
| R5 | `git mv` 后 `git blame` 断链 | `git log --follow` 可查；备份分支保留完整历史，不得 rebase 掉 |
| R6 | 与并行推进的语义改动（多出口循环出口所有权、P12 字面形态的后端级重写）撞车 | 目录化单独一轮做完并立即提交；后端级重写若先落地，则在其之后再做 Phase 3 |
| R7 | 重复出现 `src/opt/` 那样的残骸 | Phase 0.3 清旧残骸；本方案每个 Phase 收尾都跑 §6 第 1、3 条"残留扫描" |

## 8. 什么时候才值得启动（触发条件）

目录化不产生任何编译器能力，因此**由事件触发，不按日历触发**。满足任一才开工：

1. 需要往 `ssa.rs` 再加第三类以上职责，或它超过 ~9000 行（当前 7753）；
2. 要做 P12 的字面形态（后端级 `Var` + 真 SPILL/RELOAD）—— 它必然同时改 IR 与 codegen 两侧，届时顺手把布局一次定下来；
3. 新增 IR 指令形态的成本高到说明结构在挡路（参考：`MemberAddr`/`IndexAddr` 各自只需 ~10 个编辑点，若变成 ~25 个即视为挡路）；
4. 实际发生过"找不到某政策在哪"的成本（可量化：能否在 3 分钟内定位「SSA 化降级政策」与「重建出口边守卫」分别在哪）。

若只是"看着不顺眼"，答案是**不做**：Phase 2 的成本被 E0761 与 `mod.rs` 规则强行抬高，
而现状的收益空间本来就小（19 处外部引用、0 处构建脚本依赖、`ssa_` 前缀已表达归属）。

## 9. 本方案范围之外、但值得单独推进的三件小事

它们与目录化无关，任何时点都可独立做（其中前两件几乎零风险）：

1. **`ssa_gvn` 改名或加注**（= Phase 1，7 处改动）：这是当下真实的认知负担，
   与新同学第一次读代码时的误判直接相关。
2. **删 `src/opt/` 空目录**（= Phase 0.3）：避免有人以为优化器已经分模块了。
3. **`ssa_pipelining.rs` 的测试与主体同文件**：该文件 3168 行里有 2640 行是测试
   （83%），主体只 527 行。若想只降"读代码时的噪声"而不做整轮目录化，两条廉价做法：
   - 文件头加一行指引注释（「主体止于第 527 行，其后全是用例」），零风险；
   - 或用 `#[cfg(test)] #[path = "ssa_pipelining_tests.rs"] mod tests;` 把测试块真正搬出。
     已用最小样例实测成立（`src/host.rs` 内写该声明 + `src/host_tests.rs`，
     `rustc --test` 编译并跑出 1 passed）：`#[path]` 是**相对当前文件所在目录**解析的。
     这里用 `#[path]` 不违反 §4 对它的否决 —— 那条否决针对的是**对外模块路径**与位置解耦，
     而测试模块不是对外路径，不影响任何调用方。

## 附：数据复取

本文所有计数都能用下面这段一次复核（只读、单目标 bash）：

```bash
cd /root/dcc
wc -l src/ssa*.rs src/opt.rs
grep -rn "ssa_gvn::\|ssa_passes::\|ssa_pipelining::\|ssa_regalloc::\|crate::ssa::" \
  src/ --include=*.rs | grep -v "^src/ssa" | sed -E 's#^(src/[^:]+):.*#\1#' | sort | uniq -c | sort -rn
grep -oE "^pub (fn|struct|enum) [a-zA-Z_]+" src/ssa.rs | awk '{print $3}' | sort -u | wc -l
grep -rho "ssa_gvn\.rs\|ssa_passes\.rs\|ssa_pipelining\.rs\|ssa_regalloc\.rs\|ssa\.rs" docs/ | wc -l
grep -n "file!()\|module_path!()\|#\[path" src/ssa*.rs; echo "（无输出 = 无路径隐式依赖）"
git ls-files src/opt | wc -l; ls -d src/opt 2>/dev/null   # 空目录残骸是否为 0
```

若复核后的数字与本文不符，**以复测为准**并回来修订本文（写明修订日期与差异）。

---

## 10. 执行记录（2026-10，Phase 0-5）

备份分支：`backup/pre-ssa-layout-rewrite`（= HEAD `46c9e5c`，开工前工作区干净）。
硬判据（产物零字节变化）**全程成立**：每个 Phase 结束时三档产物哈希与 Phase 0 基线
逐字节一致，Phase 4 又用备份分支**重新构建**的二进制做了一次对拍，两边哈希相同。

### 10.1 复测数据（与 §2.1 的差异）

| 取数时点的 §2.1 | 开工实测 |
|---|---|
| `ssa.rs` 7753 | **9089**（+1336，§8 条件 1 成立） |
| `ssa_passes.rs` 4907 | 4935 |
| `ssa_pipelining.rs` 3168 | 3176 |
| `ssa_regalloc.rs` 973 | 973 |
| `ssa_gvn.rs` 1388 | 1405 |
| `opt.rs` 3373 | 3516 |

外部耦合面也比 §2.2 预估更小：`crate::ssa::` 形态的引用实测 16 处，分布在 `opt.rs`(11)、
`cpp/lower_ir.rs`(2)、`cpp/native.rs`(1)、`codegen/x86_64/emit/functions.rs`(1)、`ast_gvn.rs`(1)
（后两者是 `crate::ast_gvn`）；§2.2 列的 `codegen/x86_64/codegen.rs` 与 `ifunc.rs` 已仅剩注释。
`file!()` / `module_path!()` / `#[path]` 复测仍为 0 处（路径不进产物、不进用例名）。

### 10.2 落地形态（替代 §3 的目标布局）

```
src/ssa/
  mod.rs        103 行：13 条 pub mod + 14 条 pub use（只列目录外真有消费者的名字）
                + 9 条仅 `ssa` 内部共享的再导出 + 地图注释（无实现）
  ir.rs         846   cfg.rs        494   structure.rs  431
  build.rs     2098   lower.rs      571   loop_exit.rs  508
  inline_temps.rs 333  rebuild.rs   1518   expr_rebuild.rs 1040
  dump.rs        34   passes.rs    4935   pipelining.rs 3176   regalloc.rs 973
  tests.rs     1305   unary_rebuild_ty_tests.rs  52
src/ast_gvn.rs 1410（原 ssa_gvn.rs 1405，头注释净增 5）
```

行号→文件的映射按 127 个顶层条目（含各自紧邻上方的注释块）无缝拼出，每块搬完都用
只读 `diff` 与源区间做**逐字节对照**（10 个实现文件均 0 输出），因此「没改一行代码」是
可证明的而非声称的。两个测试块是例外：判据为「剥掉一层 4 空格缩进后逐行一致」，
另外各自新增了一小段 `use`（共 16 行，原因见 §10.3 第 3 条）。

### 10.3 与设计的偏差清单（均以实测为准，理由逐条）

1. **拆成 10 个实现文件而不是 §3 的 8 个**。两个原因：（a）「多出口循环出口归属」一组
   （`LoopExitOwner`/`LoopExitPlan`/`loop_shape`/`loop_exit_plan`/`owner_of`/`exit_class_of`/
   `resolve_loop_exit_stub`/`is_eh_pad`/`has_multi_exit_cycle`）写方案时还不存在，新增
   `loop_exit.rs`；（b）为了守住 §1 目标 2「职责文件 < 2000 行」，把 §3 的 `rebuild.rs ~2600`
   控流/表达式两个轴拆开（`rebuild.rs` 1518 + `expr_rebuild.rs` 1040），从 `build.rs` 拆出
   被 `Builder` 调用的降级辅助与 phi 插入（`lower.rs` 571）。`build.rs` 2098 行是**本次拆分的
   10 个实现文件里**最大的（`passes/pipelining/regalloc` 是整文件收编，不在该口径内），
   来头是 `impl Builder` 本身 1951 行且不宜拆。
2. **`has_multi_exit_cycle` 从 §3 的 `cfg.rs` 改放 `loop_exit.rs`**。它与 `loop_exit_plan` 是
   「判据与发射器同源」的一对（见 `loop_exit.rs` 头注释）；分家止好把要防止漂移的东西摆在一起。
3. **两个测试块分放两个文件**，不是 §3/§5 2.4 说的集中到 `ssa/tests.rs`。集中会把第二块的
   用例名变成 `ssa::tests::unary_rebuild_ty_tests::*`，而分放两个同名文件使两个模块路径完全不变
   —— §6 第 5 条要的是「用例不静默丢失」，名字不变比「只有一个测试文件」更值钱。
   两个文件的内容 = 源区间逐行剥掉一层 4 空格缩进（只剥外层 `#[cfg(test)] mod X {`/`}` 外壳），
   另在顶部补 16 行 `use`——必须补的原因是：原来 `use super::*;` 的 `super` 是那个带
   `use std::collections::…` / `use crate::ast::{…}` 的 9089 行文件， glob 能带进这些名字；
   拆完后 `super` 是只含声明与再导出的 `mod.rs`，那些名字全部失效。
   已先实测确认该区间**没有裸跳行字符串字面量**
   （所有多行串均用 `\`+换行续行，会吞掉下一行前导空白），所以剥缩进是一次纯空白变更。
4. **Phase 1 的改动面是 12 处而不是 7 处**。除代码与注释外，还包含 `opt.rs` 里
   `opt_skipped("ssa_gvn")` 这个 **`DCC_OPT_SKIP` 关键字字符串** —— 它与模块同名，不改就会在
   `src/` 里留下旧名。这是全程**唯一一处诊断文案变化**，已登记到 §4.1（见下面第 6 条的对拍处理）。
   实测全仓没有任何测试用例或脚本使用旧关键字 `DCC_OPT_SKIP=ssa_gvn`。
5. **§5 3.3 预估的 6 处外部路径实测只剩 5 处**（`cpp/*` 与 `codegen/*` 的引用已降为注释）；
   目录内部引用统一取 `crate::ssa::passes::` / `crate::ssa::pipelining::` 全路径形，不用 `super::`，
   与三个文件里既有的 `crate::ssa::ordered_block_names` 写法一致。实际字面点 10 个：
   `opt.rs` 5、`ssa/passes.rs` 4、`ssa/regalloc.rs` 1。
6. **`DCC_OPT_SKIP` 的 `run_ssa_passes` / `ssa_regalloc` 两个关键字不改**，因此 §6 第 3 条
   判据按 `crate::ssa_passes|crate::ssa_pipelining|crate::ssa_regalloc|ssa_gvn` 这四种形式为限
   （实测全空）。理由：它们是用户可见旋钮，`tests/integration/opt_ssa_gate.rs` 与
   `scripts/check_reproducible_output.sh` 都按名字驱动；而 Phase 1 那条关键字已因模块改名
   必须跟着改，所以 §4.1 的逐通道对拍里，OLD/NEW 两侧**各自使用自己的关键字列表**。
7. **6 处 `// ==== 区块横幅 ====` 注释归位**：它们原属单文件里的节分隔，拆分后与主体分家。
   1 处改写为 `Switch -> If-chain 降级 pass` 留在 `structure.rs`（它仍有两个主体，分隔仍有导航价值），
   其余 5 处（Dominator tree / 入口检测 / AST->SSA 转换器 / SSA->AST 重建器 / Pass 工具打印）
   与子文件头注释语义重复，删除。删改的全是注释，产物哈希在此步未变。
8. **§2.2 说的 docs 旧路径引用 51 处，实测 101 行命中**（旧名在 docs 里有 `.rs` 后缀、
   `crate::` 前缀、散文提及三种形）。按 §5.1 执行：**执行记录里的旧名是当时的事实，不改写**；
   只更新现行状态类文档与代码里的活性指针，具体落成三类：
   - 现行文档（本节就是其中一份，另：`architecture.md` 补优化器侧模块图 + §10.2 布局，
     `support_features.md` 的两处活性指针，`README.md` 的 `src/ssa.rs` → `src/ssa/`）；
   - 代码与夹具里的活性指针注释（`opt.rs`/`loop_opt.rs`/`codegen/x86_64/ifunc.rs`/
     `tests/integration/opt_ssa_gate.rs` 及 4 份 `.c` 夹具注释里的模块路径）；
   - **12 份历史执行记录在标题下方加一条「路径不改写、新位置见本文 §10.4」的勘误指引**
     （正文一字未动）：`ssa-pass-upgrade-plan`(53 处)、`p12-physreg-spill-design`(10)、
     `wp_multi_exit_loop`(8)、`wp_ifunc_real_fix`(4)、`cpp_native_gcc_convergence_plan`(4)、
     `cxx-native-foundation-fix-plan`(3)、`wp_goto_pending`(2)、`dcc-fix-symref-consteval-solution`(2)、
     `c23-fallthrough-volatile-ssa-fix-plan`(2)、`array_index_addressing_refactor_plan`(2)、
     `wp_disasm`(1)、`longdouble_80bit_migration`(1)。这两类做法共同满足 §5.1
     「历史条目仍可查」：旧名原地保留，但任何一份记录的读者在开头就能拿到新路径。
     未加指引的两份：本文（对照表本体）与 `architecture_v1_backup_20260923.md`（文件名已自说明
     是归档快照，不应再写）。
   - 夹具**文件名**（`ssa_regalloc_locals.c` 等）与集成测试**函数名**（`ssa_regalloc_*`）未改：
     前者进《三档产物哈希》的比对表键，后者是用例名集合判据的比对对象，改它们等于动判据。
9. **§2.2 说的 51 处 docs 引用与 19 处代码引用的取数均已是旧快照**；开工时实测
   `crate::ssa::` 形态外部引用 16 处（分布在 5 个文件），全部通过 `mod.rs` 再导出保持不变。

### 10.4 旧名 → 新名 对照（历史条目反查用）

| 旧 | 新 |
|---|---|
| `src/ssa.rs` | `src/ssa/mod.rs`（声明+再导出）+ `ir/cfg/structure/build/lower/loop_exit/inline_temps/rebuild/expr_rebuild/dump/tests/unary_rebuild_ty_tests.rs` |
| `src/ssa_passes.rs` | `src/ssa/passes.rs`（模块 `crate::ssa::passes`） |
| `src/ssa_pipelining.rs` | `src/ssa/pipelining.rs`（模块 `crate::ssa::pipelining`） |
| `src/ssa_regalloc.rs` | `src/ssa/regalloc.rs`（模块 `crate::ssa::regalloc`） |
| `src/ssa_gvn.rs` / `crate::ssa_gvn` | `src/ast_gvn.rs` / `crate::ast_gvn`（它在 AST 上工作，与 SSA IR 无关） |
| `DCC_OPT_SKIP=ssa_gvn` | `DCC_OPT_SKIP=ast_gvn`（唯一随改名变化的诊断关键字） |
| `src/opt/`（空目录残骸） | 已删除（`git ls-files src/opt` = 0） |
| `DCC_OPT_SKIP=run_ssa_passes` / `=ssa_regalloc` | 未变（用户可见旋钮，不是模块路径） |

历史条目里提到的 `ssa.rs:NNNN` 行号已全部失效（目录化后行号重排）；反查符号请用名字而非行号，
`git log --follow` 与 `backup/pre-ssa-layout-rewrite` 保留了完整旧形状。

### 10.5 验收实测值（§6 逐条）

| §6 判据 | 实测 |
|---|---|
| `ls -d src/ssa/`；旧 5 文件不存在 | ✓（无 E0761、无残骸） |
| `wc -l src/ssa/mod.rs` < 500 | **103**（目标 <120 也满足）；除 `pub mod`/`pub use`/注释外无实现 |
| `crate::ssa_passes\|...\|ssa_gvn` 为空 | ✓ 0 处 |
| `cargo check --release --all-targets -j2` 0 error / 0 warning（§6 第 4 条已改为此口径） | ✓ 0 / 0；`cargo build --release -j2` 亦 0 warning |
| `cargo test --release -j2` 全绿且 **lib 条数不变** | lib **2058 passed / 2 ignored**、integration 1036/1、errors 26、其余各 target 全等；用例名集合 3212 条，归一后与基线**完全相同**（差异只有模块前缀） |
| `make test` 全绿（含三架构交叉汇编、pipeline-parity 等测试用例脚本） | ✓ `check_fixtures.sh` + `check_fixtures_selftest.sh` + `golden-cross` + `pipe-structure` + `opt-eq` |
| 产物三档哈希逐字节一致 | ✓ grand aggregate `f5e00728fa61e7db…dc059` 全程未变；`check_reproducible_output.sh` 全绿（5238 组合 × 3 连跑）；`check_disasm_coverage.sh` 读不出 162 = 基线值 |
| `git diff --stat backup/… HEAD` 以 rename 为主 | ✓ `git diff --cached --stat -M backup/pre-ssa-layout-rewrite -- src/`：22 files changed, 9361(+)/9113(-)，净额 +248 全在新增的子文件头注释、`use` 与 `mod.rs` 地图；三个收编模块均识别为 rename（内容差 `passes.rs` 16 行 / `pipelining.rs` 4 行 / `regalloc.rs` 10 行，全部是模块路径与文件头注释）；`ssa.rs → ssa/mod.rs` 因内容差超阈值不记为 rename，它的一分为十二靠上面「每块逐字节 diff」与 §10.3 第 3 条的推导链保证 |

### 10.6 Phase 4 反证的一个意外发现（与搬迁无关，但必须记录）

`DCC_SSA_IR_DUMP` 打印的 `preds=[..]` / `succs=[..]` **元素序本身就逐次运行不稳定**：
对**搬迁前**的二进制同一夹具连跑 8 次，得到两种序（3/5 分布）；搬迁后连跑 8 次为 4/4。
即这是存量的 HashMap 迭代序泄漏，只影响诊断文本，不影响产物（两侧 `.s` 均逐字节确定）。
因此逐通道对拍的判据是「退出码 + `.s` 产物 + **序归一后**的诊断文本」逐字节一致；归一仍对
集合内容敏感（任何元素增删都会改变哈希）。实测 9 个通道配置 × `-O2/-O3` × 291 夹具全部 OK。
待单独修（不属于本方案范围，已登记）：给 `preds`/`succs` 缓存的打印走 `ordered_*` 系列，
否则这个窗口在二分定位时会把「同一个二进制」当成「两个行为」。

### 10.7 重现本轮验收的命令

```bash
cd /root/dcc
cargo build --release -j2                                    # 期望 0 warning
cargo test --release -j2 -- --test-threads=2                  # 期望 lib 2058/2 ignored
JOBS=2 bash .work_tmp/layout_base/gen_hash.sh /tmp/h.txt       # 基线 grand aggregate
LEVELS="0 2" bash scripts/check_disasm_coverage.sh | tail -2    # OK（0.0531%，上限 162）
bash scripts/check_reproducible_output.sh                      # 5238 组合全可复现
DCC=$PWD/target/release/dcc DCC_REPO=$PWD JOBS=2 bash scripts/check_opt_equivalence.sh
```
（`.work_tmp/layout_base/gen_hash.sh` 与 `channels.sh`/`norm.awk` 是本轮的只读采集器，
在 `.gitignore` 内不入库；需要时可从 §10.5 的描述重现。）

### 10.8 独立复核（CodeReview）后的修订

本轮收尾做了一次不预先告知结论的独立复核。复核**独立复现了硬判据**（产物 873 行哈希
`cmp` 逐字节相同、强制重编 0 warning、用例名多重集反向归一后只差 2 条前缀、
用 `split_map` 反向验证“原文件每一非空行均出现在新文件中”），**无阻断级问题**；
但拓出了 5 条应当修的缺陷，已全部修正：

| # | 复核拓出的问题 | 修正 |
|---|---|---|
| 1 | `src/` 内仍残留 **6 处指向已删除的 `src/ssa.rs` 的活性注释**（`ir.rs:473`、`passes.rs:2215`、`ast.rs:1472`、`cpp/const_text.rs:224`、`codegen/x86_64/codegen.rs:77`、`abi/tyclass.rs:593`）。§6 第 3 条的 grep 判据按构造 **不覆盖 `ssa.rs`**，所以“判据全绿”不等于这一类已收干 | 6 处逐条改指新位置；并把 `grep -rn "ssa\.rs" src/` 列为本方案的附加残留判据（实测现为零命中） |
| 2 | 本轮新写的注释引用了**不存在的诊断通道 `DCC_SSA_DUMP`**（根源：方案 §5 4.1 自己列的 `DCC_SSA_DUMP` / `DCC_SSA_ERR_DUMP` / `DCC_REGALLOC=0` 三个名字在代码里都不存在，实测真名是 `DCC_SSA_IR_DUMP` / `DCC_SSA_BAIL` / `DCC_REGALLOC_DUMP`） | 按实测调用点改写：`dump_function` 由 `DCC_SSA_IR_DUMP`(opt.rs)、`DCC_DUMP_CPP_AST`(cpp/native.rs)、`DCC_PIPE_DUMP`(ssa/passes.rs) 三条通道共用；同步修 `mod.rs` 地图与 `architecture.md` |
| 3 | `architecture.md` 新模块图声称子文件“**单向依赖**”，但实际有两条反向边（`ir → rebuild::inst_has_side_effect`、`loop_exit → rebuild::{is_pass_hint_inst, is_loop_exit_pure_return}`） | 改为“大致分层 + 显式登记两条反向边”，并写明它们是“判据只有一份”的共享，拆模块时不得当作可断开的边 |
| 4 | **公共面未按 §5 2.3 收紧**：13 个 `pub use` 名字在本目录外连注释都未出现（它们的消费者已在 Phase 3 变成了兄弟模块）；另有 4 个 `pub(super)` 放宽无任何消费者（`store_pointee_ty`/`operand_to_expr`/`collect_var_uses_op`，其前三个本来就在 `#![allow(dead_code)]` 下） | 按“目录外真有代码消费者”为准重算：保留 **14** 个 `pub use`，其余 17 个降为 `pub(super)` + 仅 `ssa` 内部可见的普通 `use`；那 4 个退回本模块私有。`kind_operand_ssa_vars` 从 `pub(crate) use` 降为普通 `use`。例外：`OrderedNameSet` 必须留 `pub`（它是 `pub struct Function` 的 `pub` 字段类型）。`inst_to_expr` 全仓零调用（搬迁前就是死码，本轮不顺手删，只登记） |
| 5 | 本节（§10）有多处“证据强于事实”：称两个测试块是“逐字副本”（实际各补了一段 `use`）、`ast_gvn.rs` 写 1412（实 1410）、`impl Builder` 写 1952（实 1951）、`build.rs` 写“全表最大值”（同表 `passes.rs` 更大） | 已全部按实测修正（上方 §10.2/§10.3 已改）；这本账是本轮唯一的搬运凭证，数字错比没有更坏 |

**有意不改的两项**（写了理由，免得下一轮又被当成漏洞）：
- `pub mod passes/pipelining/regalloc/…` 不降为私有 `mod`：§3 目标布局明确写的是“`pub mod` 声明”，
  且收编后的三个模块本来就是库内各处的调用入口；“同一事实的第二条路径”由 §3 自己选定，
  不在本轮翻案。
- 历史文档正文里的旧链接文字保留（按 §5.1 “不改写”），但**指向已删文件的可点击链接**
  已改掉 target（文字不变），否则它们永远 404。

**修正后的复验**（同样全部实测）：`cargo build --release -j2` **0 warning**；
三档产物哈希与 Phase 0 基线**逐字节一致**（可见性收紧属纯编译期改动，已被该判据证明）；
`cargo test --release -j2 -- --test-threads=2` 全绿且 lib 仍为 **2058 passed / 2 ignored**；
`check_fixtures` / `check_fixtures_selftest`(5/5) / `check_reproducible_output`(5238 组合) 全绿。

#### 10.8.1 上述第 4 条修正过程中我自己引入过一次回归（必须记下来）

按“目录外真有消费者”收紧可见性时，我根据 **lib 构建**报的 `unused_imports` 警告删掉了
`mod.rs` 里 5 条再导出；但 `#[cfg(test)] mod tests` 正是靠其中几条解析
`loop_shape` / `loop_exit_plan` / `loop_exit_owner` / `LoopExitOwner` / `LoopExitPlan`。
结果是 **`cargo build --release` 全绿、`cargo build --tests` 编译失败 17 错**。
根因不在这一次编辑，而在**判据本身**：§6 第 4/5 条只写了 `cargo build --release -j2`
与 `cargo test --release`，如果只跑前者就看不到 test target。

两个永久修正：
1. 改判据：编译门禁一律用 **`cargo check --release --all-targets -j2`**（覆盖 lib / bins /
   tests / examples），`cargo build` 只用于产出可执行文件做产物对拍；
2. 真正的修法不是把再导出塞回 `mod.rs`（那样会重新引入 lib 的 unused 警告），而是让
   `tests.rs` 自己显式 `use crate::ssa::loop_exit::{…}` —— 跟它对待其他内部符号的方式一致。

本轮最终状态（含此修正）的实测：`cargo check --release --all-targets -j2` **0 error / 0 warning**；
`cargo build --tests -j2` 通过；`cargo test --release -j2 -- --test-threads=2` 全绿且条数与基线相同；
三档产物哈希仍与基线**逐字节一致**；`check_fixtures` / selftest(5/5) / `check_reproducible_output` /
`check_disasm_coverage`(162) / `check_pipe_structure` / `arm_mips_golden_baseline verify` /
`check_opt_equivalence`(0 OPT-DIFF) 全绿。


