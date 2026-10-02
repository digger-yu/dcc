# SSA 子系统目录化方案（src/ssa* 五文件的布局重构）

> **状态：设计完成，未开工。**本文不是待办清单，而是一份"要不要做、
> 真要做就一次做对"的实施设计。开工时机由 §8 的触发条件决定，
> 不由日历决定；§1 的判据是**产物零字节变化**。
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
- [ ] `cargo build --release -j2` **0 warning**（搬漏 `mut`、重复 `use`、越权可见性都会在这里暴露）。
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
