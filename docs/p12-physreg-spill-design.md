# P12 字面形态设计：位置进 IR、codegen 不再分配、真 SPILL/RELOAD

> **路径勘误（2026-10，SSA 目录化）**：本文正文里的旧路径**不改写**（它们记的是当时的事实）。
> 新位置：`src/ssa.rs` → `src/ssa/`（一分为十二）、`src/ssa_regalloc.rs` → `src/ssa/regalloc.rs`、
> `src/ssa_passes.rs` → `src/ssa/passes.rs`、`ssa_gvn` → `src/ast_gvn.rs`。
> 完整对照表与验收实测见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

> **状态：设计完成，未开工。** 本文不是待办清单，而是"要不要做、真要做就一次做对"的
> 实施设计。开工时机由 §9 的触发条件决定，不由日历决定。
>
> 取数时点：HEAD `964d757`，2026-10。§2 的每个数字都附复取命令（§10），隔段时间
> 再读必须先复核（行数与调用点会变）。
>
> 与既有记录的关系：P12 的**增量形态**（在 IR 上算活性、结果灌进绑定表）已经落地并被
> 产物差异验证，见 `docs/ssa-pass-upgrade-plan.md` §7.15 ~ §7.20。本文要解决的是它
> 明确没有做的事：方案原文的字面形态。§7.3 当年写的"前提不成立、需后端级重写"，
> 本文把那个前提拆成四条可分别攻克的硬约束（§3）。

## 1. 目标与非目标

**目标（每条都是可判定的）**

1. 「谁住在哪个物理寄存器」只有**一处决定**，且预算不足时能**溢出**（真 SPILL/RELOAD），
   而不是今天这种"绑不上就一辈子住栈槽"的静态边界。
2. codegen 不再为**变量与 SSA 临时值**分配位置：它只消费已定好的位置表。
3. caller-saved 与 callee-saved 都进入能力范围。今天自动分配只能碰 4 个 callee-saved
   （`rbx/r12/r14/r15`，`r13` 是帧基址），§7.15 把这条边界写死在注释里，理由就是
   "没有干涉模型"。
4. 序言/尾声保存、表达式求值、内联汇编规划器、DWARF 变量位置这四类消费者读到的
   寄存器事实**同源**。

**非目标（本方案绝不做）**

- 不做图着色/自顶向下分裂合并那一整套。本方案要的是**有干涉模型 + 能溢出**；
  评分、寄存器分裂、coalescing 留给后续独立设计。把它们写进来只会让第一阶段无法验收。
- 不重写 aarch64 / riscv64 的发射器（§2 表里它们的变量模型连 callee-saved 池都没有）。
  只要求新抽象**不把 x86_64 的细节变成前端契约**。
- 不改优化 pass 的顺序与语义（除分配结果本身带来的产物差异）。
- 不把寄存器号写进 `ast::Ident`（§4 方案 A 的否决理由）。

## 2. 现状盘点（实测）

| 事实 | 实测值 | 位置 |
|---|---|---|
| P12 增量（活性区间 → 绑定表） | 已落地，产物差异已验证 | `src/ssa_regalloc.rs` 973 行 |
| 寄存器所有权的现存事实 | **3 处**：`reg_bindings`（名字→号+类型）、`reg_allocated`（临时值池 4 位位图）、`callee_saved_used`（序言 4 位位图）；另有 asm 规划器自己的 `used_gp/clobber_gp` 集合 | `codegen.rs:193/283`、`instructions.rs:1440/1452/1465`、`asm/planner/orchestration.rs:29-36` |
| callee-saved 池 | 4 个，注释写明"绑到 caller-saved 不在这道门的能力之内" | `ssa_regalloc.rs:57` POOL、`instructions.rs:1438-1444` |
| asm GP 池 / legacy 池 | 9 个 caller-saved / 6 个 legacy；**>9 个 GP 操作数会复用同一个号，是已记录的残余错值** | `asm/planner/orchestration.rs:42-52`、`alloc_gp` 注释 |
| 表达式求值模型 | `%rax` 累加器 + `self.push()/self.pop()` **162 处** | `src/codegen/x86_64/`（§10 命令 4） |
| AST 里"号"的载体 | `ast::Ident { name, span }`，**无寄存器字段**；`Ident {` 构造点 659 处 | `src/ast.rs:9-12` |
| 帧的时序 | `reserve_late_var_slots()` → `frame_size` → `emit_sub_rsp(frame_size)` 都发生在**函数体生成之前**，而 `register_var` 在生成中还会长槽位 | `emit/functions.rs:594 → 634 → 645` |
| 帧布局是否有解算阶段 | **没有**。x86_64 的 `abi/frame.rs` 只有 91 行发射助手（`emit_sub_rsp` 等），偏移由各消费者按 `%r13+disp` 现场推算；mips 反而有一个 `FrameLayout` 结构可参考 | `src/codegen/x86_64/abi/frame.rs`、`src/codegen/mips/abi/mod.rs:394` |
| 序言保存的时序 | 无条件先发 4 条 save，函数体生成完之后 `patch_callee_saves()` 把没用到的改成 nop | `emit/func_setup.rs:83`、`emit/functions.rs:1155-1158` |
| 绑定路径宽度口径 | 与栈槽路径已对齐（`int`/`char` 可安全进绑定表） | §7.16 |
| `-g` 与分配互斥 | DWARF 只会发栈槽位置 ⇒ 开着 `-g` 时 P12-A 整条不动 | `ssa_regalloc.rs` 头注释准入条件 6 |
| 现有可依赖的测试用例 | rex_matrix 15 矩阵 5652 组合；`check_rex_hygiene.sh` 618 组合；`check_reproducible_output.sh` 5094 组合；`check_opt_equivalence.sh` 611 夹具 × 3 档；`check_fixtures.sh` + `check_fixtures_selftest.sh`；golden-cross arm/mips 6 项 | Makefile / scripts |

```bash
# 复取（只读）
wc -l src/ssa_regalloc.rs src/codegen/x86_64/instructions.rs src/codegen/x86_64/codegen.rs
grep -c "self\.push(\|self\.pop(" $(git ls-files src/codegen/x86_64)
grep -rn "Ident {" src/ --include=*.rs | wc -l
grep -rn "reg_bindings\|reg_allocated\|callee_saved_used" src/codegen/x86_64 --include=*.rs | wc -l
```

## 3. 为什么这是发射模型级重写：四条硬约束

这四条就是 §7.3 那句"前提不成立"的**全部**内容。它们各自都能单独攻克，但没有一条
能靠"改两行"绕过去；反过来说，只要这四条被逐个闭合，字面形态就不是口号而是产物。

**H1 —— 家必须在发射前定完，可今天可以在生成中继续长。**
`frame_size` 在函数体之前算并发射 `sub rsp`，而 `register_var()` 在生成复合字面量临时值、
VM 尺寸元信息时才把槽位加上来（今天靠 `reserve_late_var_slots()` 预扣兜住）。
溢出（SPILL）意味着**新增**家，而且是在看到全部代码之前不知道该加几个 ⇒ 必须要么
先做一遍布局解算（Phase 2），要么允许帧随生成增长（不可接受：所有 `%r13+disp` 会漂）。

**H2 —— 表达式栈机没有干涉概念。**
162 处 `push/pop` 的含义是"任意程序点上，rax 和栈顶槽都可能被人握着"。§7.15 之后
把绑定同时登记进两个位图，才第一次让"变量家"和"表达式溅出槽"不打架；但那是**位图**，
只覆盖 4 个 callee-saved。把 caller-saved 纳进来需要的是一张真正的干涉表
（哪个值在哪段代码上活到哪个号上），asm 规划器的注释已经用实测证明"直接扩池"会失败：
"操作数求值、写回与表达式求值之间对哪些寄存器可用的假设有重叠，扩池会打破它，
病灶未定位"。

**H3 —— 分配结果晚于编码。**
序言是先发后 patch（`patch_callee_saves`）。这是 H1 的镜像问题：单遍发射下，
"这个函数最终用哪几个 callee-saved"只有走完函数体才知道。真分配器要求
**要么两趟，要么把长度可变的编码留成占位**。选哪条决定后面所有阶段的形状，
所以必须在开工前定（§6 决定 D2）。

**H4 —— 所有权判据已经有四套。**
`reg_bindings` / `reg_allocated` / `callee_saved_used` / asm 规划器的 `used_gp+clobber_gp`。
§7.15 的结论可直接引用：**所有权判据一旦有两套，弱的那一套决定产物**。今天靠
`claim_bound_register()` 与 `mark_unavailable_gp()` 手工把四处对齐；分配器一旦开始
动态借用/释放寄存器，手工对齐必然漏点。所以第一件事不是"分配得更聪明"，而是把
所有权收成一个对象（Phase 1），且这一步**必须零行为 diff**。

## 4. 三种候选形态与选定

| 方案 | 内容 | 结论 |
|---|---|---|
| **A 字面照做** | `ast::Ident` 增加 `reg: Option<u8>`，重建时把号写进每个 `Ident`，codegen 见号即用 | **否。** ① 659 处构造点 + 三后端共享 + `ssa_gvn`/`loop_opt`/`sanitizer`/`cpp/native.rs` 都在**克隆与改写 AST**，任何一处漏拷贝就是"两个同名 Ident 带不同号"——与 §7.36 记的"地址临时值被内联进取址操作数"同一族错误，且比它更难测。② 号是后端事实，写进前端契约后 aarch64/riscv64 只能"接受但忽略"，那正是本仓最反对的影子语义。③ 一个变量在不同程序点可能需要不同位置（溢出/重载后），"Ident 携带号"表达不了"何时在哪里"，只能表达"叫什么"。 |
| **B 位置随 IR 走** | SSA IR 的每个 dest 值有一个 `Location`；分配器在 IR 上算；重建时整张表交给后端；`register asm` 变成"用户显式指定 Location"的同一条路 | **选定。** 它拿到字面形态的全部**实质价值**（分配决定产物、能溢出、codegen 不再分配），而不把后端细节塞进 AST。表是每函数一张、随 CFG 一起更新，改名/插指令/复制 pass 都必须显式处理它 —— 编译器会点名，不像 AST 字段那样静默存活。 |
| **C 继续扩绑定表**（P12-A 延长线） | 加更多变量进 `reg_bindings`，加更多寄存器进池 | **否。** 达不到目标 1/3：溢出发生在表达式中间而不是变量层，而 `reg_bindings` 的粒度是"名字全程住一个号"；caller-saved 的边界写在注释里，不是工程疏忽。 |

## 5. 选定方案的数据模型

```
// src/ssa.rs（IR 侧，不新文件）
pub enum Location {
    Stack(i64),        // %r13+disp（后端各自的帧表示由 Home 抽象承接）
    Reg(u8),           // 物理寄存器号，编号域是后端自己的
    Spill(i64),        // 分配器保留的溢出槽，与变量槽不同区
    Imm(i128),         // 常量家（可选，Phase 5 才用得上）
}
pub struct Function { ..., pub value_loc: HashMap<String /*SSA 名*/, Location> }
```

职责边界（单一真源）：

| 职责 | 归属 |
|---|---|
| 算 Location（活性 + 干涉 + 溢出决策） | `src/ssa_regalloc.rs`（升级为真正的 allocator） |
| 携带 Location | `ssa::Function::value_loc`（IR），随 SSA→AST 重建一并交付 |
| 消费 Location | `mem/access.rs`（读写一个值的家）、序言/尾声、asm 规划器、DWARF |
| 寄存器占用真相 | Phase 1 引入的 `RegBank`（claim/release/free-set/池定义），上面四类**全部**由它派生 |
| 用户显式指定 | `register asm("rN")` 与 file-scope 绑定：解析成 `Location::Reg`，走同一条通道，不再是 `register_name` 的独立分支 |

表达式中间值在 Phase 4 之前仍走栈机；Phase 4 的判据就是**取消**这条特权：
`push/pop` 的槽必须在解算阶段可数，之后不再新增家。

## 6. 关键设计决定（含被否与必须先回答的问题）

| 编号 | 决策 | 选择 | 理由 / 被否掉的替代 |
|---|---|---|---|
| D1 | Location 放 AST 还是 IR | **IR** | 见 §4 方案 A；且 IR 有 CFG 与 phi，才谈得上干涉 |
| D2 | 单趟 + patch，还是两趟 | **两趟**（先解算后装填） | 保留"先发后 patch"就意味着溢出槽位永远受"生成期不得新增家"约束（H1）；而 patch 长度可变的 spill 代码会作废此前所有 pc 相对量 —— 与 `codegen 偏移必须单一真源` 那条冲突（变长 ISA 必须整 TU 降级） |
| D3 | 分配粒度 | 先在 **SSA 值**上分配，保留"同基名版本可共用一个家"的保守退化 | 今天 P12-A 只能在**基名**粒度上分配（重建会把版本塌缩回同一个可变量）。不解决塌缩就无法谈真干涉；因此 Phase 3 之前先把"重建时按 Location 生成真局部"立起来，否则分配器再聪明也白搭 |
| D4 | caller-saved 何时开放 | Phase 5，且必须在干涉表之后 | §7.15 的实测：表达式求值每时每刻都在用它们 |
| D5 | `-g` 下怎么办 | 先发 `DW_OP_regx`/`DW_OP_GNU_regX`+ 溢出位置的 `DW_OP_GNU_entry_value` 之前，**继续整条关闭 P12**，并把这条写成显式契约（不是"遗漏"） | 绑上去而 DWARF 只发栈槽 = 调试器读一个从未写入的槽 |
| D6 | 与内联汇编的关系 | 规划器的可用集合由 `RegBank` 给出，禁止自己维护第二份"谁占着哪个号"；asm 操作数的**暂存约定重写**是 Phase 6 的前置，不在 Phase 1 顺手做 | 现在 `mark_unavailable_gp` 只是"转达事实"，扩池被实测否决 ⇒ 说明暂存约定与占用模型是两件事 |
| D7 | 验收口径 | 每个"纯搬家/纯抽象"阶段要求**产物逐字节不变**；每个"真改分配"阶段要求**产物变化可解释到指令**且三档 OPT-DIFF 0、gcc 对照绿 | §7.5 的教训：用产物差异证明生效，不用"接线完成"证明 |
| D8 | 与 SSA 目录化方案的关系 | 若 `docs/ssa-module-layout-plan.md` 先落地，`value_loc` 落在 `ssa/` 里更自然；若本方案先做，则 Phase 1/2 的新文件按目录规范放，避免再制造 `src/ssa*` 平铺 | 两份设计都改同一批文件，先后只影响搬动成本，不影响正确性 |

**开工前必须先回答的三个问题**（答不出来就还没到开工的时候）：

1. 溢出槽归谁预留、放在帧的哪一段？（与 `sret_region_base`、`vec_temp_off`、
   `CX_FRAME_BAND`、`bf_rmw_reserve` 的布局关系必须一次说清，否则就是第八个各自推算
   偏移的消费者。）
2. 表达式求值的"家"是否纳入 Location？不纳入则目标 2 只能声明为"变量与 SSA 临时值
   不再由 codegen 分配"，文档与判据都要按这个措辞；纳入则 Phase 4 的工作量是栈机重写。
3. `Location::Reg` 的编号域：全局唯一（后端各自解释）还是分 bank（GP/XMM/x87）？
   建议分 bank，否则 SSE 与 x87 通道会被迫共用一张表，而今天绑定表明确只支持 GP。

## 7. 实施步骤

每步通用形状：**做完 → 编译 0 warning → 跑测试用例 → 与上一步基线做产物字节对照 →
不符预期就回退**。构建与测试一律 `-j2` / `--test-threads=2`。

### Phase 0 —— 基线与量化（不改行为）

0.1 取三档基线存进 `.work_tmp/p12_base/`：`cargo test` 结果、
`check_reproducible_output.sh`、`check_opt_equivalence.sh`、`check_disasm_coverage.sh`、
`golden`、`golden-cross`，以及每函数 `DCC_REGALLOC_DUMP=1` 的绑定/拒绝原因分布。
0.2 只读量化脚本（bash）：全语料 `push/pop` 嵌套深度分布、每函数最大同时活标量数
（IR 上算）、callee-saved 池命中率、"因为池满而没绑"的占比。产出两张表进本文 §2。
0.3 建 `git branch backup/pre-p12-rewrite`。
**判据**：没有产物变化；两张量化表 + 基线目录。**这一步的产物就是"值不值得干"的答案**：
如果"因池满而拒绑"的占比接近 0，Phase 5 的溢出收益就只是纸面收益，应停在 Phase 4。

### Phase 1 —— 所有权模型合一（要求零行为 diff）

1.1 新增 `RegBank`（放 `src/codegen/x86_64/abi/`，池定义 + claim/release + 空闲集查询 +
"分 bank"接口），把 `reg_allocated`、`callee_saved_used` 两个位图的**含义**吸收进去，
位图字段保留但只作为 `RegBank` 的内部表示。
1.2 `claim_bound_register()` / `alloc_callee_saved()` / `free_callee_saved()` /
`reset_reg_tracker()` 改为 `RegBank` 的薄封装（调用点不动，先建边界）。
1.3 asm 规划器的 `used_gp/clobber_gp` 初始化改为从 `RegBank` 一次性取，删掉
`mark_unavailable_gp` 的手工转达调用点。
1.4 单测：同一份绑定 + 同一棵表达式树下，`RegBank` 给出的空闲集与旧三处判据逐位相同。
**判据**：5652 矩阵组合 + 618 产物组合 + 5094 可复现组合 + 三档 OPT-DIFF 全部逐字节
不变。**任一产物变化都说明"合一"偷偷改了行为**，必须逐条解释到行；解释不清楚就回退。

### Phase 2 —— 两趟骨架：布局解算独立（要求零行为 diff）

2.1 抽出 `fn resolve_frame_layout(...) -> LayoutMap`（今天不存在这一步，见 §2 最后一行）：
变量、参数、
`sret`/向量/复数/位域/trampoline/静态链 各带，**并预留 spill band**（大小取 Phase 0
量化出的上界，先固定，Phase 5 再改成按分配结果定）。
2.2 `register_var()` 的生成期分配路径改为"只能从已预留区里取"，取不到 ⇒ 硬错误，
不允许像今天这样静默把帧往下长（那是 H1 的根）。
2.3 帧发射（`emit_sub_rsp`）与所有 `%r13+disp` 消费者改读同一份 `LayoutMap`。
**判据**：产物逐字节不变（spill band 预留不影响代码，除非帧大小变化 ⇒ 若变了，必须
解释为什么，并把差异记进 §7 的 Phase 2 结论）；`stack_clash_protection`/
`auto_var_init_zero` 两条按帧大小分支的路径必须在新布局下重测。

### Phase 3 —— Location 进 IR（要求产物不变）

3.1 `ssa::Function` 增加 `value_loc`；`ssa_regalloc` 从"写 `register_name`"改为
"填 `value_loc`"；`opt.rs` 里重建前把表交给 `Codegen`（x86_64 先实现，其余后端
显式声明"忽略 Location"，并加一条断言：非默认 Location 出现在未支持后端时硬错误，
不允许静默降级）。
3.2 `VarDecl::register_name` 变成 Location 的**用户输入通道**之一（解析阶段
`register asm("rbx")` → `Location::Reg`），删除 `emit/stmt.rs`、`codegen.rs:1077`、
`elf/rel_symbols.rs:97` 三处对 `register_name` 的独立判断，统一查 `value_loc`。
3.3 穷举点检查：IR 新增字段后，所有"复制 Function / 复制 BasicBlock"的 pass
（GVN、SCCP、展开、流水落地、EH rename）必须显式处理它 —— 让编译器点名，不留兜底。
**判据**：**产物与 P12-A 逐字节相同**（这一步只是搬家，改了分配就是错）。这是整个方案
里最强的一条等价性证明，也是 D7 的第一次兑现。

### Phase 4 —— codegen 不再分配变量的家

4.1 `mov_to_stack`/`mov_from_stack`/`load_named`/地址形式（`AddrOf`/`MemberAddr`/
`IndexAddr`）一律按 Location 走，删掉"先查 var_map 再查绑定表"的双轨顺序。
4.2 表达式 `push/pop` 的槽位在解算阶段可数（每个表达式节点深度静态可算），
生成期不再新增家。
4.3 打开 `-g` 的守卫保持不变（D5），但改由 Location 层判断而不是 pass 入口跳过。
**判据**：产物预期仍不变（分配结果此时仍等价于 P12-A）；`DCC_REGALLOC_DUMP` 增加
"每个值的家"视图；测试用例全绿。

### Phase 5 —— 真 SPILL/RELOAD + caller-saved

5.1 `ssa_regalloc` 升级为带干涉的 allocator：按 bank 分配（GP 先），预算不足时把
可溢出值落到 spill band，产生 `Location::Spill`，reload 点由后端在读取该值时插入
（宽度口径必须复用 §7.16 那一条真源，不能新写一套截断规则）。
5.2 干涉对象必须包括：本条 asm 的操作数与 clobber、嵌套函数按帧偏移访问外层变量的
那条路（P12-A 的准入条件 4）、异常落地垫的寄存器约定。
5.3 度量：spill 次数、寄存器借用深度进 `DCC_REGALLOC_DUMP`；没有下降数据的收益不算收益。
**判据**：产物**会变**（这是预期），所以必须新增常驻夹具：深表达式（≥5 层嵌套二元式）、
循环携带变量数 > 池大小的循环、绑定变量与内联 asm 混用、`register asm` 与自动分配
争用同一个号；全部逐档与 gcc 对照 + 反向对照（关掉分配的产物与打开的产物差异必须
出现在预期的指令上）。三档 OPT-DIFF 必须仍为 0；`check_opt_equivalence.sh` 的
COMPILE-FAIL 分桶不得新增 CF-UNKNOWN。

### Phase 6 —— 与相邻开放项合流（各自独立立项）

6.1 asm 操作数暂存约定重写（§7.15 记录的残余错值：>9 个 GP 号会复用）。
6.2 DWARF 寄存器位置表达式，之后取消"开 `-g` 就关掉 P12"这条边界。
6.3 P16 软件流水的真轮转寄存器：解除"同基名无法同时保留两份 ⇒ 保守拒收"
（`docs/ssa-pass-upgrade-plan.md` §7.20/§7.35），这是本方案最直接的下游收益。
6.4 aarch64 / riscv64 是否跟进：在它们的 `Home` 抽象上明确"只支持栈"，并让
Location 的非栈取值在这两个后端触发显式硬错误而不是静默忽略。

## 8. 验收清单（全绿才算完成）

- [ ] Phase 1/2/3/4 各自满足"产物逐字节不变"，且每一步都有对应的字节对照记录（不是"跑过了"）。
- [ ] `cargo test --release -j2 -- --test-threads=2` 全绿；新增用例：`RegBank` 空闲集等价、
      Location 视图、Phase 5 的溢出形态逐档 gcc 对照。
- [ ] `check_fixtures.sh` / `check_fixtures_selftest.sh` / `test_all.sh` /
      `check_opt_equivalence.sh`（OPT-DIFF=0，CF-UNKNOWN=0）/ `check_reproducible_output.sh` /
      `check_rex_hygiene.sh` / `check_pipe_structure.sh` / `check_disasm_coverage.sh` /
      `make golden golden-cross` 全绿。
- [ ] 内核全树构建（`make kernel-full-build`）与 QEMU 启动矩阵不退化——P12 改的是所有
      函数的前缀与帧布局，这是唯一能覆盖真实规模的路径。
- [ ] `docs/ssa-pass-upgrade-plan.md` 状态表里 P12 行从"◐ 增量已落地"改写为字面形态的
      实际达成范围，§7.3 的"前提不成立"就地标注为已闭合或被哪条约束继续挡住。
- [ ] §6 的三个必答问题在开工评审时已有书面答案，且写回本文（不允许"边做边定"）。

## 9. 什么时候才值得启动（触发条件）

P12 字面形态不产生新的语言能力，只产生性能与"分配器自主权"，因此**由事件触发**：

1. P16 需要"多形态可流水"——今天因"同基名不能同时保留两份寄存器值"而保守拒收的循环
   （§7.35）是真实收益缺口，且它就在优化器路线图上。
2. 内联汇编操作数多于 9 个 GP 号的**残余错值**必须修（§7.15/orchestration 注释）——
   修它需要真正的占用/暂存模型，与本方案同源。
3. 内核某类构建真的需要 caller-saved 绑定（今天只能 callee-saved 4 个）。
4. `-g` 下要有寄存器分配（Phase 6.2 之后）。

若只是"方案原文这么写"，答案是**不做**：P12-A 已经拿到"分配结果进入产物且正确"这一
实质价值，而字面形态的成本是两条后端级重写（H1/H2）加一次全语料对撞。

## 10. 数据复取

```bash
cd /root/dcc
wc -l src/ssa_regalloc.rs src/codegen/x86_64/instructions.rs src/codegen/x86_64/codegen.rs
grep -rn "reg_bindings\|reg_allocated\|callee_saved_used" src/codegen/x86_64 --include=*.rs
grep -c "self\.push(\|self\.pop(" src/codegen/x86_64/emit/expr/*.rs src/codegen/x86_64/*.rs
grep -rn "Ident {" src/ --include=*.rs | wc -l
sed -n '580,650p' src/codegen/x86_64/emit/functions.rs     # 帧时序
sed -n '77,100p' src/codegen/x86_64/emit/func_setup.rs    # patch_callee_saves（“单遍编译下序言先于函数体发射”）
sed -n '40,60p' src/codegen/x86_64/asm/planner/orchestration.rs   # 池与被否的扩池
sed -n '1,20p' src/codegen/x86_64/abi/frame.rs           # 今天只有发射助手，没有布局解算
DCC_REGALLOC_DUMP=1 ./target/release/dcc -O2 -c tests/fixtures/c/register_binding_width.c -o /dev/null
```

若复核后的数字与本文不符，**以复测为准**并回来修订本文（写明修订日期与差异）。
