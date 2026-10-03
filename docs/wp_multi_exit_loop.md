# 工作包 LOOP —— 多出口循环的重建侧出口所有权设计

> **路径勘误（2026-10，SSA 目录化）**：本文正文里的旧路径**不改写**（它们记的是当时的事实）。
> 本包涉及的判据与发射器现在 `src/ssa/loop_exit.rs`（与 `src/ssa/rebuild.rs` 同源）；
> `src/ssa.rs` → `src/ssa/`、`src/ssa_passes.rs` → `src/ssa/passes.rs`。
> 完整对照表与验收实测见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

负责人：LOOP 工作包。范围：`src/ssa.rs` 的 SSA→AST 结构化重建（`rebuild_bb` /
`rebuild_cycle`）与它的准入判据 `has_multi_exit_cycle`。

本页只写**这一族缺陷**的设计与实测边界；历史条文见 `docs/ssa-pass-upgrade-plan.md`
§7.28 / §7.31 未修项 1 / §7.32 / §7.40「七、仍未修」第 3 项。

---

## 0. 一句话根因

`rebuild_cycle` 把头块的 `Branch` **提升**成 `while (cond)` 这个动作本身，就等于把
头块那条体外出边**改判**为「循环的落出口」—— 提升之后头块不再发 `if`，那条边在整个
发射器里**再也没有第二个发射点**。于是：

* 「续体取哪个块」（`rebuild_cycle` 里 `exit_blocks.into_iter().next()`）与
* 「出口边发射点有没有守卫」（5 个点里只有 3 个接了 `is_loop_exit_pure_return` /
  `should_inline_loop_exit_target`）

这两件事是**互为掩盖**的：头块的体外出边常常正好就是体内那条 `return` 块，所以
「续体取第一个」恰好取到了它，而「裸 `break`」又把它当落点接住 —— 两侧同时错才对得上。
只改任一侧都会把错值暴露出来（§7.32 实测：14 条错值 + dcc 退出码 4）。

因此本轮的修法是**一笔改完四件事**：续体唯一化、提升许可、5 个发射点同守卫、判据同源；
外加本轮实测新查出的两个缺口（第 6 个发射点 `switch`、循环入口边的汇聚点，见 §1.5/§1.6）。

---

## 1. 出口所有权模型

### 1.1 结构事实的唯一来源

一个自然循环的 `(header, latches, body)` 由 `loop_shape(func, header, doms)` 给出，
**判据与重建共用这一份**（旧代码两边各算一次：判据用 `hb.preds` 过滤回边、重建另算，
`preds`/`succs` 是构造期缓存，SSA pass 改过 terminator 即脏 —— 两边看到的循环可能不是
同一个，「判据放行但发射器无路可发」有一半就来自这里）。后继一律从
`term_succs(&bb.term)` 现取。

`body` 含头块；`has_multi_exit_cycle` 直接用它，`rebuild_cycle` 用「去掉头块」的版本，
两者对 `in_loop` / 折叠 / 前驱判定的结果完全一致（所有相关谓词都显式再比一次
`== header`）。

### 1.2 折叠：`resolve_loop_exit_stub`

源码里一句 `break;` 的落点块常常**自己没有语句**（全部语义在后继里）。折叠条件严格：

```
沿链走当且仅当: 目标块 phis 为空 ∧ 真实语句数为 0 ∧ terminator 是无条件 Jump ∧ 下一跳不回循环内
```

任一条不满足就停在当前块（哪怕它仍是桩块形态）。**不折叠的后果实测很严重**：旧判据
数的「循环外后继」把桩块与它身后的合并块算成两个 ⇒ 今天连「`for` 里一个普通
`break`」都整函数退 SSA（实测 S5 / S7 / S13，见 §2）。

### 1.3 出口边的粗分类与三种落地方式

对每条出口边 `src -> tgt`，取 `fold = resolve_loop_exit_stub(tgt)`，先做与续体无关的
粗分类 `ExitClass`：

| 类 | 条件 | 含义 |
| --- | --- | --- |
| `Ret` | `fold` 的 terminator 是 `Ret` ∧ 无 phi ∧ **单一前驱**（= 既有谓词 `is_loop_exit_pure_return`，不另写一份） | 体内 `return` 的落点，不占用续体名额 |
| `Inline` | `fold` 无 phi ∧ 自带 ≥1 条真实语句 ∧ **全部前驱 ∈ body ∪ {header}** | 只被循环内路径到达 ⇒ 就地展开不会双份执行 |
| `Shared` | 其余 | 循环外也有前驱进入 ⇒ 它必须是那个「只发一次」的落点 |

最终所有权 `LoopExitOwner`（判据与 5+1 个发射点的**唯一真源** `loop_exit_owner`）：

| 所有权 | 落地方式 | 前提条件 | 发射位置 |
| --- | --- | --- | --- |
| `Continuation` | 循环之后发一次（自然落点） | 由 §1.4 的规则**唯一**确定 | `rebuild_cycle` 末尾 `rebuild_bb(cont, …, outer_ctx, stop_at)` |
| `PureReturn` | **到达边**就地发 `Stmt::Return` | `Ret` 类（Ret + 无 phi + 单前驱） | `rebuild_bb` 的 3 个点 + `rebuild_cycle` 的 2 个点 + `switch` 的 1 个点 |
| `InlineOnEdge` | **到达边**就地展开本块语句，再由其 terminator 递归成 `break`/`return` | `Inline` 类（有语句 + 无 phi + 前驱全在循环内） | 同上 |
| `Unsupported` | 无路可发 | 以上都不是（例：多前驱的 `Ret` 块且不是续体；零语句但 terminator 非 `Jump` 的块；折叠后回循环内的块） | ⇒ **判据必须挡下整个函数**（不优化且正确） |

`Continuation` 优先于类别：一条边的 `fold == cont` 时一律发 `break`（绝不得就地展开，
否则续体双份执行 —— 这正是 §1.5 那个 `switch` 缺口的形态）。

**`Label` + `Goto` 显式回入**（本项目 C++ 零成本 EH 的落地垫已证明 `Stmt::Label` /
`Stmt::Goto` 这套机制可用）是 Layer 2 的手段，用来覆盖今天 `Unsupported` 的两类：
多前驱 `Ret` 块（各路径的返回值需在同一标签处汇聚）与「同一循环有 >1 个 `Shared` 块」。
本轮**没有**启用它（见 §3 的分层结论）。

### 1.4 续体的唯一确定规则（不是「取第一个出口」）

按优先级，第一条成立即定：

* **R1** `Shared` 代表块恰 1 个 ⇒ 它就是续体。理由：`Shared` 块循环外也有前驱，
  除当落点外**没有别的发法**，它才是「必须发一次」的那一类。
* **R2** 没有任何 `Shared` ⇒ 取 latch 的非回边体外后继（折叠去重后）恰 1 个者。
  这是「循环正常跑完从尾部落出」的结构落点。
* **R3** 还没有 ⇒ 节点头 `Branch` 恰有一侧继续循环时，取另一侧（折叠后）。这是
  「头判定直接落出」的结构落点。
* **R4** 三者皆无 ⇒ `cont = None`：本循环永不落出（所有出口都是体内 `return`），
  循环之后**不得**发任何块。
* 违反「恰 1 个」的地方（`Shared` > 1，或两个 latch 各自落出到不同的块）= 两个结构
  落点候选而 `while` 只能表达一个 ⇒ `expressible = false`。

确定性：出口边按 `src` 字典序收集、`shared` / `tail_exits` 保序去重 ⇒ 结果与
`HashMap` 迭代序无关（本项目对「产物字节可复现」有既有要求）。

### 1.5 提升许可 `allow_header_lift`（根因级新增约束）

```
allow_header_lift = (节点头没有出口边) ? true
                                    : (cont == fold(节点头那条出口边)) && !头块写用户可见存储
```

只有当头块的出口边**就是**续体时才允许把 `Branch` 提成 `while (cond)`；否则必须退回

```c
while (1) { if (c) return …; … }
```

的形态（`rebuild_cycle` 里体现为第三个 `branched_from_header` 触发条件
`!plan.allow_header_lift && has_exit_side`），让那条边回到 `if` 语句上、由出口边守卫
安排。§7.32 那次放松把续体取法改对了却没管提升，实测就是
`该走到 return 7: got 1 want 7` 与 `n=0 一次都不跑: got 0 want 1` 两类错值。

### 1.6 5 + 1 个出口边发射点，同一个真源

| # | 位置 | 旧行为 | 现行为 |
| --- | --- | --- | --- |
| 1 | `rebuild_bb` `Terminator::Jump(t)`，`t` 在体外 | 已接守卫（`is_loop_exit_pure_return ∥ should_inline_loop_exit_target`） | `loop_exit_owner(t) ∈ {PureReturn, InlineOnEdge}` ⇒ 展开；否则 `break` |
| 2 | `rebuild_bb` `Branch` **true 侧**在体外 | 同上 | 同上 |
| 3 | `rebuild_bb` `Branch` **false 侧**在体外 | 同上 | 同上 |
| 4 | `rebuild_cycle` 节点头 `Branch` 的 `t` 侧（`branched_from_header` 路径） | **裸 `Stmt::Break`**，不过守卫 | 先 `expand_on_edge(t)`（内部 `loop_exit_owner`），否则 `break` |
| 5 | `rebuild_cycle` 节点头 `Branch` 的 `f` 侧 | **裸 `Stmt::Break`**，不过守卫 | 同上 |
| 6 | `rebuild_bb` `Terminator::SwitchTable` 的每个 `targets[i]` 与 `default`（本轮实测新发现） | **无条件**递归展开 ⇒ 循环外目标被就地展开，与 `rebuild_cycle` 之后发的那一次续体**双份执行**；体内 `return` 块被当续体时返回值丢掉 | `switch_edge_blocked(target)`：`PureReturn`/`InlineOnEdge` 才展开，否则 `break` |

「为什么单侧改必然错」：发射点的职责是「这条边到底谁发」，续体的职责是「落点发几次」。
把续体改成 R1–R4 而发射点仍发裸 `break` ⇒ 体内 `return` 的返回值丢（点 4/5 的旧形态）；
把发射点全接上守卫而续体仍取「第一个出口」⇒ 循环之后多发一个**不相干**的块（点 1–3
的旧形态配上 §7.28 的接线）。只有同源才自洽：`has_multi_exit_cycle` 与 6 个点调的是
同一个 `loop_exit_plan` / `loop_exit_owner`。

### 1.7 判据与发射器同源

`has_multi_exit_cycle(func)` = 「存在某个自然循环，其 `loop_exit_plan(...).expressible`
为 `false`」。等价性由构造保证：

* 判据侧与发射器侧都调 `loop_shape` + `loop_exit_plan`（同一个 `cont`、同一份
  `ExitClass` 判据、`is_loop_exit_pure_return` 只有一处定义）；
* `expressible = false` 的唯一来源是「某条边的 owner 是 `Unsupported`」或「两个结构落点
  候选」或「节点头直跳循环外（§1.8）」或「头块带具名写 + 头块 `break` 到 latch 提供的
  续体（§2.5）」—— 前三种情形发射器确实无路可发，第四种是**两条路实测都错**（不是推理）。

**反向**（发射器无路 ⇒ 判据必须挡）本轮补了两处，都是「判据比发射器宽」的历史遗留：

### 1.8 本轮新查出的两个「判据过宽」缺口

* **节点头 `Jump` 直接跳出循环**（`header_jump_escape`）：`rebuild_cycle` 的
  `body_entries` 只收体内后继，于是发成 `while (1) { <头块语句> }` 后直接落到续体，
  真实循环体一条都不执行。旧判据在这里不挡（体外后继只有 1 个）。现判为
  `expressible = false`。
* **循环入口边的汇聚点**（`loop_entry_join`，`rebuild_bb` 的 `Branch` 直线条带区）：
  preheader `Branch(进循环 / 跳过循环)` 的两侧分别走到「循环头」与「该循环的续体」，
  而 `branch_join` 判不出这个汇聚点（循环头自身入边 >1 就立刻返回，两侧不等后回落到
  `from` 的后支配者；体内有 `return` 时后支配者是**虚拟 EXIT 占位名**）。结果续体被
  内联进「进入循环的那条路径」（发在 `if` 里面），另一条路径空无一物 —— 实测
  `for(int i=0;i<n;i++){if(i==2)return 7;} return 1;` 在 `n=0` 返回 **0** 而不是 1。
  修法：`join` 若不是真实块就改问 `loop_entry_join`（一侧是 cycle entry 且其
  `plan.cont == 另一侧` ⇒ 续体就是汇聚点），并由 `stop_at` 让两条路径都不吃它、
  在 `If` 之后发一次。这条是 §7.31「未修项 1 前半」里「反向补丁会把 n=0 变成 0」的**真修**，
  不是绕开。

---

## 2. 实测边界

取证工具：`DCC_SSA_IR_DUMP=1`（只在报障时用，不进任何判据）+ 临时 `#[ignore]` 的
`ssa::tests::multi_exit_shape_probe`（逐形状打印真实 CFG、每条出口边的 terminator/phi/
语句数/前驱集合/是否头块直出/是否 latch 落出/折叠链，以及本轮计划的
`cont / expressible / allow_header_lift / 每条边的 owner`）：

```
cargo test --release -j1 --lib -- --test-threads=1 --ignored --nocapture multi_exit_shape_probe
```

原始输出：`/root/dcc/.work_tmp/probe1.txt`（修前，332 行）、`.work_tmp/probe2.txt`（修后）。
下表的 `挡?` 前两列都来自**程序读数**，不是记忆：`旧` = 旧计数规则（体外后继 > 1）在
同一份 IR 上的重放（`.work_tmp/probe2.txt` 里的 `blocked_before`），`新` = 本轮
`loop_exit_plan(...).expressible`。

### 2.1 形状清单

| # | 源码片段 | 出口边 → 出口块 terminator | 代表块前驱 | 纯 return | 零语句桩 | 旧挡? | 新挡? | cont / lift |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| S1 | `for(i<n) acc+=i*3;` | `for.step→for.end` `Ret acc` | `{for.cond, for.step}` | 否 | 否 | 否 | 否 | `for.end` / lift=1 |
| S2 | `for(...){if(even)continue; acc+=i;}` | `for.step→for.end` `Ret acc` | `{for.cond, for.step}` | 否 | 否 | 否 | 否 | `for.end` / lift=1 |
| S3 | `for(i<n){if(i==2)return 7;} return 1;` | `for.body→if.then` `Ret 7`；`for.step→for.end` `Ret 1` | `{for.body}`；`{for.cond,for.step}` | 是；否 | 否；否 | **挡** | 放行 | `for.end` / lift=**0** |
| S4 | `for(...){if(i==1)return 9;if(i==3)break;s+=i;}` | `for.body→if.then` `Ret 9`；`if.end→if.then.0` `Jump for.end`；`for.step→for.end` `Ret s` | `{for.body}`；折→`for.end`；`{if.then.0,for.cond,for.step}` | 是；—；否 | 否；**是**；否 | **挡** | 放行 | `for.end` / lift=0 |
| S5 | `for(...){if(i==3)break;s+=i;}` | `for.body→if.then` `Jump for.end`；`for.step→for.end` `Ret s` | 折→`for.end`；`{if.then,for.cond,for.step}` | 否 | **是** | **挡** | 放行 | `for.end` / lift=1 |
| S6 | `for(...){if(i==1){g=101;break;}}` | `for.body→if.then`（**2 条语句** + `Jump for.end`）；`for.step→for.end` | `{for.body}`（全在循环内） | 否 | 否（有语句） | **挡** | 放行 = `InlineOnEdge` | `for.end` / lift=0 |
| S7 | `for(...){if(i==1)break;if(i==2)break;s+=i;}` | 两个桩块 `if.then` / `if.then.0` 都 `Jump for.end` | 折→同一个 `for.end` | 否 | **是** | **挡** | 放行 | `for.end` / lift=1 |
| S8 | `do{i++;if(i==3)return 42;}while(i<n); return -1;`（旋转后节点头即分支点） | `do.body→if.then` `Ret 42`；`do.cond→do.end` `Ret -1` | `{do.body}`；`{do.cond}` | 是；**是** | 否 | **挡** | 放行 | `do.end`（R2 结构落点优先） / lift=0 |
| S9 | `for(;;){if(i>=n)return 5;if(i==2)break;i++;} return 6;`（无限循环 + 节点头分支） | `for.body→if.then` `Ret 5`；`if.end→if.then.0` `Jump for.end`；`for.step→for.end` `Ret 6` | `{for.body}`；折→`for.end`；`{for.step,if.then.0,for.cond}` | 是；—；否 | 否；**是** | **挡** | 放行 | `for.end` / lift=0 |
| S10 | `for(...){if(i==1)return 9;if(i==2)return 9;s+=i;}` | `if.then` `Ret 9` 与 `if.then.0` `Ret 9`（**两个不同的纯 return 块**） | 各自单前驱 | 是 | 否 | **挡** | 放行（两个都 `PureReturn`，不占续体名额） | `for.end` / lift=0 |
| S11 | `for(...){if(i==2)return s;s+=i;}`（返回值携带循环变量） | `for.body→if.then` `Ret s.3` | `{for.body}` | 是 | 否 | **挡** | 放行 | `for.end` / lift=0 |
| S12 | 嵌套：内层 `for(j<i){if(j==1)return 77;}` | 内层 `for.step.0→for.end.0` `Jump for.step`（**折到外层 latch**）；外层 `for.body.0→if.then` `Ret 77`；外层 `for.step→for.end` | 折→`for.step`（外层体内） | 否 | **是** | **挡** | 放行 | 内层 cont=`for.step` / lift=0；外层 cont=`for.end` / lift=1 |
| S13 | `for(...){if(i==2)break;s+=i;} if(s>3)return 1; return 2;`（`break` 落到桩块，循环后面还有代码） | `for.body→if.then` `Jump for.end`；`for.step→for.end`（**`Branch`**，非 Ret） | 折→`for.end`；`{for.step,if.then,for.cond}` | 否 | **是** | **挡** | 放行 | `for.end` / lift=1 |
| S14 | `while(k-- > 0){if(k==2)return 7;s+=k;}`（节点头自带不可丢弃的写） | `while.body→if.then` `Ret 7`；`while.cond→while.end` `Ret s` | `{while.body}`；`{while.cond}` | 是；是 | 否 | **挡** | 放行 | `while.end` / lift=**0**（两个条件各自拦住提升：§1.5 的 `!header_writes_user_storage` 与发射器原有的 `header_insts_droppable`） |
| S15 | `for(;;){if(n<=0)return 3;n--;}`（无落出口，尾部隐式 `Ret None`） | `for.body→if.then` `Ret 3`；`for.step→for.end` `Ret None` | `{for.body}`；`{for.cond,for.step}` | 是；否 | 否 | **挡** | 放行 | `for.end` / lift=0 |

### 2.2 挡住是否**必要** —— 逐条结论

* **S5 / S7 / S13（今天被挡，但折叠后只有 1 个共享出口）= 不必要，而且是最大的覆盖面损失。**
  这三格说明旧退路把「`for` 里一句普通 `break`」都整函数挡掉（桩块 `if.then` 与它身后的
  `for.end` 被数成两个出口）。本轮释放它们 = 所有带 `break` 的循环重新回到 SSA 管线。
* **S3 / S4 / S6 / S8–S12 / S14 / S15 的「两个循环外后继」是真的多出口，但挡得并不充分：**
  旧判据既没区分「体内纯 `return`」（不占续体名额），也没区分「自带语句且前驱全在循环内」
  的 `Inline` 块，同时还**没数**真正的硬约束（两个结构落点候选、头块直跳循环外）。
  即：对能表达的挡得多，对表达不了的可能挡得少 —— 双向都错，这正是历史上「放松一次就出
  错值」的土壤。
* **S8 / S9 是历史上「只改一侧就错」的两个现场**：节点头本身就是分支点（旋转后的 do-while
  与无限循环），旧代码在这里发裸 `Stmt::Break`，体内 `return` 的返回值随提升丢掉；而
  `allow_header_lift`（§1.5）把它们改判回 `while(1){ if(c) return …; … }` 形态后，两侧才第一次
  各自有发射点。
* **S12 的折叠必须按「各自循环」判定**：内层循环的 latch 落点 `for.end.0` 是零语句桩，它
  `Jump` 到的 `for.step` 是**外层循环的体内块** —— 对内层而言它是续体（折出内层 body 即停），
  对外层而言它是体内 latch。旧判据用一个全局的「循环外后继」集合处理不了这种嵌套。
* **S14 的两条独立拦截要分清**：`allow_header_lift=0`（§1.5 的第二个条件：头块写的是具名
  变量 `k`）且 `header_insts_droppable=0`（发射器侧原有守卫）。两者是 `∧` 关系，本轮没有
  放松后者 —— 提升 + 丢头块指令就是 `tests/integration/opt_ssa_gate.rs` 头部注释第 3 条记的
  那个历史缺陷（`while (k-- > 0)` 永不退出）。

### 2.3 对抗样本（证明退路是「变窄」而不是「没了」）

probe 里另加 T1–T6 六个专门用来「应当仍被挡」的形状，实测读数见 §3.2。

## 3. 分层实现与绿灯

### 3.1 实测矩阵（全部是跑出来的读数，不是推算）

取证方式：`-O0/-O1/-O2/-O3` 四档逐字节比较 gcc(-O0) 与 dcc 的 **stdout 与退出码**
（驱动：`.work_tmp/diff_one.sh`，判定一律以 gcc 为准，不读自家反汇编）。

**第一轮（裁决前，在途改动原样，未动一行代码）**

| 判据 | 读数 | 原始日志 |
| --- | --- | --- |
| `--lib -- --test-threads=1 --no-fail-fast ssa` | **100 passed / 1 failed**：`ssa::tests::multi_exit_cycle_predicate_is_narrow_and_sound` panic 在 `src/ssa.rs:8085`「体内 return + 循环落出 = 两个出口，必须判为多出口」 | `.work_tmp/step1_lib_ssa.log` |
| `--test integration -- … loop` | 15 passed / 0 failed（`loop_multi_exit_matches_gcc_at_all_levels`、`ssa_gate_switch_loop…`、`plain_loops_still_go_through_ssa` 全在里） | `.work_tmp/step1_int_loop.log` |
| `--test integration` 全量 | 1020 passed / **1 failed** / 1 ignored，唯一红 = `seg_qualifier_parses_all_positions_and_emits_segment_prefixes`（`tests/integration/c_lang.rs:557`，夹具自检 rc=-1，`__seg_*` 工作包在途） | `.work_tmp/step1_int_all.log` |
| 名字过滤 `opt_ssa` | **0 匹配**：本仓 integration 用例由 `include!` 注到 crate 根，测试名不带模块前缀 ⇒ 只能按 `loop` / `ssa` / `--exact <用例名>` 过滤（记下以免下一个人误读成「用例没跑」） | ——— |
| `#[ignore]` 探针（21 形） | 每一形都 `blocked_before=true` / `blocked_after=false` —— **新计划一个都没挡** | `.work_tmp/probe1.txt`、`probe2.txt` |

探针读数就是裁决的导火索：「全放行」不等于安全，于是构造对抗夹具逐形跑四档。

**第二轮（对抗夹具，裁决前）**

| 夹具 | 读数 |
| --- | --- |
| `tests/fixtures/c/loop_multi_exit.c` | -O0/-O1/-O2/-O3 全 OK |
| `tests/fixtures/opt/loop_exit_ownership.c`（新建 16 形） | 四档全 OK |
| `tests/fixtures/opt/loop_exit_ownership_adv.c`（新建对抗集） | **-O2/-O3 两条 DIFF**：`do_break` gcc `1 1 3 6 6 6 6` vs dcc `2 2 5 9 9 9 9`；`goto_out2` gcc `7 7 -3 …` vs dcc `7 7 -8 …` |
| 归因（`.work_tmp/attr/`） | `DCC_OPT_SKIP=run_ssa_passes` 下 `do_break` **正确** ⇒ 本包在途改动造成；`goto_out2` **两侧都错** ⇒ AST 层既有缺陷，与本包无关 |

**收口后（第 4 步重跑，最终状态）**

| 判据 | 读数 | 原始日志 |
| --- | --- | --- |
| `--lib -- --test-threads=1 --no-fail-fast ssa` | **101 passed / 0 failed / 1 ignored**（含改写后的 `multi_exit_cycle_predicate_is_narrow_and_sound`） | `.work_tmp/step7_lib_ssa.log` |
| `--lib` 全量 | **2010 passed / 0 failed / 2 ignored** | `.work_tmp/step7_lib_all.log` |
| `--test integration` 全量 | 1022 passed / **1 failed** / 1 ignored，唯一红仍是 `seg_qualifier_*`（与本包无关） | `.work_tmp/step7_int_all.log` |
| `--test integration -- … loop` | **17 passed / 0 failed** | `.work_tmp/step7_int_loop.log` |
| `--test integration -- … ssa` | **27 passed / 0 failed** | `.work_tmp/step7_int_ssa.log` |
| `loop_multi_exit_matches_gcc_at_all_levels` | ok（1 passed） | `.work_tmp/step7_one_*.log` |
| `ssa_gate_switch_loop_matches_gcc_at_all_levels` | ok | 同上 |
| `plain_loops_still_go_through_ssa`（已扩成四面捕手） | ok | 同上 |
| `loop_exit_ownership_shapes_match_gcc_at_all_levels`（新） | ok | 同上 |
| `loop_exit_ownership_boundary_matches_gcc_at_all_levels`（新） | ok | 同上 |
| 三份夹具四档 gcc 差分 | 全 OK；`loop_exit_ownership_goto_pending.c` -O2/-O3 仍 DIFF（预期，见 §4 第 1 条） | `.work_tmp/step7_diff_*.log` |
| `DCC_SSA_BAIL=1` 逐夹具数 bail | ownership **0** 条、loop_multi_exit **0** 条、adv **1** 条（`a_do_break`） | `.work_tmp/step7_driver.log` |

放行集 `tests/fixtures/opt/loop_exit_ownership.c`（最终 **19** 形 × 7 个入参）与边界集
`tests/fixtures/opt/loop_exit_ownership_adv.c`（**13** 形，其中 `a_do_break` 整函数退回 AST）
四档逐行与 gcc 一致。

### 3.2 探针读数：旧口径 vs 新计划

`multi_exit_shape_probe`（`#[ignore]`，只作取证）逐形状打 `blocked_before` / `blocked_after`：

* **S1/S2 = 两列都不挡**（新旧一致，覆盖面没有丢）。
* **S3–S15 + T1–T6：旧口径全挡（`blocked_before=true`），新计划全放行（`blocked_after=false`）**。
  收口后重跑探针（`.work_tmp/step8_probe.log`）读数一字未变 —— 因为真正必须挡的 A9 一族
  **不在这 21 形里**（T5 是 `do{if(i==2)break;i++;}while(i<n);return i;`，头块不带具名写，
  属于实测放行集）。这恰好说明探针的局限：拿它当「覆盖面探针」可以，拿它当
  「安全性证明」不行 —— 安全结论只能来自逐形 gcc 差分。
  放行不是「模型自证」，是每个形状都跑了四档 gcc 差分（§3.1 的第二轮 + 原夹具）。
  其中 T1/T2（多前驱 `Ret`）实测在 IR 里 `if.then` **只有单前驱**（`||` 被短路降级成两条
  边汇入同一块前已折叠），所以 §1.3 为「多前驱 `Ret`」留的 Layer 2 缺口这一批没有真的被
  踩到 —— 记下以免下一个人把它当已验证的能力。
* 旧退路的**必要性**没有被证明消失：§2.4 的 A9（`do{s+=i;i++;if(i==3)break;}while(i<n);`）
  证明「新计划全放行」里至少有一族是放早了，已整族挡回 AST。

### 3.3 收尾时的挡下集（与旧口径的关系）

挡下的只剩四类，每一条都是「发射器确实无路可发」或「无路可发的两种发法实测都错」：

1. `shared.len() > 1`：两个都必须共享的出口块（Layer 2 的 `Label`/`Goto` 才能覆盖）；
2. `tail_exits.len() > 1`：两个 latch 各自落出到不同块；
3. `header_jump_escape`：节点头 `Jump` 直跳循环外（循环体不可达）；
4. **本轮新增**：`header_break_with_named_write`（§2.4 的 A9）—— 头块带具名写 **且** 它的
   出口边折到**由另一个 latch 提供的**续体（三个条件缺一不可，第三个就是防 `while(n--)`
   被误挡的 `!tail_exits.is_empty()`）。

实测确认的放行集 = `loop_exit_ownership.c` 的 19 形 + `loop_exit_ownership_adv.c` 里除
`a_do_break` 外的 12 形 + 原 `tests/fixtures/c/loop_multi_exit.c`：普通 `for` / `continue` /
单个 `break` / 多个 `break` 合并到桩块 / 体内 `return` / `return`+`break` 同现 / 旋转 do-while +
`return` / 无限循环 / 循环内 `switch` / `while(k-- > 0)` 一族（含体内 `break`、`return`+`break`
同现）全部进 SSA —— 一句话：**多出口本身不再是理由，「无路可发」与「两种发法实测都错」才是**。
实测验证方法：`DCC_SSA_BAIL=1` 逐夹具数 bail 条数（最终一轮：ownership 夹具 **0** 条、
`loop_multi_exit.c` **0** 条、adv 夹具 **1** 条 = `a_do_break`）。

### 3.4 取证方法本身的一个坑（写下来以免后人拿空跑的断言当证据）

`compile_asm_text_with_env(tag, …)` 把 tag 写进产物首行 `.file "…/<tag>-<pid>.c"`。于是
「同一个函数开 / 关某条路径，逐字节比两份 -O2 汇编」这类**产物差异证明**如果直接比全文：

* `assert_ne!` 永远绿（文件名本身就是差异）—— 看着像「真的走了 SSA」，其实什么也证明不了；
* `assert_eq!` 永远红（同一个原因）—— 本轮新加的反向断言首跑就是被它挡红的
  （`.work_tmp/step4b_int_loop.log` 里两份汇编除去 `.file` 一字不差）。

现在统一走 `asm_body()`（剔掉 `.file` 行再比）。剔掉后的实测读数：体内 `return` 的多出口
循环差 800+ 行（真在 SSA）；`while (k-- > 0){acc+=k;}` 差 699 行（真在 SSA）；被挡的
`do_break` 一族差 **0** 行（真的整函数掉出了 SSA）。

---

### 2.4 实测边界表（形状 / 读数 / 是否放行 / 缺什么）

「读数」列全是四档 gcc 差分的原始输出，`挡` 列读的是 `loop_exit_plan(...).expressible`。

| 编号 | 形状 | 实测读数（gcc → dcc） | 放行? | 缺的机制 |
| --- | --- | --- | --- | --- |
| O1 | `for(i<n){if(i==2)return 7;} return 1;` | `1 1 1 7 7 7 7` 全档一致 | 放行 | —— |
| O2 | `for(...){if(i==1)return 9;if(i==3)break;s+=i;}` | `0 0 9 9 9 9 9` | 放行 | —— |
| O3 | 两个 `break` 合并 | 全 `0` | 放行 | —— |
| O4 | `continue`-only | `0 0 1 1 4 4 9` | 放行 | —— |
| O5 | 旋转 do-while，节点头即分支点，体内 `return` | `-1 -1 -1 42 42 42 42` | 放行 | —— |
| O6 | 无限循环 + 节点头 `return`/`break` 并存 | `5 5 5 6 6 6 6` | 放行 | —— |
| O7 | `break` 落到零语句桩块，循环后还有分支（两条分支都踩到） | `2 2 1 1 1 1 1` | 放行 | —— |
| O8 | 侧出口自带语句再 `break`（`InlineOnEdge`） | `0 0 101 101 101 101 101` | 放行 | —— |
| O9 | 两个不同的纯 `return` 块（9 / 8） | `0 0 9 9 9 9 9` | 放行 | —— |
| O10 | `return` 携带循环变量 | `0 0 1 1 1 1 1` | 放行 | —— |
| O11 | `while(k-- > 0){if(k==2)return 7;s+=k;}`（头块带具名写，但无独立 latch 落出） | `0 0 1 7 7 7 7` | 放行 | —— |
| O12 | 纯无限循环无落出口（`cont = None`，R4） | 全 `3` | 放行 | —— |
| O13 | 嵌套：内层提前 `return` | `0 1 2 77 77 77 77` | 放行 | —— |
| O14 | 循环内 `switch`：`case`→`return`、`case`→`break`、`default`→`continue`（第 6 个发射点） | `0 0 1 3 300 300 300` | 放行 | —— |
| O15 | `if(i==1||i==2)return 5;`（`\|\|` 短路） | `0 0 5 5 5 5 5` | 放行 | —— |
| O16 | 侧出口写 + `return` + 循环后共享落点 | `0 0 9 9 9 9 9` | 放行 | —— |
| O17 | `while(k-- > 0){acc+=k;}`（头块带具名写、**无任何侧出口**） | `0 0 1 3 6 10 15` | 放行 | —— |
| O18 | `while(k-- > 0){if(k==2)break;s+=k;}`（侧出口与条件落同一个点） | `0 0 1 0 3 7 12` | 放行 | —— |
| O19 | `while(k-- > 0)` + `return` 与 `break` 同现 | `1 1 1 3 91 91 91` | 放行 | —— |
| A1 | `goto` 出循环（单标签） | `100 100 101 1 1 1 1` | 放行 | —— |
| A3 | 两次 `break` 前各写不同值（落点是带 phi 的汇聚块） | `0 0 11 11 11 11 11` | 放行 | —— |
| A4 | 无限循环 + 只有 `break`，循环后有代码 | `1 2 4 7 11 16 22` | 放行 | —— |
| A5 | 无限循环 + 两个 `return`，无落出口 | `1 1 2 2 2 2 2` | 放行 | —— |
| A6 | 内层 `break` + 外层 `return` | `0 1 2 500 500 500 500` | 放行 | —— |
| A7 | 内层 `return`+`break` 与外层 `break` 同现，外层后还有分支 | `0 1 900 900 900 900 900` | 放行 | —— |
| A8 | `continue` 造第二个 latch + 尾部 `break` | `0 0 0 2 2 2 2` | 放行 | —— |
| A9 | **旋转 do-while：头块带具名写（`s+=i;i++;`）+ `break` 到续体** | gcc `1 1 3 6 6 6 6` → 放行时 dcc **`2 2 5 9 9 9 9`**（-O2/-O3）；挡回后四档一致 | **挡** | 见 §2.5 |
| A9b | `do{if(i==2)break;i++;}while(i<n);return i;`（头块不带具名写） | `0 1 2 2 2 2 2` | 放行 | —— |
| A9c | 同 A9 但侧出口是 `return`（判别因子对照） | `0 1 2 77 77 77 77` | 放行 | —— |
| A10 | 循环内 `switch` + `goto` 出循环 | `0 1 2 5 40 40 40` | 放行 | —— |
| A11 | 两条路径汇入同一个 `return` 值 | `0 0 5 5 5 5 5` | 放行 | —— |
| A12 | `for` 套 `while`，内层 `break` + 外层 `return` | `0 101 3 3 3 3 3` | 放行 | —— |
| G2 | `goto` 出循环到**第二个**标签（双标签 + 循环后代码） | gcc `7 7 -3 -3 -3 -3 -3` → dcc **`7 7 -8 -8 -8 -8 -8`**（-O2/-O3）；**关掉整条 SSA 往返仍错** | 与门无关 | 见 §4 |

注（本轮自己踩到的一次测不准）：O7 最初写成 `if (s > 3) return 1; return 2;`，实测读数
全 `2` —— 那个条件下 `return 1` 在 n=0..6 **永不可达**，「循环后有分支」这一面其实一条边
都没测。本轮把它改成 `if (s > 0)`（读数 `2 2 1 1 1 1 1`，两条边都踩到）后重跑四档仍全绿。
lib 单测与 probe 里的 S13 仍用 `s > 3` 原形 —— 它们钉的是**判据**（挡不挡），不执行代码，
死分支无害。

### 2.5 A9 的根因（本轮唯一「放松过头」的实测结论）

`do { s += i; i++; if (i == 3) break; } while (i < n); return s + i;` 的 IR 里循环头是
`do.body`，它**同时**是三件事：循环体块（带 `s+=i`、`i++` 两条对具名变量的写）、分支点
（`_tbin_4 = i==3`）、以及那条 `break` 出口边的发出点；自然落出在 latch `do.cond` 上。

* 旧口径在这里挡（体外后继 = `if.then` + `do.end` 两个）⇒ 走 AST ⇒ 对。
* 折叠后两个出口合成一个 `do.end` ⇒ 新计划判为可表达，`cont == fold(if.then)` ⇒
  `allow_header_lift = true` ⇒ `rebuild_cycle` 把 `Branch` 提成 `while (i != 3)`，**整块丢弃
  头块指令**；发射器原有的 `header_insts_droppable` 本想拦住这类形状，但它只问「dest 是否
  登记为可内联」，而 `i.2 = _tupd_2 + 0` 恰恰是可内联的拷贝 ⇒ 对「写具名变量」判成可丢弃。
  丢掉后重建只能在读它的地方重算，写的时机相对 `s += i` 被挪到前面 ⇒ `s` 用了自增后的 `i`
  （实测 `do_break(3)` = 9 = `0+1+2+3` 而不是 `0+1+2`）。
* **只把提升否掉不够**：实测退回 `while(1){ if(c){…} else {…} }` 形态（第三个触发条件
  `!plan.allow_header_lift && has_exit_side`）后读数**完全没变**（仍 `2 2 5 9 …`），即那条
  回退路径在「头块既是体块又是分支点」上同样发不对。

⇒ 按「不收在实测过的子集里就不放行」的政策，把这一族直接判 `expressible = false`
（`header_break_with_named_write`，`loop_exit_plan` 第 5 步），整函数退回 AST。
真修需要的机制（下一轮的入口，不在本轮）：头块的具名写必须**固定成循环体内按原序发射的
语句**，且与「把头块 `Branch` 提成 `while` 条件」互斥 —— 也就是把头块指令的发射与
`cond_expr` 的提取拆成两条独立的路，同时 `header_insts_droppable` 要把「写用户可见存储」
单独判为不可丢弃（它现在的注释说的就是这个意图，代码没做到）。
判别因子不是「侧出口是 `break` 还是 `return`」，也不是「头块有没有具名写」，而是
**头块那条出口边是不是额外的 `break`**（即续体由**另一个 latch 块**提供）：

* A9（`do{s+=i;i++;if(i==3)break;}while(i<n);`）：头块 `do.body` 带具名写，续体 `do.end`
  来自 latch `do.cond` 的落出，而头块自己那条边又折到同一个 `do.end` ⇒ 挡。
* A9c（同形状但侧出口是 `return`）：头块 `Branch` 两边都在循环内 ⇒ `header_exit = None`
  ⇒ 不沾这条约束，实测四档一致（`0 1 2 77 77 77 77`）。
* O11/O17/O18/O19（`while (k-- > 0)` 一族）：头块也带具名写（`k = k - 1`），但**没有**独立
  的 latch 落出（`tail_exits` 为空），头块那条边就是循环条件本身的落出 ⇒ 放行。旧发射器
  原有的 `header_insts_droppable` 已经拦住提升，头块的写回到 `while(1){ if(!(k--)) break; … }`
  形态里按原序发射，实测四档与 gcc 一致。

**这个限定是实测踩出来的，不是推理出来的**：第一轮收窄没带 `!tail_exits.is_empty()`，
当时 `DCC_SSA_BAIL=1` 报出 `共享续体的循环 (f_while_kdec)` —— 即 `while (k-- > 0){…}` 这一整族
（C 里最常见的循环之一）被连带挡掉。那是**相对 HEAD 的覆盖面倒退**（HEAD 只数体外后继，
单出口的 `while(n--)` 本来进 SSA），而且倒退不会让任何 gcc 差分变红（AST 路径照样算对）。
现在由两面捕手钉住：lib 侧把 O11/O17/O18/O19 四个形状放进「必须不挡」表，执行侧在
`plain_loops_still_go_through_ssa` 里加「`while (k-- > 0)` 的 -O2 产物必须与 `DCC_OPT_SKIP=run_ssa_passes`
不同」（逐字节，摘掉 `.file` 行后比较）。

---

## 4. 需要跨所有权代做的（本包不碰）

1. **G2：`goto` 跳到循环后第二个标签 ⇒ -O2/-O3 静默错值，与 SSA 完全无关。**
   复现：`tests/fixtures/opt/loop_exit_ownership_goto_pending.c`（已单独成文件，**故意不入
   全语料门**，免得把一列不属于多出口循环的红扫进门禁）。
   实测归因：gcc `7 7 -3 -3 -3 -3 -3`；dcc -O2/-O3 `7 7 -8 -8 -8 -8 -8`；
   `DCC_OPT_SKIP=run_ssa_passes`（整条 SSA 往返关掉，等价于含 `goto` 函数今天的行为）
   **同样错** ⇒ 病灶在 AST 层 pass（`out:` 的 `return s` 被丢，`out:` 直落进 `late:`），
   不在 `src/ssa.rs`。含 `goto` 的函数本来就被 `is_structured` 挡在 SSA 外。
   需要谁：优化器 AST 层（`src/opt.rs` 的 pass 序列）或 `src/codegen`，二选一由掌握
   那一侧的进程定位。**本包未改动 `src/opt.rs`**（也没改 `src/ssa_passes.rs`）——
   `git status` 可验：本包只动了 `src/ssa.rs`、`tests/integration/opt_ssa_gate.rs`、
   `tests/fixtures/opt/loop_exit_ownership*.c` 与本文件。`has_multi_exit_cycle` 的调用点
   （`src/opt.rs:236`）签名与语义未变，无需跟着改。
2. **全语料门要新增两条**：`tests/fixtures/opt/loop_exit_ownership.c`（放行集，**19** 形）与
   `tests/fixtures/opt/loop_exit_ownership_adv.c`（边界集，**13** 形，其中 `a_do_break` 故意是
   被挡下的形状 —— 收进门禁正好让它同时看住「挡了之后 AST 路径还是对的」）。
   两条都已四档实测与 gcc 一致，可直接收；它们同时也已经有 integration 用例包住
   （`loop_exit_ownership_shapes_match_gcc_at_all_levels` /
   `loop_exit_ownership_boundary_matches_gcc_at_all_levels`）。
   `loop_exit_ownership_goto_pending.c` **先不要收**（见上一条），修好后改名再收。
3. **全语料门本包没跑**（按纪律只跑单夹具四档）：`scripts/check_opt_equivalence.sh` 归你在
   集成阶段跑。需要特别看的一类读数：本轮收窄后仍有四个挡点（§3.3），它们只会让函数
   **掉出 SSA**（产物变笨，不会变错）；如果门里出现值错，优先查新形状是否撞进了「旋转
   do-while + 头块具名写」那一族的变体（§2.5）。
4. **与本包无关的唯一红**：`seg_qualifier_parses_all_positions_and_emits_segment_prefixes`
   （`tests/integration/c_lang.rs:557`，夹具自检 rc=-1）——`__seg_*` 那条线在途
   （`src/driver.rs`、`src/codegen/**`、`tests/integration/c_link_debug.rs`、
   `tests/fixtures/c/seg_object_body.c` 都在改动集里），按所有权交给那边的进程。

